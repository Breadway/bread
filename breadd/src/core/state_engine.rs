use std::sync::Arc;

use anyhow::Result;
use bread_shared::BreadEvent;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing::warn;

use crate::core::subscriptions::{SubscriptionId, SubscriptionTable};
use crate::core::types::{Device, DeviceClass, InterfaceState, ModuleLoadState, RuntimeState};
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
    ClearSubscriptions,
    SetModuleStatus {
        name: String,
        status: ModuleLoadState,
        last_error: Option<String>,
    },
    SetProfile {
        name: String,
    },
}

impl StateHandle {
    pub fn new(state: Arc<RwLock<RuntimeState>>, command_tx: mpsc::UnboundedSender<StateCommand>) -> Self {
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

    pub fn register_subscription(&self, id: SubscriptionId, pattern: String, once: bool) -> Result<()> {
        self.command_tx
            .send(StateCommand::RegisterSubscription {
                id,
                pattern,
                once,
            })
            .map_err(|_| anyhow::anyhow!("state engine command channel closed"))
    }

    pub fn clear_subscriptions(&self) {
        let _ = self.command_tx.send(StateCommand::ClearSubscriptions);
    }

    pub fn set_module_status(&self, name: String, status: ModuleLoadState, last_error: Option<String>) {
        let _ = self.command_tx.send(StateCommand::SetModuleStatus {
            name,
            status,
            last_error,
        });
    }

    pub fn set_profile(&self, name: String) {
        let _ = self.command_tx.send(StateCommand::SetProfile { name });
    }
}

pub async fn run_state_engine(
    mut event_rx: mpsc::UnboundedReceiver<BreadEvent>,
    mut command_rx: mpsc::UnboundedReceiver<StateCommand>,
    state: Arc<RwLock<RuntimeState>>,
    lua_tx: mpsc::UnboundedSender<LuaMessage>,
    event_stream_tx: broadcast::Sender<BreadEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut subscriptions = SubscriptionTable::default();

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
                handle_command(cmd, &state, &mut subscriptions).await;
            }
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };

                apply_event_to_state(&state, &event).await;

                let _ = event_stream_tx.send(event.clone());

                let matches = subscriptions.match_event(&event.event);
                for sub in &matches {
                    let _ = lua_tx.send(LuaMessage::Event {
                        subscription_id: sub.id,
                        event: event.clone(),
                    });
                }

                for sub in matches.into_iter().filter(|s| s.once) {
                    subscriptions.remove(sub.id);
                    let _ = lua_tx.send(LuaMessage::SubscriptionCancelled { id: sub.id });
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
) {
    match cmd {
        StateCommand::RegisterSubscription { id, pattern, once } => {
            subscriptions.add_with_id(id, pattern, once);
        }
        StateCommand::ClearSubscriptions => {
            subscriptions.clear();
        }
        StateCommand::SetModuleStatus {
            name,
            status,
            last_error,
        } => {
            let mut guard = state.write().await;
            if let Some(existing) = guard.modules.iter_mut().find(|m| m.name == name) {
                existing.status = status;
                existing.last_error = last_error;
            } else {
                guard.modules.push(crate::core::types::ModuleStatus {
                    name,
                    status,
                    last_error,
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

async fn apply_event_to_state(state: &Arc<RwLock<RuntimeState>>, event: &BreadEvent) {
    let mut guard = state.write().await;
    match event.event.as_str() {
        "bread.monitor.connected" => {
            if let Some(name) = event.data.get("name").and_then(Value::as_str) {
                if let Some(m) = guard.monitors.iter_mut().find(|m| m.name == name) {
                    m.connected = true;
                } else {
                    guard.monitors.push(crate::core::types::Monitor {
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
                if let Some(m) = guard.monitors.iter_mut().find(|m| m.name == name) {
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
            guard.active_workspace = ws;
        }
        "bread.window.focus.changed" => {
            guard.active_window = event
                .data
                .get("window")
                .or_else(|| event.data.get("class"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }
        "bread.device.connected" => {
            apply_device_change(&mut guard, &event.data, true);
        }
        "bread.device.disconnected" => {
            apply_device_change(&mut guard, &event.data, false);
        }
        "bread.network.connected" | "bread.network.disconnected" => {
            if let Some(online) = event.data.get("online").and_then(Value::as_bool) {
                guard.network.online = online;
            }
            if let Some(ifaces) = event.data.get("interfaces").and_then(Value::as_object) {
                guard.network.interfaces.clear();
                for (name, meta) in ifaces {
                    let up = meta.get("up").and_then(Value::as_bool).unwrap_or(false);
                    guard.network.interfaces.insert(name.clone(), InterfaceState { up });
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
                guard.power.ac_connected = ac;
            }
            if let Some(battery) = event.data.get("battery_percent").and_then(Value::as_u64) {
                guard.power.battery_percent = Some(battery.min(100) as u8);
                guard.power.battery_low = battery <= 20;
            }
        }
        "bread.profile.activated" => {
            if let Some(name) = event.data.get("name").and_then(Value::as_str) {
                if guard.profile.active != name {
                    let previous = guard.profile.active.clone();
                    guard.profile.history.push(previous);
                    guard.profile.active = name.to_string();
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
