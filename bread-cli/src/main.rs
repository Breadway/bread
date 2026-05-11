use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use bread_sync::{
    config::{bread_config_dir, sync_repo_path, SyncConfig},
    delegates::{copy_delegates_to_repo, expand_tilde, restore_delegates_from_repo, sync_dir},
    git,
    machine::{list_machines, machine_name, MachineProfile},
    packages::snapshot_packages,
};

// ─── CLI structure ────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(author, version, about = "Bread CLI - the reactive desktop automation fabric")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Hot-reload all Lua modules
    Reload {
        /// Watch config directory and reload on changes
        #[arg(long)]
        watch: bool,
    },
    /// Dump current runtime state
    State {
        /// Optional dotted path into RuntimeState
        path: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Stream live normalized events
    Events {
        #[arg(long)]
        filter: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
        /// Comma-separated fields to display
        #[arg(long)]
        fields: Option<String>,
        /// Replay events from the last N seconds
        #[arg(long)]
        since: Option<u64>,
    },
    /// Manage installed Lua modules
    Modules {
        #[command(subcommand)]
        action: ModulesAction,
    },
    /// Sync system state to/from a Git remote
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
    /// List available profiles
    ProfileList,
    /// Activate a profile
    ProfileActivate { name: String },
    /// Manually emit an event
    Emit {
        event: String,
        #[arg(short, long, default_value = "{}")]
        data: String,
    },
    /// Health check daemon connectivity
    Ping,
    /// Fetch daemon health details
    Health,
    /// Diagnose daemon and module health
    Doctor {
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ModulesAction {
    /// Install a module from a source (github:user/repo[@ref] or /local/path)
    Install {
        source: String,
    },
    /// Remove an installed module
    Remove {
        name: String,
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// List installed modules with status
    List,
    /// Update installed modules to latest
    Update {
        /// Update only this specific module
        name: Option<String>,
    },
    /// Show detailed manifest info for a module
    Info {
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum SyncAction {
    /// Initialize sync for this machine
    Init {
        /// Git remote URL
        #[arg(long)]
        remote: Option<String>,
    },
    /// Snapshot and push current state
    Push {
        /// Commit message
        #[arg(long, short = 'm')]
        message: Option<String>,
    },
    /// Pull and apply latest state from remote
    Pull {
        /// Also run package install commands
        #[arg(long)]
        install_packages: bool,
    },
    /// Show what has changed since last push
    Status,
    /// Show file-level diff vs remote
    Diff {
        /// Diff against remote HEAD instead of working tree
        #[arg(long)]
        remote: bool,
    },
    /// List known machines from sync repo
    Machines,
}

// ─── Module manifest ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ModuleManifest {
    name: String,
    version: String,
    description: String,
    author: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_at: Option<String>,
}

// ─── Entry point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = daemon_socket_path();

    match &cli.command {
        Commands::Reload { watch } => {
            if *watch {
                watch_reload(&socket).await?;
            } else {
                let response = send_request_or_die(&socket, "modules.reload", json!({})).await?;
                print_reload(&response);
            }
        }
        Commands::State { path, json } => {
            let response = if let Some(path) = path {
                send_request_or_die(&socket, "state.get", json!({ "key": path })).await?
            } else {
                send_request_or_die(&socket, "state.dump", json!({})).await?
            };
            if *json {
                print_json(&response)?;
            } else {
                print_state_formatted(path.as_deref(), &response);
            }
        }
        Commands::Events { filter, json, fields, since } => {
            stream_events(&socket, filter.clone(), *json, fields.clone(), *since).await?;
        }
        Commands::Modules { action } => {
            handle_modules(action, &socket).await?;
        }
        Commands::Sync { action } => {
            handle_sync(action, &socket).await?;
        }
        Commands::ProfileList => {
            let response = send_request_or_die(&socket, "profile.list", json!({})).await?;
            print_json(&response)?;
        }
        Commands::ProfileActivate { name } => {
            let response = send_request_or_die(
                &socket,
                "profile.activate",
                json!({ "name": name }),
            )
            .await?;
            print_json(&response)?;
        }
        Commands::Emit { event, data } => {
            let parsed = serde_json::from_str::<Value>(data).unwrap_or_else(|_| json!({}));
            let response = send_request_or_die(
                &socket,
                "emit",
                json!({ "event": event, "data": parsed }),
            )
            .await?;
            print_json(&response)?;
        }
        Commands::Ping => {
            let response = send_request_or_die(&socket, "ping", json!({})).await?;
            print_json(&response)?;
        }
        Commands::Health => {
            let response = send_request_or_die(&socket, "health", json!({})).await?;
            print_json(&response)?;
        }
        Commands::Doctor { json } => {
            if *json {
                let response = send_request_or_die(&socket, "health", json!({})).await?;
                print_json(&response)?;
            } else {
                print_doctor(&socket).await?;
            }
        }
    }

    Ok(())
}

// ─── Modules sub-commands ─────────────────────────────────────────────────────

async fn handle_modules(action: &ModulesAction, socket: &Path) -> Result<()> {
    match action {
        ModulesAction::Install { source } => {
            modules_install(source, socket).await?;
        }
        ModulesAction::Remove { name, yes } => {
            modules_remove(name, *yes, socket).await?;
        }
        ModulesAction::List => {
            modules_list(socket).await?;
        }
        ModulesAction::Update { name } => {
            modules_update(name.as_deref(), socket).await?;
        }
        ModulesAction::Info { name } => {
            modules_info(name, socket).await?;
        }
    }
    Ok(())
}

async fn modules_install(source: &str, socket: &Path) -> Result<()> {
    let modules_dir = modules_directory()?;

    if let Some(rest) = source.strip_prefix("github:") {
        install_github_module(rest, source, &modules_dir)?;
    } else if source.starts_with('/') || source.starts_with("./") || source.starts_with("~/") {
        let local_path = expand_tilde(source);
        install_local_module(&local_path, &modules_dir)?;
    } else {
        eprintln!("bread: unknown source format '{source}'");
        eprintln!("  expected: github:user/repo[@ref]  or  /local/path");
        std::process::exit(1);
    }

    // Reload daemon
    if let Ok(response) = send_request(socket, "modules.reload", json!({})).await {
        let _ = response;
    }
    Ok(())
}

fn install_local_module(src: &Path, modules_dir: &Path) -> Result<()> {
    let manifest_path = src.join("bread.module.toml");
    if !manifest_path.exists() {
        eprintln!(
            "bread: no bread.module.toml found at {}",
            manifest_path.display()
        );
        std::process::exit(1);
    }
    let raw = std::fs::read_to_string(&manifest_path)?;
    let mut manifest: ModuleManifest = toml::from_str(&raw)?;
    manifest.installed_at = Some(chrono::Utc::now().to_rfc3339());

    let dest = modules_dir.join(&manifest.name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    copy_dir_all(src, &dest)?;

    // Write updated manifest with installed_at
    let manifest_dest = dest.join("bread.module.toml");
    std::fs::write(&manifest_dest, toml::to_string_pretty(&manifest)?)?;

    println!("installed {} v{}", manifest.name, manifest.version);
    Ok(())
}

fn install_github_module(spec: &str, source_str: &str, modules_dir: &Path) -> Result<()> {
    let (repo_spec, git_ref) = if let Some((r, v)) = spec.split_once('@') {
        (r, Some(v.to_string()))
    } else {
        (spec, None)
    };

    let (user, repo) = repo_spec
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid github spec '{}': expected user/repo", spec))?;

    let client = reqwest::blocking::Client::builder()
        .user_agent("bread-cli/0.1")
        .build()?;

    let resolved_ref = match git_ref {
        Some(r) => r,
        None => {
            let url = format!("https://api.github.com/repos/{user}/{repo}");
            let resp: Value = client.get(&url).send()?.json()?;
            resp.get("default_branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string()
        }
    };

    let tarball_url = format!(
        "https://api.github.com/repos/{user}/{repo}/tarball/{resolved_ref}"
    );
    let bytes = client.get(&tarball_url).send()?.bytes()?;

    // Extract to a temp dir
    let tmp = tempfile_dir()?;
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(&bytes));
    let mut archive = tar::Archive::new(gz);
    archive.unpack(&tmp)?;

    // The tarball has a single top-level directory; find it
    let extracted_dir = std::fs::read_dir(&tmp)?
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .map(|e| e.path())
        .ok_or_else(|| anyhow!("tarball contained no directory"))?;

    let manifest_path = extracted_dir.join("bread.module.toml");
    if !manifest_path.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
        eprintln!(
            "bread: no bread.module.toml found in github:{}/{} (ref {})",
            user, repo, resolved_ref
        );
        std::process::exit(1);
    }

    let raw = std::fs::read_to_string(&manifest_path)?;
    let mut manifest: ModuleManifest = toml::from_str(&raw)?;
    manifest.installed_at = Some(chrono::Utc::now().to_rfc3339());
    manifest.source = source_str.to_string();

    let dest = modules_dir.join(&manifest.name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    copy_dir_all(&extracted_dir, &dest)?;

    let manifest_dest = dest.join("bread.module.toml");
    std::fs::write(&manifest_dest, toml::to_string_pretty(&manifest)?)?;

    let _ = std::fs::remove_dir_all(&tmp);
    println!("installed {} v{}", manifest.name, manifest.version);
    Ok(())
}

async fn modules_remove(name: &str, yes: bool, socket: &Path) -> Result<()> {
    let modules_dir = modules_directory()?;
    let module_dir = modules_dir.join(name);

    if !module_dir.exists() {
        eprintln!("bread: module '{name}' is not installed");
        std::process::exit(1);
    }

    if !yes {
        eprint!("remove {name}? (y/n) ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("cancelled");
            return Ok(());
        }
    }

    std::fs::remove_dir_all(&module_dir)?;
    println!("removed {name}");

    if let Ok(response) = send_request(socket, "modules.reload", json!({})).await {
        let _ = response;
    }
    Ok(())
}

async fn modules_list(socket: &Path) -> Result<()> {
    let modules_dir = modules_directory()?;
    let manifests = scan_modules(&modules_dir)?;

    // Try to get daemon status
    let daemon_modules = send_request(socket, "modules.list", json!({}))
        .await
        .ok()
        .and_then(|v| v.as_array().cloned());

    for manifest in &manifests {
        let status = daemon_modules
            .as_ref()
            .and_then(|mods| {
                mods.iter().find(|m| {
                    m.get("name").and_then(Value::as_str) == Some(&manifest.name)
                })
            })
            .and_then(|m| m.get("status").and_then(Value::as_str))
            .unwrap_or(if daemon_modules.is_some() { "unknown" } else { "unknown" });

        println!(
            "  {:<20} {:<10} {:<12} {}",
            manifest.name, manifest.version, status, manifest.source
        );
    }
    Ok(())
}

async fn modules_update(name: Option<&str>, socket: &Path) -> Result<()> {
    let modules_dir = modules_directory()?;

    let to_update: Vec<ModuleManifest> = if let Some(name) = name {
        let manifest = load_manifest(&modules_dir.join(name))?;
        vec![manifest]
    } else {
        scan_modules(&modules_dir)?
    };

    for manifest in to_update {
        if !manifest.source.starts_with("github:") {
            eprintln!(
                "warn: cannot update '{}' — local module, reinstall manually",
                manifest.name
            );
            continue;
        }
        let old_version = manifest.version.clone();
        let source = manifest.source.clone();
        let rest = source.trim_start_matches("github:");
        install_github_module(rest, &source, &modules_dir)?;
        let new_manifest = load_manifest(&modules_dir.join(&manifest.name))?;
        if new_manifest.version == old_version {
            println!("{} already up to date", manifest.name);
        } else {
            println!(
                "updated {} v{} → v{}",
                manifest.name, old_version, new_manifest.version
            );
        }
    }

    if let Ok(response) = send_request(socket, "modules.reload", json!({})).await {
        let _ = response;
    }
    Ok(())
}

async fn modules_info(name: &str, socket: &Path) -> Result<()> {
    let modules_dir = modules_directory()?;
    let module_dir = modules_dir.join(name);

    if !module_dir.exists() {
        eprintln!("bread: module '{name}' is not installed");
        std::process::exit(1);
    }

    let manifest = load_manifest(&module_dir)?;
    let status = send_request(socket, "modules.list", json!({}))
        .await
        .ok()
        .and_then(|v| v.as_array().cloned())
        .and_then(|mods| {
            mods.iter()
                .find(|m| m.get("name").and_then(Value::as_str) == Some(name))
                .and_then(|m| m.get("status").and_then(Value::as_str))
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("name:         {}", manifest.name);
    println!("version:      {}", manifest.version);
    println!("description:  {}", manifest.description);
    println!("author:       {}", manifest.author);
    println!("source:       {}", manifest.source);
    println!(
        "installed_at: {}",
        manifest.installed_at.as_deref().unwrap_or("unknown")
    );
    println!("status:       {status}");
    Ok(())
}

// ─── Sync sub-commands ────────────────────────────────────────────────────────

async fn handle_sync(action: &SyncAction, socket: &Path) -> Result<()> {
    match action {
        SyncAction::Init { remote } => sync_init(remote.as_deref()).await?,
        SyncAction::Push { message } => sync_push(message.as_deref()).await?,
        SyncAction::Pull { install_packages } => sync_pull(*install_packages, socket).await?,
        SyncAction::Status => sync_status().await?,
        SyncAction::Diff { remote } => sync_diff(*remote).await?,
        SyncAction::Machines => sync_machines().await?,
    }
    Ok(())
}

async fn sync_init(remote_arg: Option<&str>) -> Result<()> {
    if SyncConfig::is_initialized()? {
        eprintln!(
            "bread: sync already initialized. Edit {} to reconfigure.",
            bread_sync::config::config_path()?.display()
        );
        std::process::exit(1);
    }

    let remote_url = if let Some(url) = remote_arg {
        url.to_string()
    } else {
        eprint!("Sync remote URL (git remote or path): ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let url = input.trim().to_string();
        if url.is_empty() {
            anyhow::bail!("remote URL is required");
        }
        url
    };

    let default_hostname = hostname_or_unknown();
    eprint!("Machine name [{}]: ", default_hostname);
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let machine_name = {
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            default_hostname.clone()
        } else {
            trimmed
        }
    };

    eprint!("Machine tags (comma-separated, e.g. mobile,battery): ");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let tags: Vec<String> = input
        .trim()
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();

    let cfg = SyncConfig {
        remote: bread_sync::config::RemoteConfig {
            url: Some(remote_url.clone()),
            branch: "main".to_string(),
        },
        machine: bread_sync::config::MachineConfig {
            name: Some(machine_name.clone()),
            tags,
        },
        ..Default::default()
    };
    cfg.save()?;

    // Validate remote if it looks like a URL
    if !remote_url.starts_with('/') {
        println!("remote does not exist yet — it will be created on first push");
    }

    println!("sync initialized:");
    println!("  machine: {machine_name}");
    println!("  remote:  {remote_url}");
    Ok(())
}

async fn sync_push(message: Option<&str>) -> Result<()> {
    let cfg = require_sync_config()?;
    let remote_url = cfg.remote.url.as_deref().ok_or_else(|| {
        anyhow!("sync.toml has no remote URL — run: bread sync init")
    })?;
    let branch = cfg.remote.branch.clone();
    let repo_path = sync_repo_path()?;

    let repo = tokio::task::spawn_blocking({
        let remote_url = remote_url.to_string();
        let repo_path = repo_path.clone();
        move || git::clone_or_open(&remote_url, &repo_path)
    })
    .await??;

    // Snapshot bread config
    let bread_dir = bread_config_dir()?;
    let bread_dest = repo_path.join("bread");
    sync_dir(&bread_dir, &bread_dest, &cfg.delegates.exclude)?;

    // Snapshot delegates
    copy_delegates_to_repo(&cfg.delegates, &repo_path)?;

    // Snapshot packages
    if cfg.packages.enabled {
        snapshot_packages(&cfg.packages.managers, &repo_path)?;
    }

    // Write machine profile
    let profile = MachineProfile::new(&cfg)?;
    profile.write_to_repo(&repo_path)?;

    // Stage all
    git::stage_all(&repo)?;

    // Check for changes
    if !git::has_changes(&repo)? {
        println!("nothing to push — already up to date");
        return Ok(());
    }

    // Commit
    let machine = machine_name(&cfg)?;
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let commit_msg = message
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("sync: {machine} {timestamp}"));
    git::commit(&repo, &commit_msg)?;

    // Set remote and push
    if let Ok(()) = git::set_remote(&repo, "origin", remote_url) {}
    git::push(&repo, "origin", &branch)?;

    println!("pushed: {commit_msg}");
    println!("  bread config: {}", bread_dir.display());
    if cfg.packages.enabled {
        println!("  packages: {}", cfg.packages.managers.join(", "));
    }
    Ok(())
}

async fn sync_pull(install_packages: bool, socket: &Path) -> Result<()> {
    let cfg = require_sync_config()?;
    let remote_url = cfg.remote.url.as_deref().ok_or_else(|| {
        anyhow!("sync.toml has no remote URL — run: bread sync init")
    })?;
    let branch = cfg.remote.branch.clone();
    let repo_path = sync_repo_path()?;

    let repo = tokio::task::spawn_blocking({
        let remote_url = remote_url.to_string();
        let repo_path = repo_path.clone();
        move || git::clone_or_open(&remote_url, &repo_path)
    })
    .await??;

    git::pull(&repo, "origin", &branch)?;

    // Restore bread config
    let bread_src = repo_path.join("bread");
    let bread_dest = bread_config_dir()?;
    if bread_src.exists() {
        sync_dir(&bread_src, &bread_dest, &[])?;
    }

    // Restore delegates
    restore_delegates_from_repo(&cfg.delegates, &repo_path)?;

    // Package installs
    if install_packages && cfg.packages.enabled {
        run_package_installs(&repo_path, &cfg.packages.managers)?;
    } else if cfg.packages.enabled {
        let pkg_dir = repo_path.join("packages");
        if pkg_dir.exists() {
            println!(
                "note: run 'bread sync pull --install-packages' to install missing packages"
            );
        }
    }

    // Reload daemon
    if let Ok(response) = send_request(socket, "modules.reload", json!({})).await {
        let _ = response;
    }

    println!("pulled and applied latest state");
    Ok(())
}

async fn sync_status() -> Result<()> {
    let cfg = require_sync_config()?;
    let repo_path = sync_repo_path()?;

    if !repo_path.join(".git").exists() {
        println!("sync repo not yet initialised — run: bread sync push");
        return Ok(());
    }

    let repo = git::init_or_open(&repo_path)?;
    let machine = machine_name(&cfg)?;
    let remote_url = cfg.remote.url.as_deref().unwrap_or("(none)");
    let last_push = git::last_commit_time(&repo);

    println!("bread sync status");
    println!("  machine      {machine}");
    println!("  remote       {remote_url}");
    println!("  last push    {last_push}");

    let local_changes = git::status_lines(&repo)?;
    println!();
    println!("local changes (not yet pushed):");
    if local_changes.is_empty() {
        println!("  none");
    } else {
        for (ch, path) in &local_changes {
            println!("  {ch}  {path}");
        }
    }

    // Fetch to check remote
    let _ = git::fetch(&repo, "origin");
    let has_remote = git::remote_has_changes(&repo, "origin", &cfg.remote.branch);
    println!();
    println!("remote changes (not yet pulled):");
    if has_remote {
        println!("  (run 'bread sync pull' to apply)");
    } else {
        println!("  none");
    }
    Ok(())
}

async fn sync_diff(show_remote: bool) -> Result<()> {
    let cfg = require_sync_config()?;
    let repo_path = sync_repo_path()?;

    if !repo_path.join(".git").exists() {
        println!("sync repo not initialised — run: bread sync push");
        return Ok(());
    }

    let repo = git::init_or_open(&repo_path)?;

    let diff = if show_remote {
        git::fetch(&repo, "origin")?;
        git::diff_remote(&repo, "origin", &cfg.remote.branch)?
    } else {
        git::diff_workdir(&repo)?
    };

    if diff.is_empty() {
        println!("no differences");
    } else {
        print!("{diff}");
    }
    Ok(())
}

async fn sync_machines() -> Result<()> {
    let repo_path = sync_repo_path()?;
    if !repo_path.join(".git").exists() {
        println!("sync repo not initialised — run: bread sync push");
        return Ok(());
    }
    let machines = list_machines(&repo_path);
    if machines.is_empty() {
        println!("no machines found in sync repo");
        return Ok(());
    }
    for m in machines {
        let tags = if m.tags.is_empty() {
            "(none)".to_string()
        } else {
            m.tags.join(", ")
        };
        println!(
            "  {:<20} last sync: {:<20} tags: {}",
            m.name, m.last_sync, tags
        );
    }
    Ok(())
}

fn run_package_installs(repo_root: &Path, managers: &[String]) -> Result<()> {
    let pkg_dir = repo_root.join("packages");

    for mgr in managers {
        match mgr.as_str() {
            "pacman" => {
                let f = pkg_dir.join("pacman.txt");
                if f.exists() {
                    let names = bread_sync::packages::parse_pacman(&std::fs::read_to_string(&f)?);
                    let status = std::process::Command::new("sudo")
                        .args(["pacman", "-S", "--needed"])
                        .args(&names)
                        .status();
                    if let Err(e) = status {
                        eprintln!("warn: pacman install failed: {e}");
                    }
                }
            }
            "pip" => {
                let f = pkg_dir.join("pip.txt");
                if f.exists() {
                    let status = std::process::Command::new("pip")
                        .args(["install", "--user", "-r"])
                        .arg(&f)
                        .status();
                    if let Err(e) = status {
                        eprintln!("warn: pip install failed: {e}");
                    }
                }
            }
            "npm" => {
                let f = pkg_dir.join("npm.txt");
                if f.exists() {
                    let names = bread_sync::packages::parse_npm(&std::fs::read_to_string(&f)?);
                    for name in names {
                        let _ = std::process::Command::new("npm")
                            .args(["install", "-g", &name])
                            .status();
                    }
                }
            }
            "cargo" => {
                let f = pkg_dir.join("cargo.txt");
                if f.exists() {
                    let entries = bread_sync::packages::parse_cargo(&std::fs::read_to_string(&f)?);
                    for entry in entries {
                        let name = entry.split_whitespace().next().unwrap_or(&entry);
                        let _ = std::process::Command::new("cargo")
                            .args(["install", name])
                            .status();
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// ─── Helper functions ─────────────────────────────────────────────────────────

fn require_sync_config() -> Result<SyncConfig> {
    if !SyncConfig::is_initialized()? {
        eprintln!("bread: sync not initialized. Run: bread sync init");
        std::process::exit(1);
    }
    SyncConfig::load()
}

fn modules_directory() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("cannot determine config directory"))?;
    let dir = config_dir.join("bread").join("modules");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn scan_modules(modules_dir: &Path) -> Result<Vec<ModuleManifest>> {
    let mut out = Vec::new();
    if !modules_dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(modules_dir)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        if let Ok(manifest) = load_manifest(&entry.path()) {
            out.push(manifest);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn load_manifest(module_dir: &Path) -> Result<ModuleManifest> {
    let path = module_dir.join("bread.module.toml");
    let raw = std::fs::read_to_string(&path)?;
    Ok(toml::from_str(&raw)?)
}

fn copy_dir_all(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

fn tempfile_dir() -> Result<PathBuf> {
    let tmp = std::env::temp_dir().join(format!("bread-install-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    Ok(tmp)
}

fn hostname_or_unknown() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

// ─── IPC helpers ──────────────────────────────────────────────────────────────

fn daemon_socket_path() -> PathBuf {
    if let Ok(runtime) = env::var("XDG_RUNTIME_DIR") {
        return Path::new(&runtime).join("bread").join("breadd.sock");
    }
    PathBuf::from("/tmp/bread/breadd.sock")
}

/// Like `send_request` but prints a nice message and exits 1 if daemon is unreachable.
async fn send_request_or_die(socket: &Path, method: &str, params: Value) -> Result<Value> {
    match send_request(socket, method, params).await {
        Ok(v) => Ok(v),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("No such file")
                || msg.contains("Connection refused")
                || msg.contains("not found")
            {
                eprintln!(
                    "bread: daemon is not running. Start it with: systemctl --user start breadd"
                );
                std::process::exit(1);
            }
            Err(err)
        }
    }
}

async fn send_request(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let stream = UnixStream::connect(socket).await?;
    let (read_half, mut write_half) = stream.into_split();
    let request = json!({
        "id": "1",
        "method": method,
        "params": params,
    });

    write_half
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .await?;

    let mut lines = BufReader::new(read_half).lines();
    let Some(line) = lines.next_line().await? else {
        anyhow::bail!("daemon closed connection without response");
    };
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        anyhow::bail!(error.to_string());
    }
    Ok(response.get("result").cloned().unwrap_or_else(|| json!({})))
}

async fn stream_events(
    socket: &Path,
    filter: Option<String>,
    raw_json: bool,
    fields: Option<String>,
    since: Option<u64>,
) -> Result<()> {
    if let Some(seconds) = since {
        let replay =
            send_request(socket, "events.replay", json!({ "since_ms": seconds * 1000 })).await?;
        if let Some(list) = replay.as_array() {
            for item in list {
                if raw_json {
                    println!("{}", serde_json::to_string_pretty(item)?);
                } else {
                    print_event(item, fields.as_deref());
                }
            }
        }
    }

    let stream = UnixStream::connect(socket).await?;
    let (read_half, mut write_half) = stream.into_split();
    let request = json!({
        "id": "1",
        "method": "events.subscribe",
        "params": { "filter": filter },
    });

    write_half
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .await?;

    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let value: Value = serde_json::from_str(&line)?;
        if raw_json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            print_event(&value, fields.as_deref());
        }
    }
    Ok(())
}

// ─── Display helpers ──────────────────────────────────────────────────────────

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_state_formatted(path: Option<&str>, value: &Value) {
    if let Some(path) = path {
        println!("{path}");
    }
    print_value(value, 0);
}

fn print_value(value: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                println!("{pad}{key}");
                print_value(val, indent + 2);
            }
        }
        Value::Array(list) => {
            for (idx, val) in list.iter().enumerate() {
                println!("{pad}[{idx}]");
                print_value(val, indent + 2);
            }
        }
        other => {
            println!("{pad}{}", other);
        }
    }
}

fn print_event(event: &Value, fields: Option<&str>) {
    if let Some(fields) = fields {
        let mut out = serde_json::Map::new();
        for field in fields.split(',') {
            let field = field.trim();
            if field.is_empty() {
                continue;
            }
            if let Some(val) = event.get(field) {
                out.insert(field.to_string(), val.clone());
            }
        }
        println!("{}", Value::Object(out));
        return;
    }

    let ts = event.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
    let event_name = event.get("event").and_then(Value::as_str).unwrap_or("?");
    let source = event.get("source").and_then(Value::as_str).unwrap_or("?");
    let time = format_timestamp(ts);
    println!("{time}  {event_name}  source={source}");
    if let Some(data) = event.get("data") {
        println!("  data: {}", data);
    }
}

fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;

    let local_secs = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = secs as libc::time_t;
        libc::localtime_r(&t, &mut tm);
        tm.tm_hour as u64 * 3600 + tm.tm_min as u64 * 60 + tm.tm_sec as u64
    };

    let h = (local_secs / 3600) % 24;
    let m = (local_secs / 60) % 60;
    let s = local_secs % 60;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, millis)
}

fn print_reload(value: &Value) {
    println!("reloading lua runtime...");
    if let Some(mods) = value.get("modules").and_then(Value::as_array) {
        for module in mods {
            let name = module.get("name").and_then(Value::as_str).unwrap_or("?");
            let status = module.get("status").and_then(Value::as_str).unwrap_or("?");
            let error = module.get("last_error").and_then(Value::as_str);
            if let Some(error) = error {
                println!("  ✗ {name}  {status}");
                println!("      {error}");
            } else {
                println!("  ✓ {name}  {status}");
            }
        }
    }
}

async fn watch_reload(socket: &Path) -> Result<()> {
    let config_dir = config_directory();
    println!("watching {} for changes...", config_dir.display());

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&config_dir, RecursiveMode::Recursive)?;

    while let Some(msg) = rx.recv().await {
        if msg.is_err() {
            continue;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        while rx.try_recv().is_ok() {}
        let response = send_request_or_die(socket, "modules.reload", json!({})).await?;
        print_reload(&response);
    }
    Ok(())
}

async fn print_doctor(socket: &Path) -> Result<()> {
    let stream = match UnixStream::connect(socket).await {
        Ok(stream) => stream,
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                println!("bread doctor");
                println!("  daemon     ✗ not running");
                println!("  socket     {}  (not found)", socket.display());
                println!();
                println!("  start the daemon:   systemctl --user start breadd");
                println!("  view logs:          journalctl --user -u breadd -f");
                return Ok(());
            }
            return Err(err.into());
        }
    };

    let response = send_request_with_stream(stream, "health", json!({})).await?;
    render_doctor(&response);
    Ok(())
}

fn render_doctor(health: &Value) {
    println!("bread doctor");
    let ok = health.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let pid = health.get("pid").and_then(Value::as_u64).unwrap_or(0);
    let version = health.get("version").and_then(Value::as_str).unwrap_or("unknown");
    let uptime_ms = health.get("uptime_ms").and_then(Value::as_u64).unwrap_or(0);
    let socket = health.get("socket").and_then(Value::as_str).unwrap_or("?");
    println!(
        "  daemon     {} (pid {})",
        if ok { "✓ running" } else { "✗ unreachable" },
        pid
    );
    println!("  version    {version}");
    println!("  uptime     {}s", uptime_ms / 1000);
    println!("  socket     {socket}");

    if let Some(adapters) = health.get("adapters").and_then(Value::as_object) {
        println!();
        println!("adapters");
        for (name, status) in adapters {
            println!("  {:20} {}", name, status);
        }
    }

    if let Some(modules) = health.get("modules").and_then(Value::as_array) {
        println!();
        println!("modules");
        for module in modules {
            let name = module.get("name").and_then(Value::as_str).unwrap_or("?");
            let status = module.get("status").and_then(Value::as_str).unwrap_or("?");
            let error = module.get("last_error").and_then(Value::as_str);
            println!("  {:30} {}", name, status);
            if let Some(error) = error {
                println!("    └ {error}");
            }
        }
    }

    if let Some(count) = health.get("subscriptions").and_then(Value::as_u64) {
        println!();
        println!("subscriptions  {count}");
    }

    if let Some(errors) = health.get("recent_errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            println!();
            println!("recent errors ({} total)", errors.len());
            for entry in errors.iter().take(5) {
                println!("  {entry}");
            }
        }
    }
}

async fn send_request_with_stream(
    stream: UnixStream,
    method: &str,
    params: Value,
) -> Result<Value> {
    let (read_half, mut write_half) = stream.into_split();
    let request = json!({
        "id": "1",
        "method": method,
        "params": params,
    });

    write_half
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .await?;

    let mut lines = BufReader::new(read_half).lines();
    let Some(line) = lines.next_line().await? else {
        anyhow::bail!("daemon closed connection without response");
    };
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        anyhow::bail!(error.to_string());
    }
    Ok(response.get("result").cloned().unwrap_or_else(|| json!({})))
}

fn config_directory() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("bread"))
        .unwrap_or_else(|| PathBuf::from(".config/bread"))
}
