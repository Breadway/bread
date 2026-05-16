mod modules_mgmt;

use anyhow::{Context, Result};
use bread_sync::{
    config::{bread_config_dir, SyncConfig},
    delegates, machine, packages, apply_import, stage_export, SyncRepo,
};
use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::env;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Bread CLI - the reactive desktop automation fabric"
)]
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
        /// Optional glob pattern to filter events (e.g. bread.device.*, bread.**)
        pattern: Option<String>,
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
        subcommand: ModulesCommand,
    },
    /// Manage sync (snapshot and restore system state)
    Sync {
        #[command(subcommand)]
        subcommand: SyncCommand,
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
enum ModulesCommand {
    /// Install a module from a source
    Install {
        /// Source: github:user/repo[@ref] or /path/to/dir
        source: String,
    },
    /// Remove an installed module
    Remove {
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// List all installed modules
    List,
    /// Update one or all installed modules
    Update {
        /// Module name (omit to update all)
        name: Option<String>,
    },
    /// Show full manifest details for a module
    Info { name: String },
}

#[derive(Subcommand, Debug)]
enum SyncCommand {
    /// Initialize sync for this machine
    Init {
        /// Git remote URL
        #[arg(long)]
        remote: Option<String>,
    },
    /// Snapshot and push current state
    Push {
        /// Custom commit message
        #[arg(long)]
        message: Option<String>,
    },
    /// Pull and apply latest state
    Pull {
        /// Also install packages from manifest
        #[arg(long)]
        install_packages: bool,
    },
    /// Show what has changed since last push
    Status,
    /// Show file-level diff vs last commit (or vs remote with --remote)
    Diff {
        #[arg(long)]
        remote: bool,
    },
    /// List known machines from sync repo
    Machines,
    /// Create a portable export archive (no git auth required)
    Export {
        /// Output path: directory or .tar.gz file. Defaults to ./bread-export-<machine>-<date>.tar.gz
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Apply a portable export archive to this machine
    Import {
        /// Path to a bread export directory or .tar.gz file
        from: PathBuf,
        /// Also install packages from the package manifests
        #[arg(long)]
        install_packages: bool,
        /// Skip cloning git repositories to their original locations
        #[arg(long)]
        no_clone_repos: bool,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = daemon_socket_path();

    match cli.command {
        Commands::Reload { watch } => {
            if watch {
                watch_reload(&socket).await?;
            } else {
                let response = send_request(&socket, "modules.reload", json!({})).await?;
                print_reload(&response);
            }
        }
        Commands::State { path, json } => {
            let response = if let Some(ref path) = path {
                send_request(&socket, "state.get", json!({ "key": path })).await?
            } else {
                send_request(&socket, "state.dump", json!({})).await?
            };
            if json {
                print_json(&response)?;
            } else {
                print_state_formatted(path.as_deref(), &response);
            }
        }
        Commands::Events {
            pattern,
            json,
            fields,
            since,
        } => {
            stream_events(&socket, pattern, json, fields, since).await?;
        }
        Commands::Modules { subcommand } => {
            handle_modules_cmd(subcommand, &socket).await?;
        }
        Commands::Sync { subcommand } => {
            handle_sync_cmd(subcommand, &socket).await?;
        }
        Commands::ProfileList => {
            let response = send_request(&socket, "profile.list", json!({})).await?;
            print_json(&response)?;
        }
        Commands::ProfileActivate { name } => {
            let response =
                send_request(&socket, "profile.activate", json!({ "name": name })).await?;
            print_json(&response)?;
        }
        Commands::Emit { event, data } => {
            let parsed = serde_json::from_str::<Value>(&data).unwrap_or_else(|_| json!({}));
            let response = send_request(
                &socket,
                "emit",
                json!({
                    "event": event,
                    "data": parsed,
                }),
            )
            .await?;
            print_json(&response)?;
        }
        Commands::Ping => {
            let response = send_request(&socket, "ping", json!({})).await?;
            print_json(&response)?;
        }
        Commands::Health => {
            let response = send_request(&socket, "health", json!({})).await?;
            print_json(&response)?;
        }
        Commands::Doctor { json } => {
            if json {
                let response = send_request(&socket, "health", json!({})).await?;
                print_json(&response)?;
            } else {
                print_doctor(&socket).await?;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Module subcommands
// ---------------------------------------------------------------------------

async fn handle_modules_cmd(cmd: ModulesCommand, socket: &Path) -> Result<()> {
    let mods_dir = modules_mgmt::modules_dir();

    match cmd {
        ModulesCommand::Install { source } => {
            let manifest = install_module(&source, &mods_dir).await?;
            println!("installed {} v{}", manifest.name, manifest.version);
            try_daemon_reload(socket).await;
        }

        ModulesCommand::Remove { name, yes } => {
            let module_dir = mods_dir.join(&name);
            if !module_dir.exists() {
                eprintln!("bread: module '{}' is not installed", name);
                std::process::exit(1);
            }
            if !yes {
                print!("remove {}? (y/n): ", name);
                io::stdout().flush()?;
                let mut line = String::new();
                io::stdin().read_line(&mut line)?;
                if !line.trim().eq_ignore_ascii_case("y") {
                    println!("aborted");
                    return Ok(());
                }
            }
            modules_mgmt::remove_module(&name, &mods_dir)?;
            println!("removed {}", name);
            try_daemon_reload(socket).await;
        }

        ModulesCommand::List => {
            let modules = modules_mgmt::list_modules(&mods_dir)?;
            // Try to get daemon module status
            let daemon_statuses = match send_request(socket, "modules.list", json!({})).await {
                Ok(resp) => resp
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|v| {
                        let name = v.get("name").and_then(Value::as_str)?.to_string();
                        let status = v.get("status").and_then(Value::as_str)?.to_string();
                        Some((name, status))
                    })
                    .collect::<std::collections::HashMap<_, _>>(),
                Err(_) => std::collections::HashMap::new(),
            };
            for m in &modules {
                let status = daemon_statuses
                    .get(&m.name)
                    .map(String::as_str)
                    .unwrap_or("unknown");
                println!(
                    "  {:20} {:10} {:10} {}",
                    m.name, m.version, status, m.source
                );
            }
        }

        ModulesCommand::Update { name } => {
            let targets: Vec<_> = if let Some(n) = name {
                vec![modules_mgmt::read_module_manifest(&n, &mods_dir)?]
            } else {
                modules_mgmt::list_modules(&mods_dir)?
            };

            let mut updated_any = false;
            for manifest in targets {
                if manifest.source.starts_with("github:") {
                    let old_ver = manifest.version.clone();
                    let new_manifest = install_module(&manifest.source, &mods_dir).await?;
                    if new_manifest.version == old_ver {
                        println!("{} already up to date", manifest.name);
                    } else {
                        println!(
                            "updated {} v{} → v{}",
                            manifest.name, old_ver, new_manifest.version
                        );
                        updated_any = true;
                    }
                } else {
                    eprintln!(
                        "cannot update local module '{}' — reinstall manually",
                        manifest.name
                    );
                }
            }
            if updated_any {
                try_daemon_reload(socket).await;
            }
        }

        ModulesCommand::Info { name } => {
            let m = modules_mgmt::read_module_manifest(&name, &mods_dir)?;
            let status = match send_request(socket, "modules.list", json!({})).await {
                Ok(resp) => resp
                    .as_array()
                    .and_then(|arr| {
                        arr.iter()
                            .find(|v| v.get("name").and_then(Value::as_str) == Some(&m.name))
                            .and_then(|v| v.get("status").and_then(Value::as_str))
                            .map(ToString::to_string)
                    })
                    .unwrap_or_else(|| "unknown".to_string()),
                Err(_) => "unknown".to_string(),
            };
            println!("name:         {}", m.name);
            println!("version:      {}", m.version);
            println!("description:  {}", m.description);
            println!("author:       {}", m.author);
            println!("source:       {}", m.source);
            println!("installed_at: {}", m.installed_at);
            println!("status:       {}", status);
        }
    }
    Ok(())
}

async fn install_module(
    source: &str,
    mods_dir: &std::path::Path,
) -> Result<modules_mgmt::ModuleManifest> {
    match modules_mgmt::parse_source(source)? {
        modules_mgmt::InstallSource::LocalPath(path) => {
            modules_mgmt::install_from_local(&path, source, mods_dir)
        }
        modules_mgmt::InstallSource::GitHub {
            user,
            repo,
            git_ref,
        } => install_from_github(&user, &repo, git_ref.as_deref(), source, mods_dir).await,
    }
}

async fn install_from_github(
    user: &str,
    repo: &str,
    git_ref: Option<&str>,
    source_str: &str,
    mods_dir: &Path,
) -> Result<modules_mgmt::ModuleManifest> {
    let client = reqwest::Client::builder()
        .user_agent("bread-cli/0.1")
        .build()?;

    let ref_to_use = match git_ref {
        Some(r) => r.to_string(),
        None => {
            let url = format!("https://api.github.com/repos/{user}/{repo}");
            let resp: Value = client
                .get(&url)
                .send()
                .await
                .context("failed to reach GitHub API")?
                .json()
                .await
                .context("failed to parse GitHub API response")?;
            resp.get("default_branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_string()
        }
    };

    let tarball_url = format!("https://api.github.com/repos/{user}/{repo}/tarball/{ref_to_use}");
    let bytes = client
        .get(&tarball_url)
        .send()
        .await
        .context("failed to download module archive")?
        .bytes()
        .await
        .context("failed to read module archive")?;

    let tmp = tempfile::tempdir()?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
    archive.unpack(tmp.path())?;

    // GitHub extracts to a single subdirectory (e.g. "user-repo-sha/")
    let root = std::fs::read_dir(tmp.path())?
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .map(|e| e.path())
        .ok_or_else(|| anyhow::anyhow!("no directory found in extracted archive"))?;

    modules_mgmt::install_from_local(&root, source_str, mods_dir)
}

/// Notify the daemon to reload modules. Prints a warning if the daemon is unreachable.
async fn try_daemon_reload(socket: &Path) {
    match send_request(socket, "modules.reload", json!({})).await {
        Ok(_) => {}
        Err(_) => {
            eprintln!("note: daemon not running; reload manually with 'bread reload'");
        }
    }
}

// ---------------------------------------------------------------------------
// Sync subcommands
// ---------------------------------------------------------------------------

async fn handle_sync_cmd(cmd: SyncCommand, socket: &Path) -> Result<()> {
    let cfg_dir = bread_config_dir();

    match cmd {
        SyncCommand::Init { remote } => cmd_sync_init(&cfg_dir, remote).await?,
        SyncCommand::Push { message } => cmd_sync_push(&cfg_dir, message).await?,
        SyncCommand::Pull { install_packages } => {
            cmd_sync_pull(&cfg_dir, install_packages, socket).await?
        }
        SyncCommand::Status => cmd_sync_status(&cfg_dir).await?,
        SyncCommand::Diff { remote } => cmd_sync_diff(&cfg_dir, remote).await?,
        SyncCommand::Machines => cmd_sync_machines(&cfg_dir).await?,
        SyncCommand::Export { output } => cmd_sync_export(&cfg_dir, output).await?,
        SyncCommand::Import { from, install_packages, no_clone_repos, yes } => {
            cmd_sync_import(&cfg_dir, from, install_packages, !no_clone_repos, yes, &socket).await?
        }
    }
    Ok(())
}

async fn cmd_sync_init(cfg_dir: &Path, remote: Option<String>) -> Result<()> {
    let sync_toml = cfg_dir.join("sync.toml");
    if sync_toml.exists() {
        eprintln!(
            "bread: sync already initialized. Edit {} to reconfigure.",
            sync_toml.display()
        );
        std::process::exit(1);
    }

    let remote_url = match remote {
        Some(u) => u,
        None => {
            print!("Sync remote URL (leave empty for local-only, e.g. git@github.com:you/config): ");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
    };

    let default_hostname = machine::hostname();
    print!("Machine name [{}]: ", default_hostname);
    io::stdout().flush()?;
    let mut name_line = String::new();
    io::stdin().read_line(&mut name_line)?;
    let machine_name = {
        let t = name_line.trim();
        if t.is_empty() {
            default_hostname
        } else {
            t.to_string()
        }
    };

    print!("Machine tags (comma-separated, e.g. mobile,battery): ");
    io::stdout().flush()?;
    let mut tags_line = String::new();
    io::stdin().read_line(&mut tags_line)?;
    let tags: Vec<String> = tags_line
        .trim()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    let config = SyncConfig {
        remote: bread_sync::config::RemoteConfig {
            url: remote_url.clone(),
            branch: "main".to_string(),
        },
        machine: bread_sync::config::MachineConfig {
            name: machine_name.clone(),
            tags,
        },
        packages: bread_sync::config::PackagesConfig::default(),
        delegates: bread_sync::config::DelegatesConfig::default(),
    };
    config.save(cfg_dir)?;

    println!();
    println!("sync initialized");
    println!("  machine: {}", machine_name);
    if remote_url.is_empty() {
        println!("  remote:  (local-only — use 'bread sync export' to create a portable snapshot)");
    } else {
        println!("  remote:  {}", remote_url);
        if !remote_url.starts_with('/') && !remote_url.starts_with('.') {
            println!("  note:    remote will be created on first push");
        }
    }
    println!("  config:  {}", cfg_dir.join("sync.toml").display());
    Ok(())
}

async fn cmd_sync_push(cfg_dir: &Path, message: Option<String>) -> Result<()> {
    let config = load_sync_config(cfg_dir)?;
    let repo_path = SyncConfig::local_repo_path();

    let repo = if repo_path.exists() {
        SyncRepo::open(&repo_path)?
    } else {
        SyncRepo::init(&repo_path)?
    };

    // Snapshot bread/ directory
    let bread_dest = repo_path.join("bread");
    delegates::sync_dir(cfg_dir, &bread_dest, &[".git".to_string()])?;

    // Snapshot delegate configs
    let configs_dir = repo_path.join("configs");
    let delegate_paths = delegates::resolve_include_paths(&config.delegates.include);
    for (basename, src_path) in &delegate_paths {
        if src_path.exists() {
            let dst = configs_dir.join(basename);
            delegates::sync_dir(src_path, &dst, &config.delegates.exclude)?;
        }
    }

    // Snapshot packages
    if config.packages.enabled {
        let packages_dir = repo_path.join("packages");
        for manager in &config.packages.managers {
            let dest_file = packages_dir.join(format!("{manager}.txt"));
            if let Err(e) = packages::snapshot(manager, &dest_file) {
                eprintln!("bread: warning: package snapshot for {manager} failed: {e}");
            }
        }
    }

    // Write machine profile
    let machines_dir = repo_path.join("machines");
    machine::MachineProfile::new(config.machine.name.clone(), config.machine.tags.clone())
        .write(&machines_dir)?;

    let commit_msg = message.unwrap_or_else(|| {
        format!(
            "sync: {} {}",
            config.machine.name,
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        )
    });

    if repo.commit(&commit_msg)?.is_none() {
        println!("nothing to commit — already up to date");
        return Ok(());
    }

    println!("committed sync for {}", config.machine.name);
    println!("  snapshot: {}", repo_path.display());
    println!("  tip: run 'bread sync export' to create a portable snapshot");
    if config.packages.enabled {
        println!("  packages: {}", config.packages.managers.join(", "));
    }
    Ok(())
}

async fn cmd_sync_pull(cfg_dir: &Path, install_packages: bool, socket: &Path) -> Result<()> {
    let config = load_sync_config(cfg_dir)?;
    let repo_path = SyncConfig::local_repo_path();

    if !repo_path.exists() {
        eprintln!("bread: no local snapshot found. Run 'bread sync push' first.");
        std::process::exit(1);
    }

    // Apply bread/ → ~/.config/bread/
    let bread_src = repo_path.join("bread");
    if bread_src.exists() {
        delegates::sync_dir(&bread_src, cfg_dir, &[])?;
    }

    // Apply configs/ entries back to their original locations
    let configs_dir = repo_path.join("configs");
    if configs_dir.exists() {
        let delegate_paths = delegates::resolve_include_paths(&config.delegates.include);
        for (basename, dst_path) in &delegate_paths {
            let src = configs_dir.join(basename);
            if src.exists() {
                delegates::sync_dir(&src, dst_path, &config.delegates.exclude)?;
            }
        }
    }

    // Package installs
    if config.packages.enabled {
        let packages_dir = repo_path.join("packages");
        if install_packages {
            run_package_installs(&packages_dir, &config.packages.managers)?;
        } else {
            // Check if packages differ
            let has_package_files = config
                .packages
                .managers
                .iter()
                .any(|m| packages_dir.join(format!("{m}.txt")).exists());
            if has_package_files {
                println!(
                    "note: run 'bread sync pull --install-packages' to install missing packages"
                );
            }
        }
    }

    // Notify daemon
    try_daemon_reload(socket).await;

    println!("applied sync for {}", config.machine.name);
    Ok(())
}

async fn cmd_sync_status(cfg_dir: &Path) -> Result<()> {
    let config = load_sync_config(cfg_dir)?;
    let repo_path = SyncConfig::local_repo_path();

    if !repo_path.exists() {
        println!("bread sync status");
        println!("  not yet committed — run 'bread sync push'");
        return Ok(());
    }

    let repo = SyncRepo::open(&repo_path)?;

    let last_commit = repo
        .last_commit_time()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "never".to_string());

    println!("bread sync status");
    println!("  machine      {}", config.machine.name);
    println!("  snapshot     {}", repo_path.display());
    println!("  last commit  {}", last_commit);

    let local_changes = repo.local_changes()?;
    println!();
    println!("uncommitted changes:");
    if local_changes.is_empty() {
        println!("  none");
    } else {
        for (ch, path) in &local_changes {
            println!("  {}  {}", ch, path);
        }
    }

    Ok(())
}

async fn cmd_sync_diff(cfg_dir: &Path, _vs_remote: bool) -> Result<()> {
    let _config = load_sync_config(cfg_dir)?;
    let repo_path = SyncConfig::local_repo_path();

    if !repo_path.exists() {
        eprintln!("bread: sync repo not initialized. Run: bread sync push");
        std::process::exit(1);
    }

    let repo = SyncRepo::open(&repo_path)?;
    let diff = repo.working_diff()?;
    print!("{}", diff);
    Ok(())
}

async fn cmd_sync_machines(cfg_dir: &Path) -> Result<()> {
    let _ = load_sync_config(cfg_dir)?;
    let repo_path = SyncConfig::local_repo_path();
    let machines_dir = repo_path.join("machines");

    let profiles = machine::MachineProfile::list(&machines_dir)?;
    for p in &profiles {
        let tags = if p.tags.is_empty() {
            String::new()
        } else {
            format!("  tags: {}", p.tags.join(", "))
        };
        println!("  {:20} last sync: {}{}", p.name, &p.last_sync[..16], tags);
    }
    Ok(())
}

async fn cmd_sync_export(cfg_dir: &Path, output: Option<PathBuf>) -> Result<()> {
    // Load sync config if available; fall back to machine defaults.
    let config = match SyncConfig::load(cfg_dir) {
        Ok(c) => c,
        Err(_) => {
            let name = machine::hostname();
            SyncConfig {
                remote: bread_sync::config::RemoteConfig {
                    url: String::new(),
                    branch: "main".to_string(),
                },
                machine: bread_sync::config::MachineConfig { name, tags: vec![] },
                packages: bread_sync::config::PackagesConfig::default(),
                delegates: bread_sync::config::DelegatesConfig::default(),
            }
        }
    };

    let date = chrono::Utc::now().format("%Y-%m-%d");
    let export_name = format!("bread-export-{}-{}", config.machine.name, date);

    // Decide: tarball or directory?
    let (staging_path, make_tarball, final_path) = match &output {
        Some(p) if p.extension().and_then(|e| e.to_str()) == Some("gz") => {
            // User wants a .tar.gz at a specific path
            let staging = std::env::temp_dir().join(&export_name);
            (staging, true, p.clone())
        }
        Some(p) if p.is_dir() || !p.exists() => {
            // User wants a directory
            let dir = if p.is_dir() { p.join(&export_name) } else { p.clone() };
            (dir.clone(), false, dir)
        }
        Some(p) => {
            anyhow::bail!("output path {} already exists and is not a directory", p.display());
        }
        None => {
            // Default: .tar.gz in current directory
            let tarball = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(format!("{export_name}.tar.gz"));
            let staging = std::env::temp_dir().join(&export_name);
            (staging, true, tarball)
        }
    };

    // Stage everything into the staging directory
    let manifest = stage_export(cfg_dir, &config, &staging_path)
        .context("failed to stage export")?;

    // Optionally pack into a tarball
    if make_tarball {
        create_tarball(&staging_path, &final_path)
            .context("failed to create tarball")?;
        std::fs::remove_dir_all(&staging_path).ok();
    }

    println!("exported to {}", final_path.display());
    println!("  machine:  {}", manifest.machine);
    if !manifest.configs.is_empty() {
        println!("  configs:  {}", manifest.configs.join(", "));
    }
    if !manifest.path_map.is_empty() {
        let file_count = manifest.path_map.iter().filter(|r| r.is_file).count();
        let dir_count = manifest.path_map.iter().filter(|r| !r.is_file).count();
        if file_count > 0 {
            println!("  dotfiles: {} file(s)", file_count);
        }
        if dir_count > manifest.configs.len() {
            println!("  dirs:     {} total", dir_count);
        }
    }
    if !manifest.packages.is_empty() {
        println!("  packages: {}", manifest.packages.join(", "));
    }
    if !manifest.repos.is_empty() {
        println!("  repos:    {} git repositories tracked", manifest.repos.len());
    }
    if manifest.system {
        println!("  system:   udev / modprobe / sysctl (see restore.sh for sudo commands)");
    }
    Ok(())
}

async fn cmd_sync_import(
    cfg_dir: &Path,
    from: PathBuf,
    install_packages: bool,
    clone_repos: bool,
    yes: bool,
    socket: &Path,
) -> Result<()> {
    // Determine staging directory
    let is_tarball = from.extension().and_then(|e| e.to_str()) == Some("gz");

    let (staging, _tmp_guard) = if is_tarball {
        let tmp = tempfile::tempdir().context("failed to create temp dir")?;
        extract_tarball(&from, tmp.path()).context("failed to extract tarball")?;
        // GitHub-style tarballs extract into a single subdirectory; unwrap if needed
        let inner = find_single_subdir(tmp.path()).unwrap_or_else(|| tmp.path().to_path_buf());
        (inner, Some(tmp))
    } else if from.is_dir() {
        (from.clone(), None)
    } else {
        anyhow::bail!("'{}' is not a directory or .tar.gz file", from.display());
    };

    // Read manifest for summary
    let manifest_path = staging.join("manifest.toml");
    if !manifest_path.exists() {
        anyhow::bail!("not a bread export: manifest.toml not found in {}", staging.display());
    }
    let manifest_raw = std::fs::read_to_string(&manifest_path)?;
    let manifest: bread_sync::ExportManifest = toml::from_str(&manifest_raw)
        .context("failed to parse manifest.toml")?;

    println!("bread import: {} (exported {})", manifest.machine, &manifest.exported_at[..16]);
    println!("  configs:  {}", if manifest.configs.is_empty() { "-".to_string() } else { manifest.configs.join(", ") });
    println!("  packages: {}", if manifest.packages.is_empty() { "-".to_string() } else { manifest.packages.join(", ") });
    if !manifest.repos.is_empty() {
        println!("  repos:    {} git repositories found", manifest.repos.len());
        if clone_repos {
            println!("            (will be cloned to their original locations)");
        } else {
            println!("            (skipping clone — remove --no-clone-repos to restore)");
        }
    }
    if manifest.system {
        println!("  note: system files (udev/modprobe/sysctl) will NOT be applied automatically");
    }

    if !yes {
        print!("\nApply to ~/.config and ~/.local? (y/n): ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("aborted");
            return Ok(());
        }
    }

    let applied = apply_import(&staging, cfg_dir, install_packages, clone_repos)
        .context("import failed")?;

    println!();
    for item in &applied {
        println!("  + {item}");
    }

    if manifest.system {
        println!();
        println!("system files were NOT applied automatically. To restore them:");
        println!("  {}/restore.sh", staging.display());
    }

    // Notify daemon
    try_daemon_reload(socket).await;

    Ok(())
}

fn create_tarball(src_dir: &Path, dest: &Path) -> Result<()> {
    use flate2::{write::GzEncoder, Compression};

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(dest)
        .with_context(|| format!("failed to create {}", dest.display()))?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = tar::Builder::new(encoder);

    let base_name = src_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("bread-export");

    // Walk the staging directory and append every file
    append_dir_recursive(&mut archive, src_dir, src_dir, base_name)?;

    archive.finish()?;
    Ok(())
}

fn append_dir_recursive(
    archive: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
    root: &Path,
    current: &Path,
    base_name: &str,
) -> Result<()> {
    for entry in std::fs::read_dir(current).context("failed to read dir for tarball")? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let tar_path = PathBuf::from(base_name).join(rel);

        if path.is_dir() {
            archive.append_dir(&tar_path, &path)?;
            append_dir_recursive(archive, root, &path, base_name)?;
        } else if path.is_file() {
            archive.append_path_with_name(&path, &tar_path)?;
        }
    }
    Ok(())
}

fn extract_tarball(src: &Path, dest: &Path) -> Result<()> {
    use flate2::read::GzDecoder;

    let file = std::fs::File::open(src)
        .with_context(|| format!("failed to open {}", src.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest)
        .with_context(|| format!("failed to extract {}", src.display()))?;
    Ok(())
}

/// If a directory contains exactly one subdirectory and nothing else, return it.
fn find_single_subdir(dir: &Path) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .collect();
    if entries.len() == 1 && entries[0].path().is_dir() {
        Some(entries[0].path())
    } else {
        None
    }
}

fn load_sync_config(cfg_dir: &Path) -> Result<SyncConfig> {
    match SyncConfig::load(cfg_dir) {
        Ok(c) => Ok(c),
        Err(_) => {
            eprintln!("bread: sync not initialized. Run: bread sync init");
            std::process::exit(1);
        }
    }
}

fn run_package_installs(packages_dir: &Path, managers: &[String]) -> Result<()> {
    for manager in managers {
        let file = packages_dir.join(format!("{manager}.txt"));
        if !file.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&file)?;
        match manager.as_str() {
            "pacman" => {
                let pkgs = packages::parse_pacman(&content);
                if pkgs.is_empty() {
                    continue;
                }
                let mut cmd = std::process::Command::new("sudo");
                cmd.args(["pacman", "-S", "--needed"]).args(&pkgs);
                let _ = cmd.status();
            }
            "pip" => {
                let mut cmd = std::process::Command::new("pip");
                cmd.args(["install", "--user", "-r"]).arg(&file);
                let _ = cmd.status();
            }
            "npm" => {
                let pkgs = packages::parse_npm(&content);
                for pkg in pkgs {
                    let _ = std::process::Command::new("npm")
                        .args(["install", "-g", &pkg])
                        .status();
                }
            }
            "cargo" => {
                let pkgs = packages::parse_cargo(&content);
                for pkg in pkgs {
                    let _ = std::process::Command::new("cargo")
                        .args(["install", &pkg])
                        .status();
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers (shared with original commands)
// ---------------------------------------------------------------------------

fn daemon_socket_path() -> PathBuf {
    if let Ok(runtime) = env::var("XDG_RUNTIME_DIR") {
        return Path::new(&runtime).join("bread").join("breadd.sock");
    }
    PathBuf::from("/tmp/bread/breadd.sock")
}

async fn send_request(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let stream = UnixStream::connect(socket).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound
            || e.kind() == std::io::ErrorKind::ConnectionRefused
        {
            anyhow::anyhow!(
                "bread: daemon is not running. Start it with: systemctl --user start breadd"
            )
        } else {
            e.into()
        }
    })?;

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
        let replay = send_request(
            socket,
            "events.replay",
            json!({ "since_ms": seconds * 1000 }),
        )
        .await?;
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
        "params": {
            "filter": filter,
        },
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

    // SAFETY: localtime_r is thread-safe. We pass a valid pointer to a
    // zeroed tm struct and read the result only after the call returns.
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

        let response = send_request(socket, "modules.reload", json!({})).await?;
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
    let version = health
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
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
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return Path::new(&xdg).join("bread");
    }
    if let Ok(home) = env::var("HOME") {
        return Path::new(&home).join(".config/bread");
    }
    PathBuf::from(".config/bread")
}
