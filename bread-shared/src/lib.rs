use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AdapterSource {
    Hyprland,
    Udev,
    Power,
    Network,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    pub source: AdapterSource,
    pub kind: String,
    pub payload: serde_json::Value,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreadEvent {
    pub event: String,
    pub timestamp: u64,
    pub source: AdapterSource,
    pub data: serde_json::Value,
}

impl BreadEvent {
    pub fn new(event: impl Into<String>, source: AdapterSource, data: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            timestamp: now_unix_ms(),
            source,
            data,
        }
    }
}

pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
