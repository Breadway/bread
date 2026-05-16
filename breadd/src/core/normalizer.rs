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
            AdapterSource::Bluetooth => self.normalize_bluetooth(raw),
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
        let action = raw
            .payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("change");

        // "bind" is the kernel attaching a driver to an interface — not a meaningful
        // device state change for automation purposes.
        if action == "bind" {
            return vec![];
        }

        let name = raw
            .payload
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let vendor = raw
            .payload
            .get("id_vendor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let vendor_id = raw
            .payload
            .get("vendor_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let product_id = raw
            .payload
            .get("product_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let subsystem = raw
            .payload
            .get("subsystem")
            .and_then(Value::as_str)
            .unwrap_or_default();

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

        if (verb == "connected" || verb == "disconnected")
            && !vendor_id.is_empty()
            && !product_id.is_empty()
        {
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
            let evict_before =
                now.saturating_sub(self.dedup_window_ms.saturating_mul(EVICT_MULTIPLIER));
            if evict_before > 0 {
                seen.retain(|_, &mut last| last >= evict_before);
            }
        }

        let id = raw
            .payload
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

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
        let kind = raw
            .payload
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
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
                        "address": fields.first().unwrap_or(&"")
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
                        "address": fields.first().unwrap_or(&""),
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
                    data: json!({ "address": fields.first().unwrap_or(&"") }),
                }]
            }
            "movewindow" => {
                let fields = split_hyprland_fields(data);
                vec![BreadEvent {
                    event: "bread.window.moved".to_string(),
                    timestamp: raw.timestamp,
                    source: AdapterSource::Hyprland,
                    data: json!({
                        "address": fields.first().unwrap_or(&""),
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

    fn normalize_bluetooth(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let path = raw
            .payload
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let address = raw
            .payload
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let name = raw
            .payload
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| {
                raw.payload
                    .pointer("/properties/Name")
                    .or_else(|| raw.payload.pointer("/properties/Alias"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("unknown");

        match raw.kind.as_str() {
            "bluetooth.enumerate" | "bluetooth.device.connected" => vec![BreadEvent {
                event: "bread.device.connected".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Bluetooth,
                data: json!({
                    "id": path,
                    "device": "unknown",
                    "name": name,
                    "address": address,
                    "subsystem": "bluetooth",
                    "raw": raw.payload,
                }),
            }],
            "bluetooth.device.disconnected" => vec![BreadEvent {
                event: "bread.device.disconnected".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Bluetooth,
                data: json!({
                    "id": path,
                    "device": "unknown",
                    "name": name,
                    "address": address,
                    "subsystem": "bluetooth",
                    "raw": raw.payload,
                }),
            }],
            "bluetooth.device.added" => vec![BreadEvent {
                event: "bread.bluetooth.device.paired".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Bluetooth,
                data: json!({
                    "id": path,
                    "name": name,
                    "address": address,
                    "subsystem": "bluetooth",
                    "raw": raw.payload,
                }),
            }],
            "bluetooth.device.removed" => vec![BreadEvent {
                event: "bread.bluetooth.device.unpaired".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Bluetooth,
                data: json!({
                    "id": path,
                    "address": address,
                    "subsystem": "bluetooth",
                    "raw": raw.payload,
                }),
            }],
            _ => vec![],
        }
    }

    fn normalize_network(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let online = raw
            .payload
            .get("online")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
        let evict_before =
            now.saturating_sub(self.dedup_window_ms.saturating_mul(EVICT_MULTIPLIER));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(source: AdapterSource, kind: &str, payload: Value, ts: u64) -> RawEvent {
        RawEvent {
            source,
            kind: kind.to_string(),
            payload,
            timestamp: ts,
        }
    }

    // ─── Udev ─────────────────────────────────────────────────────────────

    #[test]
    fn udev_add_emits_connected_with_identity_fields() {
        let n = EventNormalizer::new(100);
        let ev = raw(
            AdapterSource::Udev,
            "udev",
            json!({
                "action": "add",
                "name": "Logitech Mouse",
                "id_vendor": "Logitech",
                "vendor_id": "046d",
                "product_id": "c52b",
                "subsystem": "usb",
                "id": "1-1.4",
            }),
            1000,
        );
        let out = n.normalize(&ev);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.device.connected");
        assert_eq!(out[0].data.get("vendor_id").unwrap(), "046d");
        assert_eq!(out[0].data.get("product_id").unwrap(), "c52b");
        assert_eq!(out[0].data.get("name").unwrap(), "Logitech Mouse");
        assert_eq!(out[0].data.get("subsystem").unwrap(), "usb");
        assert_eq!(out[0].data.get("device").unwrap(), "unknown");
    }

    #[test]
    fn udev_remove_emits_disconnected() {
        let n = EventNormalizer::new(100);
        let ev = raw(
            AdapterSource::Udev,
            "udev",
            json!({
                "action": "remove",
                "name": "Logitech",
                "vendor_id": "046d",
                "product_id": "c52b",
                "subsystem": "usb",
                "id": "1-1.4",
            }),
            1000,
        );
        let out = n.normalize(&ev);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.device.disconnected");
    }

    #[test]
    fn udev_bind_action_is_suppressed() {
        let n = EventNormalizer::new(100);
        let ev = raw(
            AdapterSource::Udev,
            "udev",
            json!({
                "action": "bind",
                "name": "x",
                "vendor_id": "046d",
                "product_id": "c52b",
            }),
            1000,
        );
        assert!(n.normalize(&ev).is_empty());
    }

    #[test]
    fn udev_anonymous_child_interface_is_dropped() {
        let n = EventNormalizer::new(100);
        // No name, no vendor — pure USB protocol artefact.
        let ev = raw(
            AdapterSource::Udev,
            "udev",
            json!({
                "action": "add",
                "id": "3-5:1.0",
            }),
            1000,
        );
        assert!(n.normalize(&ev).is_empty());
    }

    #[test]
    fn udev_dedupes_child_nodes_of_same_physical_device() {
        let n = EventNormalizer::new(1000);
        let mk = |id: &str, ts: u64| {
            raw(
                AdapterSource::Udev,
                "udev",
                json!({
                    "action": "add",
                    "name": "Hub Device",
                    "vendor_id": "1d6b",
                    "product_id": "0002",
                    "subsystem": "usb",
                    "id": id,
                }),
                ts,
            )
        };
        // First child fires
        assert_eq!(n.normalize(&mk("usb-1", 1000)).len(), 1);
        // Sibling within window is suppressed
        assert_eq!(n.normalize(&mk("usb-2", 1050)).len(), 0);
        // After the dedup window, a sibling fires again
        assert_eq!(n.normalize(&mk("usb-3", 3000)).len(), 1);
    }

    #[test]
    fn udev_disconnect_does_not_share_dedup_with_connect() {
        let n = EventNormalizer::new(1000);
        let connect = raw(
            AdapterSource::Udev,
            "udev",
            json!({"action": "add", "name": "x", "vendor_id": "1", "product_id": "2", "id": "a"}),
            1000,
        );
        let disconnect = raw(
            AdapterSource::Udev,
            "udev",
            json!({"action": "remove", "name": "x", "vendor_id": "1", "product_id": "2", "id": "a"}),
            1100,
        );
        assert_eq!(n.normalize(&connect).len(), 1);
        // Disconnect uses a different verb in the dedup key, so it fires.
        assert_eq!(n.normalize(&disconnect).len(), 1);
    }

    // ─── Hyprland ─────────────────────────────────────────────────────────

    #[test]
    fn hyprland_workspace_change() {
        let n = EventNormalizer::new(0);
        let ev = raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "workspace", "data": "2"}),
            1,
        );
        let out = n.normalize(&ev);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.workspace.changed");
    }

    #[test]
    fn hyprland_active_window_v2_parses_address_from_fields() {
        let n = EventNormalizer::new(0);
        let ev = raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "activewindowv2", "data": "0xdeadbeef"}),
            1,
        );
        let out = n.normalize(&ev);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.window.focused");
        assert_eq!(out[0].data.get("address").unwrap(), "0xdeadbeef");
    }

    #[test]
    fn hyprland_openwindow_splits_all_fields() {
        let n = EventNormalizer::new(0);
        let ev = raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "openwindow", "data": "0xabc>>2>>firefox>>Mozilla Firefox"}),
            1,
        );
        let out = n.normalize(&ev);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.window.opened");
        let d = &out[0].data;
        assert_eq!(d.get("address").unwrap(), "0xabc");
        assert_eq!(d.get("workspace").unwrap(), "2");
        assert_eq!(d.get("class").unwrap(), "firefox");
        assert_eq!(d.get("title").unwrap(), "Mozilla Firefox");
    }

    #[test]
    fn hyprland_unknown_kind_falls_through_to_generic_event() {
        let n = EventNormalizer::new(0);
        let ev = raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "submap", "data": "resize"}),
            1,
        );
        let out = n.normalize(&ev);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.hyprland.event");
    }

    #[test]
    fn hyprland_monitor_lifecycle() {
        let n = EventNormalizer::new(0);
        let added = n.normalize(&raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "monitoradded", "data": "HDMI-A-1"}),
            1,
        ));
        let removed = n.normalize(&raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "monitorremoved", "data": "HDMI-A-1"}),
            2,
        ));
        assert_eq!(added[0].event, "bread.monitor.connected");
        assert_eq!(added[0].data.get("name").unwrap(), "HDMI-A-1");
        assert_eq!(removed[0].event, "bread.monitor.disconnected");
    }

    // ─── Power ─────────────────────────────────────────────────────────────

    #[test]
    fn power_ac_connected_emits_named_event() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Power,
            "power",
            json!({"ac_connected": true}),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.power.ac.connected");
    }

    #[test]
    fn power_battery_thresholds_select_correct_event() {
        let n = EventNormalizer::new(0);
        let cases = [
            (3, "bread.power.battery.critical"),
            (5, "bread.power.battery.critical"),
            (8, "bread.power.battery.very_low"),
            (10, "bread.power.battery.very_low"),
            (15, "bread.power.battery.low"),
            (20, "bread.power.battery.low"),
            (100, "bread.power.battery.full"),
        ];
        for (level, expected) in cases {
            let out = n.normalize(&raw(
                AdapterSource::Power,
                "power",
                json!({"battery_percent": level}),
                level * 1000,
            ));
            assert_eq!(
                out[0].event, expected,
                "level {level} should map to {expected}"
            );
        }
    }

    #[test]
    fn power_mid_range_battery_emits_generic_changed() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Power,
            "power",
            json!({"battery_percent": 50}),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.power.changed");
    }

    #[test]
    fn power_ac_and_battery_can_both_fire() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Power,
            "power",
            json!({"ac_connected": false, "battery_percent": 4}),
            1,
        ));
        let names: Vec<&str> = out.iter().map(|e| e.event.as_str()).collect();
        assert!(names.contains(&"bread.power.ac.disconnected"));
        assert!(names.contains(&"bread.power.battery.critical"));
    }

    // ─── Bluetooth ─────────────────────────────────────────────────────────

    #[test]
    fn bluetooth_connected_emits_device_connected() {
        let n = EventNormalizer::new(0);
        let ev = raw(
            AdapterSource::Bluetooth,
            "bluetooth",
            json!({
                "path": "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "address": "AA:BB:CC:DD:EE:FF",
                "properties": { "Connected": true },
            }),
            1,
        );
        let out = n.normalize(&raw(
            AdapterSource::Bluetooth,
            "bluetooth.device.connected",
            ev.payload.clone(),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.device.connected");
        assert_eq!(out[0].data.get("address").unwrap(), "AA:BB:CC:DD:EE:FF");
        assert_eq!(out[0].data.get("subsystem").unwrap(), "bluetooth");
        assert_eq!(out[0].data.get("device").unwrap(), "unknown");
    }

    #[test]
    fn bluetooth_disconnected_emits_device_disconnected() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Bluetooth,
            "bluetooth.device.disconnected",
            json!({
                "path": "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "address": "AA:BB:CC:DD:EE:FF",
                "properties": { "Connected": false },
            }),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.device.disconnected");
    }

    #[test]
    fn bluetooth_enumerate_includes_name() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Bluetooth,
            "bluetooth.enumerate",
            json!({
                "path": "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "address": "AA:BB:CC:DD:EE:FF",
                "name": "WH-1000XM4",
                "properties": {},
            }),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.device.connected");
        assert_eq!(out[0].data.get("name").unwrap(), "WH-1000XM4");
    }

    #[test]
    fn bluetooth_paired_emits_bluetooth_specific_event() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Bluetooth,
            "bluetooth.device.added",
            json!({
                "path": "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "address": "AA:BB:CC:DD:EE:FF",
                "name": "My Headphones",
                "properties": {},
            }),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.bluetooth.device.paired");
        assert_eq!(out[0].data.get("name").unwrap(), "My Headphones");
    }

    #[test]
    fn bluetooth_unpaired_emits_bluetooth_specific_event() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Bluetooth,
            "bluetooth.device.removed",
            json!({
                "path": "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "address": "AA:BB:CC:DD:EE:FF",
            }),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.bluetooth.device.unpaired");
        assert_eq!(out[0].data.get("address").unwrap(), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn bluetooth_name_falls_back_to_properties() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::Bluetooth,
            "bluetooth.device.connected",
            json!({
                "path": "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
                "address": "AA:BB:CC:DD:EE:FF",
                "properties": { "Connected": true, "Name": "Fallback Name" },
            }),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data.get("name").unwrap(), "Fallback Name");
    }

    // ─── Network ───────────────────────────────────────────────────────────

    #[test]
    fn network_online_and_offline() {
        let n = EventNormalizer::new(0);
        let online = n.normalize(&raw(
            AdapterSource::Network,
            "net",
            json!({"online": true}),
            1,
        ));
        let offline = n.normalize(&raw(
            AdapterSource::Network,
            "net",
            json!({"online": false}),
            2,
        ));
        assert_eq!(online[0].event, "bread.network.connected");
        assert_eq!(offline[0].event, "bread.network.disconnected");
    }

    // ─── System pass-through ───────────────────────────────────────────────

    #[test]
    fn system_events_pass_through_unchanged() {
        let n = EventNormalizer::new(0);
        let out = n.normalize(&raw(
            AdapterSource::System,
            "bread.custom.event",
            json!({"foo": "bar"}),
            1,
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "bread.custom.event");
        assert_eq!(out[0].source, AdapterSource::System);
        assert_eq!(out[0].data.get("foo").unwrap(), "bar");
    }

    // ─── Dedup ─────────────────────────────────────────────────────────────

    #[test]
    fn dedup_drops_duplicate_within_window() {
        let n = EventNormalizer::new(500);
        let ev = raw(AdapterSource::Network, "net", json!({"online": true}), 1000);
        assert_eq!(n.normalize(&ev).len(), 1);

        let dup = raw(AdapterSource::Network, "net", json!({"online": true}), 1200);
        assert_eq!(n.normalize(&dup).len(), 0);
    }

    #[test]
    fn dedup_allows_after_window_elapses() {
        let n = EventNormalizer::new(500);
        let first = raw(AdapterSource::Network, "net", json!({"online": true}), 1000);
        assert_eq!(n.normalize(&first).len(), 1);

        let later = raw(AdapterSource::Network, "net", json!({"online": true}), 2000);
        assert_eq!(n.normalize(&later).len(), 1);
    }

    #[test]
    fn dedup_distinguishes_different_payloads() {
        let n = EventNormalizer::new(10_000);
        let a = raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "workspace", "data": "1"}),
            1000,
        );
        let b = raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "workspace", "data": "2"}),
            1100,
        );
        assert_eq!(n.normalize(&a).len(), 1);
        // Different payloads = different dedup key
        assert_eq!(n.normalize(&b).len(), 1);
    }

    #[test]
    fn dedup_window_of_zero_allows_everything() {
        let n = EventNormalizer::new(0);
        for _ in 0..3 {
            assert_eq!(
                n.normalize(&raw(
                    AdapterSource::Network,
                    "net",
                    json!({"online": true}),
                    1000,
                ))
                .len(),
                1
            );
        }
    }

    // ─── Helper ────────────────────────────────────────────────────────────

    #[test]
    fn split_fields_handles_empty_and_single() {
        assert!(split_hyprland_fields("").is_empty());
        assert_eq!(split_hyprland_fields("only"), vec!["only"]);
        assert_eq!(split_hyprland_fields("a>>b>>c"), vec!["a", "b", "c"]);
    }
}
