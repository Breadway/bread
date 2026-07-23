use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use bread_shared::widget::{WidgetNode, WidgetPlacement, WidgetSpec};
use bread_shared::{AdapterSource, BreadEvent};
use mlua::{Error as LuaError, Function, Lua, LuaSerdeExt, RegistryKey, Table, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::{mpsc, oneshot, watch, RwLock};
use tokio::task;
use tokio::time::{interval_at, sleep, Instant};
use tracing::{error, info, warn};

use crate::core::config::{Config, ModulesConfig, NotificationsConfig};
use crate::core::state_engine::StateHandle;
use crate::core::subscriptions::SubscriptionId;
use crate::core::types::{
    DeviceRule, MatchCondition, ModuleLoadState, RuntimeState, WorkflowState, WorkflowStatus,
};
use bread_shared::now_unix_ms;

pub enum LuaMessage {
    Event {
        subscription_id: SubscriptionId,
        event: BreadEvent,
    },
    SubscriptionCancelled {
        id: SubscriptionId,
    },
    TimerFired {
        id: TimerId,
    },
    Reload {
        reply: oneshot::Sender<std::result::Result<(), String>>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEntry {
    pub timestamp: u64,
    pub module: Option<String>,
    pub message: String,
}

#[derive(Clone)]
pub struct RuntimeHandle {
    tx: mpsc::UnboundedSender<LuaMessage>,
    recent_errors: Arc<Mutex<VecDeque<ErrorEntry>>>,
}

impl RuntimeHandle {
    pub fn sender(&self) -> mpsc::UnboundedSender<LuaMessage> {
        self.tx.clone()
    }

    pub async fn reload(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(LuaMessage::Reload { reply: tx })
            .map_err(|_| anyhow!("lua runtime channel closed"))?;
        match rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(anyhow!(err)),
            Err(_) => Err(anyhow!("lua runtime dropped reload response")),
        }
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(LuaMessage::Shutdown);
    }

    pub fn recent_errors(&self) -> Vec<ErrorEntry> {
        self.recent_errors
            .lock()
            .map(|buf| buf.iter().cloned().collect())
            .unwrap_or_default()
    }
}

pub fn spawn_runtime(
    config: Config,
    state_handle: StateHandle,
    emit_tx: mpsc::UnboundedSender<BreadEvent>,
) -> Result<RuntimeHandle> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let recent_errors = Arc::new(Mutex::new(VecDeque::with_capacity(50)));
    let handle = RuntimeHandle {
        tx,
        recent_errors: recent_errors.clone(),
    };
    let thread_tx = handle.tx.clone();

    std::thread::Builder::new()
        .name("breadd-lua".to_string())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create lua runtime thread");

            rt.block_on(async move {
                let mut engine = match LuaEngine::new(
                    config,
                    state_handle,
                    emit_tx,
                    thread_tx.clone(),
                    recent_errors,
                ) {
                    Ok(engine) => engine,
                    Err(err) => {
                        error!(error = %err, "failed to initialize lua engine");
                        return;
                    }
                };

                if let Err(err) = engine.reload_internal() {
                    error!(error = %err, "initial lua load failed");
                }

                while let Some(msg) = rx.recv().await {
                    match msg {
                        LuaMessage::Event {
                            subscription_id,
                            event,
                        } => {
                            if let Err(err) = engine.handle_event(subscription_id, event) {
                                error!(error = %err, "lua event handler failed");
                            }
                        }
                        LuaMessage::SubscriptionCancelled { id } => {
                            engine.remove_handler(id);
                        }
                        LuaMessage::TimerFired { id } => {
                            if let Err(err) = engine.handle_timer(id) {
                                error!(error = %err, "lua timer handler failed");
                            }
                        }
                        LuaMessage::Reload { reply } => {
                            let result = engine.reload_internal().map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        LuaMessage::Shutdown => {
                            break;
                        }
                    }
                }

                info!("lua runtime thread exiting");
            });
        })?;

    let _ = thread_tx;
    Ok(handle)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(crate) struct TimerId(u64);

struct HandlerEntry {
    callback: RegistryKey,
    filter: Option<RegistryKey>,
    module: Option<String>,
    raw_kind: Option<String>,
    kind: HandlerKind,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum HandlerKind {
    Event,
    StateWatch,
}

struct TimerEntry {
    callback: RegistryKey,
    repeating: bool,
    cancel_tx: watch::Sender<bool>,
    /// The module active when `bread.after`/`bread.every` registered this
    /// timer, so `handle_timer` can restore module context for the callback
    /// (needed by module-scoped APIs like `bread.widget.*`) — same
    /// `current_module` capture used for `bread.on`'s `HandlerEntry.module`.
    module: Option<String>,
}

#[derive(Clone)]
struct ModuleDecl {
    name: String,
    version: Option<String>,
    after: Vec<String>,
    path: PathBuf,
    source: Option<&'static str>,
    builtin: bool,
}

struct ModuleInfo {
    table_key: RegistryKey,
}

struct LuaEngine {
    lua: Lua,
    handlers: Arc<Mutex<HashMap<SubscriptionId, HandlerEntry>>>,
    watch_ids: Arc<Mutex<HashSet<SubscriptionId>>>,
    timers: Arc<Mutex<HashMap<TimerId, TimerEntry>>>,
    next_sub_id: Arc<AtomicU64>,
    next_timer_id: Arc<AtomicU64>,
    current_module: Arc<Mutex<Option<String>>>,
    modules: Arc<Mutex<HashMap<String, ModuleInfo>>>,
    module_decls: Arc<Mutex<HashMap<String, ModuleDecl>>>,
    module_order: Arc<Mutex<Vec<String>>>,
    state_handle: StateHandle,
    emit_tx: mpsc::UnboundedSender<BreadEvent>,
    lua_tx: mpsc::UnboundedSender<LuaMessage>,
    entry_point: PathBuf,
    module_path: PathBuf,
    modules_config: ModulesConfig,
    notifications_config: NotificationsConfig,
    recent_errors: Arc<Mutex<VecDeque<ErrorEntry>>>,
}

impl LuaEngine {
    fn new(
        config: Config,
        state_handle: StateHandle,
        emit_tx: mpsc::UnboundedSender<BreadEvent>,
        lua_tx: mpsc::UnboundedSender<LuaMessage>,
        recent_errors: Arc<Mutex<VecDeque<ErrorEntry>>>,
    ) -> Result<Self> {
        Ok(Self {
            lua: Lua::new(),
            handlers: Arc::new(Mutex::new(HashMap::new())),
            watch_ids: Arc::new(Mutex::new(HashSet::new())),
            timers: Arc::new(Mutex::new(HashMap::new())),
            next_sub_id: Arc::new(AtomicU64::new(1)),
            next_timer_id: Arc::new(AtomicU64::new(1)),
            current_module: Arc::new(Mutex::new(None)),
            modules: Arc::new(Mutex::new(HashMap::new())),
            module_decls: Arc::new(Mutex::new(HashMap::new())),
            module_order: Arc::new(Mutex::new(Vec::new())),
            state_handle,
            emit_tx,
            lua_tx,
            entry_point: config.lua_entry_point(),
            module_path: config.lua_module_path(),
            modules_config: config.modules.clone(),
            notifications_config: config.notifications.clone(),
            recent_errors,
        })
    }

    fn reload_internal(&mut self) -> Result<()> {
        self.run_on_unload();
        self.cancel_all_timers();
        self.state_handle.clear_subscriptions();
        self.state_handle.clear_modules();
        self.state_handle.clear_widgets();
        self.lua = Lua::new();
        self.handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.watch_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.modules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.module_decls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.module_order
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();

        self.install_api()?;
        self.load_device_rules()?;
        self.load_profiles()?;
        self.load_init_and_modules()?;
        self.run_on_reload();

        // clear_widgets() above is silent (no event) since it's just a state
        // wipe ahead of modules re-registering. That's a problem when a
        // module goes from "registered some widgets" to "disabled and
        // skipped" across this reload: nothing re-registers, so no
        // bread.widget.registered fires, and a renderer that only refetches
        // on bread.widget.* events (see breadbar's widgets::client) never
        // learns the registry emptied out. One definitive signal per reload,
        // regardless of whether anything actually changed, closes that gap.
        let _ = self.emit_tx.send(BreadEvent::new(
            "bread.widget.cleared",
            AdapterSource::System,
            serde_json::json!({}),
        ));

        info!("lua runtime reloaded");
        Ok(())
    }

    fn install_api(&self) -> Result<()> {
        let globals = self.lua.globals();
        let bread = self.lua.create_table()?;

        let handlers = self.handlers.clone();
        let next_sub_id = self.next_sub_id.clone();
        let state_handle = self.state_handle.clone();
        let current_module = self.current_module.clone();
        let on_fn =
            self.lua
                .create_function(move |lua, (pattern, callback): (String, Function)| {
                    let id = SubscriptionId(next_sub_id.fetch_add(1, Ordering::Relaxed));
                    let key = lua.create_registry_value(callback)?;
                    let module = current_module
                        .lock()
                        .map_err(|_| LuaError::external("module context lock poisoned"))?
                        .clone();
                    handlers
                        .lock()
                        .map_err(|_| LuaError::external("handler lock poisoned"))?
                        .insert(
                            id,
                            HandlerEntry {
                                callback: key,
                                filter: None,
                                module,
                                raw_kind: None,
                                kind: HandlerKind::Event,
                            },
                        );
                    state_handle
                        .register_subscription(id, pattern, false)
                        .map_err(LuaError::external)?;
                    Ok(id.0)
                })?;
        bread.set("on", on_fn)?;

        let handlers = self.handlers.clone();
        let next_sub_id = self.next_sub_id.clone();
        let state_handle = self.state_handle.clone();
        let current_module = self.current_module.clone();
        let once_fn =
            self.lua
                .create_function(move |lua, (pattern, callback): (String, Function)| {
                    let id = SubscriptionId(next_sub_id.fetch_add(1, Ordering::Relaxed));
                    let key = lua.create_registry_value(callback)?;
                    let module = current_module
                        .lock()
                        .map_err(|_| LuaError::external("module context lock poisoned"))?
                        .clone();
                    handlers
                        .lock()
                        .map_err(|_| LuaError::external("handler lock poisoned"))?
                        .insert(
                            id,
                            HandlerEntry {
                                callback: key,
                                filter: None,
                                module,
                                raw_kind: None,
                                kind: HandlerKind::Event,
                            },
                        );
                    state_handle
                        .register_subscription(id, pattern, true)
                        .map_err(LuaError::external)?;
                    Ok(id.0)
                })?;
        bread.set("once", once_fn)?;

        let handlers = self.handlers.clone();
        let next_sub_id = self.next_sub_id.clone();
        let state_handle = self.state_handle.clone();
        let current_module = self.current_module.clone();
        let filter_fn = self
            .lua
            .create_function(move |lua, (pattern, callback, opts): (String, Function, Option<Table>)| {
                let id = SubscriptionId(next_sub_id.fetch_add(1, Ordering::Relaxed));
                let key = lua.create_registry_value(callback)?;
                let filter = if let Some(opts) = opts {
                    let filter_fn: Function = opts
                        .get("filter")
                        .map_err(|_| LuaError::external("missing filter function"))?;
                    Some(lua.create_registry_value(filter_fn)?)
                } else {
                    return Err(LuaError::external(
                        "bread.filter requires an opts table with a 'filter' function: bread.filter(pattern, fn, { filter = fn })",
                    ));
                };
                let module = current_module
                    .lock()
                    .map_err(|_| LuaError::external("module context lock poisoned"))?
                    .clone();
                handlers
                    .lock()
                    .map_err(|_| LuaError::external("handler lock poisoned"))?
                    .insert(
                        id,
                        HandlerEntry {
                            callback: key,
                            filter,
                            module,
                            raw_kind: None,
                            kind: HandlerKind::Event,
                        },
                    );
                state_handle
                    .register_subscription(id, pattern, false)
                    .map_err(LuaError::external)?;
                Ok(id.0)
            })?;
        bread.set("filter", filter_fn)?;

        let handlers = self.handlers.clone();
        let watch_ids = self.watch_ids.clone();
        let state_handle = self.state_handle.clone();
        let off_fn = self.lua.create_function(move |_lua, id: u64| {
            let sub_id = SubscriptionId(id);
            if let Ok(mut map) = handlers.lock() {
                map.remove(&sub_id);
            }
            state_handle.remove_subscription(sub_id);
            if let Ok(mut set) = watch_ids.lock() {
                if set.remove(&sub_id) {
                    state_handle.remove_watch(sub_id);
                }
            }
            Ok(())
        })?;
        bread.set("off", off_fn)?;

        let emit_tx = self.emit_tx.clone();
        let emit_fn =
            self.lua
                .create_function(move |lua, (event_name, payload): (String, Value)| {
                    let data = match payload {
                        Value::Nil => serde_json::json!({}),
                        other => lua
                            .from_value::<serde_json::Value>(other)
                            .unwrap_or_else(|_| serde_json::json!({})),
                    };
                    emit_tx
                        .send(BreadEvent::new(event_name, AdapterSource::System, data))
                        .map_err(|_| LuaError::external("event channel closed"))?;
                    Ok(())
                })?;
        bread.set("emit", emit_fn)?;

        let state_arc = self.state_handle.state_arc();
        let state_tbl = self.lua.create_table()?;
        let get_fn = self
            .lua
            .create_function(move |lua, path: String| state_value_to_lua(lua, &state_arc, &path))?;
        state_tbl.set("get", get_fn)?;

        let state_arc = self.state_handle.state_arc();
        let monitors_fn = self
            .lua
            .create_function(move |lua, ()| state_value_to_lua(lua, &state_arc, "monitors"))?;
        state_tbl.set("monitors", monitors_fn)?;

        let state_arc = self.state_handle.state_arc();
        let active_ws_fn = self.lua.create_function(move |lua, ()| {
            state_value_to_lua(lua, &state_arc, "active_workspace")
        })?;
        state_tbl.set("active_workspace", active_ws_fn)?;

        let state_arc = self.state_handle.state_arc();
        let active_win_fn = self
            .lua
            .create_function(move |lua, ()| state_value_to_lua(lua, &state_arc, "active_window"))?;
        state_tbl.set("active_window", active_win_fn)?;

        let state_arc = self.state_handle.state_arc();
        let devices_fn = self
            .lua
            .create_function(move |lua, ()| state_value_to_lua(lua, &state_arc, "devices"))?;
        state_tbl.set("devices", devices_fn)?;

        let state_arc = self.state_handle.state_arc();
        let power_fn = self
            .lua
            .create_function(move |lua, ()| state_value_to_lua(lua, &state_arc, "power"))?;
        state_tbl.set("power", power_fn)?;

        let state_arc = self.state_handle.state_arc();
        let network_fn = self
            .lua
            .create_function(move |lua, ()| state_value_to_lua(lua, &state_arc, "network"))?;
        state_tbl.set("network", network_fn)?;

        let state_arc = self.state_handle.state_arc();
        let profile_state_fn = self
            .lua
            .create_function(move |lua, ()| state_value_to_lua(lua, &state_arc, "profile"))?;
        state_tbl.set("profile", profile_state_fn)?;

        let handlers = self.handlers.clone();
        let watch_ids = self.watch_ids.clone();
        let next_sub_id = self.next_sub_id.clone();
        let state_handle = self.state_handle.clone();
        let current_module = self.current_module.clone();
        let watch_fn =
            self.lua
                .create_function(move |lua, (path, callback): (String, Function)| {
                    let id = SubscriptionId(next_sub_id.fetch_add(1, Ordering::Relaxed));
                    let key = lua.create_registry_value(callback)?;
                    let module = current_module
                        .lock()
                        .map_err(|_| LuaError::external("module context lock poisoned"))?
                        .clone();
                    handlers
                        .lock()
                        .map_err(|_| LuaError::external("handler lock poisoned"))?
                        .insert(
                            id,
                            HandlerEntry {
                                callback: key,
                                filter: None,
                                module,
                                raw_kind: None,
                                kind: HandlerKind::StateWatch,
                            },
                        );
                    watch_ids
                        .lock()
                        .map_err(|_| LuaError::external("watch id lock poisoned"))?
                        .insert(id);
                    state_handle
                        .register_watch(id, path.clone())
                        .map_err(LuaError::external)?;
                    state_handle
                        .register_subscription(id, format!("bread.state.changed.{path}"), false)
                        .map_err(LuaError::external)?;
                    Ok(id.0)
                })?;
        state_tbl.set("watch", watch_fn)?;

        bread.set("state", state_tbl)?;

        let profile_tbl = self.lua.create_table()?;
        let state_handle = self.state_handle.clone();
        let emit_tx = self.emit_tx.clone();
        let activate_fn = self.lua.create_function(move |_lua, name: String| {
            state_handle.set_profile(name.clone());
            let _ = emit_tx.send(BreadEvent::new(
                "bread.profile.activated",
                AdapterSource::System,
                serde_json::json!({ "name": name }),
            ));
            Ok(())
        })?;
        profile_tbl.set("activate", activate_fn)?;
        bread.set("profile", profile_tbl)?;

        let exec_fn = self.lua.create_function(move |_lua, cmd: String| {
            task::spawn_blocking(move || {
                match std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .status()
                {
                    Ok(status) => {
                        if !status.success() {
                            tracing::warn!(cmd = %cmd, code = ?status.code(), "bread.exec exited non-zero");
                        }
                    }
                    Err(err) => {
                        tracing::error!(cmd = %cmd, error = %err, "bread.exec failed to spawn");
                    }
                }
            });
            Ok(())
        })?;
        bread.set("exec", exec_fn)?;

        // `bread.exec` is deliberately fire-and-forget (spawn_blocking, no
        // result). This is the capturing counterpart for the common "run a
        // fast local command and read its stdout back into Lua" case (e.g.
        // `git -C <dir> rev-parse --abbrev-ref HEAD`). It blocks the calling
        // Lua callback for real, so it's only appropriate for quick
        // commands — hence the timeout. The subprocess itself runs on a
        // plain std::thread (not spawn_blocking) so the Lua thread can wait
        // on a channel with a deadline; `Command::output()` drains stdout
        // internally as it reads, so a chatty command can't deadlock this by
        // filling a pipe buffer while nobody's reading it. On timeout the
        // spawned thread and its child are left to finish/exit on their own
        // rather than force-killed — acceptable for the fast-command case
        // this exists for, not worth the extra complexity for a rare hang.
        let exec_capture_fn =
            self.lua
                .create_function(|_lua, (cmd, opts): (String, Option<Table>)| {
                    let timeout_ms: u64 = opts
                        .as_ref()
                        .and_then(|o| o.get("timeout_ms").ok())
                        .unwrap_or(2000);

                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let result = std::process::Command::new("sh").arg("-c").arg(&cmd).output();
                        let _ = tx.send(result);
                    });

                    match rx.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
                        Ok(Ok(output)) => Ok((
                            output.status.success(),
                            String::from_utf8_lossy(&output.stdout).to_string(),
                        )),
                        Ok(Err(_)) | Err(_) => Ok((false, String::new())),
                    }
                })?;
        bread.set("exec_capture", exec_capture_fn)?;

        let notify_path = self.notifications_config.notify_send_path.clone();
        let default_urgency = self.notifications_config.default_urgency.clone();
        let default_timeout = self.notifications_config.default_timeout_ms;
        let emit_tx = self.emit_tx.clone();
        let notify_fn =
            self.lua
                .create_function(move |_lua, (message, opts): (String, Option<Table>)| {
                    let title: String = opts
                        .as_ref()
                        .and_then(|o| o.get("title").ok())
                        .unwrap_or_else(|| "bread".to_string());
                    let urgency: String = opts
                        .as_ref()
                        .and_then(|o| o.get("urgency").ok())
                        .unwrap_or_else(|| default_urgency.clone());
                    let timeout: i64 = opts
                        .as_ref()
                        .and_then(|o| o.get("timeout").ok())
                        .unwrap_or(default_timeout);
                    let icon: Option<String> = opts.as_ref().and_then(|o| o.get("icon").ok());

                    let cmd_path = notify_path.clone();
                    let title_clone = title.clone();
                    let message_clone = message.clone();
                    let urgency_clone = urgency.clone();
                    task::spawn_blocking(move || {
                        let mut cmd = std::process::Command::new(cmd_path);
                        cmd.args([
                            "--app-name",
                            "bread",
                            "--urgency",
                            &urgency_clone,
                            "--expire-time",
                            &timeout.to_string(),
                        ]);
                        if let Some(icon) = icon {
                            cmd.args(["--icon", &icon]);
                        }
                        let _ = cmd.args([&title_clone, &message_clone]).status();
                    });

                    let _ = emit_tx.send(BreadEvent::new(
                        "bread.notify.sent",
                        AdapterSource::System,
                        serde_json::json!({
                            "title": title,
                            "message": message,
                            "urgency": urgency,
                        }),
                    ));

                    Ok(())
                })?;
        bread.set("notify", notify_fn)?;

        let timers = self.timers.clone();
        let next_timer_id = self.next_timer_id.clone();
        let lua_tx = self.lua_tx.clone();
        let current_module = self.current_module.clone();
        let after_fn =
            self.lua
                .create_function(move |lua, (delay_ms, callback): (u64, Function)| {
                    let id = TimerId(next_timer_id.fetch_add(1, Ordering::Relaxed));
                    let key = lua.create_registry_value(callback)?;
                    let (cancel_tx, mut cancel_rx) = watch::channel(false);
                    let module = current_module
                        .lock()
                        .map_err(|_| LuaError::external("module context lock poisoned"))?
                        .clone();
                    timers
                        .lock()
                        .map_err(|_| LuaError::external("timer lock poisoned"))?
                        .insert(
                            id,
                            TimerEntry {
                                callback: key,
                                repeating: false,
                                cancel_tx,
                                module,
                            },
                        );
                    let lua_tx = lua_tx.clone();
                    task::spawn(async move {
                        tokio::select! {
                            _ = sleep(Duration::from_millis(delay_ms)) => {
                                if !*cancel_rx.borrow() {
                                    let _ = lua_tx.send(LuaMessage::TimerFired { id });
                                }
                            }
                            _ = cancel_rx.changed() => {}
                        }
                    });
                    Ok(id.0)
                })?;
        bread.set("after", after_fn)?;

        let timers = self.timers.clone();
        let next_timer_id = self.next_timer_id.clone();
        let lua_tx = self.lua_tx.clone();
        let current_module = self.current_module.clone();
        let every_fn =
            self.lua
                .create_function(move |lua, (interval_ms, callback): (u64, Function)| {
                    let id = TimerId(next_timer_id.fetch_add(1, Ordering::Relaxed));
                    let key = lua.create_registry_value(callback)?;
                    let (cancel_tx, mut cancel_rx) = watch::channel(false);
                    let module = current_module
                        .lock()
                        .map_err(|_| LuaError::external("module context lock poisoned"))?
                        .clone();
                    timers
                        .lock()
                        .map_err(|_| LuaError::external("timer lock poisoned"))?
                        .insert(
                            id,
                            TimerEntry {
                                callback: key,
                                repeating: true,
                                cancel_tx,
                                module,
                            },
                        );
                    let lua_tx = lua_tx.clone();
                    task::spawn(async move {
                        let start = Instant::now() + Duration::from_millis(interval_ms);
                        let mut ticker = interval_at(start, Duration::from_millis(interval_ms));
                        loop {
                            tokio::select! {
                                _ = ticker.tick() => {
                                    if *cancel_rx.borrow() {
                                        break;
                                    }
                                    let _ = lua_tx.send(LuaMessage::TimerFired { id });
                                }
                                _ = cancel_rx.changed() => {
                                    if *cancel_rx.borrow() {
                                        break;
                                    }
                                }
                            }
                        }
                    });
                    Ok(id.0)
                })?;
        bread.set("every", every_fn)?;

        let timers = self.timers.clone();
        let cancel_fn = self.lua.create_function(move |_lua, id: u64| {
            let timer_id = TimerId(id);
            if let Ok(mut map) = timers.lock() {
                if let Some(entry) = map.remove(&timer_id) {
                    let _ = entry.cancel_tx.send(true);
                }
            }
            Ok(())
        })?;
        bread.set("cancel", cancel_fn)?;

        let hyprland_tbl = self.lua.create_table()?;
        let dispatch_fn =
            self.lua
                .create_function(move |_lua, (cmd, args): (String, String)| {
                    let resp = hyprland_request(&format!("dispatch {cmd} {args}"))
                        .map_err(|e| LuaError::external(e.to_string()))?;
                    Ok(resp)
                })?;
        hyprland_tbl.set("dispatch", dispatch_fn)?;

        let keyword_fn =
            self.lua
                .create_function(move |_lua, (key, value): (String, String)| {
                    let resp = hyprland_request(&format!("keyword {key} {value}"))
                        .map_err(|e| LuaError::external(e.to_string()))?;
                    Ok(resp)
                })?;
        hyprland_tbl.set("keyword", keyword_fn)?;

        let eval_fn = self.lua.create_function(move |_lua, expr: String| {
            let resp = hyprland_request(&format!("eval {expr}"))
                .map_err(|e| LuaError::external(e.to_string()))?;
            Ok(resp)
        })?;
        hyprland_tbl.set("eval", eval_fn)?;

        let active_window_fn = self.lua.create_function(move |lua, ()| {
            let resp = hyprland_request("j/activewindow")
                .map_err(|e| LuaError::external(e.to_string()))?;
            let json: JsonValue =
                serde_json::from_str(&resp).map_err(|e| LuaError::external(e.to_string()))?;
            json_to_lua(lua, &json)
                .map_err(|e| LuaError::external(e.to_string()))
        })?;
        hyprland_tbl.set("active_window", active_window_fn)?;

        let monitors_fn = self.lua.create_function(move |lua, ()| {
            let resp =
                hyprland_request("j/monitors").map_err(|e| LuaError::external(e.to_string()))?;
            let json: JsonValue =
                serde_json::from_str(&resp).map_err(|e| LuaError::external(e.to_string()))?;
            json_to_lua(lua, &json)
                .map_err(|e| LuaError::external(e.to_string()))
        })?;
        hyprland_tbl.set("monitors", monitors_fn)?;

        let workspaces_fn = self.lua.create_function(move |lua, ()| {
            let resp =
                hyprland_request("j/workspaces").map_err(|e| LuaError::external(e.to_string()))?;
            let json: JsonValue =
                serde_json::from_str(&resp).map_err(|e| LuaError::external(e.to_string()))?;
            json_to_lua(lua, &json)
                .map_err(|e| LuaError::external(e.to_string()))
        })?;
        hyprland_tbl.set("workspaces", workspaces_fn)?;

        let clients_fn = self.lua.create_function(move |lua, ()| {
            let resp =
                hyprland_request("j/clients").map_err(|e| LuaError::external(e.to_string()))?;
            let json: JsonValue =
                serde_json::from_str(&resp).map_err(|e| LuaError::external(e.to_string()))?;
            json_to_lua(lua, &json)
                .map_err(|e| LuaError::external(e.to_string()))
        })?;
        hyprland_tbl.set("clients", clients_fn)?;

        let handlers = self.handlers.clone();
        let next_sub_id = self.next_sub_id.clone();
        let state_handle = self.state_handle.clone();
        let current_module = self.current_module.clone();
        let on_raw_fn =
            self.lua
                .create_function(move |lua, (event, callback): (String, Function)| {
                    let id = SubscriptionId(next_sub_id.fetch_add(1, Ordering::Relaxed));
                    let key = lua.create_registry_value(callback)?;
                    let module = current_module
                        .lock()
                        .map_err(|_| LuaError::external("module context lock poisoned"))?
                        .clone();
                    handlers
                        .lock()
                        .map_err(|_| LuaError::external("handler lock poisoned"))?
                        .insert(
                            id,
                            HandlerEntry {
                                callback: key,
                                filter: None,
                                module,
                                raw_kind: Some(event),
                                kind: HandlerKind::Event,
                            },
                        );
                    state_handle
                        .register_subscription(id, "bread.hyprland.event".to_string(), false)
                        .map_err(LuaError::external)?;
                    Ok(id.0)
                })?;
        hyprland_tbl.set("on_raw", on_raw_fn)?;
        bread.set("hyprland", hyprland_tbl)?;

        let modules = self.modules.clone();
        let module_decls = self.module_decls.clone();
        let current_module = self.current_module.clone();
        let state_arc = self.state_handle.state_arc();
        let module_fn = self.lua.create_function(move |lua, decl: Table| {
            let name: String = decl.get("name")?;
            let expected = current_module
                .lock()
                .map_err(|_| LuaError::external("module context lock poisoned"))?
                .clone();
            if expected.as_deref() != Some(&name) {
                return Err(LuaError::external(
                    "module name does not match current load",
                ));
            }

            let decl = module_decls
                .lock()
                .map_err(|_| LuaError::external("module decls lock poisoned"))?
                .get(&name)
                .cloned()
                .ok_or_else(|| LuaError::external("module declaration not found"))?;

            let module_tbl = lua.create_table()?;
            module_tbl.set("name", decl.name.clone())?;
            if let Some(version) = decl.version.clone() {
                module_tbl.set("version", version)?;
            }

            let store_tbl = lua.create_table()?;
            let module_name = decl.name.clone();
            let state_arc_get = state_arc.clone();
            let get_fn = lua.create_function(move |lua, key: String| {
                if let Some(value) = module_store_get(&state_arc_get, &module_name, &key) {
                    return json_to_lua(lua, &value).map_err(|e| LuaError::external(e.to_string()));
                }
                Ok(Value::Nil)
            })?;
            store_tbl.set("get", get_fn)?;

            let module_name = decl.name.clone();
            let state_arc_set = state_arc.clone();
            let set_fn = lua.create_function(move |lua, (key, value): (String, Value)| {
                let json = lua
                    .from_value::<JsonValue>(value)
                    .unwrap_or(JsonValue::Null);
                module_store_set(&state_arc_set, &module_name, key, json);
                Ok(())
            })?;
            store_tbl.set("set", set_fn)?;
            module_tbl.set("store", store_tbl)?;

            let key = lua.create_registry_value(module_tbl.clone())?;
            modules
                .lock()
                .map_err(|_| LuaError::external("module registry lock poisoned"))?
                .insert(decl.name.clone(), ModuleInfo { table_key: key });

            // Register in package.loaded so require("bread.devices") etc. works
            let package: Table = lua.globals().get("package")?;
            let loaded: Table = package.get("loaded")?;
            loaded.set(decl.name.clone(), module_tbl.clone())?;

            Ok(module_tbl)
        })?;
        bread.set("module", module_fn)?;

        // bread.widget — declarative, live-updating widgets rendered by
        // sibling bread* apps (breadbar) in their bar/popover free space.
        // Follows the same Rust-backed-registry pattern as bread.workflow
        // (see install_workflow_helpers / workflow_register below):
        // mutate RuntimeState.widgets via the try_write spin-lock, then
        // emit a bread.widget.* event so subscribers (and, indirectly,
        // breadbar's events.subscribe stream) observe the change.
        let widget_tbl = self.lua.create_table()?;

        let state_arc = self.state_handle.state_arc();
        let current_module = self.current_module.clone();
        let emit_tx = self.emit_tx.clone();
        let widget_register_fn = self.lua.create_function(
            move |lua, spec_table: Table| -> mlua::Result<(bool, Option<String>)> {
                let module = current_module
                    .lock()
                    .map_err(|_| LuaError::external("module context lock poisoned"))?
                    .clone()
                    .ok_or_else(|| {
                        LuaError::external("bread.widget.register must be called from within a module")
                    })?;
                let args: WidgetRegisterArgs = match lua.from_value(Value::Table(spec_table)) {
                    Ok(a) => a,
                    Err(e) => return Ok((false, Some(e.to_string()))),
                };
                Ok(match widget_register(&state_arc, &module, args) {
                    Ok(spec) => {
                        let data = serde_json::to_value(&spec).unwrap_or_default();
                        let _ = emit_tx.send(BreadEvent::new(
                            "bread.widget.registered",
                            AdapterSource::System,
                            data,
                        ));
                        (true, None)
                    }
                    Err(e) => (false, Some(e.to_string())),
                })
            },
        )?;
        widget_tbl.set("register", widget_register_fn)?;

        let state_arc = self.state_handle.state_arc();
        let current_module = self.current_module.clone();
        let emit_tx = self.emit_tx.clone();
        let widget_update_fn = self.lua.create_function(
            move |lua, (local_id, patch): (String, Table)| -> mlua::Result<(bool, Option<String>)> {
                let module = current_module
                    .lock()
                    .map_err(|_| LuaError::external("module context lock poisoned"))?
                    .clone()
                    .ok_or_else(|| {
                        LuaError::external("bread.widget.update must be called from within a module")
                    })?;
                let args: WidgetUpdateArgs = match lua.from_value(Value::Table(patch)) {
                    Ok(a) => a,
                    Err(e) => return Ok((false, Some(e.to_string()))),
                };
                Ok(match widget_update(&state_arc, &module, &local_id, args) {
                    Ok(Some(spec)) => {
                        let data = serde_json::to_value(&spec).unwrap_or_default();
                        let _ = emit_tx.send(BreadEvent::new(
                            "bread.widget.updated",
                            AdapterSource::System,
                            data,
                        ));
                        (true, None)
                    }
                    Ok(None) => (false, Some("no such widget".to_string())),
                    Err(e) => (false, Some(e.to_string())),
                })
            },
        )?;
        widget_tbl.set("update", widget_update_fn)?;

        let state_arc = self.state_handle.state_arc();
        let current_module = self.current_module.clone();
        let emit_tx = self.emit_tx.clone();
        let widget_remove_fn = self.lua.create_function(move |_lua, local_id: String| {
            let module = current_module
                .lock()
                .map_err(|_| LuaError::external("module context lock poisoned"))?
                .clone()
                .ok_or_else(|| {
                    LuaError::external("bread.widget.remove must be called from within a module")
                })?;
            let full_id = format!("{module}.{local_id}");
            let removed = widget_remove(&state_arc, &module, &local_id);
            if removed {
                let _ = emit_tx.send(BreadEvent::new(
                    "bread.widget.removed",
                    AdapterSource::System,
                    serde_json::json!({ "id": full_id }),
                ));
            }
            Ok(removed)
        })?;
        widget_tbl.set("remove", widget_remove_fn)?;

        let state_arc = self.state_handle.state_arc();
        let current_module = self.current_module.clone();
        let widget_list_fn = self.lua.create_function(move |lua, ()| {
            let module = current_module
                .lock()
                .map_err(|_| LuaError::external("module context lock poisoned"))?
                .clone()
                .ok_or_else(|| {
                    LuaError::external("bread.widget.list must be called from within a module")
                })?;
            let json = widget_list_json(&state_arc, &module);
            json_to_lua(lua, &json)
                .map_err(|e| LuaError::external(e.to_string()))
        })?;
        widget_tbl.set("list", widget_list_fn)?;

        bread.set("widget", widget_tbl)?;

        // bread.machine — hostname/tags; reads an optional, externally-managed
        // ~/.config/bread/sync.toml if present (bread does not create it)
        let machine_tbl = self.lua.create_table()?;

        let name_fn = self
            .lua
            .create_function(|_lua, ()| Ok(lua_machine_name()))?;
        machine_tbl.set("name", name_fn)?;

        let tags_fn = self.lua.create_function(|lua, ()| {
            let tags = lua_machine_tags();
            let tbl = lua.create_table()?;
            for (i, tag) in tags.iter().enumerate() {
                tbl.set(i + 1, tag.clone())?;
            }
            Ok(tbl)
        })?;
        machine_tbl.set("tags", tags_fn)?;

        let has_tag_fn = self
            .lua
            .create_function(|_lua, tag: String| Ok(lua_machine_tags().contains(&tag)))?;
        machine_tbl.set("has_tag", has_tag_fn)?;

        bread.set("machine", machine_tbl)?;

        // bread.fs — file system helpers
        let fs_tbl = self.lua.create_table()?;

        let write_fn = self
            .lua
            .create_function(|_lua, (path, content): (String, String)| {
                let expanded = lua_expand_path(&path);
                if let Some(parent) = expanded.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| LuaError::external(e.to_string()))?;
                }
                std::fs::write(&expanded, content).map_err(|e| LuaError::external(e.to_string()))
            })?;
        fs_tbl.set("write", write_fn)?;

        let read_fn = self.lua.create_function(|_lua, path: String| {
            let expanded = lua_expand_path(&path);
            match std::fs::read_to_string(&expanded) {
                Ok(s) => Ok(Some(s)),
                Err(_) => Ok(None),
            }
        })?;
        fs_tbl.set("read", read_fn)?;

        let exists_fn = self
            .lua
            .create_function(|_lua, path: String| Ok(lua_expand_path(&path).exists()))?;
        fs_tbl.set("exists", exists_fn)?;

        // Distinct from `read`: `/proc/<pid>/cwd` and friends are symlinks
        // whose *target path* is the payload, not a file to open and read —
        // `std::fs::read_to_string` on one of those fails with "Is a
        // directory" (or reads the wrong thing for a symlink-to-file).
        let readlink_fn = self.lua.create_function(|_lua, path: String| {
            let expanded = lua_expand_path(&path);
            match std::fs::read_link(&expanded) {
                Ok(target) => Ok(Some(target.to_string_lossy().to_string())),
                Err(_) => Ok(None),
            }
        })?;
        fs_tbl.set("readlink", readlink_fn)?;

        let expand_fn = self.lua.create_function(|_lua, path: String| {
            Ok(lua_expand_path(&path).to_string_lossy().to_string())
        })?;
        fs_tbl.set("expand", expand_fn)?;

        bread.set("fs", fs_tbl)?;

        // bread.json — for parsing output from things like `kitty @ ls` or
        // any other JSON-emitting CLI invoked via bread.exec_capture. Uses
        // the same null-handling as every other JSON entry point into Lua
        // (json_to_lua, not a bare to_value) so `nil` behaves as Lua nil,
        // not a sentinel.
        let json_tbl = self.lua.create_table()?;
        let decode_fn = self.lua.create_function(|lua, s: String| {
            match serde_json::from_str::<JsonValue>(&s) {
                Ok(v) => json_to_lua(lua, &v).map(Some).or(Ok(None)),
                Err(_) => Ok(None),
            }
        })?;
        json_tbl.set("decode", decode_fn)?;
        bread.set("json", json_tbl)?;

        // bread.bluetooth — BlueZ control
        let bluetooth_tbl = self.lua.create_table()?;

        let power_fn = self.lua.create_function(move |_lua, enabled: bool| {
            bluetooth_spawn(move || async move {
                if let Err(e) = bluetooth_set_powered(enabled).await {
                    tracing::warn!("bread.bluetooth.power failed: {e}");
                }
            });
            Ok(())
        })?;
        bluetooth_tbl.set("power", power_fn)?;

        let powered_fn = self
            .lua
            .create_function(move |_lua, ()| Ok(bluetooth_query(bluetooth_get_powered).ok()))?;
        bluetooth_tbl.set("powered", powered_fn)?;

        let connect_fn = self.lua.create_function(move |_lua, address: String| {
            bluetooth_spawn(move || async move {
                if let Err(e) = bluetooth_connect(address).await {
                    tracing::warn!("bread.bluetooth.connect failed: {e}");
                }
            });
            Ok(())
        })?;
        bluetooth_tbl.set("connect", connect_fn)?;

        let disconnect_fn = self.lua.create_function(move |_lua, address: String| {
            bluetooth_spawn(move || async move {
                if let Err(e) = bluetooth_disconnect(address).await {
                    tracing::warn!("bread.bluetooth.disconnect failed: {e}");
                }
            });
            Ok(())
        })?;
        bluetooth_tbl.set("disconnect", disconnect_fn)?;

        let scan_fn = self.lua.create_function(move |_lua, enabled: bool| {
            bluetooth_spawn(move || async move {
                if let Err(e) = bluetooth_set_scanning(enabled).await {
                    tracing::warn!("bread.bluetooth.scan failed: {e}");
                }
            });
            Ok(())
        })?;
        bluetooth_tbl.set("scan", scan_fn)?;

        let devices_fn = self.lua.create_function(move |lua, ()| {
            let devs = match bluetooth_query(bluetooth_list_devices) {
                Ok(d) => d,
                Err(_) => return Ok(Value::Nil),
            };
            let tbl = lua.create_table()?;
            for (i, dev) in devs.iter().enumerate() {
                let dt = lua.create_table()?;
                dt.set("address", dev.address.clone())?;
                dt.set("name", dev.name.clone())?;
                dt.set("connected", dev.connected)?;
                dt.set("paired", dev.paired)?;
                tbl.set(i + 1, dt)?;
            }
            Ok(Value::Table(tbl))
        })?;
        bluetooth_tbl.set("devices", devices_fn)?;

        bread.set("bluetooth", bluetooth_tbl)?;

        globals.set("bread", bread)?;
        self.install_require_loader()?;
        self.install_wait_helper()?;
        self.install_workflow_helpers()?;
        self.install_log_helpers()?;
        self.install_debounce()?;
        Ok(())
    }

    fn load_device_rules(&self) -> Result<()> {
        let devices_path = self
            .entry_point
            .parent()
            .map(|p| p.join("devices.lua"))
            .unwrap_or_else(|| std::path::PathBuf::from("devices.lua"));

        if !devices_path.exists() {
            return Ok(());
        }

        let source = fs::read_to_string(&devices_path)
            .map_err(|e| anyhow!("failed to read devices.lua: {e}"))?;

        let rules_value: mlua::Value = self
            .lua
            .load(&source)
            .set_name("devices.lua")
            .eval()
            .map_err(|e| anyhow!("devices.lua error: {e}"))?;

        let mlua::Value::Table(tbl) = rules_value else {
            return Err(anyhow!("devices.lua must return a table of rules"));
        };

        let mut rules: Vec<DeviceRule> = Vec::new();
        for pair in tbl.sequence_values::<mlua::Table>() {
            let entry = pair.map_err(|e| anyhow!("devices.lua rule error: {e}"))?;
            let device: String = entry.get("device").unwrap_or_default();
            if device.is_empty() {
                continue;
            }

            // If the rule has a `match` key, each entry in it is a separate condition (OR logic).
            // Otherwise the rule table itself is the single condition.
            let conditions: Vec<MatchCondition> =
                if let Ok(mlua::Value::Table(match_tbl)) = entry.get::<_, mlua::Value>("match") {
                    match_tbl
                        .sequence_values::<mlua::Table>()
                        .filter_map(|r| r.ok())
                        .map(|t| parse_match_condition(&t))
                        .collect()
                } else {
                    vec![parse_match_condition(&entry)]
                };

            if !conditions.is_empty() {
                rules.push(DeviceRule { device, conditions });
            }
        }

        self.state_handle.set_device_rules(rules);
        Ok(())
    }

    fn load_profiles(&self) -> Result<()> {
        let profiles_path = self
            .entry_point
            .parent()
            .map(|p| p.join("profiles.lua"))
            .unwrap_or_else(|| PathBuf::from("profiles.lua"));

        if !profiles_path.exists() {
            return Ok(());
        }

        let path_str = profiles_path.to_string_lossy().to_string();
        self.lua.globals().set("__profiles_path", path_str)?;
        self.lua
            .load(
                r#"
                local ok, result = pcall(loadfile, __profiles_path)
                __profiles_path = nil
                if ok and type(result) == "function" then
                    ok, result = pcall(result)
                end
                if ok and type(result) == "table" then
                    bread.on("bread.profile.activated", function(event)
                        local name = event.data and event.data.name
                        local fn = name and result[name]
                        if type(fn) == "function" then
                            fn(event)
                        end
                    end)
                end
                "#,
            )
            .set_name("profiles.lua")
            .exec()
            .map_err(|e| anyhow!("profiles.lua error: {e}"))
    }

    fn load_init_and_modules(&self) -> Result<()> {
        self.load_lua_file(&self.entry_point, "init", false)?;

        let mut files = list_lua_files(&self.module_path)?;
        files.sort();

        let disabled: HashSet<String> = self.modules_config.disable.iter().cloned().collect();

        let mut decls = Vec::new();
        if self.modules_config.builtin {
            decls.extend(builtin_module_decls(&disabled));
        }
        for path in files
            .into_iter()
            .filter(|p| !is_lib_path(&self.module_path, p))
        {
            let name = module_name_from_path(&self.module_path, &path);
            // bos-settings' module picker writes filenames (e.g.
            // "widget.lua") into `disable`, not the bare module name a file
            // registers under via bread.module({name=...}) — match either
            // form so both conventions work rather than requiring the UI
            // and the daemon to agree on one exact string.
            let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            if disabled.contains(&name) || disabled.contains(filename) {
                self.state_handle
                    .set_module_status(name, ModuleLoadState::Disabled, None, false);
                continue;
            }

            match self.scan_module_decl(&path) {
                Ok(decl) => decls.push(decl),
                Err(err) => {
                    self.state_handle.set_module_status(
                        name,
                        ModuleLoadState::LoadError,
                        Some(err.to_string()),
                        false,
                    );
                }
            }
        }

        let (ordered, dep_errors) = order_module_decls(decls);

        let mut decl_map = self.module_decls.lock().unwrap_or_else(|e| e.into_inner());
        decl_map.clear();
        for decl in &ordered {
            decl_map.insert(decl.name.clone(), decl.clone());
        }
        drop(decl_map);

        for (name, err) in dep_errors {
            self.state_handle
                .set_module_status(name, ModuleLoadState::LoadError, Some(err), false);
        }

        let mut load_order = Vec::new();
        for decl in ordered {
            load_order.push(decl.name.clone());
            match self.load_module(&decl) {
                Ok(()) => {
                    self.state_handle.set_module_status(
                        decl.name.clone(),
                        ModuleLoadState::Loaded,
                        None,
                        decl.builtin,
                    );
                }
                Err(err) => {
                    self.state_handle.set_module_status(
                        decl.name.clone(),
                        ModuleLoadState::LoadError,
                        Some(err.to_string()),
                        decl.builtin,
                    );
                }
            }
        }

        *self.module_order.lock().unwrap_or_else(|e| e.into_inner()) = load_order;

        Ok(())
    }

    fn load_module(&self, decl: &ModuleDecl) -> Result<()> {
        self.set_current_module(Some(decl.name.clone()));
        let result = if let Some(source) = decl.source {
            self.load_lua_source(source, &decl.name)
        } else {
            self.load_lua_file(&decl.path, &decl.name, decl.builtin)
        };
        self.set_current_module(None);
        result?;

        if !self.module_is_registered(&decl.name) {
            return Err(anyhow!("module did not call bread.module"));
        }

        self.run_on_load(&decl.name)
    }

    fn load_lua_file(&self, path: &Path, module_name: &str, builtin: bool) -> Result<()> {
        if !path.exists() {
            warn!(path = %path.display(), "lua file does not exist; skipping");
            self.state_handle.set_module_status(
                module_name.to_string(),
                ModuleLoadState::NotFound,
                None,
                builtin,
            );
            return Ok(());
        }

        let src = fs::read_to_string(path)?;
        self.lua
            .load(&src)
            .set_name(path.to_string_lossy().as_ref())
            .exec()?;
        Ok(())
    }

    fn load_lua_source(&self, source: &str, module_name: &str) -> Result<()> {
        self.lua
            .load(source)
            .set_name(module_name)
            .exec()
            .map_err(|e| anyhow!(e.to_string()))
    }

    fn handle_event(&self, id: SubscriptionId, event: BreadEvent) -> Result<()> {
        let (callback, filter, raw_kind, kind, module) = {
            let handlers = self.handlers.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = handlers.get(&id) else {
                return Ok(());
            };
            let callback: Function = self.lua.registry_value(&entry.callback)?;
            let filter = match entry.filter.as_ref() {
                Some(key) => Some(self.lua.registry_value::<Function>(key)?),
                None => None,
            };
            (
                callback,
                filter,
                entry.raw_kind.clone(),
                entry.kind,
                entry.module.clone(),
            )
        };

        if let Some(kind) = raw_kind.as_deref() {
            let matches = event
                .data
                .get("kind")
                .and_then(JsonValue::as_str)
                .map(|k| k == kind)
                .unwrap_or(false);
            if !matches {
                return Ok(());
            }
        }

        if let Some(filter) = filter {
            let event_value = json_to_lua(&self.lua, &event)?;
            let allowed = filter.call::<_, bool>(event_value).unwrap_or(false);
            if !allowed {
                return Ok(());
            }
        }

        self.set_current_module(module.clone());
        let result = match kind {
            HandlerKind::Event => {
                let event_value = json_to_lua(&self.lua, &event)?;
                callback.call::<_, ()>(event_value)
            }
            HandlerKind::StateWatch => {
                let new_val = event.data.get("new").cloned().unwrap_or(JsonValue::Null);
                let old_val = event.data.get("old").cloned().unwrap_or(JsonValue::Null);
                let new_lua = json_to_lua(&self.lua, &new_val)?;
                let old_lua = json_to_lua(&self.lua, &old_val)?;
                callback.call::<_, ()>((new_lua, old_lua))
            }
        };
        self.set_current_module(None);

        if let Err(err) = result {
            error!(subscription = id.0, error = %err, "lua callback failed");
            self.handle_callback_error(module.as_deref(), id, err);
        }
        Ok(())
    }

    fn handle_timer(&self, id: TimerId) -> Result<()> {
        let (callback, repeating, module) = {
            let timers = self.timers.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = timers.get(&id) else {
                return Ok(());
            };
            let callback: Function = self.lua.registry_value(&entry.callback)?;
            (callback, entry.repeating, entry.module.clone())
        };
        self.set_current_module(module);
        let result = callback.call::<_, ()>(());
        self.set_current_module(None);
        if let Err(err) = result {
            error!(timer = id.0, error = %err, "lua timer callback failed");
        }

        if !repeating {
            if let Ok(mut map) = self.timers.lock() {
                map.remove(&id);
            }
        }
        Ok(())
    }

    fn remove_handler(&self, id: SubscriptionId) {
        if let Ok(mut map) = self.handlers.lock() {
            map.remove(&id);
        }
    }

    fn run_on_load(&self, name: &str) -> Result<()> {
        if let Some(hook) = self.get_module_hook(name, "on_load") {
            self.set_current_module(Some(name.to_string()));
            let result = hook.call::<_, ()>(());
            self.set_current_module(None);
            if let Err(err) = result {
                error!(module = %name, error = %err, "module on_load failed");
                // Propagate rather than setting LoadError here directly: the
                // caller (load_module, via load_init_and_modules) is the
                // single place that decides Loaded vs LoadError for a
                // module, so this failure isn't immediately clobbered back
                // to Loaded by that outer Ok(()) branch — which is exactly
                // what used to happen when this function swallowed the
                // error and always returned successfully.
                return Err(anyhow!(err.to_string()));
            }
        }
        Ok(())
    }

    fn run_on_reload(&self) {
        let order = self
            .module_order
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for name in order {
            if let Some(hook) = self.get_module_hook(&name, "on_reload") {
                self.set_current_module(Some(name.clone()));
                let result = hook.call::<_, ()>(());
                self.set_current_module(None);
                if let Err(err) = result {
                    error!(module = %name, error = %err, "module on_reload failed");
                    let builtin = self.module_is_builtin(&name);
                    self.state_handle.set_module_status(
                        name.to_string(),
                        ModuleLoadState::Degraded,
                        Some(err.to_string()),
                        builtin,
                    );
                }
            }
        }
    }

    fn run_on_unload(&self) {
        let order = self
            .module_order
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for name in order.into_iter().rev() {
            if let Some(hook) = self.get_module_hook(&name, "on_unload") {
                self.set_current_module(Some(name.clone()));
                let result = hook.call::<_, ()>(());
                self.set_current_module(None);
                if let Err(err) = result {
                    error!(module = %name, error = %err, "module on_unload failed");
                    let builtin = self.module_is_builtin(&name);
                    self.state_handle.set_module_status(
                        name.to_string(),
                        ModuleLoadState::Degraded,
                        Some(err.to_string()),
                        builtin,
                    );
                }
            }
        }
    }

    fn handle_callback_error(&self, module: Option<&str>, id: SubscriptionId, err: LuaError) {
        if let Some(module) = module {
            let builtin = self.module_is_builtin(module);
            if let Ok(mut buf) = self.recent_errors.lock() {
                if buf.len() >= 50 {
                    buf.pop_front();
                }
                buf.push_back(ErrorEntry {
                    timestamp: now_unix_ms(),
                    module: Some(module.to_string()),
                    message: err.to_string(),
                });
            }
            self.state_handle.set_module_status(
                module.to_string(),
                ModuleLoadState::Degraded,
                Some(err.to_string()),
                builtin,
            );
            if let Some(hook) = self.get_module_hook(module, "on_error") {
                match hook.call::<_, bool>(err.to_string()) {
                    Ok(keep) => {
                        if !keep {
                            self.remove_handler(id);
                            self.state_handle.remove_subscription(id);
                            self.state_handle.remove_watch(id);
                        }
                    }
                    Err(hook_err) => {
                        error!(module = %module, error = %hook_err, "module on_error failed");
                    }
                }
            }
        }
    }

    fn get_module_hook(&self, name: &str, hook: &str) -> Option<Function<'_>> {
        let modules = self.modules.lock().ok()?;
        let info = modules.get(name)?;
        let table: Table = self.lua.registry_value(&info.table_key).ok()?;
        match table.get::<_, Value>(hook).ok()? {
            Value::Function(func) => Some(func),
            _ => None,
        }
    }

    fn module_is_registered(&self, name: &str) -> bool {
        self.modules
            .lock()
            .map(|map| map.contains_key(name))
            .unwrap_or(false)
    }

    fn module_is_builtin(&self, name: &str) -> bool {
        self.module_decls
            .lock()
            .ok()
            .and_then(|map| map.get(name).map(|d| d.builtin))
            .unwrap_or(false)
    }

    fn set_current_module(&self, name: Option<String>) {
        if let Ok(mut guard) = self.current_module.lock() {
            *guard = name;
        }
    }

    fn cancel_all_timers(&self) {
        if let Ok(mut map) = self.timers.lock() {
            for (_, entry) in map.drain() {
                let _ = entry.cancel_tx.send(true);
            }
        }
    }

    fn install_log_helpers(&self) -> Result<()> {
        // bread.log(msg)   → tracing::info
        // bread.warn(msg)  → tracing::warn
        // bread.error(msg) → tracing::error
        //
        // Each accepts any Lua value and coerces it to a string via tostring()
        // so callers can do bread.log(some_table) without a crash.
        self.lua
            .load(
                r#"
            local _bread = bread

            local function stringify(v)
                if type(v) == "string" then
                    return v
                end
                return tostring(v)
            end

            function _bread.log(msg)
                _bread.__log_info(stringify(msg))
            end

            function _bread.warn(msg)
                _bread.__log_warn(stringify(msg))
            end

            function _bread.error(msg)
                _bread.__log_error(stringify(msg))
            end
        "#,
            )
            .exec()?;

        // Register the raw Rust-backed log functions that the Lua wrappers call.
        let globals = self.lua.globals();
        let bread: mlua::Table = globals.get("bread")?;

        let info_fn = self.lua.create_function(|_, msg: String| {
            tracing::info!(target: "bread.lua", "{}", msg);
            Ok(())
        })?;
        bread.set("__log_info", info_fn)?;

        let warn_fn = self.lua.create_function(|_, msg: String| {
            tracing::warn!(target: "bread.lua", "{}", msg);
            Ok(())
        })?;
        bread.set("__log_warn", warn_fn)?;

        let error_fn = self.lua.create_function(|_, msg: String| {
            tracing::error!(target: "bread.lua", "{}", msg);
            Ok(())
        })?;
        bread.set("__log_error", error_fn)?;

        Ok(())
    }

    fn install_debounce(&self) -> Result<()> {
        // bread.debounce(delay_ms, fn) → wrapped_fn
        //
        // Returns a new function. When that function is called, it resets a
        // timer. The original function is only called once the timer expires
        // without being reset. Useful for rapid hardware events (e.g. monitor
        // topology changes that fire multiple events in quick succession).
        //
        // Because the Lua runtime is single-threaded, we implement this in
        // pure Lua using bread.cancel / bread.after.
        self.lua
            .load(
                r#"
            function bread.debounce(delay_ms, fn)
                local timer_id = nil
                return function(...)
                    local args = { ... }
                    if timer_id then
                        bread.cancel(timer_id)
                        timer_id = nil
                    end
                    timer_id = bread.after(delay_ms, function()
                        timer_id = nil
                        fn(table.unpack(args))
                    end)
                end
            end
        "#,
            )
            .exec()?;
        Ok(())
    }

    fn scan_module_decl(&self, path: &Path) -> Result<ModuleDecl> {
        const MODULE_DECL_ABORT: &str = "__bread_module_decl__";
        let lua = Lua::new();
        let decl_cell: Rc<RefCell<Option<ModuleDecl>>> = Rc::new(RefCell::new(None));
        let decl_cell_cloned = decl_cell.clone();
        let module_path = path.to_path_buf();

        let module_fn = lua.create_function(move |_lua, table: Table| -> mlua::Result<()> {
            let name: String = table.get("name")?;
            let version: Option<String> = table.get("version").ok();
            let after: Vec<String> = table.get("after").unwrap_or_default();
            *decl_cell_cloned.borrow_mut() = Some(ModuleDecl {
                name,
                version,
                after,
                path: module_path.clone(),
                source: None,
                builtin: false,
            });
            Err(LuaError::RuntimeError(MODULE_DECL_ABORT.to_string()))
        })?;

        // Build a minimal bread stub: bread.module() captures the decl and aborts;
        // all other bread.* accesses return a no-op callable so modules that call
        // bread.log() or bread.fs.exists() before bread.module() don't crash during scanning.
        let bread = lua.create_table()?;
        bread.set("module", module_fn)?;
        lua.globals().set("bread", bread)?;
        lua.load(
            r#"
            local _noop = function(...) end
            local _noop_tbl_mt = { __index = function() return _noop end, __call = _noop }
            local _noop_tbl = setmetatable({}, _noop_tbl_mt)
            setmetatable(bread, {
                __index = function(_, k)
                    if k == "module" then return rawget(bread, "module") end
                    return _noop_tbl
                end
            })
        "#,
        )
        .exec()?;

        let src = fs::read_to_string(path)?;
        let result = lua
            .load(&src)
            .set_name(path.to_string_lossy().as_ref())
            .exec();
        // bread.module() throws MODULE_DECL_ABORT to abort scanning early.
        // mlua may wrap the error in CallbackError, so match on string content.
        if let Err(err) = result {
            if !err.to_string().contains(MODULE_DECL_ABORT) {
                return Err(anyhow!(err.to_string()));
            }
        }

        let decl = decl_cell.borrow().clone();
        decl.ok_or_else(|| anyhow!("module missing bread.module declaration"))
    }

    fn install_require_loader(&self) -> Result<()> {
        let module_path = self.module_path.clone();
        let loader = self.lua.create_function(move |lua, name: String| {
            if !name.starts_with("bread.") {
                return Ok(Value::Nil);
            }

            let rel = name.trim_start_matches("bread.").replace('.', "/");
            let path = module_path.join(format!("{rel}.lua"));
            if !path.exists() {
                return Ok(Value::Nil);
            }

            let src = fs::read_to_string(&path).map_err(|e| LuaError::external(e.to_string()))?;
            let func = lua
                .load(&src)
                .set_name(path.to_string_lossy().as_ref())
                .into_function()
                .map_err(|e| LuaError::external(e.to_string()))?;
            Ok(Value::Function(func))
        })?;

        let globals = self.lua.globals();
        let bread: Table = globals.get("bread")?;
        bread.set("__require_loader", loader)?;

        self.lua
            .load(
                r#"
            local searchers = package.searchers or package.loaders
            if searchers then
                table.insert(searchers, 1, function(name)
                    return bread.__require_loader(name)
                end)
            end
            "#,
            )
            .exec()?;

        Ok(())
    }

    fn install_wait_helper(&self) -> Result<()> {
        self.lua
            .load(
                r#"
                bread.spawn = function(fn)
                    local co = coroutine.create(fn)
                    local ok, err = coroutine.resume(co)
                    if not ok then
                        error(err)
                    end
                end

                bread.wait = function(pattern, opts)
                    if type(pattern) ~= "string" then
                        error("bread.wait requires a pattern string")
                    end
                    opts = opts or {}
                    local co = coroutine.running()
                    if not co then
                        error("bread.wait must be called inside a coroutine")
                    end
                    local id
                    local timer
                    id = bread.once(pattern, function(event)
                        if timer then
                            bread.cancel(timer)
                        end
                        coroutine.resume(co, event)
                    end)
                    if opts.timeout then
                        timer = bread.after(opts.timeout, function()
                            bread.off(id)
                            coroutine.resume(co, nil)
                        end)
                    end
                    return coroutine.yield()
                end
                "#,
            )
            .exec()?;
        Ok(())
    }

    /// `bread.workflow` (define/start/step/status/list) and the multi-condition
    /// wait helpers `bread.wait_any`/`bread.wait_all`. Composition (spawning,
    /// yielding, timeouts) is plain Lua built on the existing `bread.spawn`/
    /// `bread.on`/`bread.once`/`bread.after`/`bread.cancel`/`bread.off`
    /// primitives from [`install_wait_helper`](Self::install_wait_helper) —
    /// mirroring how that method itself works. Only the *introspectable
    /// status* piece needs a Rust host bridge (the `__workflow_*` functions
    /// below), since `workflows.list` is served over IPC from the async side
    /// while the workflow body runs as a Lua coroutine on this dedicated Lua
    /// thread; both sides read/write the same `Arc<RwLock<RuntimeState>>`
    /// that `module_store_get`/`module_store_set` already use for exactly
    /// this kind of cross-thread bridging.
    fn install_workflow_helpers(&self) -> Result<()> {
        let globals = self.lua.globals();
        let bread: Table = globals.get("bread")?;

        let state_arc = self.state_handle.state_arc();
        let register_fn = self.lua.create_function(move |_lua, name: String| {
            workflow_register(&state_arc, &name);
            Ok(())
        })?;
        bread.set("__workflow_register", register_fn)?;

        let state_arc = self.state_handle.state_arc();
        let step_fn = self
            .lua
            .create_function(move |_lua, (name, label): (String, String)| {
                workflow_step(&state_arc, &name, &label);
                Ok(())
            })?;
        bread.set("__workflow_step", step_fn)?;

        let state_arc = self.state_handle.state_arc();
        let finish_fn = self.lua.create_function(move |_lua, name: String| {
            workflow_finish(&state_arc, &name);
            Ok(())
        })?;
        bread.set("__workflow_finish", finish_fn)?;

        let state_arc = self.state_handle.state_arc();
        let fail_fn = self
            .lua
            .create_function(move |_lua, (name, error): (String, String)| {
                workflow_fail(&state_arc, &name, &error);
                Ok(())
            })?;
        bread.set("__workflow_fail", fail_fn)?;

        let state_arc = self.state_handle.state_arc();
        let timeout_fn = self.lua.create_function(move |_lua, name: String| {
            workflow_timeout(&state_arc, &name);
            Ok(())
        })?;
        bread.set("__workflow_timeout", timeout_fn)?;

        let state_arc = self.state_handle.state_arc();
        let status_fn = self.lua.create_function(move |lua, name: String| {
            match workflow_status_json(&state_arc, &name) {
                Some(json) => json_to_lua(lua, &json).map_err(|e| LuaError::external(e.to_string())),
                None => Ok(Value::Nil),
            }
        })?;
        bread.set("__workflow_status", status_fn)?;

        let state_arc = self.state_handle.state_arc();
        let list_fn = self.lua.create_function(move |lua, ()| {
            let json = workflow_list_json(&state_arc);
            json_to_lua(lua, &json)
                .map_err(|e| LuaError::external(e.to_string()))
        })?;
        bread.set("__workflow_list", list_fn)?;

        self.lua
            .load(
                r#"
                bread.wait_any = function(patterns, opts)
                    if type(patterns) ~= "table" then
                        error("bread.wait_any requires a table of patterns")
                    end
                    opts = opts or {}
                    local co = coroutine.running()
                    if not co then
                        error("bread.wait_any must be called inside a coroutine")
                    end
                    local ids = {}
                    local timer
                    local resumed = false
                    local function cleanup()
                        for _, id in ipairs(ids) do
                            bread.off(id)
                        end
                        if timer then
                            bread.cancel(timer)
                        end
                    end
                    for _, pattern in ipairs(patterns) do
                        local id = bread.once(pattern, function(event)
                            if resumed then return end
                            resumed = true
                            cleanup()
                            coroutine.resume(co, event, pattern)
                        end)
                        table.insert(ids, id)
                    end
                    if opts.timeout then
                        timer = bread.after(opts.timeout, function()
                            if resumed then return end
                            resumed = true
                            cleanup()
                            coroutine.resume(co, nil, nil)
                        end)
                    end
                    return coroutine.yield()
                end

                bread.wait_all = function(patterns, opts)
                    if type(patterns) ~= "table" then
                        error("bread.wait_all requires a table of patterns")
                    end
                    opts = opts or {}
                    local co = coroutine.running()
                    if not co then
                        error("bread.wait_all must be called inside a coroutine")
                    end
                    local remaining = {}
                    local count = 0
                    for _, p in ipairs(patterns) do
                        if remaining[p] == nil then
                            remaining[p] = true
                            count = count + 1
                        end
                    end
                    local results = {}
                    local got = 0
                    local timer
                    local resumed = false
                    local ids = {}
                    local function finish(timed_out)
                        if resumed then return end
                        resumed = true
                        for _, id in ipairs(ids) do
                            bread.off(id)
                        end
                        if timer then
                            bread.cancel(timer)
                        end
                        if timed_out then
                            results.timed_out = true
                        end
                        coroutine.resume(co, results)
                    end
                    for _, pattern in ipairs(patterns) do
                        local id = bread.once(pattern, function(event)
                            if remaining[pattern] then
                                remaining[pattern] = nil
                                results[pattern] = event
                                got = got + 1
                                if got >= count then
                                    finish(false)
                                end
                            end
                        end)
                        table.insert(ids, id)
                    end
                    if opts.timeout then
                        timer = bread.after(opts.timeout, function()
                            finish(true)
                        end)
                    end
                    return coroutine.yield()
                end

                bread.workflow = {}
                local __workflow_bodies = {}
                local __co_to_workflow = setmetatable({}, { __mode = "k" })

                bread.workflow.define = function(name, fn)
                    if type(name) ~= "string" then
                        error("bread.workflow.define requires a name string")
                    end
                    __workflow_bodies[name] = fn
                end

                bread.workflow.start = function(name, opts)
                    local fn = __workflow_bodies[name]
                    if not fn then
                        error("bread.workflow.start: no workflow defined with name '" .. tostring(name) .. "'")
                    end
                    opts = opts or {}

                    bread.__workflow_register(name)

                    local deadline_timer
                    if opts.deadline then
                        deadline_timer = bread.after(opts.deadline, function()
                            bread.__workflow_timeout(name)
                        end)
                    end

                    local co = coroutine.create(function()
                        local ok, err = pcall(fn, opts.args)
                        if deadline_timer then
                            bread.cancel(deadline_timer)
                        end
                        if ok then
                            bread.__workflow_finish(name)
                        else
                            bread.__workflow_fail(name, tostring(err))
                        end
                    end)
                    __co_to_workflow[co] = name

                    local ok, err = coroutine.resume(co)
                    if not ok then
                        bread.__workflow_fail(name, tostring(err))
                    end
                end

                bread.workflow.step = function(label)
                    local co = coroutine.running()
                    local name = co and __co_to_workflow[co]
                    if not name then
                        error("bread.workflow.step must be called inside a running workflow body")
                    end
                    bread.__workflow_step(name, label)
                end

                bread.workflow.status = function(name)
                    return bread.__workflow_status(name)
                end

                bread.workflow.list = function()
                    return bread.__workflow_list()
                end
                "#,
            )
            .exec()?;
        Ok(())
    }
}

fn workflow_register(state_arc: &Arc<RwLock<RuntimeState>>, name: &str) {
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    let now = now_unix_ms();
    if let Some(entry) = guard.workflows.iter_mut().find(|w| w.name == name) {
        entry.state = WorkflowState::Running;
        entry.step = None;
        entry.started_at = now;
        entry.updated_at = now;
        entry.error = None;
    } else {
        guard.workflows.push(WorkflowStatus {
            name: name.to_string(),
            state: WorkflowState::Running,
            step: None,
            started_at: now,
            updated_at: now,
            error: None,
        });
    }
}

fn workflow_step(state_arc: &Arc<RwLock<RuntimeState>>, name: &str, label: &str) {
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    if let Some(entry) = guard.workflows.iter_mut().find(|w| w.name == name) {
        entry.step = Some(label.to_string());
        entry.updated_at = now_unix_ms();
    }
}

fn workflow_finish(state_arc: &Arc<RwLock<RuntimeState>>, name: &str) {
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    if let Some(entry) = guard.workflows.iter_mut().find(|w| w.name == name) {
        entry.state = WorkflowState::Done;
        entry.updated_at = now_unix_ms();
    }
}

fn workflow_fail(state_arc: &Arc<RwLock<RuntimeState>>, name: &str, error: &str) {
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    if let Some(entry) = guard.workflows.iter_mut().find(|w| w.name == name) {
        entry.state = WorkflowState::Failed;
        entry.error = Some(error.to_string());
        entry.updated_at = now_unix_ms();
    }
}

fn workflow_timeout(state_arc: &Arc<RwLock<RuntimeState>>, name: &str) {
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    if let Some(entry) = guard.workflows.iter_mut().find(|w| w.name == name) {
        // Don't clobber a workflow that already reached a terminal state
        // between the deadline firing and this callback running.
        if entry.state == WorkflowState::Running {
            entry.state = WorkflowState::TimedOut;
            entry.updated_at = now_unix_ms();
        }
    }
}

fn workflow_status_json(state_arc: &Arc<RwLock<RuntimeState>>, name: &str) -> Option<JsonValue> {
    let guard = loop {
        if let Ok(g) = state_arc.try_read() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    let entry = guard.workflows.iter().find(|w| w.name == name)?;
    serde_json::to_value(entry).ok()
}

fn workflow_list_json(state_arc: &Arc<RwLock<RuntimeState>>) -> JsonValue {
    let guard = loop {
        if let Ok(g) = state_arc.try_read() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    serde_json::to_value(&guard.workflows).unwrap_or_else(|_| JsonValue::Array(vec![]))
}

/// Table shape accepted by `bread.widget.register`. Parsed directly from the
/// Lua table via mlua's serde bridge, so `root`'s nested `children` tables
/// deserialize straight into a `WidgetNode` tree without a manual walker.
#[derive(Debug, Deserialize)]
struct WidgetRegisterArgs {
    id: String,
    placement: WidgetPlacement,
    #[serde(default)]
    order: i32,
    #[serde(default = "default_widget_visible")]
    visible: bool,
    #[serde(default)]
    tooltip: Option<String>,
    root: WidgetNode,
}

/// Table shape accepted by `bread.widget.update` — every field optional, so
/// a caller can patch just what changed (typically just `root` on a timer).
#[derive(Debug, Default, Deserialize)]
struct WidgetUpdateArgs {
    #[serde(default)]
    root: Option<WidgetNode>,
    #[serde(default)]
    tooltip: Option<String>,
    #[serde(default)]
    visible: Option<bool>,
    #[serde(default)]
    order: Option<i32>,
}

fn default_widget_visible() -> bool {
    true
}

fn widget_register(
    state_arc: &Arc<RwLock<RuntimeState>>,
    module: &str,
    args: WidgetRegisterArgs,
) -> std::result::Result<WidgetSpec, bread_shared::widget::WidgetValidationError> {
    args.root.validate()?;
    let spec = WidgetSpec {
        id: format!("{module}.{}", args.id),
        module: module.to_string(),
        placement: args.placement,
        order: args.order,
        visible: args.visible,
        tooltip: args.tooltip,
        root: args.root,
        updated_at: now_unix_ms(),
    };
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    if let Some(existing) = guard.widgets.iter_mut().find(|w| w.id == spec.id) {
        *existing = spec.clone();
    } else {
        guard.widgets.push(spec.clone());
    }
    Ok(spec)
}

fn widget_update(
    state_arc: &Arc<RwLock<RuntimeState>>,
    module: &str,
    local_id: &str,
    args: WidgetUpdateArgs,
) -> std::result::Result<Option<WidgetSpec>, bread_shared::widget::WidgetValidationError> {
    if let Some(root) = &args.root {
        root.validate()?;
    }
    let full_id = format!("{module}.{local_id}");
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    let Some(entry) = guard.widgets.iter_mut().find(|w| w.id == full_id) else {
        return Ok(None);
    };
    if let Some(root) = args.root {
        entry.root = root;
    }
    if let Some(tooltip) = args.tooltip {
        entry.tooltip = Some(tooltip);
    }
    if let Some(visible) = args.visible {
        entry.visible = visible;
    }
    if let Some(order) = args.order {
        entry.order = order;
    }
    entry.updated_at = now_unix_ms();
    Ok(Some(entry.clone()))
}

fn widget_remove(state_arc: &Arc<RwLock<RuntimeState>>, module: &str, local_id: &str) -> bool {
    let full_id = format!("{module}.{local_id}");
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    let before = guard.widgets.len();
    guard.widgets.retain(|w| w.id != full_id);
    before != guard.widgets.len()
}

fn widget_list_json(state_arc: &Arc<RwLock<RuntimeState>>, module: &str) -> JsonValue {
    let guard = loop {
        if let Ok(g) = state_arc.try_read() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    let mine: Vec<&WidgetSpec> = guard.widgets.iter().filter(|w| w.module == module).collect();
    serde_json::to_value(&mine).unwrap_or_else(|_| JsonValue::Array(vec![]))
}

fn order_module_decls(decls: Vec<ModuleDecl>) -> (Vec<ModuleDecl>, Vec<(String, String)>) {
    let mut errors = Vec::new();
    let mut map: HashMap<String, ModuleDecl> = HashMap::new();
    for decl in decls {
        if map.contains_key(&decl.name) {
            errors.push((decl.name.clone(), "duplicate module name".to_string()));
            continue;
        }
        map.insert(decl.name.clone(), decl);
    }

    let mut deps: HashMap<String, HashSet<String>> = HashMap::new();
    let mut reverse: HashMap<String, HashSet<String>> = HashMap::new();
    let mut invalid: HashSet<String> = HashSet::new();

    for (name, decl) in map.iter() {
        let mut missing = Vec::new();
        for dep in &decl.after {
            if map.contains_key(dep) {
                deps.entry(name.clone()).or_default().insert(dep.clone());
                reverse.entry(dep.clone()).or_default().insert(name.clone());
            } else {
                missing.push(dep.clone());
            }
        }
        if !missing.is_empty() {
            errors.push((
                name.clone(),
                format!("missing dependency: {}", missing.join(", ")),
            ));
            invalid.insert(name.clone());
        }
    }

    let mut ready: Vec<String> = map
        .keys()
        .filter(|name| !deps.contains_key(*name) && !invalid.contains(*name))
        .cloned()
        .collect();
    ready.sort();

    let mut ordered = Vec::new();
    let mut deps = deps;

    while let Some(name) = ready.pop() {
        if let Some(decl) = map.get(&name) {
            ordered.push(decl.clone());
        }
        if let Some(children) = reverse.remove(&name) {
            for child in children {
                if invalid.contains(&child) {
                    continue;
                }
                if let Some(entry) = deps.get_mut(&child) {
                    entry.remove(&name);
                    if entry.is_empty() {
                        deps.remove(&child);
                        ready.push(child);
                        ready.sort();
                    }
                }
            }
        }
    }

    for (name, _) in deps {
        if !invalid.contains(&name) {
            errors.push((name, "circular dependency".to_string()));
        }
    }

    (ordered, errors)
}

fn module_name_from_path(module_root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(module_root).unwrap_or(path);
    let mut name = rel.with_extension("").to_string_lossy().replace('/', ".");
    if name.starts_with('.') {
        name.remove(0);
    }
    name
}

fn is_lib_path(module_root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(module_root).unwrap_or(path);
    rel.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .map(|c| c == "lib")
        .unwrap_or(false)
}

/// `lua.to_value()`'s default `Options` map JSON null / Rust `Option::None`
/// to a distinct `lua.null()` sentinel rather than real Lua `nil`, to
/// preserve JSON round-trip fidelity — but bread never round-trips a Lua
/// value back into JSON through mlua, so that distinction buys nothing here
/// and only sets a trap for the ordinary Lua idiom `if not value then ...`,
/// which silently doesn't catch the sentinel (found via a module crashing
/// on `#value` when `active_window` was null: `not <sentinel>` is `false`,
/// same as any other non-nil value). Every JSON/state value handed to Lua
/// goes through this instead of a bare `to_value` call.
fn json_to_lua<'lua, T>(lua: &'lua Lua, value: &T) -> mlua::Result<Value<'lua>>
where
    T: Serialize + ?Sized,
{
    lua.to_value_with(
        value,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

fn state_value_to_lua<'lua>(
    lua: &'lua Lua,
    state_arc: &Arc<RwLock<RuntimeState>>,
    path: &str,
) -> mlua::Result<Value<'lua>> {
    // The Lua thread runs a current_thread runtime. blocking_read and block_in_place
    // both require the multi-thread runtime and panic here. try_read succeeds
    // immediately in the common case; the write lock is held for microseconds.
    let snapshot = loop {
        if let Ok(g) = state_arc.try_read() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    let mut value =
        serde_json::to_value(&*snapshot).map_err(|e| LuaError::external(e.to_string()))?;
    if path.is_empty() {
        return json_to_lua(lua, &value).map_err(|e| LuaError::external(e.to_string()));
    }
    for part in path.split('.') {
        value = value
            .get(part)
            .cloned()
            .ok_or_else(|| LuaError::external("state path not found"))?;
    }
    json_to_lua(lua, &value).map_err(|e| LuaError::external(e.to_string()))
}

fn module_store_get(
    state_arc: &Arc<RwLock<RuntimeState>>,
    module: &str,
    key: &str,
) -> Option<JsonValue> {
    let guard = loop {
        if let Ok(g) = state_arc.try_read() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    let entry = guard.modules.iter().find(|m| m.name == module)?;
    entry.store.get(key).cloned()
}

fn module_store_set(
    state_arc: &Arc<RwLock<RuntimeState>>,
    module: &str,
    key: String,
    value: JsonValue,
) {
    let mut guard = loop {
        if let Ok(g) = state_arc.try_write() {
            break g;
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    };
    if let Some(entry) = guard.modules.iter_mut().find(|m| m.name == module) {
        entry.store.insert(key, value);
        return;
    }

    let mut store = HashMap::new();
    store.insert(key, value);
    guard.modules.push(crate::core::types::ModuleStatus {
        name: module.to_string(),
        status: ModuleLoadState::Loaded,
        last_error: None,
        builtin: false,
        store,
    });
}

fn lua_expand_path(path: &str) -> std::path::PathBuf {
    if path == "~" {
        if let Some(home) = dirs_home() {
            return home;
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return home.join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

fn dirs_home() -> Option<std::path::PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        return Some(std::path::PathBuf::from(home));
    }
    None
}

fn lua_machine_name() -> String {
    if let Ok(sync_toml) = read_sync_toml() {
        if let Some(name) = sync_toml
            .get("machine")
            .and_then(|m| m.get("name"))
            .and_then(|v| v.as_str())
        {
            return name.to_string();
        }
    }
    lua_hostname()
}

fn lua_hostname() -> String {
    // Try gethostname via libc
    let mut buf = [0u8; 256];
    unsafe {
        if libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) == 0 {
            if let Ok(s) = std::ffi::CStr::from_ptr(buf.as_ptr() as *const libc::c_char).to_str() {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    // Fall back to /etc/hostname
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = h.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn lua_machine_tags() -> Vec<String> {
    if let Ok(sync_toml) = read_sync_toml() {
        if let Some(tags) = sync_toml
            .get("machine")
            .and_then(|m| m.get("tags"))
            .and_then(|v| v.as_array())
        {
            return tags
                .iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect();
        }
    }
    vec![]
}

fn read_sync_toml() -> anyhow::Result<toml::Value> {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(|_| std::path::PathBuf::from(".config"));
    let path = config_dir.join("bread").join("sync.toml");
    let raw = std::fs::read_to_string(path)?;
    Ok(raw.parse::<toml::Value>()?)
}

const BUILTIN_MONITORS: &str = r#"
local M = bread.module({ name = "bread.monitors", version = "1.0.0" })

local workflows = {}
local layouts = {}

local function matches_when(event_name, when)
    if when == "connected" then
        return event_name == "bread.monitor.connected"
    elseif when == "disconnected" then
        return event_name == "bread.monitor.disconnected"
    elseif when == "changed" then
        return event_name == "bread.monitor.changed"
    end
    return false
end

local function matches_monitors(list, event)
    if not list or #list == 0 then
        return true
    end
    local name = event.data and event.data.name
    if not name then
        return false
    end
    for _, monitor in ipairs(list) do
        if monitor == name then
            return true
        end
    end
    return false
end

local function run_workflow(wf, event)
    if type(wf.run) == "function" then
        wf.run(event)
    elseif type(wf.run) == "string" then
        bread.exec(wf.run)
    end
end

function M.on(opts)
    table.insert(workflows, opts)
end

function M.layout(name, fn)
    layouts[name] = fn
end

function M.apply(name)
    return function()
        local fn = layouts[name]
        if fn then
            fn()
        end
    end
end

function M.on_load()
    bread.on("bread.monitor.**", function(event)
        for _, wf in ipairs(workflows) do
            if matches_when(event.event, wf.when) and matches_monitors(wf.monitors, event) then
                run_workflow(wf, event)
            end
        end
    end)
end

return M
"#;

const BUILTIN_DEVICES: &str = r#"
local M = bread.module({ name = "bread.devices", version = "1.0.0" })

local rules = {}

local function matches_rule(rule, event)
    local when = rule.when
    local data = event.data or {}

    if when == "connected" and not event.event:match("%.connected$") then
        return false
    elseif when == "disconnected" and not event.event:match("%.disconnected$") then
        return false
    end

    if rule.device and data.device ~= rule.device then
        return false
    end

    if rule.name and data.name and not tostring(data.name):match(rule.name) then
        return false
    end

    return true
end

local function run_rule(rule, event)
    if type(rule.run) == "function" then
        rule.run(event)
    elseif type(rule.run) == "string" then
        bread.exec(rule.run)
    end
end

function M.on(opts)
    table.insert(rules, opts)
end

function M.on_load()
    bread.on("bread.device.**", function(event)
        for _, rule in ipairs(rules) do
            if matches_rule(rule, event) then
                run_rule(rule, event)
            end
        end
    end)
end

return M
"#;

const BUILTIN_WORKSPACES: &str = r#"
local M = bread.module({ name = "bread.workspaces", version = "1.0.0", after = { "bread.monitors" } })

local assignments = {}
local rules = {}

function M.assign(workspace, monitor)
    table.insert(assignments, { workspace = workspace, monitor = monitor })
end

function M.pin(opts)
    table.insert(rules, opts)
end

function M.apply_assignments()
    local monitors = bread.state.monitors()
    local active = {}
    for _, m in ipairs(monitors) do
        if m.connected then
            active[m.name] = true
        end
    end

    for _, a in ipairs(assignments) do
        if active[a.monitor] then
            bread.hyprland.dispatch("moveworkspacetomonitor", a.workspace .. " " .. a.monitor)
        end
    end
end

function M.on_load()
    bread.on("bread.monitor.**", function()
        M.apply_assignments()
    end)

    bread.on("bread.window.opened", function(event)
        for _, rule in ipairs(rules) do
            if event.data and event.data.class and event.data.class:match(rule.app) then
                local address = event.data.address or ""
                bread.hyprland.dispatch("movetoworkspacesilent", rule.workspace .. ",address:" .. address)
            end
        end
    end)

    bread.once("bread.system.startup", function()
        M.apply_assignments()
    end)
end

return M
"#;

const BUILTIN_BINDS: &str = r#"
local M = bread.module({ name = "bread.binds", version = "1.0.0" })

local active = {}

local function bind_string(opts)
    local mods = table.concat(opts.mods or {}, " ")
    local args = opts.args or ""
    if mods ~= "" then
        return mods .. ", " .. opts.key .. ", " .. opts.dispatch .. ", " .. args
    end
    return opts.key .. ", " .. opts.dispatch .. ", " .. args
end

function M.add(opts)
    local bind = bind_string(opts)
    bread.hyprland.keyword("bind", bind)
    active[opts.key] = opts
    return opts.key
end

function M.remove(key)
    local bind = active[key]
    if not bind then
        return
    end
    bread.hyprland.keyword("unbind", bind_string(bind))
    active[key] = nil
end

function M.replace(key, opts)
    M.remove(key)
    return M.add(opts)
end

function M.on_unload()
    for key, _ in pairs(active) do
        M.remove(key)
    end
end

return M
"#;

fn builtin_module_decls(disabled: &HashSet<String>) -> Vec<ModuleDecl> {
    let mut out = Vec::new();

    let entries = vec![
        ("bread.monitors", "1.0.0", Vec::new(), BUILTIN_MONITORS),
        ("bread.devices", "1.0.0", Vec::new(), BUILTIN_DEVICES),
        (
            "bread.workspaces",
            "1.0.0",
            vec!["bread.monitors".to_string()],
            BUILTIN_WORKSPACES,
        ),
        ("bread.binds", "1.0.0", Vec::new(), BUILTIN_BINDS),
    ];

    for (name, version, after, source) in entries {
        if disabled.contains(name) {
            continue;
        }
        out.push(ModuleDecl {
            name: name.to_string(),
            version: Some(version.to_string()),
            after,
            path: PathBuf::from(format!("<builtin:{name}>")),
            source: Some(source),
            builtin: true,
        });
    }

    out
}

fn hyprland_request_socket() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());

    if let Ok(instance) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        return Ok(PathBuf::from(runtime)
            .join("hypr")
            .join(instance)
            .join(".socket.sock"));
    }

    let hypr_dir = PathBuf::from(&runtime).join("hypr");
    let mut sockets: Vec<PathBuf> = std::fs::read_dir(&hypr_dir)
        .map_err(|_| anyhow!("no Hyprland instance found ({})", hypr_dir.display()))?
        .flatten()
        .map(|e| e.path().join(".socket.sock"))
        .filter(|p| p.exists())
        .collect();

    match sockets.len() {
        0 => Err(anyhow!(
            "no Hyprland instance found in {}",
            hypr_dir.display()
        )),
        1 => Ok(sockets.remove(0)),
        _ => {
            warn!(
                "multiple Hyprland instances found in {}; using the first one",
                hypr_dir.display()
            );
            Ok(sockets.remove(0))
        }
    }
}

fn hyprland_request(request: &str) -> Result<String> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let socket = hyprland_request_socket()?;
    let mut stream = UnixStream::connect(&socket)?;
    stream.write_all(request.as_bytes())?;
    let mut buffer = String::new();
    stream.read_to_string(&mut buffer)?;
    Ok(buffer)
}

fn parse_match_condition(tbl: &mlua::Table) -> MatchCondition {
    MatchCondition {
        vendor_id: tbl.get("vendor_id").ok(),
        product_id: tbl.get("product_id").ok(),
        name: tbl.get("name").ok(),
        vendor: tbl.get("vendor").ok(),
        name_contains: tbl.get("name_contains").ok(),
        id_input_keyboard: tbl.get("id_input_keyboard").ok(),
        id_input_mouse: tbl.get("id_input_mouse").ok(),
        id_input_tablet: tbl.get("id_input_tablet").ok(),
        usb_hub: tbl.get("usb_hub").ok(),
        id_usb_class: tbl.get("id_usb_class").ok(),
        subsystem: tbl.get("subsystem").ok(),
    }
}

fn list_lua_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("lua") {
                out.push(path);
            }
        }
    }
    Ok(out)
}

// ─── Bluetooth helpers ────────────────────────────────────────────────────────

/// Spawn a dedicated thread with its own Tokio runtime for a fire-and-forget
/// async Bluetooth operation. Needed because the Lua thread runs inside
/// `block_on` on a current-thread runtime, so nested `block_on` is forbidden.
fn bluetooth_spawn<F, Fut>(factory: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!(error = %e, "bluetooth action: failed to build tokio runtime");
                return;
            }
        };
        rt.block_on(factory());
    });
}

/// Like `bluetooth_spawn` but waits for the result via a sync channel so Lua
/// gets a return value.
fn bluetooth_query<F, Fut, T>(factory: F) -> anyhow::Result<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(factory()),
            Err(e) => Err(anyhow::anyhow!(
                "bluetooth query: failed to build tokio runtime: {e}"
            )),
        };
        let _ = tx.send(result);
    });
    rx.recv()
        .map_err(|_| anyhow::anyhow!("bluetooth query thread failed"))?
}

async fn bluetooth_find_adapter(conn: &zbus::Connection) -> anyhow::Result<String> {
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};
    let msg = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;
    let objects: std::collections::HashMap<
        OwnedObjectPath,
        std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>>,
    > = msg.body()?;
    for (path, interfaces) in &objects {
        if interfaces.contains_key("org.bluez.Adapter1") {
            return Ok(path.as_str().to_string());
        }
    }
    Err(anyhow::anyhow!("no Bluetooth adapter found"))
}

async fn bluetooth_set_powered(enabled: bool) -> anyhow::Result<()> {
    let conn = zbus::Connection::system().await?;
    let adapter = bluetooth_find_adapter(&conn).await?;
    conn.call_method(
        Some("org.bluez"),
        adapter.as_str(),
        Some("org.freedesktop.DBus.Properties"),
        "Set",
        &(
            "org.bluez.Adapter1",
            "Powered",
            zbus::zvariant::Value::from(enabled),
        ),
    )
    .await?;
    Ok(())
}

async fn bluetooth_get_powered() -> anyhow::Result<bool> {
    let conn = zbus::Connection::system().await?;
    let adapter = bluetooth_find_adapter(&conn).await?;
    let msg = conn
        .call_method(
            Some("org.bluez"),
            adapter.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.bluez.Adapter1", "Powered"),
        )
        .await?;
    let (value,): (zbus::zvariant::OwnedValue,) = msg.body()?;
    let json = serde_json::to_value(&value).unwrap_or(serde_json::json!(false));
    Ok(json.as_bool().unwrap_or(false))
}

async fn bluetooth_connect(address: String) -> anyhow::Result<()> {
    let conn = zbus::Connection::system().await?;
    let adapter = bluetooth_find_adapter(&conn).await?;
    let dev_path = format!("{}/dev_{}", adapter, address.replace(':', "_"));
    conn.call_method(
        Some("org.bluez"),
        dev_path.as_str(),
        Some("org.bluez.Device1"),
        "Connect",
        &(),
    )
    .await?;
    Ok(())
}

async fn bluetooth_disconnect(address: String) -> anyhow::Result<()> {
    let conn = zbus::Connection::system().await?;
    let adapter = bluetooth_find_adapter(&conn).await?;
    let dev_path = format!("{}/dev_{}", adapter, address.replace(':', "_"));
    conn.call_method(
        Some("org.bluez"),
        dev_path.as_str(),
        Some("org.bluez.Device1"),
        "Disconnect",
        &(),
    )
    .await?;
    Ok(())
}

async fn bluetooth_set_scanning(enabled: bool) -> anyhow::Result<()> {
    let conn = zbus::Connection::system().await?;
    let adapter = bluetooth_find_adapter(&conn).await?;
    let method = if enabled {
        "StartDiscovery"
    } else {
        "StopDiscovery"
    };
    conn.call_method(
        Some("org.bluez"),
        adapter.as_str(),
        Some("org.bluez.Adapter1"),
        method,
        &(),
    )
    .await?;
    Ok(())
}

struct BluetoothDevice {
    address: String,
    name: String,
    connected: bool,
    paired: bool,
}

async fn bluetooth_list_devices() -> anyhow::Result<Vec<BluetoothDevice>> {
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};
    let conn = zbus::Connection::system().await?;
    let msg = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;
    let objects: std::collections::HashMap<
        OwnedObjectPath,
        std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>>,
    > = msg.body()?;

    let mut devices = Vec::new();
    for interfaces in objects.values() {
        if let Some(props) = interfaces.get("org.bluez.Device1") {
            let json = serde_json::to_value(props).unwrap_or_else(|_| serde_json::json!({}));
            devices.push(BluetoothDevice {
                address: json
                    .get("Address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                name: json
                    .get("Name")
                    .or_else(|| json.get("Alias"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                connected: json
                    .get("Connected")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                paired: json
                    .get("Paired")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            });
        }
    }
    Ok(devices)
}
