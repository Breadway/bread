use anyhow::Result;
use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

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
    /// List loaded modules and status
    Modules,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = daemon_socket_path();

    match &cli.command {
        Commands::Reload { watch } => {
            if *watch {
                watch_reload(&socket).await?;
            } else {
                let response = send_request(&socket, "modules.reload", json!({})).await?;
                print_reload(&response);
            }
        }
        Commands::State { path, json } => {
            if *json {
                let response = if let Some(path) = path {
                    send_request(&socket, "state.get", json!({ "key": path })).await?
                } else {
                    send_request(&socket, "state.dump", json!({})).await?
                };
                print_json(&response)?;
            } else {
                let response = if let Some(path) = path {
                    send_request(&socket, "state.get", json!({ "key": path })).await?
                } else {
                    send_request(&socket, "state.dump", json!({})).await?
                };
                print_state_formatted(path.as_deref(), &response);
            }
        }
        Commands::Events {
            filter,
            json,
            fields,
            since,
        } => {
            stream_events(&socket, filter.clone(), *json, fields.clone(), *since).await?;
        }
        Commands::Modules => {
            let response = send_request(&socket, "modules.list", json!({})).await?;
            print_json(&response)?;
        }
        Commands::ProfileList => {
            let response = send_request(&socket, "profile.list", json!({})).await?;
            print_json(&response)?;
        }
        Commands::ProfileActivate { name } => {
            let response = send_request(&socket, "profile.activate", json!({ "name": name })).await?;
            print_json(&response)?;
        }
        Commands::Emit { event, data } => {
            let parsed = serde_json::from_str::<Value>(data).unwrap_or_else(|_| json!({}));
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
            if *json {
                let response = send_request(&socket, "health", json!({})).await?;
                print_json(&response)?;
            } else {
                print_doctor(&socket).await?;
            }
        }
    }

    Ok(())
}

fn daemon_socket_path() -> PathBuf {
    if let Ok(runtime) = env::var("XDG_RUNTIME_DIR") {
        return Path::new(&runtime).join("bread").join("breadd.sock");
    }
    PathBuf::from("/tmp/bread/breadd.sock")
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
        let replay = send_request(socket, "events.replay", json!({ "since_ms": seconds * 1000 })).await?;
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
        tm.tm_hour as u64 * 3600
            + tm.tm_min as u64 * 60
            + tm.tm_sec as u64
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

        // Debounce: drain any follow-up events that arrive within 150ms.
        // A single file save typically generates 2-3 fs events in rapid succession.
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
    let version = health.get("version").and_then(Value::as_str).unwrap_or("unknown");
    let uptime_ms = health.get("uptime_ms").and_then(Value::as_u64).unwrap_or(0);
    let socket = health.get("socket").and_then(Value::as_str).unwrap_or("?");
    println!("  daemon     {} (pid {})", if ok { "✓ running" } else { "✗ unreachable" }, pid);
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
