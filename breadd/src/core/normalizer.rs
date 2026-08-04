use std::collections::HashMap;
use std::sync::RwLock;

use bread_shared::{apps::validate_app_namespace, AdapterSource, BreadEvent, RawEvent};
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
    /// Mirrors `[compat] legacy_hyprland_event_names` in `breadd.toml`. When
    /// `true` (the default during the deprecation window), `normalize_hyprland`
    /// dual-emits both its legacy flat event names and their namespaced
    /// `bread.hyprland.*` equivalents. See `with_legacy_hyprland_event_names`.
    legacy_hyprland_event_names: bool,
}

impl EventNormalizer {
    pub fn new(dedup_window_ms: u64) -> Self {
        Self {
            dedup_window_ms,
            recent: RwLock::new(HashMap::new()),
            seen_devices: RwLock::new(HashMap::new()),
            legacy_hyprland_event_names: true,
        }
    }

    /// Overrides whether the Hyprland adapter's legacy flat event names
    /// (`bread.workspace.changed` etc.) keep firing alongside their
    /// `bread.hyprland.*` equivalents. Defaults to `true` via `new`, mirroring
    /// `[compat] legacy_hyprland_event_names`'s documented default during the
    /// deprecation window; `main.rs` overrides this from
    /// `config.compat.legacy_hyprland_event_names`.
    pub fn with_legacy_hyprland_event_names(mut self, enabled: bool) -> Self {
        self.legacy_hyprland_event_names = enabled;
        self
    }

    pub fn normalize(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let mut out = match &raw.source {
            AdapterSource::Udev => self.normalize_udev(raw),
            AdapterSource::Hyprland => self.normalize_hyprland(raw),
            AdapterSource::Power => self.normalize_power(raw),
            AdapterSource::Network => self.normalize_network(raw),
            AdapterSource::Bluetooth => self.normalize_bluetooth(raw),
            AdapterSource::Terminal => self.normalize_terminal(raw),
            AdapterSource::Git => self.normalize_git(raw),
            AdapterSource::Filesystem => self.normalize_filesystem(raw),
            AdapterSource::Systemd => self.normalize_systemd(raw),
            AdapterSource::Podman => self.normalize_podman(raw),
            AdapterSource::Remote => self.normalize_remote(raw),
            AdapterSource::App(_) => self.normalize_app(raw),
            AdapterSource::System => vec![BreadEvent {
                event: raw.kind.clone(),
                timestamp: raw.timestamp,
                source: raw.source.clone(),
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
            "workspace" | "workspacev2" => {
                self.emit_hyprland_dual("bread.workspace.changed", raw.payload.clone(), raw)
            }
            "createworkspace" => {
                self.emit_hyprland_dual("bread.workspace.created", json!({ "workspace": data }), raw)
            }
            "destroyworkspace" => self.emit_hyprland_dual(
                "bread.workspace.destroyed",
                json!({ "workspace": data }),
                raw,
            ),
            "monitoradded" => {
                self.emit_hyprland_dual("bread.monitor.connected", json!({ "name": data }), raw)
            }
            "monitorremoved" => {
                self.emit_hyprland_dual("bread.monitor.disconnected", json!({ "name": data }), raw)
            }
            "activewindow" => {
                self.emit_hyprland_dual("bread.window.focus.changed", raw.payload.clone(), raw)
            }
            "activewindowv2" => {
                let fields = split_hyprland_fields(data);
                self.emit_hyprland_dual(
                    "bread.window.focused",
                    json!({
                        "address": fields.first().unwrap_or(&"")
                    }),
                    raw,
                )
            }
            "openwindow" => {
                let fields = split_hyprland_fields(data);
                self.emit_hyprland_dual(
                    "bread.window.opened",
                    json!({
                        "address": fields.first().unwrap_or(&""),
                        "workspace": fields.get(1).unwrap_or(&""),
                        "class": fields.get(2).unwrap_or(&""),
                        "title": fields.get(3).unwrap_or(&""),
                    }),
                    raw,
                )
            }
            "closewindow" => {
                let fields = split_hyprland_fields(data);
                self.emit_hyprland_dual(
                    "bread.window.closed",
                    json!({ "address": fields.first().unwrap_or(&"") }),
                    raw,
                )
            }
            "movewindow" => {
                let fields = split_hyprland_fields(data);
                self.emit_hyprland_dual(
                    "bread.window.moved",
                    json!({
                        "address": fields.first().unwrap_or(&""),
                        "workspace": fields.get(1).unwrap_or(&""),
                    }),
                    raw,
                )
            }
            _ => vec![BreadEvent {
                event: "bread.hyprland.event".to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: raw.payload.clone(),
            }],
        }
    }

    /// Emits a Hyprland event under its namespaced `bread.hyprland.<rest>`
    /// name — always — plus, when `legacy_hyprland_event_names` is enabled
    /// (the default during the deprecation window), a second `BreadEvent`
    /// under the pre-namespace flat name (e.g. `bread.workspace.changed`).
    /// This is the single mechanism behind Workstream C: the two names carry
    /// identical `data`/`timestamp`/`source`, so a module can migrate to
    /// `bread.hyprland.*` at its own pace without missing events either way.
    fn emit_hyprland_dual(
        &self,
        legacy_event: &str,
        data: Value,
        raw: &RawEvent,
    ) -> Vec<BreadEvent> {
        let rest = legacy_event.strip_prefix("bread.").unwrap_or(legacy_event);
        let namespaced_event = format!("bread.hyprland.{rest}");

        let mut out = Vec::with_capacity(2);
        if self.legacy_hyprland_event_names {
            out.push(BreadEvent {
                event: legacy_event.to_string(),
                timestamp: raw.timestamp,
                source: AdapterSource::Hyprland,
                data: data.clone(),
            });
        }
        out.push(BreadEvent {
            event: namespaced_event,
            timestamp: raw.timestamp,
            source: AdapterSource::Hyprland,
            data,
        });
        out
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
        // The sysfs NetworkAdapter puts `online: bool` directly in the payload.
        // The rtnetlink adapter omits it; derive connectivity from the event kind instead.
        let online = if let Some(v) = raw.payload.get("online").and_then(Value::as_bool) {
            v
        } else {
            match raw.kind.as_str() {
                "link.up" | "address.added" => true,
                "link.down" | "address.removed" => false,
                "route.default.changed" => raw
                    .payload
                    .get("gateway")
                    .map(|v| !v.is_null())
                    .unwrap_or(false),
                _ => return vec![],
            }
        };

        let name = if online {
            "bread.network.connected"
        } else {
            "bread.network.disconnected"
        };

        let mut data = raw.payload.clone();
        if let Some(obj) = data.as_object_mut() {
            obj.insert("online".to_string(), Value::Bool(online));
        }

        vec![BreadEvent {
            event: name.to_string(),
            timestamp: raw.timestamp,
            source: AdapterSource::Network,
            data,
        }]
    }

    // Adapter contracts: each of these adapters emits `RawEvent.kind` already
    // namespaced for its family (e.g. filesystem emits "file.changed",
    // "detected", "build_artifact.created"), so normalization here is just a
    // `bread.<family>.` prefix — except systemd (`unit.*` -> `service.*`) and
    // podman's health-status rename, which need a small rewrite.
    fn normalize_terminal(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        vec![BreadEvent {
            event: format!("bread.terminal.{}", raw.kind),
            timestamp: raw.timestamp,
            source: raw.source.clone(),
            data: raw.payload.clone(),
        }]
    }

    fn normalize_remote(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        vec![BreadEvent {
            event: format!("bread.remote.{}", raw.kind),
            timestamp: raw.timestamp,
            source: raw.source.clone(),
            data: raw.payload.clone(),
        }]
    }

    fn normalize_git(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        vec![BreadEvent {
            event: format!("bread.git.{}", raw.kind),
            timestamp: raw.timestamp,
            source: raw.source.clone(),
            data: raw.payload.clone(),
        }]
    }

    fn normalize_filesystem(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        vec![BreadEvent {
            event: format!("bread.project.{}", raw.kind),
            timestamp: raw.timestamp,
            source: raw.source.clone(),
            data: raw.payload.clone(),
        }]
    }

    fn normalize_systemd(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        // Adapter emits "unit.started"/"unit.stopped"/"unit.failed"; the public
        // namespace is `service.*`, not `unit.*`.
        let suffix = raw.kind.strip_prefix("unit.").unwrap_or(raw.kind.as_str());
        vec![BreadEvent {
            event: format!("bread.service.{suffix}"),
            timestamp: raw.timestamp,
            source: raw.source.clone(),
            data: raw.payload.clone(),
        }]
    }

    fn normalize_podman(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        // Adapter emits "container.started"/"container.stopped"/"container.health_status";
        // the public name for the latter is `container.health.changed`.
        let event = if raw.kind == "container.health_status" {
            "bread.container.health.changed".to_string()
        } else {
            format!("bread.{}", raw.kind)
        };
        vec![BreadEvent {
            event,
            timestamp: raw.timestamp,
            source: raw.source.clone(),
            data: raw.payload.clone(),
        }]
    }

    /// Sibling `bread*` app events. Unlike the other sources, `raw.kind`
    /// already carries the full dotted event name (the IPC boundary builds
    /// it that way before construction), so this is validate-and-wrap, not
    /// a transform. The namespace check is defense in depth — the IPC layer
    /// already validates before constructing the `RawEvent` — so a
    /// malformed event here is dropped silently rather than treated as an
    /// adapter failure.
    fn normalize_app(&self, raw: &RawEvent) -> Vec<BreadEvent> {
        let AdapterSource::App(app) = &raw.source else {
            return vec![];
        };
        if !validate_app_namespace(app, &raw.kind) {
            return vec![];
        }
        vec![BreadEvent {
            event: raw.kind.clone(),
            timestamp: raw.timestamp,
            source: raw.source.clone(),
            data: raw.payload.clone(),
        }]
    }

    fn accept(&self, event: &BreadEvent) -> bool {
        // Terminal commands legitimately repeat (running the same command twice
        // in quick succession); the dedup window exists for noisy hardware
        // signals, not user-initiated terminal activity, so exempt it.
        if matches!(&event.source, AdapterSource::Terminal) {
            return true;
        }

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
    //
    // `EventNormalizer::new` defaults `legacy_hyprland_event_names` to `true`
    // (mirrors `[compat]`'s documented default), so by default every mapped
    // Hyprland kind dual-emits: the legacy flat name plus its namespaced
    // `bread.hyprland.*` sibling. See `emit_hyprland_dual`.

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
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|e| e.event == "bread.workspace.changed"));
        assert!(out
            .iter()
            .any(|e| e.event == "bread.hyprland.workspace.changed"));
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
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|e| e.event == "bread.window.focused"));
        assert!(out
            .iter()
            .any(|e| e.event == "bread.hyprland.window.focused"));
        for ev in &out {
            assert_eq!(ev.data.get("address").unwrap(), "0xdeadbeef");
        }
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
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|e| e.event == "bread.window.opened"));
        assert!(out
            .iter()
            .any(|e| e.event == "bread.hyprland.window.opened"));
        for ev in &out {
            let d = &ev.data;
            assert_eq!(d.get("address").unwrap(), "0xabc");
            assert_eq!(d.get("workspace").unwrap(), "2");
            assert_eq!(d.get("class").unwrap(), "firefox");
            assert_eq!(d.get("title").unwrap(), "Mozilla Firefox");
        }
    }

    #[test]
    fn hyprland_unknown_kind_falls_through_to_generic_event() {
        // The `bread.hyprland.event` fallback is already namespaced, so it's
        // exempt from dual-emit — it never had a legacy flat name to begin
        // with.
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
        assert_eq!(added.len(), 2);
        assert!(added.iter().any(|e| e.event == "bread.monitor.connected"));
        assert!(added
            .iter()
            .any(|e| e.event == "bread.hyprland.monitor.connected"));
        for ev in &added {
            assert_eq!(ev.data.get("name").unwrap(), "HDMI-A-1");
        }

        assert_eq!(removed.len(), 2);
        assert!(removed
            .iter()
            .any(|e| e.event == "bread.monitor.disconnected"));
        assert!(removed
            .iter()
            .any(|e| e.event == "bread.hyprland.monitor.disconnected"));
    }

    /// (a) With the default config (`legacy_hyprland_event_names = true`),
    /// every one of the 10 dual-emit mappings fires both its legacy flat
    /// name and its namespaced `bread.hyprland.*` sibling, with identical
    /// data on each.
    #[test]
    fn hyprland_dual_emit_covers_all_ten_mappings_by_default() {
        let n = EventNormalizer::new(0);
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "workspace",
                "2",
                "bread.workspace.changed",
                "bread.hyprland.workspace.changed",
            ),
            (
                "workspacev2",
                "2,name",
                "bread.workspace.changed",
                "bread.hyprland.workspace.changed",
            ),
            (
                "createworkspace",
                "3",
                "bread.workspace.created",
                "bread.hyprland.workspace.created",
            ),
            (
                "destroyworkspace",
                "3",
                "bread.workspace.destroyed",
                "bread.hyprland.workspace.destroyed",
            ),
            (
                "monitoradded",
                "HDMI-A-1",
                "bread.monitor.connected",
                "bread.hyprland.monitor.connected",
            ),
            (
                "monitorremoved",
                "HDMI-A-1",
                "bread.monitor.disconnected",
                "bread.hyprland.monitor.disconnected",
            ),
            (
                "activewindow",
                "firefox,Mozilla Firefox",
                "bread.window.focus.changed",
                "bread.hyprland.window.focus.changed",
            ),
            (
                "activewindowv2",
                "0xdead",
                "bread.window.focused",
                "bread.hyprland.window.focused",
            ),
            (
                "openwindow",
                "0xabc>>2>>firefox>>Mozilla Firefox",
                "bread.window.opened",
                "bread.hyprland.window.opened",
            ),
            (
                "closewindow",
                "0xabc",
                "bread.window.closed",
                "bread.hyprland.window.closed",
            ),
            (
                "movewindow",
                "0xabc,2",
                "bread.window.moved",
                "bread.hyprland.window.moved",
            ),
        ];

        for (kind, data, legacy_event, namespaced_event) in cases {
            let ev = raw(
                AdapterSource::Hyprland,
                "hypr",
                json!({"kind": kind, "data": data}),
                1,
            );
            let out = n.normalize(&ev);
            assert_eq!(out.len(), 2, "kind {kind} should dual-emit exactly 2 events");
            assert!(
                out.iter().any(|e| &e.event == legacy_event),
                "kind {kind} missing legacy event {legacy_event}"
            );
            assert!(
                out.iter().any(|e| &e.event == namespaced_event),
                "kind {kind} missing namespaced event {namespaced_event}"
            );
            let legacy_data = out.iter().find(|e| &e.event == legacy_event).unwrap().data.clone();
            let namespaced_data = out
                .iter()
                .find(|e| &e.event == namespaced_event)
                .unwrap()
                .data
                .clone();
            assert_eq!(
                legacy_data, namespaced_data,
                "kind {kind}: legacy and namespaced events should carry identical data"
            );
        }
    }

    /// (b) With `legacy_hyprland_event_names = false`, only the namespaced
    /// `bread.hyprland.*` names fire — the legacy flat names are fully
    /// suppressed, not just relegated to a secondary slot.
    #[test]
    fn hyprland_legacy_names_suppressed_when_compat_disabled() {
        let n = EventNormalizer::new(0).with_legacy_hyprland_event_names(false);
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "workspace",
                "2",
                "bread.workspace.changed",
                "bread.hyprland.workspace.changed",
            ),
            (
                "createworkspace",
                "3",
                "bread.workspace.created",
                "bread.hyprland.workspace.created",
            ),
            (
                "destroyworkspace",
                "3",
                "bread.workspace.destroyed",
                "bread.hyprland.workspace.destroyed",
            ),
            (
                "monitoradded",
                "HDMI-A-1",
                "bread.monitor.connected",
                "bread.hyprland.monitor.connected",
            ),
            (
                "monitorremoved",
                "HDMI-A-1",
                "bread.monitor.disconnected",
                "bread.hyprland.monitor.disconnected",
            ),
            (
                "activewindow",
                "firefox,Mozilla Firefox",
                "bread.window.focus.changed",
                "bread.hyprland.window.focus.changed",
            ),
            (
                "activewindowv2",
                "0xdead",
                "bread.window.focused",
                "bread.hyprland.window.focused",
            ),
            (
                "openwindow",
                "0xabc>>2>>firefox>>Mozilla Firefox",
                "bread.window.opened",
                "bread.hyprland.window.opened",
            ),
            (
                "closewindow",
                "0xabc",
                "bread.window.closed",
                "bread.hyprland.window.closed",
            ),
            (
                "movewindow",
                "0xabc,2",
                "bread.window.moved",
                "bread.hyprland.window.moved",
            ),
        ];

        for (kind, data, legacy_event, namespaced_event) in cases {
            let ev = raw(
                AdapterSource::Hyprland,
                "hypr",
                json!({"kind": kind, "data": data}),
                1,
            );
            let out = n.normalize(&ev);
            assert_eq!(
                out.len(),
                1,
                "kind {kind} should emit exactly 1 event when legacy names are disabled"
            );
            assert_eq!(
                out[0].event, *namespaced_event,
                "kind {kind} should emit only the namespaced event"
            );
            assert!(
                !out.iter().any(|e| &e.event == legacy_event),
                "kind {kind} leaked legacy event {legacy_event} despite being disabled"
            );
        }

        // The already-namespaced fallback is unaffected either way.
        let fallback = n.normalize(&raw(
            AdapterSource::Hyprland,
            "hypr",
            json!({"kind": "submap", "data": "resize"}),
            1,
        ));
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].event, "bread.hyprland.event");
    }

    /// (c) A module that subscribes only to `bread.hyprland.*` gets full
    /// coverage of workspace, monitor, and window activity — regardless of
    /// the `[compat]` setting — since the namespaced name is unconditional.
    /// This is the actual promise Workstream C makes: "portable automation"
    /// only means something if the namespaced form alone is a complete feed.
    #[test]
    fn hyprland_namespace_only_subscriber_gets_full_coverage_regardless_of_compat() {
        let kinds_and_data: &[(&str, &str)] = &[
            ("workspace", "2"),
            ("createworkspace", "3"),
            ("destroyworkspace", "3"),
            ("monitoradded", "HDMI-A-1"),
            ("monitorremoved", "HDMI-A-1"),
            ("activewindow", "firefox,Mozilla Firefox"),
            ("activewindowv2", "0xdead"),
            ("openwindow", "0xabc>>2>>firefox>>Mozilla Firefox"),
            ("closewindow", "0xabc"),
            ("movewindow", "0xabc,2"),
        ];

        for legacy_enabled in [true, false] {
            let n = EventNormalizer::new(0).with_legacy_hyprland_event_names(legacy_enabled);
            for (kind, data) in kinds_and_data {
                let out = n.normalize(&raw(
                    AdapterSource::Hyprland,
                    "hypr",
                    json!({"kind": kind, "data": data}),
                    1,
                ));
                let namespaced_hits = out
                    .iter()
                    .filter(|e| e.event.starts_with("bread.hyprland."))
                    .count();
                assert_eq!(
                    namespaced_hits, 1,
                    "kind {kind} (legacy_enabled={legacy_enabled}) should always emit exactly \
                     one bread.hyprland.* event, matching a bread.hyprland.* subscriber's view"
                );
            }
        }
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
        // Hyprland's "workspace" kind dual-emits (legacy + namespaced name,
        // see the Hyprland test section below), so each distinct payload
        // produces 2 events rather than 1 — but the point of this test is
        // that neither call is suppressed by dedup, since the payloads
        // differ and thus so does the dedup key.
        assert_eq!(n.normalize(&a).len(), 2);
        // Different payloads = different dedup key
        assert_eq!(n.normalize(&b).len(), 2);
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
