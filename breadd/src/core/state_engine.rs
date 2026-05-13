use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use bread_shared::{AdapterSource, BreadEvent};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing::warn;

use crate::core::subscriptions::{SubscriptionId, SubscriptionTable};
use crate::core::types::{
    Device, DeviceRule, InterfaceState, MatchCondition, ModuleLoadState, RuntimeState,
};
use crate::lua::LuaMessage;

#[derive(Clone)]
pub struct StateHandle {
    state: Arc<RwLock<RuntimeState>>,
    command_tx: mpsc::UnboundedSender<StateCommand>,
}

pub enum StateCommand {
    RegisterSubscription {
        id: SubscriptionId,
        pattern: String,
        once: bool,
    },
    RemoveSubscription {
        id: SubscriptionId,
    },
    RegisterWatch {
        id: SubscriptionId,
        path: String,
    },
    RemoveWatch {
        id: SubscriptionId,
    },
    ClearSubscriptions,
    ClearModules,
    SetModuleStatus {
        name: String,
        status: ModuleLoadState,
        last_error: Option<String>,
        builtin: bool,
    },
    SetProfile {
        name: String,
    },
    SetDeviceRules(Vec<DeviceRule>),
}

impl StateHandle {
    pub fn new(
        state: Arc<RwLock<RuntimeState>>,
        command_tx: mpsc::UnboundedSender<StateCommand>,
    ) -> Self {
        Self { state, command_tx }
    }

    pub fn state_arc(&self) -> Arc<RwLock<RuntimeState>> {
        self.state.clone()
    }

    pub async fn state_get(&self, path: &str) -> Option<Value> {
        let state = self.state.read().await;
        let full = serde_json::to_value(&*state).ok()?;

        if path.is_empty() {
            return Some(full);
        }

        let mut current = &full;
        for part in path.split('.') {
            current = current.get(part)?;
        }
        Some(current.clone())
    }

    pub async fn state_dump(&self) -> Value {
        let state = self.state.read().await;
        serde_json::to_value(&*state).unwrap_or_else(|_| serde_json::json!({}))
    }

    pub fn register_subscription(
        &self,
        id: SubscriptionId,
        pattern: String,
        once: bool,
    ) -> Result<()> {
        self.command_tx
            .send(StateCommand::RegisterSubscription { id, pattern, once })
            .map_err(|_| anyhow::anyhow!("state engine command channel closed"))
    }

    pub fn remove_subscription(&self, id: SubscriptionId) {
        let _ = self
            .command_tx
            .send(StateCommand::RemoveSubscription { id });
    }

    pub fn register_watch(&self, id: SubscriptionId, path: String) -> Result<()> {
        self.command_tx
            .send(StateCommand::RegisterWatch { id, path })
            .map_err(|_| anyhow::anyhow!("state engine command channel closed"))
    }

    pub fn remove_watch(&self, id: SubscriptionId) {
        let _ = self.command_tx.send(StateCommand::RemoveWatch { id });
    }

    pub fn clear_subscriptions(&self) {
        let _ = self.command_tx.send(StateCommand::ClearSubscriptions);
    }

    pub fn clear_modules(&self) {
        let _ = self.command_tx.send(StateCommand::ClearModules);
    }

    pub fn set_module_status(
        &self,
        name: String,
        status: ModuleLoadState,
        last_error: Option<String>,
        builtin: bool,
    ) {
        let _ = self.command_tx.send(StateCommand::SetModuleStatus {
            name,
            status,
            last_error,
            builtin,
        });
    }

    pub fn set_profile(&self, name: String) {
        let _ = self.command_tx.send(StateCommand::SetProfile { name });
    }

    pub fn set_device_rules(&self, rules: Vec<DeviceRule>) {
        let _ = self.command_tx.send(StateCommand::SetDeviceRules(rules));
    }
}

pub async fn run_state_engine(
    mut event_rx: mpsc::UnboundedReceiver<BreadEvent>,
    mut command_rx: mpsc::UnboundedReceiver<StateCommand>,
    state: Arc<RwLock<RuntimeState>>,
    lua_tx: mpsc::UnboundedSender<LuaMessage>,
    event_stream_tx: broadcast::Sender<BreadEvent>,
    subscription_count: Arc<AtomicU64>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut subscriptions = SubscriptionTable::default();
    let mut watches: HashMap<SubscriptionId, String> = HashMap::new();
    let mut device_rules: Vec<DeviceRule> = Vec::new();

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            maybe_cmd = command_rx.recv() => {
                let Some(cmd) = maybe_cmd else {
                    break;
                };
                if let StateCommand::SetDeviceRules(rules) = cmd {
                    device_rules = rules;
                } else {
                    handle_command(cmd, &state, &mut subscriptions, &mut watches, &subscription_count).await;
                }
            }
            maybe_event = event_rx.recv() => {
                let Some(mut event) = maybe_event else {
                    break;
                };

                // Resolve device name from user rules and patch the event data before
                // any subscriber sees it, then emit the named companion event.
                let device_event = if event.event == "bread.device.connected"
                    || event.event == "bread.device.disconnected"
                {
                    let is_disconnect = event.event == "bread.device.disconnected";
                    let id = event.data.get("id").and_then(Value::as_str).unwrap_or("unknown").to_string();

                    // On disconnect, udev strips vendor/product identifiers from the event.
                    // Look up the device by id in the current state (it's still present
                    // because apply_event_to_state hasn't run yet) and reuse the stored name.
                    let device = if is_disconnect {
                        state.read().await
                            .devices.connected.iter()
                            .find(|d| d.id == id)
                            .map(|d| d.device.clone())
                            .unwrap_or_else(|| resolve_device(&device_rules, &event.data))
                    } else {
                        resolve_device(&device_rules, &event.data)
                    };

                    if let Some(data) = event.data.as_object_mut() {
                        data.insert("device".to_string(), Value::String(device.clone()));
                    }
                    let verb = if is_disconnect { "disconnected" } else { "connected" };
                    Some(BreadEvent::new(
                        format!("bread.device.{}.{}", device, verb),
                        AdapterSource::Udev,
                        json!({ "id": id, "device": device }),
                    ))
                } else {
                    None
                };

                let (before_snapshot, after_snapshot) = if watches.is_empty() {
                    (None, None)
                } else {
                    let mut guard = state.write().await;
                    let before = serde_json::to_value(&*guard).ok();
                    apply_event_to_state(&mut guard, &event);
                    let after = serde_json::to_value(&*guard).ok();
                    (before, after)
                };

                if watches.is_empty() {
                    let mut guard = state.write().await;
                    apply_event_to_state(&mut guard, &event);
                }

                dispatch_event(&event, &mut subscriptions, &lua_tx, &event_stream_tx, &subscription_count);

                if let Some(dev_ev) = device_event {
                    let mut guard = state.write().await;
                    apply_event_to_state(&mut guard, &dev_ev);
                    drop(guard);
                    dispatch_event(&dev_ev, &mut subscriptions, &lua_tx, &event_stream_tx, &subscription_count);
                }

                if let (Some(before), Some(after)) = (before_snapshot, after_snapshot) {
                    for (_id, path) in watches.iter() {
                        let old_val = value_at_path(&before, path).unwrap_or(Value::Null);
                        let new_val = value_at_path(&after, path).unwrap_or(Value::Null);
                        if old_val != new_val {
                            let synthetic = BreadEvent::new(
                                format!("bread.state.changed.{path}"),
                                AdapterSource::System,
                                json!({
                                    "path": path,
                                    "new": new_val,
                                    "old": old_val,
                                }),
                            );
                            dispatch_event(&synthetic, &mut subscriptions, &lua_tx, &event_stream_tx, &subscription_count);
                        }
                    }
                }
            }
        }
    }

    warn!("state engine loop exited");
}

async fn handle_command(
    cmd: StateCommand,
    state: &Arc<RwLock<RuntimeState>>,
    subscriptions: &mut SubscriptionTable,
    watches: &mut HashMap<SubscriptionId, String>,
    subscription_count: &Arc<AtomicU64>,
) {
    match cmd {
        StateCommand::RegisterSubscription { id, pattern, once } => {
            subscriptions.add_with_id(id, pattern, once);
            subscription_count.fetch_add(1, Ordering::Relaxed);
        }
        StateCommand::RemoveSubscription { id } => {
            if subscriptions.remove(id) {
                subscription_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
        StateCommand::RegisterWatch { id, path } => {
            watches.insert(id, path);
        }
        StateCommand::RemoveWatch { id } => {
            watches.remove(&id);
        }
        StateCommand::ClearSubscriptions => {
            subscriptions.clear();
            watches.clear();
            subscription_count.store(0, Ordering::Relaxed);
        }
        StateCommand::ClearModules => {
            state.write().await.modules.clear();
        }
        StateCommand::SetModuleStatus {
            name,
            status,
            last_error,
            builtin,
        } => {
            let mut guard = state.write().await;
            if let Some(existing) = guard.modules.iter_mut().find(|m| m.name == name) {
                existing.status = status;
                existing.last_error = last_error;
                existing.builtin = builtin;
            } else {
                guard.modules.push(crate::core::types::ModuleStatus {
                    name,
                    status,
                    last_error,
                    builtin,
                    store: HashMap::new(),
                });
            }
        }
        StateCommand::SetProfile { name } => {
            let mut guard = state.write().await;
            if guard.profile.active != name {
                let previous = guard.profile.active.clone();
                guard.profile.history.push(previous);
                guard.profile.active = name;
            }
        }
        StateCommand::SetDeviceRules(_) => {
            // Handled directly in run_state_engine before this function is called.
        }
    }
}

fn dispatch_event(
    event: &BreadEvent,
    subscriptions: &mut SubscriptionTable,
    lua_tx: &mpsc::UnboundedSender<LuaMessage>,
    event_stream_tx: &broadcast::Sender<BreadEvent>,
    subscription_count: &Arc<AtomicU64>,
) {
    let _ = event_stream_tx.send(event.clone());

    let matches = subscriptions.match_event(&event.event);
    for sub in &matches {
        let _ = lua_tx.send(LuaMessage::Event {
            subscription_id: sub.id,
            event: event.clone(),
        });
    }

    for sub in matches.into_iter().filter(|s| s.once) {
        if subscriptions.remove(sub.id) {
            subscription_count.fetch_sub(1, Ordering::Relaxed);
        }
        let _ = lua_tx.send(LuaMessage::SubscriptionCancelled { id: sub.id });
    }
}

fn value_at_path(value: &Value, path: &str) -> Option<Value> {
    if path.is_empty() {
        return Some(value.clone());
    }
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current.clone())
}

fn apply_event_to_state(state: &mut RuntimeState, event: &BreadEvent) {
    match event.event.as_str() {
        "bread.monitor.connected" => {
            if let Some(name) = event.data.get("name").and_then(Value::as_str) {
                if let Some(m) = state.monitors.iter_mut().find(|m| m.name == name) {
                    m.connected = true;
                } else {
                    state.monitors.push(crate::core::types::Monitor {
                        name: name.to_string(),
                        connected: true,
                        resolution: event
                            .data
                            .get("resolution")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        position: event
                            .data
                            .get("position")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                    });
                }
            }
        }
        "bread.monitor.disconnected" => {
            if let Some(name) = event.data.get("name").and_then(Value::as_str) {
                if let Some(m) = state.monitors.iter_mut().find(|m| m.name == name) {
                    m.connected = false;
                }
            }
        }
        "bread.workspace.changed" => {
            let ws = event
                .data
                .get("workspace")
                .or_else(|| event.data.get("id"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            state.active_workspace = ws;
        }
        "bread.window.focus.changed" | "bread.window.focused" => {
            state.active_window = event
                .data
                .get("window")
                .or_else(|| event.data.get("class"))
                .or_else(|| event.data.get("address"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }
        "bread.device.connected" => {
            apply_device_change(state, &event.data, true);
        }
        "bread.device.disconnected" => {
            apply_device_change(state, &event.data, false);
        }
        "bread.network.connected" | "bread.network.disconnected" => {
            if let Some(online) = event.data.get("online").and_then(Value::as_bool) {
                state.network.online = online;
            }
            if let Some(ifaces) = event.data.get("interfaces").and_then(Value::as_object) {
                state.network.interfaces.clear();
                for (name, meta) in ifaces {
                    let up = meta.get("up").and_then(Value::as_bool).unwrap_or(false);
                    state
                        .network
                        .interfaces
                        .insert(name.clone(), InterfaceState { up });
                }
            }
        }
        "bread.power.changed"
        | "bread.power.ac.connected"
        | "bread.power.ac.disconnected"
        | "bread.power.battery.low"
        | "bread.power.battery.very_low"
        | "bread.power.battery.critical"
        | "bread.power.battery.full" => {
            if let Some(ac) = event.data.get("ac_connected").and_then(Value::as_bool) {
                state.power.ac_connected = ac;
            }
            if let Some(battery) = event.data.get("battery_percent").and_then(Value::as_u64) {
                state.power.battery_percent = Some(battery.min(100) as u8);
                state.power.battery_low = battery <= 20;
            }
        }
        "bread.profile.activated" => {
            if let Some(name) = event.data.get("name").and_then(Value::as_str) {
                if state.profile.active != name {
                    let previous = state.profile.active.clone();
                    state.profile.history.push(previous);
                    state.profile.active = name.to_string();
                }
            }
        }
        _ => {}
    }
}

fn resolve_device(rules: &[DeviceRule], data: &Value) -> String {
    for rule in rules {
        if !rule.conditions.is_empty() && rule.conditions.iter().all(|c| condition_matches(c, data))
        {
            return rule.device.clone();
        }
    }
    "unknown".to_string()
}

fn condition_matches(cond: &MatchCondition, data: &Value) -> bool {
    if let Some(ref expected) = cond.vendor_id {
        let actual = data.get("vendor_id").and_then(Value::as_str).unwrap_or("");
        if actual.to_lowercase() != expected.to_lowercase() {
            return false;
        }
    }
    if let Some(ref expected) = cond.product_id {
        let actual = data.get("product_id").and_then(Value::as_str).unwrap_or("");
        if actual.to_lowercase() != expected.to_lowercase() {
            return false;
        }
    }
    if let Some(ref expected) = cond.name {
        let actual = data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if actual != expected.to_lowercase() {
            return false;
        }
    }
    if let Some(ref expected) = cond.vendor {
        let actual = data
            .get("vendor")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if actual != expected.to_lowercase() {
            return false;
        }
    }
    if let Some(ref contains) = cond.name_contains {
        let name = data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let vendor = data
            .get("vendor")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let combined = format!("{name} {vendor}");
        if !combined.contains(contains.to_lowercase().as_str()) {
            return false;
        }
    }
    if let Some(expected) = cond.id_input_keyboard {
        if data
            .get("id_input_keyboard")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            != expected
        {
            return false;
        }
    }
    if let Some(expected) = cond.id_input_mouse {
        if data
            .get("id_input_mouse")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            != expected
        {
            return false;
        }
    }
    if let Some(expected) = cond.id_input_tablet {
        if data
            .get("id_input_tablet")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            != expected
        {
            return false;
        }
    }
    if cond.usb_hub == Some(true) {
        let ifaces = data
            .get("id_usb_interfaces")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let has_hub = ifaces.contains(":0900") || ifaces.contains(":0902");
        let has_secondary = ifaces.contains(":0e")
            || ifaces.contains(":0200")
            || ifaces.contains(":0100")
            || ifaces.contains(":0801");
        if !(has_hub && has_secondary) {
            return false;
        }
    }
    if let Some(ref expected) = cond.id_usb_class {
        let actual = data
            .get("id_usb_class")
            .and_then(Value::as_str)
            .unwrap_or("");
        if actual.to_lowercase() != expected.to_lowercase()
            && actual.to_lowercase() != format!("0x{}", expected.to_lowercase())
        {
            return false;
        }
    }
    if let Some(ref expected) = cond.subsystem {
        let actual = data
            .get("subsystem")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if actual != expected.to_lowercase() {
            return false;
        }
    }
    true
}

fn apply_device_change(state: &mut RuntimeState, data: &Value, connected: bool) {
    let id = data
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    if connected {
        if state.devices.connected.iter().any(|d| d.id == id) {
            return;
        }

        let device = data
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        state.devices.connected.push(Device {
            id,
            name: data
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            device,
            subsystem: data
                .get("subsystem")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            vendor_id: data
                .get("vendor_id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            product_id: data
                .get("product_id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        });
    } else {
        state.devices.connected.retain(|d| d.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str, data: Value) -> BreadEvent {
        BreadEvent {
            event: name.to_string(),
            timestamp: 0,
            source: AdapterSource::System,
            data,
        }
    }

    // ─── value_at_path ────────────────────────────────────────────────────

    #[test]
    fn value_at_path_returns_root_for_empty_path() {
        let v = json!({"a": 1});
        assert_eq!(value_at_path(&v, ""), Some(json!({"a": 1})));
    }

    #[test]
    fn value_at_path_navigates_nested_keys() {
        let v = json!({"a": {"b": {"c": 42}}});
        assert_eq!(value_at_path(&v, "a.b.c"), Some(json!(42)));
    }

    #[test]
    fn value_at_path_returns_none_on_missing_key() {
        let v = json!({"a": 1});
        assert!(value_at_path(&v, "missing").is_none());
        assert!(value_at_path(&v, "a.b.c").is_none());
    }

    // ─── apply_event_to_state: monitors ───────────────────────────────────

    #[test]
    fn monitor_connect_adds_new_monitor() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev(
                "bread.monitor.connected",
                json!({"name": "DP-1", "resolution": "1920x1080", "position": "0x0"}),
            ),
        );
        assert_eq!(state.monitors.len(), 1);
        assert_eq!(state.monitors[0].name, "DP-1");
        assert!(state.monitors[0].connected);
        assert_eq!(state.monitors[0].resolution.as_deref(), Some("1920x1080"));
        assert_eq!(state.monitors[0].position.as_deref(), Some("0x0"));
    }

    #[test]
    fn monitor_reconnect_does_not_duplicate() {
        let mut state = RuntimeState::default();
        let mk = || ev("bread.monitor.connected", json!({"name": "DP-1"}));
        apply_event_to_state(&mut state, &mk());
        apply_event_to_state(
            &mut state,
            &ev("bread.monitor.disconnected", json!({"name": "DP-1"})),
        );
        apply_event_to_state(&mut state, &mk());
        assert_eq!(state.monitors.len(), 1);
        assert!(state.monitors[0].connected);
    }

    #[test]
    fn monitor_disconnect_keeps_record_but_flips_connected_flag() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev("bread.monitor.connected", json!({"name": "DP-1"})),
        );
        apply_event_to_state(
            &mut state,
            &ev("bread.monitor.disconnected", json!({"name": "DP-1"})),
        );
        assert_eq!(state.monitors.len(), 1);
        assert!(!state.monitors[0].connected);
    }

    // ─── apply_event_to_state: workspace + window ─────────────────────────

    #[test]
    fn workspace_changed_updates_active_workspace() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev("bread.workspace.changed", json!({"workspace": "3"})),
        );
        assert_eq!(state.active_workspace.as_deref(), Some("3"));
        // Falls back to `id` when `workspace` is absent.
        apply_event_to_state(
            &mut state,
            &ev("bread.workspace.changed", json!({"id": "5"})),
        );
        assert_eq!(state.active_workspace.as_deref(), Some("5"));
    }

    #[test]
    fn window_focus_change_updates_active_window() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev("bread.window.focus.changed", json!({"window": "firefox"})),
        );
        assert_eq!(state.active_window.as_deref(), Some("firefox"));
        // Falls back to `class`, then `address`.
        apply_event_to_state(
            &mut state,
            &ev("bread.window.focused", json!({"address": "0xdeadbeef"})),
        );
        assert_eq!(state.active_window.as_deref(), Some("0xdeadbeef"));
    }

    // ─── apply_device_change ──────────────────────────────────────────────

    #[test]
    fn device_connect_adds_device_with_all_fields() {
        let mut state = RuntimeState::default();
        apply_device_change(
            &mut state,
            &json!({
                "id": "1-1.4",
                "name": "Logitech Mouse",
                "device": "mouse",
                "subsystem": "usb",
                "vendor_id": "046d",
                "product_id": "c52b",
            }),
            true,
        );
        assert_eq!(state.devices.connected.len(), 1);
        let d = &state.devices.connected[0];
        assert_eq!(d.id, "1-1.4");
        assert_eq!(d.name, "Logitech Mouse");
        assert_eq!(d.device, "mouse");
        assert_eq!(d.subsystem, "usb");
        assert_eq!(d.vendor_id.as_deref(), Some("046d"));
        assert_eq!(d.product_id.as_deref(), Some("c52b"));
    }

    #[test]
    fn device_connect_is_idempotent_for_same_id() {
        let mut state = RuntimeState::default();
        let data = json!({"id": "x", "device": "dock", "name": "Dock"});
        apply_device_change(&mut state, &data, true);
        apply_device_change(&mut state, &data, true);
        assert_eq!(state.devices.connected.len(), 1);
    }

    #[test]
    fn device_disconnect_removes_matching_id() {
        let mut state = RuntimeState::default();
        apply_device_change(&mut state, &json!({"id": "a", "device": "x"}), true);
        apply_device_change(&mut state, &json!({"id": "b", "device": "y"}), true);
        assert_eq!(state.devices.connected.len(), 2);

        apply_device_change(&mut state, &json!({"id": "a"}), false);
        assert_eq!(state.devices.connected.len(), 1);
        assert_eq!(state.devices.connected[0].id, "b");
    }

    #[test]
    fn device_disconnect_of_unknown_id_is_noop() {
        let mut state = RuntimeState::default();
        apply_device_change(&mut state, &json!({"id": "a", "device": "x"}), true);
        apply_device_change(&mut state, &json!({"id": "ghost"}), false);
        assert_eq!(state.devices.connected.len(), 1);
    }

    // ─── apply_event_to_state: power ──────────────────────────────────────

    #[test]
    fn power_event_updates_ac_and_battery_low_flag() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev(
                "bread.power.battery.low",
                json!({"ac_connected": false, "battery_percent": 18}),
            ),
        );
        assert!(!state.power.ac_connected);
        assert_eq!(state.power.battery_percent, Some(18));
        assert!(state.power.battery_low);

        // 25% is no longer "low"
        apply_event_to_state(
            &mut state,
            &ev("bread.power.changed", json!({"battery_percent": 25})),
        );
        assert!(!state.power.battery_low);
    }

    #[test]
    fn power_clamps_battery_percent_to_100() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev("bread.power.changed", json!({"battery_percent": 250u64})),
        );
        assert_eq!(state.power.battery_percent, Some(100));
    }

    // ─── apply_event_to_state: network ────────────────────────────────────

    #[test]
    fn network_event_updates_online_flag_and_interfaces() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev(
                "bread.network.connected",
                json!({
                    "online": true,
                    "interfaces": {
                        "wlan0": {"up": true},
                        "eth0": {"up": false},
                    }
                }),
            ),
        );
        assert!(state.network.online);
        assert_eq!(state.network.interfaces.len(), 2);
        assert!(state.network.interfaces["wlan0"].up);
        assert!(!state.network.interfaces["eth0"].up);
    }

    // ─── apply_event_to_state: profile ────────────────────────────────────

    #[test]
    fn profile_activated_pushes_previous_to_history() {
        let mut state = RuntimeState::default();
        // Initial active is "default".
        apply_event_to_state(
            &mut state,
            &ev("bread.profile.activated", json!({"name": "battery"})),
        );
        assert_eq!(state.profile.active, "battery");
        assert_eq!(state.profile.history, vec!["default"]);

        apply_event_to_state(
            &mut state,
            &ev("bread.profile.activated", json!({"name": "ac"})),
        );
        assert_eq!(state.profile.active, "ac");
        assert_eq!(state.profile.history, vec!["default", "battery"]);
    }

    #[test]
    fn profile_activated_to_same_name_is_noop() {
        let mut state = RuntimeState::default();
        apply_event_to_state(
            &mut state,
            &ev("bread.profile.activated", json!({"name": "default"})),
        );
        assert_eq!(state.profile.active, "default");
        assert!(state.profile.history.is_empty());
    }

    #[test]
    fn unknown_event_does_not_mutate_state() {
        let mut state = RuntimeState::default();
        let before = serde_json::to_value(&state).unwrap();
        apply_event_to_state(
            &mut state,
            &ev("bread.unknown.event", json!({"foo": "bar"})),
        );
        let after = serde_json::to_value(&state).unwrap();
        assert_eq!(before, after);
    }

    // ─── condition_matches ────────────────────────────────────────────────

    #[test]
    fn condition_vendor_id_matches_case_insensitively() {
        let cond = MatchCondition {
            vendor_id: Some("046D".to_string()),
            ..Default::default()
        };
        assert!(condition_matches(&cond, &json!({"vendor_id": "046d"})));
        assert!(!condition_matches(&cond, &json!({"vendor_id": "1234"})));
    }

    #[test]
    fn condition_name_contains_searches_name_and_vendor() {
        let cond = MatchCondition {
            name_contains: Some("logi".to_string()),
            ..Default::default()
        };
        assert!(condition_matches(&cond, &json!({"name": "Logitech MX"})));
        assert!(condition_matches(&cond, &json!({"vendor": "Logitech Inc"})));
        assert!(!condition_matches(&cond, &json!({"name": "Apple"})));
    }

    #[test]
    fn condition_input_flags_match_booleans() {
        let cond = MatchCondition {
            id_input_keyboard: Some(true),
            ..Default::default()
        };
        assert!(condition_matches(
            &cond,
            &json!({"id_input_keyboard": true})
        ));
        assert!(!condition_matches(
            &cond,
            &json!({"id_input_keyboard": false})
        ));
        // Missing field defaults to false.
        assert!(!condition_matches(&cond, &json!({})));
    }

    #[test]
    fn condition_usb_hub_requires_hub_and_secondary_class() {
        let cond = MatchCondition {
            usb_hub: Some(true),
            ..Default::default()
        };
        assert!(condition_matches(
            &cond,
            &json!({"id_usb_interfaces": ":0900:0e00:"})
        ));
        // Hub alone is not enough.
        assert!(!condition_matches(
            &cond,
            &json!({"id_usb_interfaces": ":0900:"})
        ));
        // Secondary alone is not enough.
        assert!(!condition_matches(
            &cond,
            &json!({"id_usb_interfaces": ":0e00:"})
        ));
    }

    #[test]
    fn condition_id_usb_class_accepts_with_or_without_0x_prefix() {
        let cond = MatchCondition {
            id_usb_class: Some("0e".to_string()),
            ..Default::default()
        };
        assert!(condition_matches(&cond, &json!({"id_usb_class": "0e"})));
        assert!(condition_matches(&cond, &json!({"id_usb_class": "0x0e"})));
        assert!(!condition_matches(&cond, &json!({"id_usb_class": "ff"})));
    }

    #[test]
    fn condition_empty_matches_anything() {
        let cond = MatchCondition::default();
        assert!(condition_matches(&cond, &json!({})));
        assert!(condition_matches(&cond, &json!({"vendor_id": "anything"})));
    }

    // ─── resolve_device ───────────────────────────────────────────────────

    #[test]
    fn resolve_device_returns_first_matching_rule() {
        let rules = vec![
            DeviceRule {
                device: "mouse".to_string(),
                conditions: vec![MatchCondition {
                    vendor_id: Some("046d".to_string()),
                    ..Default::default()
                }],
            },
            DeviceRule {
                device: "dock".to_string(),
                conditions: vec![MatchCondition {
                    vendor_id: Some("17ef".to_string()),
                    ..Default::default()
                }],
            },
        ];
        assert_eq!(
            resolve_device(&rules, &json!({"vendor_id": "046d"})),
            "mouse"
        );
        assert_eq!(
            resolve_device(&rules, &json!({"vendor_id": "17ef"})),
            "dock"
        );
        assert_eq!(
            resolve_device(&rules, &json!({"vendor_id": "0000"})),
            "unknown"
        );
    }

    #[test]
    fn resolve_device_skips_rules_with_no_conditions() {
        let rules = vec![DeviceRule {
            device: "wildcard".to_string(),
            conditions: vec![],
        }];
        assert_eq!(resolve_device(&rules, &json!({})), "unknown");
    }

    #[test]
    fn resolve_device_with_empty_ruleset_returns_unknown() {
        assert_eq!(resolve_device(&[], &json!({"vendor_id": "x"})), "unknown");
    }
}
