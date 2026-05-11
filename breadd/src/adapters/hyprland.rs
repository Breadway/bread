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
    let instance = env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .map_err(|_| anyhow!("HYPRLAND_INSTANCE_SIGNATURE is not set"))?;
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    Ok(PathBuf::from(runtime)
        .join("hypr")
        .join(instance)
        .join(".socket2.sock"))
}

fn parse_hyprland_line(line: &str) -> (String, String) {
    if let Some((kind, data)) = line.split_once(">>") {
        return (kind.to_string(), data.to_string());
    }

    ("unknown".to_string(), line.to_string())
}
