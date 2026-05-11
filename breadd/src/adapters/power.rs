use std::fs;
use std::path::Path;

use anyhow::Result;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::debug;

use crate::adapters::Adapter;

#[derive(Clone)]
pub struct PowerAdapter {
    poll_interval_secs: u64,
}

impl PowerAdapter {
    pub fn new(poll_interval_secs: u64) -> Self {
        Self { poll_interval_secs }
    }
}

#[async_trait::async_trait]
impl Adapter for PowerAdapter {
    fn name(&self) -> &'static str {
        "power"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        debug!("power adapter started");

        let mut last = read_power_state();
        tx.send(power_raw_event(&last)).await?;

        loop {
            sleep(Duration::from_secs(self.poll_interval_secs.max(5))).await;
            let now = read_power_state();
            if now != last {
                tx.send(power_raw_event(&now)).await?;
                last = now;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PowerSnapshot {
    ac_connected: bool,
    battery_percent: Option<u8>,
}

fn power_raw_event(snapshot: &PowerSnapshot) -> RawEvent {
    RawEvent {
        source: AdapterSource::Power,
        kind: "power.snapshot".to_string(),
        payload: json!({
            "ac_connected": snapshot.ac_connected,
            "battery_percent": snapshot.battery_percent,
        }),
        timestamp: now_unix_ms(),
    }
}

fn read_power_state() -> PowerSnapshot {
    let power_dir = Path::new("/sys/class/power_supply");
    let mut ac_connected = false;
    let mut battery_percent = None;

    if let Ok(entries) = fs::read_dir(power_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let typ = fs::read_to_string(path.join("type")).unwrap_or_default();
            if typ.trim().eq_ignore_ascii_case("Mains") || typ.trim().eq_ignore_ascii_case("USB") {
                let online = fs::read_to_string(path.join("online")).unwrap_or_default();
                if online.trim() == "1" {
                    ac_connected = true;
                }
            } else if typ.trim().eq_ignore_ascii_case("Battery") {
                let cap = fs::read_to_string(path.join("capacity")).unwrap_or_default();
                if let Ok(parsed) = cap.trim().parse::<u8>() {
                    battery_percent = Some(parsed.min(100));
                }
            }
        }
    }

    PowerSnapshot {
        ac_connected,
        battery_percent,
    }
}
