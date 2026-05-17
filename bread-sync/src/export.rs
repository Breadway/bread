use anyhow::{Context, Result};
use chrono::Utc;
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{expand_path, SyncConfig};
use crate::delegates::sync_dir;
use crate::machine::{hostname, MachineProfile};
use crate::packages;

/// Maps a staged path back to the original absolute path on the source machine.
/// Drives the import — no hardcoded paths needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRecord {
    /// Relative path within the export (e.g. "configs/hypr").
    pub staging: String,
    /// Original path with `~` (e.g. "~/.config/hypr").
    pub original: String,
    /// Whether this is a single file (false = directory).
    #[serde(default)]
    pub is_file: bool,
}

/// A git repository found on the machine, keyed by its remote URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoRecord {
    /// Path relative to $HOME (e.g. "Projects/bread").
    pub path: String,
    /// Remote URL (e.g. "https://github.com/Breadway/bread.git").
    pub remote: String,
    /// Branch that was checked out at export time.
    pub branch: String,
}

/// Manifest stored in the export root as `manifest.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportManifest {
    pub version: u32,
    pub machine: String,
    pub hostname: String,
    pub exported_at: String,
    /// Explicit staging→original path map for all captured items.
    #[serde(default)]
    pub path_map: Vec<PathRecord>,
    /// High-level list of config dir names (for display).
    pub configs: Vec<String>,
    /// Git repos found on the source machine.
    #[serde(default)]
    pub repos: Vec<GitRepoRecord>,
    pub system: bool,
    pub packages: Vec<String>,
    // Legacy fields kept for forward compat (ignored on import)
    #[serde(default)]
    pub bread: bool,
    #[serde(default)]
    pub dotfiles: Vec<String>,
    #[serde(default)]
    pub local_bin: Vec<String>,
    #[serde(default)]
    pub systemd_units: Vec<String>,
}

/// Config directories always included in the export (if they exist on disk).
static BUILTIN_CONFIGS: &[(&str, &str)] = &[
    ("hypr", "~/.config/hypr"),
    ("fish", "~/.config/fish"),
    ("kitty", "~/.config/kitty"),
    ("nvim", "~/.config/nvim"),
    ("ags", "~/.config/ags"),
    ("wofi", "~/.config/wofi"),
    ("waybar", "~/.config/waybar"),
    ("dunst", "~/.config/dunst"),
    ("mako", "~/.config/mako"),
    ("hyprlock", "~/.config/hyprlock"),
    ("hyprpaper", "~/.config/hyprpaper"),
    ("swaylock", "~/.config/swaylock"),
    ("wlogout", "~/.config/wlogout"),
    ("swappy", "~/.config/swappy"),
    ("btop", "~/.config/btop"),
    ("waypaper", "~/.config/waypaper"),
    ("wal", "~/.config/wal"),
    ("gtk-3.0", "~/.config/gtk-3.0"),
    ("gtk-4.0", "~/.config/gtk-4.0"),
    ("keyd", "~/.config/keyd"),
    ("autostart", "~/.config/autostart"),
];

/// Standalone dotfiles captured as individual files: (staging-name, source-path).
static BUILTIN_DOTFILES: &[(&str, &str)] = &[
    (".gitconfig", "~/.gitconfig"),
    ("user-dirs.dirs", "~/.config/user-dirs.dirs"),
    ("mimeapps.list", "~/.config/mimeapps.list"),
    ("ssh_config", "~/.ssh/config"),
    (".zshrc", "~/.zshrc"),
    (".zprofile", "~/.zprofile"),
    (".zshenv", "~/.zshenv"),
];

/// System-level directories. World-readable ones are copied directly;
/// root-only ones (networkmanager, bluetooth) require running with sudo.
static SYSTEM_PATHS: &[(&str, &str)] = &[
    ("udev", "/etc/udev/rules.d"),
    ("modprobe", "/etc/modprobe.d"),
    ("sysctl", "/etc/sysctl.d"),
    ("networkmanager", "/etc/NetworkManager/system-connections"),
    ("bluetooth", "/var/lib/bluetooth"),
];

/// Directories excluded from every recursive copy.
static DEFAULT_EXCLUDES: &[&str] = &[
    "**/.git",
    "**/*.cache",
    "**/node_modules",
    "**/@girs",
    "**/__pycache__",
    "fish_variables?*",
];

/// Directories skipped when searching for git repos.
static GIT_SKIP_DIRS: &[&str] = &[
    ".local",
    "Nextcloud",
    "target",
    "node_modules",
    "__pycache__",
    ".cache",
    "snap",
    "flatpak",
    "@girs",
    "Steam",
];

// ── stage_export ────────────────────────────────────────────────────────────

/// Build a self-contained snapshot directory at `staging`.
pub fn stage_export(cfg_dir: &Path, config: &SyncConfig, staging: &Path) -> Result<ExportManifest> {
    fs::create_dir_all(staging)?;

    let excludes: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
    let mut path_map: Vec<PathRecord> = Vec::new();
    let mut included_configs: Vec<String> = Vec::new();

    // Helper: tilde-ify an absolute path for storage in the manifest.
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let tilde = |p: &Path| -> String {
        p.strip_prefix(&home)
            .map(|rel| format!("~/{}", rel.display()))
            .unwrap_or_else(|_| p.display().to_string())
    };

    // 1. Bread config → bread/
    let bread_dest = staging.join("bread");
    sync_dir(cfg_dir, &bread_dest, &excludes).context("failed to snapshot bread config")?;
    path_map.push(PathRecord {
        staging: "bread".to_string(),
        original: tilde(cfg_dir),
        is_file: false,
    });

    // 2. Built-in + delegate configs → configs/<name>/
    let configs_dir = staging.join("configs");

    for (name, raw_path) in BUILTIN_CONFIGS {
        let src = expand_path(raw_path);
        if src.exists() {
            let dst = configs_dir.join(name);
            sync_dir(&src, &dst, &excludes)
                .with_context(|| format!("failed to snapshot {raw_path}"))?;
            path_map.push(PathRecord {
                staging: format!("configs/{name}"),
                original: raw_path.to_string(),
                is_file: false,
            });
            included_configs.push(name.to_string());
        }
    }

    let delegate_paths = crate::delegates::resolve_include_paths(&config.delegates.include);
    for (basename, src_path) in &delegate_paths {
        if src_path.exists() && !included_configs.contains(basename) {
            let dst = configs_dir.join(basename);
            sync_dir(src_path, &dst, &config.delegates.exclude)
                .with_context(|| format!("failed to snapshot delegate {}", src_path.display()))?;
            path_map.push(PathRecord {
                staging: format!("configs/{basename}"),
                original: tilde(src_path),
                is_file: false,
            });
            included_configs.push(basename.clone());
        }
    }

    // 3. Dotfiles → dotfiles/
    let dotfiles_dir = staging.join("dotfiles");
    fs::create_dir_all(&dotfiles_dir)?;

    for (dest_name, raw_path) in BUILTIN_DOTFILES {
        let src = expand_path(raw_path);
        if src.exists() {
            fs::copy(&src, dotfiles_dir.join(dest_name))
                .with_context(|| format!("failed to copy {raw_path}"))?;
            path_map.push(PathRecord {
                staging: format!("dotfiles/{dest_name}"),
                original: raw_path.to_string(),
                is_file: true,
            });
        }
    }

    // 4. ~/.local/bin custom scripts → local-bin/
    // Skip symlinks (point to installed binaries) and files >512 KB (compiled artifacts).
    let local_bin_src = expand_path("~/.local/bin");
    let local_bin_dst = staging.join("local-bin");
    if local_bin_src.exists() {
        fs::create_dir_all(&local_bin_dst)?;
        let mut any = false;
        for entry in fs::read_dir(&local_bin_src).context("failed to read ~/.local/bin")? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.file_type().is_symlink() || meta.len() > 512 * 1024 {
                continue;
            }
            let path = entry.path();
            if path.is_file() {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                fs::copy(&path, local_bin_dst.join(&name))?;
                any = true;
            }
        }
        if any {
            path_map.push(PathRecord {
                staging: "local-bin".to_string(),
                original: "~/.local/bin".to_string(),
                is_file: false,
            });
        }
    }

    // 5. ~/.local/share/fonts → local-fonts/
    let fonts_src = expand_path("~/.local/share/fonts");
    let fonts_dst = staging.join("local-fonts");
    if fonts_src.exists() {
        sync_dir(&fonts_src, &fonts_dst, &excludes).context("failed to snapshot fonts")?;
        path_map.push(PathRecord {
            staging: "local-fonts".to_string(),
            original: "~/.local/share/fonts".to_string(),
            is_file: false,
        });
    }

    // 7. ~/.config/systemd/user → systemd/
    let systemd_src = expand_path("~/.config/systemd/user");
    let systemd_dst = staging.join("systemd");
    if systemd_src.exists() {
        sync_dir(&systemd_src, &systemd_dst, &excludes)
            .context("failed to snapshot systemd user units")?;
        path_map.push(PathRecord {
            staging: "systemd".to_string(),
            original: "~/.config/systemd/user".to_string(),
            is_file: false,
        });
    }

    // 8. System configs → system/ (read-only; restore needs sudo)
    let system_dst = staging.join("system");
    let mut has_system = false;
    for (name, raw_path) in SYSTEM_PATHS {
        let src = PathBuf::from(raw_path);
        if !src.exists() {
            continue;
        }
        match sync_dir(&src, &system_dst.join(name), &excludes) {
            Ok(_) => has_system = true,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("Permission denied") || msg.contains("permission denied") {
                    eprintln!(
                        "bread: warning: {raw_path} requires sudo to export (skipping — re-run with sudo to include)"
                    );
                } else {
                    eprintln!("bread: warning: failed to snapshot {raw_path}: {e}");
                }
            }
        }
    }

    // 9. Package snapshots → packages/
    let packages_dir = staging.join("packages");
    let mut included_managers: Vec<String> = Vec::new();
    if config.packages.enabled {
        for manager in &config.packages.managers {
            let dest_file = packages_dir.join(format!("{manager}.txt"));
            match packages::snapshot(manager, &dest_file) {
                Ok(true) => included_managers.push(manager.clone()),
                Ok(false) => {}
                Err(e) => eprintln!("bread: warning: package snapshot for {manager} failed: {e}"),
            }
        }
    }

    // 10. Machine profile → machines/
    let machines_dir = staging.join("machines");
    MachineProfile::new(config.machine.name.clone(), config.machine.tags.clone())
        .write(&machines_dir)?;

    // 11. Git repositories — find all repos with a remote, commit+push each
    let nc_dirs = nextcloud_sync_dirs(&home);
    if !nc_dirs.is_empty() {
        let labels: Vec<_> = nc_dirs
            .iter()
            .map(|p| {
                p.strip_prefix(&home)
                    .map(|r| format!("~/{}", r.display()))
                    .unwrap_or_else(|_| p.display().to_string())
            })
            .collect();
        eprintln!(
            "bread: skipping Nextcloud-tracked folders: {}",
            labels.join(", ")
        );
    }
    let repos = find_git_repos(&home);
    commit_and_push_repos(&repos, &home);

    // 12. Manifest
    let manifest = ExportManifest {
        version: 2,
        machine: config.machine.name.clone(),
        hostname: hostname(),
        exported_at: Utc::now().to_rfc3339(),
        path_map,
        configs: included_configs,
        repos,
        system: has_system,
        packages: included_managers,
        bread: true,
        dotfiles: vec![],
        local_bin: vec![],
        systemd_units: vec![],
    };
    fs::write(
        staging.join("manifest.toml"),
        toml::to_string_pretty(&manifest).context("failed to serialize manifest")?,
    )?;

    // 11. restore.sh
    let restore_path = staging.join("restore.sh");
    fs::write(&restore_path, generate_restore_sh(&manifest))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&restore_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&restore_path, perms)?;
    }

    Ok(manifest)
}

// ── apply_import ────────────────────────────────────────────────────────────

/// Apply a staged snapshot directory to this machine.
/// Returns a list of human-readable descriptions of what was applied.
pub fn apply_import(
    staging: &Path,
    cfg_dir: &Path,
    install_packages: bool,
    clone_repos: bool,
) -> Result<Vec<String>> {
    let mut applied: Vec<String> = Vec::new();

    // Read manifest to get the path map
    let manifest_path = staging.join("manifest.toml");
    let path_map: Vec<PathRecord> = if manifest_path.exists() {
        let raw = fs::read_to_string(&manifest_path)?;
        toml::from_str::<ExportManifest>(&raw)
            .map(|m| m.path_map)
            .unwrap_or_default()
    } else {
        vec![]
    };

    if !path_map.is_empty() {
        // Manifest-driven restore: use path_map for exact original locations
        for record in &path_map {
            let src = staging.join(&record.staging);
            if !src.exists() {
                continue;
            }
            let dst = expand_path(&record.original);

            if record.is_file {
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                // Secure directory permissions for SSH
                if record.staging.contains("ssh_config") {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Some(p) = dst.parent() {
                            if let Ok(m) = fs::metadata(p) {
                                let mut perms = m.permissions();
                                perms.set_mode(0o700);
                                let _ = fs::set_permissions(p, perms);
                            }
                        }
                    }
                }
                fs::copy(&src, &dst)
                    .with_context(|| format!("failed to restore {}", record.original))?;
                applied.push(record.original.clone());
            } else {
                sync_dir(&src, &dst, &[])
                    .with_context(|| format!("failed to restore {}", record.original))?;
                applied.push(record.original.clone());

                // Reload systemd if this was the systemd dir
                if record.staging == "systemd" {
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "daemon-reload"])
                        .status();
                }

                // Rebuild font cache after restoring fonts
                if record.staging == "local-fonts" {
                    let _ = std::process::Command::new("fc-cache").arg("-f").status();
                }

                // Make local-bin scripts executable
                if record.staging == "local-bin" {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(entries) = fs::read_dir(&dst) {
                            for entry in entries.filter_map(|e| e.ok()) {
                                if entry.path().is_file() {
                                    if let Ok(m) = fs::metadata(entry.path()) {
                                        let mut perms = m.permissions();
                                        perms.set_mode(perms.mode() | 0o111);
                                        let _ = fs::set_permissions(entry.path(), perms);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        // Legacy fallback for v1 exports without path_map
        let bread_src = staging.join("bread");
        if bread_src.exists() {
            sync_dir(&bread_src, cfg_dir, &[])?;
            applied.push("~/.config/bread".to_string());
        }
        let configs_dir = staging.join("configs");
        if configs_dir.exists() {
            let config_home = expand_path("~/.config");
            for entry in fs::read_dir(&configs_dir)?.filter_map(|e| e.ok()) {
                let src = entry.path();
                if src.is_dir() {
                    let name = src.file_name().unwrap().to_string_lossy().to_string();
                    sync_dir(&src, &config_home.join(&name), &[])?;
                    applied.push(format!("~/.config/{name}"));
                }
            }
        }
    }

    // Package installs
    if install_packages {
        let packages_dir = staging.join("packages");
        if packages_dir.exists() {
            install_packages_from(&packages_dir)?;
            applied.push("packages installed".to_string());
        }
    }

    // Clone git repos
    if clone_repos {
        let manifest_path = staging.join("manifest.toml");
        if manifest_path.exists() {
            let raw = fs::read_to_string(&manifest_path)?;
            if let Ok(manifest) = toml::from_str::<ExportManifest>(&raw) {
                let home = dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()));
                for repo in &manifest.repos {
                    let dest = home.join(&repo.path);
                    if dest.exists() {
                        applied.push(format!("skip (exists): ~/{}", repo.path));
                        continue;
                    }
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    eprint!("  cloning ~/{} ... ", repo.path);
                    let status = std::process::Command::new("git")
                        .args(["clone", "--branch", &repo.branch, &repo.remote])
                        .arg(&dest)
                        .status();
                    match status {
                        Ok(s) if s.success() => {
                            eprintln!("done");
                            applied.push(format!("cloned ~/{}", repo.path));
                        }
                        _ => {
                            eprintln!("failed");
                            applied.push(format!("clone failed: ~/{}", repo.path));
                        }
                    }
                }
            }
        }
    }

    Ok(applied)
}

// ── commit_and_push_repos ───────────────────────────────────────────────────

fn commit_and_push_repos(repos: &[GitRepoRecord], home: &Path) {
    if repos.is_empty() {
        return;
    }
    eprintln!("bread: committing and pushing {} repo(s)...", repos.len());
    for repo in repos {
        let dir = home.join(&repo.path);
        let dir_str = dir.to_string_lossy();

        // Stage all changes
        let add = std::process::Command::new("git")
            .args(["-C", &dir_str, "add", "-A"])
            .output();
        if add.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("  ~/{}: git add failed, skipping", repo.path);
            continue;
        }

        // Check if there's anything staged
        let has_changes = std::process::Command::new("git")
            .args(["-C", &dir_str, "diff", "--cached", "--quiet"])
            .status()
            .map(|s| !s.success())
            .unwrap_or(false);

        if has_changes {
            let commit = std::process::Command::new("git")
                .args(["-C", &dir_str, "commit", "-m", "Commiting for bread sync"])
                .output();
            match commit {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    eprintln!(
                        "  ~/{}: commit failed: {}",
                        repo.path,
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                    continue;
                }
                Err(e) => {
                    eprintln!("  ~/{}: commit failed: {}", repo.path, e);
                    continue;
                }
            }
        }

        // Push
        eprint!("  ~/{}: pushing... ", repo.path);
        let push = std::process::Command::new("git")
            .args(["-C", &dir_str, "push"])
            .output();
        match push {
            Ok(o) if o.status.success() => eprintln!("ok"),
            Ok(o) => eprintln!("failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => eprintln!("failed: {}", e),
        }
    }
}

// ── find_git_repos ──────────────────────────────────────────────────────────

/// Read ~/.config/Nextcloud/nextcloud.cfg and return all configured local sync roots.
/// Always includes ~/Nextcloud if it exists, even without a config file.
fn nextcloud_sync_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    let cfg = home.join(".config/Nextcloud/nextcloud.cfg");
    if let Ok(content) = fs::read_to_string(&cfg) {
        for line in content.lines() {
            if let Some(raw) = line.trim().strip_prefix("localPath=") {
                let p = PathBuf::from(raw);
                let p = if p.is_absolute() { p } else { home.join(p) };
                if !dirs.contains(&p) {
                    dirs.push(p);
                }
            }
        }
    }

    // Always treat ~/Nextcloud as off-limits if it exists
    let default_nc = home.join("Nextcloud");
    if default_nc.exists() && !dirs.contains(&default_nc) {
        dirs.push(default_nc);
    }

    dirs
}

fn find_git_repos(home: &Path) -> Vec<GitRepoRecord> {
    let nc_dirs = nextcloud_sync_dirs(home);
    let mut repos: Vec<GitRepoRecord> = Vec::new();

    // Home root at depth 1 only (e.g. ~/bread, ~/yay, ~/colorshell)
    walk_repos(home, home, 0, 1, &mut repos, &nc_dirs);

    // Deeper search in common project directories
    for subdir in &[
        "Projects",
        "Documents",
        "src",
        "dev",
        "code",
        "repos",
        "builds",
    ] {
        let p = home.join(subdir);
        if p.exists() {
            walk_repos(&p, home, 0, 3, &mut repos, &nc_dirs);
        }
    }

    // .config at depth 1 (e.g. ~/.config/hypr, ~/.config/wificonf)
    let config_dir = home.join(".config");
    if config_dir.exists() {
        walk_repos(&config_dir, home, 0, 1, &mut repos, &nc_dirs);
    }

    // Deduplicate by path, sort for determinism
    repos.sort_by(|a, b| a.path.cmp(&b.path));
    repos.dedup_by(|a, b| a.path == b.path);
    repos
}

fn walk_repos(
    dir: &Path,
    home: &Path,
    depth: u32,
    max_depth: u32,
    repos: &mut Vec<GitRepoRecord>,
    nc_dirs: &[PathBuf],
) {
    // Skip anything inside a Nextcloud sync root
    if nc_dirs.iter().any(|nc| dir.starts_with(nc)) {
        return;
    }

    if dir.join(".git").exists() {
        if let Ok(repo) = Repository::open(dir) {
            let remote_url = repo
                .find_remote("origin")
                .ok()
                .and_then(|r| r.url().map(str::to_string));

            if let Some(remote) = remote_url {
                let branch = repo
                    .head()
                    .ok()
                    .and_then(|h| h.shorthand().map(str::to_string))
                    .unwrap_or_else(|| "main".to_string());

                let rel = dir
                    .strip_prefix(home)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| dir.to_string_lossy().to_string());

                repos.push(GitRepoRecord {
                    path: rel,
                    remote,
                    branch,
                });
            }
        }
        return; // don't recurse into git repos (skip submodules)
    }

    if depth >= max_depth {
        return;
    }

    if let Ok(entries) = fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if GIT_SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_repos(&path, home, depth + 1, max_depth, repos, nc_dirs);
        }
    }
}

// ── package install ─────────────────────────────────────────────────────────

fn install_packages_from(packages_dir: &Path) -> Result<()> {
    let pacman_file = packages_dir.join("pacman.txt");
    if pacman_file.exists() {
        let pkgs = packages::parse_pacman(&fs::read_to_string(&pacman_file)?);
        if !pkgs.is_empty() {
            eprintln!("bread: installing {} pacman packages...", pkgs.len());
            let _ = std::process::Command::new("sudo")
                .args(["pacman", "-S", "--needed"])
                .args(&pkgs)
                .status();
        }
    }
    let cargo_file = packages_dir.join("cargo.txt");
    if cargo_file.exists() {
        for pkg in packages::parse_cargo(&fs::read_to_string(&cargo_file)?) {
            let _ = std::process::Command::new("cargo")
                .args(["install", &pkg])
                .status();
        }
    }
    let pip_file = packages_dir.join("pip.txt");
    if pip_file.exists() {
        let _ = std::process::Command::new("pip")
            .args(["install", "--user", "-r"])
            .arg(&pip_file)
            .status();
    }
    let npm_file = packages_dir.join("npm.txt");
    if npm_file.exists() {
        for pkg in packages::parse_npm(&fs::read_to_string(&npm_file)?) {
            let _ = std::process::Command::new("npm")
                .args(["install", "-g", &pkg])
                .status();
        }
    }
    Ok(())
}

// ── restore.sh ───────────────────────────────────────────────────────────────

fn generate_restore_sh(manifest: &ExportManifest) -> String {
    let ts = &manifest.exported_at[..16];
    let mut s = String::new();

    s.push_str("#!/bin/bash\n");
    s.push_str("set -e\n");
    s.push_str("cd \"$(dirname \"$0\")\"\n");
    s.push_str("RESTORE_DIR=\"$(pwd)\"\n\n");
    s.push_str(&format!(
        "echo \"Restoring bread snapshot for {} ({})\"\n\n",
        manifest.machine, ts
    ));

    // Config dirs and dotfiles from path_map
    let dirs: Vec<&PathRecord> = manifest.path_map.iter().filter(|r| !r.is_file).collect();
    let files: Vec<&PathRecord> = manifest.path_map.iter().filter(|r| r.is_file).collect();

    if !dirs.is_empty() {
        s.push_str("# configs and directories\n");
        for r in &dirs {
            let dst = &r.original;
            let src = &r.staging;
            s.push_str(&format!("if [ -e \"$RESTORE_DIR/{src}\" ]; then\n"));
            s.push_str(&format!("  mkdir -p \"{dst}\"\n"));
            s.push_str(&format!("  cp -r \"$RESTORE_DIR/{src}/.\" \"{dst}/\"\n"));
            if r.staging == "systemd" {
                s.push_str("  systemctl --user daemon-reload\n");
            }
            if r.staging == "local-bin" {
                s.push_str("  chmod +x \"${dst}\"/*\n");
            }
            s.push_str(&format!("  echo \"[OK] {dst}\"\n"));
            s.push_str("fi\n");
        }
        s.push('\n');
    }

    if !files.is_empty() {
        s.push_str("# dotfiles\n");
        for r in &files {
            let dst = &r.original;
            let src = &r.staging;
            s.push_str(&format!("if [ -f \"$RESTORE_DIR/{src}\" ]; then\n"));
            if r.staging.contains("ssh_config") {
                s.push_str("  mkdir -p ~/.ssh && chmod 700 ~/.ssh\n");
            }
            // Expand ~ in destination for shell
            let dst_shell = dst.replace('~', "$HOME");
            s.push_str(&format!("  cp \"$RESTORE_DIR/{src}\" \"{dst_shell}\"\n"));
            s.push_str(&format!("  echo \"[OK] {dst}\"\n"));
            s.push_str("fi\n");
        }
        s.push('\n');
    }

    // Packages
    if !manifest.packages.is_empty() {
        s.push_str("echo \"\"\n");
        s.push_str("echo \"--- Package restore commands (not run automatically) ---\"\n");
        if manifest.packages.contains(&"pacman".to_string()) {
            s.push_str("echo \"  pacman:  awk '{print \\$1}' \\\"$RESTORE_DIR/packages/pacman.txt\\\" | sudo pacman -S --needed -\"\n");
        }
        if manifest.packages.contains(&"cargo".to_string()) {
            s.push_str("echo \"  cargo:   grep -v '^ ' \\\"$RESTORE_DIR/packages/cargo.txt\\\" | awk '{print \\$1}' | xargs -I{} cargo install {}\"\n");
        }
        if manifest.packages.contains(&"pip".to_string()) {
            s.push_str(
                "echo \"  pip:     pip install --user -r \\\"$RESTORE_DIR/packages/pip.txt\\\"\"\n",
            );
        }
        if manifest.packages.contains(&"npm".to_string()) {
            s.push_str("echo \"  npm:     awk -F/ '{print \\$NF}' \\\"$RESTORE_DIR/packages/npm.txt\\\" | xargs npm install -g\"\n");
        }
        s.push('\n');
    }

    // System files
    if manifest.system {
        s.push_str("echo \"\"\n");
        s.push_str("echo \"--- System files (require sudo, not applied automatically) ---\"\n");
        s.push_str("if [ -d \"$RESTORE_DIR/system/udev\" ]; then\n");
        s.push_str("  echo \"  udev:           sudo cp \\\"$RESTORE_DIR/system/udev/\\\"* /etc/udev/rules.d/ && sudo udevadm control --reload-rules\"\n");
        s.push_str("fi\n");
        s.push_str("if [ -d \"$RESTORE_DIR/system/modprobe\" ]; then\n");
        s.push_str("  echo \"  modprobe:       sudo cp \\\"$RESTORE_DIR/system/modprobe/\\\"* /etc/modprobe.d/\"\n");
        s.push_str("fi\n");
        s.push_str("if [ -d \"$RESTORE_DIR/system/sysctl\" ]; then\n");
        s.push_str("  echo \"  sysctl:         sudo cp \\\"$RESTORE_DIR/system/sysctl/\\\"* /etc/sysctl.d/ && sudo sysctl --system\"\n");
        s.push_str("fi\n");
        s.push_str("if [ -d \"$RESTORE_DIR/system/networkmanager\" ]; then\n");
        s.push_str("  echo \"  networkmanager: sudo cp \\\"$RESTORE_DIR/system/networkmanager/\\\"* /etc/NetworkManager/system-connections/ && sudo chmod 600 /etc/NetworkManager/system-connections/* && sudo systemctl restart NetworkManager\"\n");
        s.push_str("fi\n");
        s.push_str("if [ -d \"$RESTORE_DIR/system/bluetooth\" ]; then\n");
        s.push_str("  echo \"  bluetooth:      sudo cp -r \\\"$RESTORE_DIR/system/bluetooth/\\\"* /var/lib/bluetooth/ && sudo systemctl restart bluetooth\"\n");
        s.push_str("fi\n\n");
    }

    // Git repos
    if !manifest.repos.is_empty() {
        s.push_str("echo \"\"\n");
        s.push_str("echo \"--- Git repositories ---\"\n");
        for repo in &manifest.repos {
            let dest = format!("$HOME/{}", repo.path);
            let branch = &repo.branch;
            let remote = &repo.remote;
            // Create parent dir and clone; skip if already present
            let parent = std::path::Path::new(&repo.path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if !parent.is_empty() {
                s.push_str(&format!("mkdir -p \"$HOME/{parent}\"\n"));
            }
            s.push_str(&format!("if [ ! -d \"{dest}/.git\" ]; then\n"));
            s.push_str(&format!(
                "  git clone --branch {branch} {remote} \"{dest}\" && echo \"[OK] ~/{}\"\n",
                repo.path
            ));
            s.push_str(&format!(
                "else\n  echo \"[skip] ~/{} (already exists)\"\nfi\n",
                repo.path
            ));
        }
    }

    s
}
