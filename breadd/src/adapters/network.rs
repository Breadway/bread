use std::collections::BTreeMap;
use std::fs;

use anyhow::Result;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::debug;

use crate::adapters::Adapter;

#[derive(Clone, Default)]
pub struct NetworkAdapter;

#[async_trait::async_trait]
impl Adapter for NetworkAdapter {
    fn name(&self) -> &'static str {
        "network"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        debug!("network adapter started");
        let mut last = read_network_state();
        tx.send(network_raw_event(&last)).await?;

        loop {
            sleep(Duration::from_secs(5)).await;
            let now = read_network_state();
            if now != last {
                tx.send(network_raw_event(&now)).await?;
                last = now;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct NetworkSnapshot {
    interfaces: BTreeMap<String, bool>,
    online: bool,
}

fn network_raw_event(snapshot: &NetworkSnapshot) -> RawEvent {
    let interfaces = snapshot
        .interfaces
        .iter()
        .map(|(name, up)| (name.clone(), json!({ "up": up })))
        .collect::<serde_json::Map<String, serde_json::Value>>();

    RawEvent {
        source: AdapterSource::Network,
        kind: "network.snapshot".to_string(),
        payload: json!({
            "online": snapshot.online,
            "interfaces": interfaces,
        }),
        timestamp: now_unix_ms(),
    }
}

fn read_network_state() -> NetworkSnapshot {
    let mut interfaces = BTreeMap::new();

    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "lo" {
                continue;
            }
            let oper = fs::read_to_string(entry.path().join("operstate")).unwrap_or_default();
            let up = oper.trim() == "up";
            interfaces.insert(name, up);
        }
    }

    let online = has_default_route();

    NetworkSnapshot { interfaces, online }
}

fn has_default_route() -> bool {
    if let Ok(routes) = fs::read_to_string("/proc/net/route") {
        for line in routes.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() > 2 && cols[1] == "00000000" {
                return true;
            }
        }
    }

    false
}
