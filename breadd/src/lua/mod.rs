use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use bread_shared::{AdapterSource, BreadEvent};
use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::task;
use tracing::{error, info, warn};

use crate::core::config::Config;
use crate::core::state_engine::StateHandle;
use crate::core::subscriptions::SubscriptionId;
use crate::core::types::ModuleLoadState;

pub enum LuaMessage {
    Event {
        subscription_id: SubscriptionId,
        event: BreadEvent,
    },
    SubscriptionCancelled {
        id: SubscriptionId,
    },
    Reload {
        reply: oneshot::Sender<std::result::Result<(), String>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct RuntimeHandle {
    tx: mpsc::UnboundedSender<LuaMessage>,
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
}

pub fn spawn_runtime(
    config: Config,
    state_handle: StateHandle,
    emit_tx: mpsc::UnboundedSender<BreadEvent>,
) -> Result<RuntimeHandle> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = RuntimeHandle { tx };
    let thread_tx = handle.tx.clone();

    std::thread::Builder::new()
        .name("breadd-lua".to_string())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create lua runtime thread");

            rt.block_on(async move {
                let mut engine = match LuaEngine::new(config, state_handle, emit_tx) {
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

struct LuaEngine {
    lua: Lua,
    handlers: Arc<Mutex<HashMap<SubscriptionId, RegistryKey>>>,
    next_sub_id: Arc<AtomicU64>,
    state_handle: StateHandle,
    emit_tx: mpsc::UnboundedSender<BreadEvent>,
    entry_point: PathBuf,
    module_path: PathBuf,
}

impl LuaEngine {
    fn new(config: Config, state_handle: StateHandle, emit_tx: mpsc::UnboundedSender<BreadEvent>) -> Result<Self> {
        Ok(Self {
            lua: Lua::new(),
            handlers: Arc::new(Mutex::new(HashMap::new())),
            next_sub_id: Arc::new(AtomicU64::new(1)),
            state_handle,
            emit_tx,
            entry_point: config.lua_entry_point(),
            module_path: config.lua_module_path(),
        })
    }

    fn reload_internal(&mut self) -> Result<()> {
        self.state_handle.clear_subscriptions();
        self.lua = Lua::new();
        self.handlers
            .lock()
            .expect("lua handlers mutex poisoned")
            .clear();

        self.install_api()?;
        self.load_init_and_modules()?;
        info!("lua runtime reloaded");
        Ok(())
    }

    fn install_api(&self) -> Result<()> {
        let globals = self.lua.globals();
        let bread = self.lua.create_table()?;

        let handlers = self.handlers.clone();
        let next_sub_id = self.next_sub_id.clone();
        let state_handle = self.state_handle.clone();
        let on_fn = self.lua.create_function(move |lua, (pattern, callback): (String, Function)| {
            let id = SubscriptionId(next_sub_id.fetch_add(1, Ordering::Relaxed));
            let key = lua.create_registry_value(callback)?;
            handlers
                .lock()
                .map_err(|_| mlua::Error::external("handler lock poisoned"))?
                .insert(id, key);
            state_handle
                .register_subscription(id, pattern, false)
                .map_err(mlua::Error::external)?;
            Ok(id.0)
        })?;
        bread.set("on", on_fn)?;

        let handlers = self.handlers.clone();
        let next_sub_id = self.next_sub_id.clone();
        let state_handle = self.state_handle.clone();
        let once_fn = self.lua.create_function(move |lua, (pattern, callback): (String, Function)| {
            let id = SubscriptionId(next_sub_id.fetch_add(1, Ordering::Relaxed));
            let key = lua.create_registry_value(callback)?;
            handlers
                .lock()
                .map_err(|_| mlua::Error::external("handler lock poisoned"))?
                .insert(id, key);
            state_handle
                .register_subscription(id, pattern, true)
                .map_err(mlua::Error::external)?;
            Ok(id.0)
        })?;
        bread.set("once", once_fn)?;

        let emit_tx = self.emit_tx.clone();
        let emit_fn = self.lua.create_function(move |lua, (event_name, payload): (String, Value)| {
            let data = match payload {
                Value::Nil => serde_json::json!({}),
                other => lua
                    .from_value::<serde_json::Value>(other)
                    .unwrap_or_else(|_| serde_json::json!({})),
            };
            emit_tx
                .send(BreadEvent::new(event_name, AdapterSource::System, data))
                .map_err(|_| mlua::Error::external("event channel closed"))?;
            Ok(())
        })?;
        bread.set("emit", emit_fn)?;

        let state_arc = self.state_handle.state_arc();
        let state_tbl = self.lua.create_table()?;
        let get_fn = self.lua.create_function(move |lua, path: String| {
            let snapshot = state_arc.blocking_read();
            let mut value = serde_json::to_value(&*snapshot)
                .map_err(|e| mlua::Error::external(e.to_string()))?;
            if path.is_empty() {
                return lua
                    .to_value(&value)
                    .map_err(|e| mlua::Error::external(e.to_string()));
            }
            for part in path.split('.') {
                value = value
                    .get(part)
                    .cloned()
                    .ok_or_else(|| mlua::Error::external("state path not found"))?;
            }
            lua.to_value(&value)
                .map_err(|e| mlua::Error::external(e.to_string()))
        })?;
        state_tbl.set("get", get_fn)?;
        bread.set("state", state_tbl)?;

        let profile_tbl = self.lua.create_table()?;
        let state_handle = self.state_handle.clone();
        let activate_fn = self.lua.create_function(move |_lua, name: String| {
            state_handle.set_profile(name.clone());
            Ok(())
        })?;
        profile_tbl.set("activate", activate_fn)?;
        bread.set("profile", profile_tbl)?;

        // Fire-and-forget: the process is launched on a blocking thread and the
        // Lua handler returns immediately. The Lua runtime is never stalled waiting
        // for a slow or hanging process. Exit code is logged but not returned to Lua.
        let exec_fn = self.lua.create_function(move |_lua, cmd: String| {
            task::spawn_blocking(move || {
                match std::process::Command::new("sh")
                    .arg("-lc")
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

        globals.set("bread", bread)?;
        Ok(())
    }

    fn load_init_and_modules(&self) -> Result<()> {
        self.load_lua_file(&self.entry_point, "init")?;

        let mut files = list_lua_files(&self.module_path)?;
        files.sort();
        for path in files {
            let module_name = path
                .file_stem()
                .and_then(|v| v.to_str())
                .unwrap_or("unknown")
                .to_string();
            match self.load_lua_file(&path, &module_name) {
                Ok(()) => {
                    self.state_handle
                        .set_module_status(module_name, ModuleLoadState::Loaded, None);
                }
                Err(err) => {
                    self.state_handle.set_module_status(
                        module_name,
                        ModuleLoadState::LoadError,
                        Some(err.to_string()),
                    );
                }
            }
        }

        Ok(())
    }

    fn load_lua_file(&self, path: &Path, module_name: &str) -> Result<()> {
        if !path.exists() {
            warn!(path = %path.display(), "lua file does not exist; skipping");
            self.state_handle.set_module_status(
                module_name.to_string(),
                ModuleLoadState::NotFound,
                None,
            );
            return Ok(());
        }

        let src = fs::read_to_string(path)?;
        self.lua.load(&src).set_name(path.to_string_lossy().as_ref()).exec()?;
        Ok(())
    }

    fn handle_event(&self, id: SubscriptionId, event: BreadEvent) -> Result<()> {
        let handlers = self.handlers.lock().expect("lua handlers mutex poisoned");
        let Some(reg) = handlers.get(&id) else {
            return Ok(());
        };
        let callback: Function = self.lua.registry_value(reg)?;
        let event_value = self.lua.to_value(&event)?;
        if let Err(err) = callback.call::<_, ()>(event_value) {
            error!(subscription = id.0, error = %err, "lua callback failed");
        }
        Ok(())
    }

    fn remove_handler(&self, id: SubscriptionId) {
        if let Ok(mut map) = self.handlers.lock() {
            map.remove(&id);
        }
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
