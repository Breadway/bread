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
                data: raw.payload.clone(),
            }],
            "monitorremoved" => vec![BreadEvent {
                event: "bread.monitor.disconnected".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: raw.payload.clone(),
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

fn classify_device(payload: &Value) -> DeviceClass {
    let subsystem = payload
        .get("subsystem")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();

    // --- Property-based classification (reliable, hardware-agnostic) ---

    // udev sets ID_INPUT_KEYBOARD=1 for anything that presents as a keyboard HID device.
    if payload.get("id_input_keyboard").and_then(Value::as_bool).unwrap_or(false) {
        return DeviceClass::Keyboard;
    }

    // ID_INPUT_MOUSE=1 covers mice and trackballs.
    if payload.get("id_input_mouse").and_then(Value::as_bool).unwrap_or(false) {
        return DeviceClass::Mouse;
    }

    // ID_INPUT_TABLET=1 covers drawing tablets (Wacom etc).
    if payload.get("id_input_tablet").and_then(Value::as_bool).unwrap_or(false) {
        return DeviceClass::Tablet;
    }

    // USB class 0x09 = Hub. Docks expose a hub interface; they also typically
    // expose video (0x0e), audio (0x01), and ethernet (CDC 0x02) interfaces.
    // We check for hub + at least one of those secondary interfaces.
    if let Some(ifaces) = payload.get("id_usb_interfaces").and_then(Value::as_str) {
        let ifaces_lc = ifaces.to_lowercase();
        let has_hub = ifaces_lc.contains(":0900") || ifaces_lc.contains(":0902");
        let has_secondary = ifaces_lc.contains(":0e")   // video
            || ifaces_lc.contains(":0200") // CDC ethernet
            || ifaces_lc.contains(":0100") // audio
            || ifaces_lc.contains(":0801"); // mass storage
        if has_hub && has_secondary {
            return DeviceClass::Dock;
        }
    }

    // USB class 0x01 = Audio.
    if let Some(cls) = payload.get("id_usb_class").and_then(Value::as_str) {
        if cls == "01" || cls.to_lowercase() == "0x01" {
            return DeviceClass::Audio;
        }
        // USB class 0x08 = Mass Storage.
        if cls == "08" || cls.to_lowercase() == "0x08" {
            return DeviceClass::Storage;
        }
    }

    // DRM subsystem = display connector.
    if subsystem == "drm" {
        return DeviceClass::Display;
    }

    // Block devices = storage.
    if subsystem == "block" {
        return DeviceClass::Storage;
    }

    // Sound subsystem = audio.
    if subsystem == "sound" {
        return DeviceClass::Audio;
    }

    // --- Name-based fallback (catches user-registered patterns and obvious names) ---
    // This runs last so the property-based rules above always win.

    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| payload.get("id_model").and_then(Value::as_str))
        .unwrap_or_default()
        .to_lowercase();

    let vendor = payload
        .get("id_vendor")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();

    let combined = format!("{name} {vendor}");

    if combined.contains("dock") || combined.contains("hub") || combined.contains("thunderbolt") {
        return DeviceClass::Dock;
    }
    if combined.contains("keyboard") || combined.contains("kbd") {
        return DeviceClass::Keyboard;
    }
    if combined.contains("mouse") || combined.contains("trackball") || combined.contains("trackpoint") {
        return DeviceClass::Mouse;
    }
    if combined.contains("tablet") || combined.contains("wacom") || combined.contains("stylus") {
        return DeviceClass::Tablet;
    }
    if combined.contains("audio") || combined.contains("headset") || combined.contains("speaker") || combined.contains("dac") {
        return DeviceClass::Audio;
    }
    if combined.contains("storage") || combined.contains("drive") || combined.contains("flash") || combined.contains("disk") {
        return DeviceClass::Storage;
    }

    DeviceClass::Unknown
}
