use std::env;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::adapters::Adapter;

#[derive(Clone, Default)]
pub struct HyprlandAdapter;

#[async_trait::async_trait]
impl Adapter for HyprlandAdapter {
    fn name(&self) -> &'static str {
        "hyprland"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        debug!("hyprland adapter started");
        let socket = hyprland_event_socket()?;
        let stream = UnixStream::connect(&socket).await?;
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await? {
            let (kind, data) = parse_hyprland_line(&line);
            tx.send(RawEvent {
                source: AdapterSource::Hyprland,
                kind: "hyprland.event".to_string(),
                payload: json!({
                    "kind": kind,
                    "raw": line,
                    "data": data,
                }),
                timestamp: now_unix_ms(),
            })
            .await?;
        }

        warn!("hyprland socket closed");
        Err(anyhow!("hyprland socket closed"))
    }
}

fn hyprland_event_socket() -> Result<PathBuf> {
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());

    // If the env var is set, use it directly.
    if let Ok(instance) = env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        return Ok(PathBuf::from(runtime)
            .join("hypr")
            .join(instance)
            .join(".socket2.sock"));
    }

    // Otherwise scan $XDG_RUNTIME_DIR/hypr/ for a running instance.
    // Hyprland creates a per-instance directory there containing .socket2.sock.
    // This handles the case where breadd starts as a systemd user service before
    // Hyprland has exported HYPRLAND_INSTANCE_SIGNATURE into the environment.
    let hypr_dir = PathBuf::from(&runtime).join("hypr");
    let mut sockets: Vec<PathBuf> = std::fs::read_dir(&hypr_dir)
        .map_err(|_| anyhow!("no Hyprland instance found ({})", hypr_dir.display()))?
        .flatten()
        .map(|e| e.path().join(".socket2.sock"))
        .filter(|p| p.exists())
        .collect();

    match sockets.len() {
        0 => Err(anyhow!(
            "no Hyprland instance found in {}",
            hypr_dir.display()
        )),
        1 => Ok(sockets.remove(0)),
        n => {
            warn!("found {n} Hyprland instances, using first");
            Ok(sockets.remove(0))
        }
    }
}

fn parse_hyprland_line(line: &str) -> (String, String) {
    if let Some((kind, data)) = line.split_once(">>") {
        return (kind.to_string(), data.to_string());
    }

    ("unknown".to_string(), line.to_string())
}
