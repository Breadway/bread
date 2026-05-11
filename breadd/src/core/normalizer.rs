use std::collections::HashMap;
use std::sync::RwLock;

use bread_shared::{AdapterSource, BreadEvent, RawEvent};
use serde_json::{json, Value};

use crate::core::types::DeviceClass;

/// How many multiples of `dedup_window_ms` an entry must be idle before eviction.
const EVICT_MULTIPLIER: u64 = 60;

pub struct EventNormalizer {
    dedup_window_ms: u64,
    recent: RwLock<HashMap<String, u64>>,
}

impl EventNormalizer {
    pub fn new(dedup_window_ms: u64) -> Self {
        Self {
            dedup_window_ms,
            recent: RwLock::new(HashMap::new()),
        }
    }

    pub fn normalize(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let mut out = match raw.source {
            AdapterSource::Udev => self.normalize_udev(raw),
            AdapterSource::Hyprland => self.normalize_hyprland(raw),
            AdapterSource::Power => self.normalize_power(raw),
            AdapterSource::Network => self.normalize_network(raw),
            AdapterSource::System => vec![BreadEvent {
                event: raw.kind.clone(),
                timestamp: raw.timestamp,
                source: raw.source,
                data: raw.payload.clone(),
            }],
        };

        out.retain(|ev| self.accept(ev));
        out
    }

    fn normalize_udev(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let action = raw.payload.get("action").and_then(Value::as_str).unwrap_or("change");
        let id = raw.payload.get("id").and_then(Value::as_str).unwrap_or("unknown");
        let class = classify_device(&raw.payload);
        let class_str = serde_json::to_string(&class)
            .unwrap_or_else(|_| "\"unknown\"".to_string())
            .replace('"', "");

        let verb = match action {
            "add" => "connected",
            "remove" => "disconnected",
            _ => "changed",
        };

        let mut events = vec![BreadEvent {
            event: format!("bread.device.{}", verb),
            timestamp: raw.timestamp,
            source: AdapterSource::Udev,
            data: json!({
                "id": id,
                "class": class,
                "raw": raw.payload,
            }),
        }];

        events.push(BreadEvent {
            event: format!("bread.device.{}.{}", class_str, verb),
            timestamp: raw.timestamp,
            source: AdapterSource::Udev,
            data: json!({
                "id": id,
                "class": class,
            }),
        });

        events
    }

    fn normalize_hyprland(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let kind = raw.payload.get("kind").and_then(Value::as_str).unwrap_or("unknown");
        let mapped = match kind {
            "workspace" | "workspacev2" => "bread.workspace.changed",
            "monitoradded" => "bread.monitor.connected",
            "monitorremoved" => "bread.monitor.disconnected",
            "activewindow" | "activewindowv2" => "bread.window.focus.changed",
            "openwindow" => "bread.window.opened",
            "closewindow" => "bread.window.closed",
            _ => "bread.hyprland.event",
        };

        vec![BreadEvent {
            event: mapped.to_string(),
            timestamp: raw.timestamp,
            source: AdapterSource::Hyprland,
            data: raw.payload.clone(),
        }]
    }

    fn normalize_power(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let mut events = Vec::new();

        if let Some(ac) = raw.payload.get("ac_connected").and_then(Value::as_bool) {
            events.push(BreadEvent {
                event: if ac {
                    "bread.power.ac.connected".to_string()
                } else {
                    "bread.power.ac.disconnected".to_string()
                },
                timestamp: raw.timestamp,
                source: AdapterSource::Power,
                data: raw.payload.clone(),
            });
        }

        if let Some(level) = raw.payload.get("battery_percent").and_then(Value::as_u64) {
            let battery_event = if level <= 5 {
                Some("bread.power.battery.critical")
            } else if level <= 10 {
                Some("bread.power.battery.very_low")
            } else if level <= 20 {
                Some("bread.power.battery.low")
            } else if level >= 100 {
                Some("bread.power.battery.full")
            } else {
                None
            };

            if let Some(event) = battery_event {
                events.push(BreadEvent {
                    event: event.to_string(),
                    timestamp: raw.timestamp,
                    source: AdapterSource::Power,
                    data: raw.payload.clone(),
                });
            }
        }

        if events.is_empty() {
            events.push(BreadEvent {
                event: "bread.power.changed".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Power,
                data: raw.payload.clone(),
            });
        }

        events
    }

    fn normalize_network(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let online = raw.payload.get("online").and_then(Value::as_bool).unwrap_or(false);
        let name = if online {
            "bread.network.connected"
        } else {
            "bread.network.disconnected"
        };

        vec![BreadEvent {
            event: name.to_string(),
            timestamp: raw.timestamp,
            source: AdapterSource::Network,
            data: raw.payload.clone(),
        }]
    }

    fn accept(&self, event: &BreadEvent) -> bool {
        let key = format!("{}:{}", event.event, event.data);
        let now = event.timestamp;

        // Fast path: check under read lock first.
        {
            let recent = self.recent.read().unwrap_or_else(|p| p.into_inner());
            if let Some(last) = recent.get(&key) {
                if now.saturating_sub(*last) < self.dedup_window_ms {
                    return false;
                }
            }
        }

        // Slow path: acquire write lock, re-check, insert, and periodically evict.
        let mut recent = self.recent.write().unwrap_or_else(|p| p.into_inner());

        // Re-check after acquiring write lock (another thread may have inserted between locks).
        if let Some(last) = recent.get(&key) {
            if now.saturating_sub(*last) < self.dedup_window_ms {
                return false;
            }
        }

        recent.insert(key.clone(), now);

        // Evict stale entries to prevent unbounded growth.
        let evict_before = now.saturating_sub(self.dedup_window_ms.saturating_mul(EVICT_MULTIPLIER));
        if evict_before > 0 {
            recent.retain(|_, &mut last| last >= evict_before);
        }

        true
    }
}

fn classify_device(payload: &Value) -> DeviceClass {
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let subsystem = payload
        .get("subsystem")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();

    if name.contains("dock") {
        return DeviceClass::Dock;
    }
    if subsystem == "input" && name.contains("keyboard") {
        return DeviceClass::Keyboard;
    }
    if subsystem == "input" && name.contains("mouse") {
        return DeviceClass::Mouse;
    }
    if subsystem == "drm" {
        return DeviceClass::Display;
    }
    if subsystem == "sound" || name.contains("audio") {
        return DeviceClass::Audio;
    }
    if subsystem == "block" || name.contains("storage") {
        return DeviceClass::Storage;
    }

    DeviceClass::Unknown
}
