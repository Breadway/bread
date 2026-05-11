use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use bread_shared::{AdapterSource, BreadEvent};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing::warn;

use crate::core::subscriptions::{SubscriptionId, SubscriptionTable};
use crate::core::types::{Device, DeviceClass, InterfaceState, ModuleLoadState, RuntimeState};
use crate::lua::LuaMessage;

#[derive(Clone)]
pub struct StateHandle {
    state: Arc<RwLock<RuntimeState>>,
    command_tx: mpsc::UnboundedSender<StateCommand>,
    subscription_count: Arc<AtomicU64>,
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
    SetModuleStatus {
        name: String,
        status: ModuleLoadState,
        last_error: Option<String>,
        builtin: bool,
    },
    SetProfile {
        name: String,
    },
}

impl StateHandle {
    pub fn new(
        state: Arc<RwLock<RuntimeState>>,
        command_tx: mpsc::UnboundedSender<StateCommand>,
        subscription_count: Arc<AtomicU64>,
    ) -> Self {
        Self {
            state,
            command_tx,
            subscription_count,
        }
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

    pub fn register_subscription(&self, id: SubscriptionId, pattern: String, once: bool) -> Result<()> {
        self.command_tx
            .send(StateCommand::RegisterSubscription {
                id,
                pattern,
                once,
            })
            .map_err(|_| anyhow::anyhow!("state engine command channel closed"))
    }

    pub fn remove_subscription(&self, id: SubscriptionId) {
        let _ = self.command_tx.send(StateCommand::RemoveSubscription { id });
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

    pub fn subscription_count(&self) -> Arc<AtomicU64> {
        self.subscription_count.clone()
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
                handle_command(cmd, &state, &mut subscriptions, &mut watches, &subscription_count).await;
            }
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    break;
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
                        resolution: event.data.get("resolution").and_then(Value::as_str).map(ToString::to_string),
                        position: event.data.get("position").and_then(Value::as_str).map(ToString::to_string),
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
                    state.network.interfaces.insert(name.clone(), InterfaceState { up });
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

        let class = data
            .get("class")
            .and_then(|v| serde_json::from_value::<DeviceClass>(v.clone()).ok())
            .unwrap_or(DeviceClass::Unknown);

        state.devices.connected.push(Device {
            id,
            name: data
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            class,
            subsystem: data
                .get("subsystem")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        });
    } else {
        state.devices.connected.retain(|d| d.id != id);
    }
}
