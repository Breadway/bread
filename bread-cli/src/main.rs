use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::env;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Parser, Debug)]
#[command(author, version, about = "Bread CLI - the reactive desktop automation fabric")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Hot-reload all Lua modules
    Reload,
    /// Dump current runtime state
    State,
    /// Stream live normalized events
    Events {
        #[arg(long)]
        filter: Option<String>,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = daemon_socket_path();

    match &cli.command {
        Commands::Reload => {
            let response = send_request(&socket, "modules.reload", json!({})).await?;
            print_json(&response)?;
        }
        Commands::State => {
            let response = send_request(&socket, "state.dump", json!({})).await?;
            print_json(&response)?;
        }
        Commands::Events { filter } => {
            stream_events(&socket, filter.clone()).await?;
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

async fn stream_events(socket: &Path, filter: Option<String>) -> Result<()> {
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
        println!("{}", serde_json::to_string_pretty(&value)?);
    }

    Ok(())
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
