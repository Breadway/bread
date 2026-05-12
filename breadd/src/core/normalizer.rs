use std::collections::HashMap;
use std::sync::RwLock;

use bread_shared::{AdapterSource, BreadEvent, RawEvent};
use serde_json::{json, Value};

/// How many multiples of `dedup_window_ms` an entry must be idle before eviction.
const EVICT_MULTIPLIER: u64 = 60;

pub struct EventNormalizer {
    dedup_window_ms: u64,
    recent: RwLock<HashMap<String, u64>>,
    /// Tracks the first time a physical device (keyed by verb+vendor_id+product_id)
    /// fired within the current window, so subsequent child-node events from the
    /// same plug-in are suppressed at the normalizer level.
    seen_devices: RwLock<HashMap<String, u64>>,
}

impl EventNormalizer {
    pub fn new(dedup_window_ms: u64) -> Self {
        Self {
            dedup_window_ms,
            recent: RwLock::new(HashMap::new()),
            seen_devices: RwLock::new(HashMap::new()),
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

        // "bind" is the kernel attaching a driver to an interface — not a meaningful
        // device state change for automation purposes.
        if action == "bind" {
            return vec![];
        }

        let name = raw.payload.get("name").and_then(Value::as_str).unwrap_or("unknown");
        let vendor = raw.payload.get("id_vendor").and_then(Value::as_str).unwrap_or_default();
        let vendor_id = raw.payload.get("vendor_id").and_then(Value::as_str).unwrap_or_default();
        let product_id = raw.payload.get("product_id").and_then(Value::as_str).unwrap_or_default();
        let subsystem = raw.payload.get("subsystem").and_then(Value::as_str).unwrap_or_default();

        // Drop anonymous child USB interfaces (e.g. 3-5:1.0, 3-5:1.1) that carry
        // no identity information — they are USB protocol artefacts, not devices.
        if name == "unknown" && vendor.is_empty() && vendor_id.is_empty() {
            return vec![];
        }

        // For connected/disconnected, suppress duplicate events from child nodes of
        // the same physical device (e.g. input66, mouse0, event17 all from one plug-in).
        // Key by verb+vendor_id+product_id so a second distinct device of the same
        // model plugged in after the window still fires correctly.
        let verb = match action {
            "add" => "connected",
            "remove" => "disconnected",
            _ => "changed",
        };

        if (verb == "connected" || verb == "disconnected") && !vendor_id.is_empty() && !product_id.is_empty() {
            let device_key = format!("{}:{}:{}", verb, vendor_id, product_id);
            let now = raw.timestamp;
            let already_seen = {
                let seen = self.seen_devices.read().unwrap_or_else(|p| p.into_inner());
                seen.get(&device_key)
                    .map(|&last| now.saturating_sub(last) < self.dedup_window_ms)
                    .unwrap_or(false)
            };
            if already_seen {
                return vec![];
            }
            let mut seen = self.seen_devices.write().unwrap_or_else(|p| p.into_inner());
            seen.insert(device_key, now);
            // Evict stale entries
            let evict_before = now.saturating_sub(self.dedup_window_ms.saturating_mul(EVICT_MULTIPLIER));
            if evict_before > 0 {
                seen.retain(|_, &mut last| last >= evict_before);
            }
        }

        let id = raw.payload.get("id").and_then(Value::as_str).unwrap_or("unknown");

        // Device name is always "unknown" here; the state engine applies user-defined
        // classification rules from devices.lua before dispatching to subscribers.
        vec![BreadEvent {
            event: format!("bread.device.{}", verb),
            timestamp: raw.timestamp,
            source: AdapterSource::Udev,
            data: json!({
                "id": id,
                "device": "unknown",
                "name": name,
                "vendor": vendor,
                "vendor_id": vendor_id,
                "product_id": product_id,
                "subsystem": subsystem,
                "raw": raw.payload,
            }),
        }]
    }

    fn normalize_hyprland(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let kind = raw.payload.get("kind").and_then(Value::as_str).unwrap_or("unknown");
        let data = raw
            .payload
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or("");

        match kind {
            "workspace" | "workspacev2" => vec![BreadEvent {
                event: "bread.workspace.changed".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: raw.payload.clone(),
            }],
            "createworkspace" => vec![BreadEvent {
                event: "bread.workspace.created".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: json!({ "workspace": data }),
            }],
            "destroyworkspace" => vec![BreadEvent {
                event: "bread.workspace.destroyed".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: json!({ "workspace": data }),
            }],
            "monitoradded" => vec![BreadEvent {
                event: "bread.monitor.connected".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: json!({ "name": data }),
            }],
            "monitorremoved" => vec![BreadEvent {
                event: "bread.monitor.disconnected".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: json!({ "name": data }),
            }],
            "activewindow" => vec![BreadEvent {
                event: "bread.window.focus.changed".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: raw.payload.clone(),
            }],
            "activewindowv2" => {
                let fields = split_hyprland_fields(data);
                vec![BreadEvent {
                    event: "bread.window.focused".to_string(),
                    timestamp: raw.timestamp,
                    source: AdapterSource::Hyprland,
                    data: json!({
                        "address": fields.get(0).unwrap_or(&"")
                    }),
                }]
            }
            "openwindow" => {
                let fields = split_hyprland_fields(data);
                vec![BreadEvent {
                    event: "bread.window.opened".to_string(),
                    timestamp: raw.timestamp,
                    source: AdapterSource::Hyprland,
                    data: json!({
                        "address": fields.get(0).unwrap_or(&""),
                        "workspace": fields.get(1).unwrap_or(&""),
                        "class": fields.get(2).unwrap_or(&""),
                        "title": fields.get(3).unwrap_or(&""),
                    }),
                }]
            }
            "closewindow" => {
                let fields = split_hyprland_fields(data);
                vec![BreadEvent {
                    event: "bread.window.closed".to_string(),
                    timestamp: raw.timestamp,
                    source: AdapterSource::Hyprland,
                    data: json!({ "address": fields.get(0).unwrap_or(&"") }),
                }]
            }
            "movewindow" => {
                let fields = split_hyprland_fields(data);
                vec![BreadEvent {
                    event: "bread.window.moved".to_string(),
                    timestamp: raw.timestamp,
                    source: AdapterSource::Hyprland,
                    data: json!({
                        "address": fields.get(0).unwrap_or(&""),
                        "workspace": fields.get(1).unwrap_or(&""),
                    }),
                }]
            }
            _ => vec![BreadEvent {
                event: "bread.hyprland.event".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: raw.payload.clone(),
            }],
        }
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

fn split_hyprland_fields(data: &str) -> Vec<&str> {
    if data.is_empty() {
        return Vec::new();
    }
    data.split(">>").collect()
}

