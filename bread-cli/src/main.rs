mod hooks_git;
mod hooks_shell;
mod modules_mgmt;

use anyhow::Result;
use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::collections::HashMap;
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
        /// Render events as a causality tree via `caused_by` instead of a
        /// flat stream (events emitted from inside a `bread.emit()` call
        /// made by another event's Lua handler are nested under it).
        /// Overrides `--json` — tree rendering always uses the formatted view.
        #[arg(long)]
        tree: bool,
    },
    /// Manage installed Lua modules
    Modules {
        #[command(subcommand)]
        subcommand: ModulesCommand,
    },
    /// Install shell/git hook integrations that feed events into breadd
    Hooks {
        #[command(subcommand)]
        subcommand: HooksCommand,
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
        /// Source to tag the event with (terminal/git/remote); routes through
        /// the normalizer instead of being tagged System. Requires --kind.
        #[arg(long)]
        source: Option<String>,
        /// Adapter-specific raw kind (e.g. "command.started"); only used with --source.
        #[arg(long)]
        kind: Option<String>,
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
enum HooksCommand {
    /// Install shell integration hooks (precmd/preexec/chpwd + SSH session
    /// detection) for the current or a named shell
    InstallShell {
        /// Force bash/zsh/fish instead of auto-detecting from $SHELL
        shell: Option<String>,
    },
    /// Install git hooks (post-commit, post-checkout, post-merge) into the
    /// current repository
    InstallGit,
}

#[derive(Subcommand, Debug)]
enum ModulesCommand {
    /// Install a module from a local directory
    Install {
        /// Path to a local module directory
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
    /// Show full manifest details for a module
    Info { name: String },
    /// Statically scan an installed module's Lua source and suggest a
    /// `[[permissions]]` block for its `bread.module.toml` manifest
    Audit { name: String },
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
            tree,
        } => {
            stream_events(&socket, pattern, json, fields, since, tree).await?;
        }
        Commands::Modules { subcommand } => {
            handle_modules_cmd(subcommand, &socket).await?;
        }
        Commands::Hooks { subcommand } => match subcommand {
            HooksCommand::InstallShell { shell } => hooks_shell::install_shell(shell)?,
            HooksCommand::InstallGit => hooks_git::install_git()?,
        },
        Commands::ProfileList => {
            let response = send_request(&socket, "profile.list", json!({})).await?;
            print_json(&response)?;
        }
        Commands::ProfileActivate { name } => {
            let response =
                send_request(&socket, "profile.activate", json!({ "name": name })).await?;
            print_json(&response)?;
        }
        Commands::Emit {
            event,
            data,
            source,
            kind,
        } => {
            let parsed = serde_json::from_str::<Value>(&data).unwrap_or_else(|_| json!({}));
            let mut params = json!({
                "event": event,
                "data": parsed,
            });
            if let Some(source) = source {
                params["source"] = json!(source);
            }
            if let Some(kind) = kind {
                params["kind"] = json!(kind);
            }
            let response = send_request(&socket, "emit", params).await?;
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
            let manifest = install_module(&source, &mods_dir)?;
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
            match &m.permissions {
                None => println!(
                    "permissions:  (none declared — full, ungated bread.* access; see 'bread doctor')"
                ),
                Some(perms) if perms.is_empty() => {
                    println!("permissions:  (declared empty — baseline access only)")
                }
                Some(perms) => {
                    println!("permissions:");
                    for p in perms {
                        let mut line = format!("  - {:?}", p.kind);
                        if let Some(path) = &p.path {
                            line.push_str(&format!(" path={path}"));
                        }
                        if let Some(bin) = &p.bin {
                            line.push_str(&format!(" bin={bin}"));
                        }
                        println!("{line}");
                    }
                }
            }
        }

        ModulesCommand::Audit { name } => {
            let module_dir = mods_dir.join(&name);
            if !module_dir.exists() {
                eprintln!("bread: module '{}' is not installed", name);
                std::process::exit(1);
            }
            let suggested = modules_mgmt::audit_module(&module_dir)?;
            if suggested.is_empty() {
                println!(
                    "bread: no capability-gated bread.* calls found in '{}' — \
                     it appears to only use baseline APIs (events, timers, json, \
                     logging). Declaring `permissions = []` in bread.module.toml \
                     documents that intentionally and avoids the 'no permissions \
                     declared' warning from `bread doctor`.",
                    name
                );
                return Ok(());
            }
            println!(
                "bread: suggested permissions for '{}' (best-effort static scan — \
                 review before pasting into bread.module.toml; false positives \
                 are possible, missing an actually-needed permission should be rare \
                 for direct bread.exec()-style call sites):\n",
                name
            );
            print!("{}", modules_mgmt::render_permissions_toml(&suggested)?);
        }
    }
    Ok(())
}

fn install_module(
    source: &str,
    mods_dir: &std::path::Path,
) -> Result<modules_mgmt::ModuleManifest> {
    let path = modules_mgmt::parse_source(source)?;
    modules_mgmt::install_from_local(&path, source, mods_dir)
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
    tree: bool,
) -> Result<()> {
    // Tree rendering needs `id`/`caused_by` visible in a consistent shape,
    // so it always uses the formatted view — a live-streaming-friendly
    // indent-as-you-go tree rather than buffering the whole stream.
    let mut causality = CausalityTracker::default();

    if let Some(seconds) = since {
        let replay = send_request(
            socket,
            "events.replay",
            json!({ "since_ms": seconds * 1000 }),
        )
        .await?;
        if let Some(list) = replay.as_array() {
            for item in list {
                if tree {
                    causality.print(item);
                } else if raw_json {
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

    // Consume the subscribe ack before entering the event loop.
    match lines.next_line().await? {
        Some(ack) => {
            let v: Value = serde_json::from_str(&ack)?;
            if let Some(err) = v.get("error").and_then(Value::as_str) {
                anyhow::bail!("{err}");
            }
        }
        None => anyhow::bail!("daemon closed connection during subscribe"),
    }

    while let Some(line) = lines.next_line().await? {
        let value: Value = serde_json::from_str(&line)?;
        if tree {
            causality.print(&value);
        } else if raw_json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            print_event(&value, fields.as_deref());
        }
    }

    Ok(())
}

/// Tracks `id` -> `caused_by` for events seen so far in this stream/replay
/// batch, so each new event can be indented under its parent as it arrives
/// — no buffering, no waiting for the stream to end. An event whose parent
/// hasn't been seen yet (e.g. the parent predates a replay window, or the
/// causing event is filtered out by the subscription pattern) is rendered
/// as its own root rather than blocking on a parent that may never show up.
#[derive(Default)]
struct CausalityTracker {
    parents: HashMap<String, Option<String>>,
}

impl CausalityTracker {
    /// Depth = number of ancestors reachable by following `caused_by`.
    /// Guards against cycles/self-loops (shouldn't happen, but a rendering
    /// bug here must never hang the CLI) with both a seen-set and a hard cap.
    fn depth(&self, id: &str) -> usize {
        let mut depth = 0;
        let mut current = id.to_string();
        let mut seen = std::collections::HashSet::new();
        while let Some(Some(parent)) = self.parents.get(&current) {
            if depth >= 64 || !seen.insert(current.clone()) {
                break;
            }
            depth += 1;
            current = parent.clone();
        }
        depth
    }

    fn print(&mut self, event: &Value) {
        let id = event.get("id").and_then(Value::as_str).map(str::to_string);
        let caused_by = event
            .get("caused_by")
            .and_then(Value::as_str)
            .map(str::to_string);

        let depth = if let Some(id) = &id {
            self.parents.insert(id.clone(), caused_by.clone());
            self.depth(id)
        } else {
            0
        };

        let ts = event.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
        let event_name = event.get("event").and_then(Value::as_str).unwrap_or("?");
        let source = event.get("source").and_then(Value::as_str).unwrap_or("?");
        let time = format_timestamp(ts);
        let indent = "  ".repeat(depth);
        let connector = if depth > 0 { "\u{2514}\u{2500} " } else { "" };
        let id_display = id.as_deref().unwrap_or("?");
        println!("{indent}{connector}{time}  {event_name}  source={source}  id={id_display}");
        if let Some(data) = event.get("data") {
            println!("{indent}  data: {data}");
        }
    }
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
    if !socket.exists() {
        println!("bread doctor");
        println!("  daemon     ✗ not running");
        println!("  socket     {}  (not found)", socket.display());
        println!();
        println!("  start the daemon:   systemctl --user start breadd");
        println!("  view logs:          journalctl --user -u breadd -f");
        return Ok(());
    }

    let response = send_request(socket, "health", json!({})).await?;
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
        let mut ungated_count = 0;
        for module in modules {
            let name = module.get("name").and_then(Value::as_str).unwrap_or("?");
            let status = module.get("status").and_then(Value::as_str).unwrap_or("?");
            let error = module.get("last_error").and_then(Value::as_str);
            let ungated = module
                .get("ungated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            println!("  {:30} {}", name, status);
            if let Some(error) = error {
                println!("    └ {error}");
            }
            if ungated {
                ungated_count += 1;
                println!(
                    "    └ ⚠ running with full, ungated access — no permissions \
                     manifest declared (add `[[permissions]]` to its \
                     bread.module.toml, or run `bread modules audit {name}` \
                     for a suggested block)"
                );
            }
        }
        if ungated_count > 0 {
            println!();
            println!(
                "  {ungated_count} module(s) running with full, ungated bread.* access — \
                 see above"
            );
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

fn config_directory() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return Path::new(&xdg).join("bread");
    }
    if let Ok(home) = env::var("HOME") {
        return Path::new(&home).join(".config/bread");
    }
    PathBuf::from(".config/bread")
}
