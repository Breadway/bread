//! The Lua half of `bread-module-host`: a `bread` table whose functions are
//! RPC-backed proxies to `breadd` instead of directly touching daemon state,
//! plus a dispatch loop that turns `ModuleHostPush` messages (from
//! `crate::io`) into Lua callback invocations.
//!
//! Structurally a slimmed-down sibling of `breadd/src/lua/mod.rs`'s
//! `LuaEngine`/`spawn_runtime`: one dedicated thread runs Lua synchronously
//! and reacts to messages from a channel (`HostMessage` here, `LuaMessage`
//! there); a separate thread/task owns the actual async I/O. Only ONE
//! module is ever loaded per `bread-module-host` process, so there's no
//! module registry, load ordering, or `after` dependency resolution here —
//! `breadd` already resolved all of that before deciding this module needed
//! its own process.
//!
//! `bread.module()`'s `store` is process-local (an in-memory table, not
//! synced back to `breadd`) — a documented gap vs. the in-process
//! implementation's `bread.module().store`, which persists in
//! `RuntimeState` and is visible to `bread modules info`/other modules.
//! Fine for a single module's own private scratch state; not fine yet for
//! anything that expects cross-module visibility. See `Documentation.md`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use bread_shared::{BreadEvent, ModulePermission, PermissionKind};
use mlua::{Error as LuaError, Function, Lua, LuaSerdeExt, RegistryKey, Table, Value as LuaValue};
use serde_json::{json, Value as JsonValue};
use tracing::error;

use crate::io::{call, IoCommand};

/// Timeout for a single RPC round trip to `breadd`. Generous relative to a
/// same-host Unix socket hop — this exists to fail loudly if the connection
/// wedges rather than to accommodate genuinely slow calls.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Pure-Lua `bread.spawn`/`bread.wait` sugar, copied verbatim from
/// `breadd/src/lua/mod.rs`'s `install_wait_helper`. It only depends on
/// `coroutine` plus `bread.once`/`bread.on`/`bread.after`/`bread.cancel`,
/// all of which this module provides as RPC-backed bindings above, so the
/// suspension mechanism works unmodified against a remote event source.
///
/// Deliberately duplicated rather than shared: extracting this into
/// `bread-shared` (so `breadd` and `bread-module-host` load the same
/// constant instead of two hand-kept-in-sync copies) is flagged as
/// follow-up work in `Documentation.md` — doing it here would also require
/// making `breadd`'s currently-private `const BUILTIN_*`/wait-helper
/// strings public, which is a larger refactor than this workstream's time
/// budget covers.
const WAIT_HELPER: &str = r#"
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
"#;

fn json_to_lua<'lua>(lua: &'lua Lua, value: &JsonValue) -> mlua::Result<LuaValue<'lua>> {
    Ok(match value {
        JsonValue::Null => LuaValue::Nil,
        JsonValue::Bool(b) => LuaValue::Boolean(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                LuaValue::Integer(i as i64)
            } else {
                LuaValue::Number(n.as_f64().unwrap_or(0.0))
            }
        }
        JsonValue::String(s) => LuaValue::String(lua.create_string(s)?),
        JsonValue::Array(arr) => {
            let tbl = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                tbl.set(i + 1, json_to_lua(lua, v)?)?;
            }
            LuaValue::Table(tbl)
        }
        JsonValue::Object(obj) => {
            let tbl = lua.create_table()?;
            for (k, v) in obj.iter() {
                tbl.set(k.clone(), json_to_lua(lua, v)?)?;
            }
            LuaValue::Table(tbl)
        }
    })
}

/// The Lua VM plus the bookkeeping needed to route `ModuleHostPush`
/// messages to the right registered callback. Lives entirely on one thread
/// (`mlua::Lua` is `!Send`) — see `main.rs`.
pub struct ModuleHostLua {
    lua: Lua,
    /// subscription_id or timer_id -> the Lua callback registered for it.
    handlers: Rc<RefCell<HashMap<String, RegistryKey>>>,
    registered: Rc<RefCell<bool>>,
    module_table_key: Rc<RefCell<Option<RegistryKey>>>,
    module_name: String,
}

impl ModuleHostLua {
    pub fn new(
        cmd_tx: mpsc::Sender<IoCommand>,
        module_name: String,
        permissions: Vec<ModulePermission>,
    ) -> Result<Self> {
        let lua = Lua::new();
        let bread = lua.create_table()?;
        let handlers: Rc<RefCell<HashMap<String, RegistryKey>>> = Rc::new(RefCell::new(HashMap::new()));
        let registered = Rc::new(RefCell::new(false));
        let module_table_key: Rc<RefCell<Option<RegistryKey>>> = Rc::new(RefCell::new(None));

        Self::install_module_fn(
            &lua,
            &bread,
            module_name.clone(),
            registered.clone(),
            module_table_key.clone(),
        )?;
        Self::install_logging(&lua, &bread, cmd_tx.clone())?;
        Self::install_json(&lua, &bread)?;
        Self::install_events(&lua, &bread, cmd_tx.clone(), handlers.clone())?;
        Self::install_timers(&lua, &bread, cmd_tx.clone(), handlers.clone())?;
        Self::install_emit(&lua, &bread, cmd_tx.clone())?;

        let granted: HashSet<PermissionKind> = permissions.iter().map(|p| p.kind).collect();
        Self::install_fs(&lua, &bread, cmd_tx.clone(), &granted)?;
        Self::install_exec(&lua, &bread, cmd_tx.clone(), &granted)?;
        Self::install_state(&lua, &bread, cmd_tx, &granted)?;

        lua.globals().set("bread", bread)?;
        lua.load(WAIT_HELPER).set_name("<bread-module-host wait helper>").exec()?;

        Ok(Self {
            lua,
            handlers,
            registered,
            module_table_key,
            module_name,
        })
    }

    fn install_module_fn(
        lua: &Lua,
        bread: &Table,
        expected_name: String,
        registered: Rc<RefCell<bool>>,
        module_table_key: Rc<RefCell<Option<RegistryKey>>>,
    ) -> Result<()> {
        let store: Rc<RefCell<HashMap<String, JsonValue>>> = Rc::new(RefCell::new(HashMap::new()));
        let module_fn = lua.create_function(move |lua, table: Table| -> mlua::Result<Table> {
            let name: String = table.get("name")?;
            if name != expected_name {
                return Err(LuaError::RuntimeError(format!(
                    "bread.module({{name = \"{name}\"}}) does not match the module breadd spawned this process for (\"{expected_name}\")"
                )));
            }
            let version: Option<String> = table.get("version").ok();

            let module_tbl = lua.create_table()?;
            module_tbl.set("name", name.clone())?;
            if let Some(v) = version {
                module_tbl.set("version", v)?;
            }

            let store_tbl = lua.create_table()?;
            let store_get = store.clone();
            let get_fn = lua.create_function(move |lua, key: String| {
                match store_get.borrow().get(&key) {
                    Some(v) => json_to_lua(lua, v),
                    None => Ok(LuaValue::Nil),
                }
            })?;
            store_tbl.set("get", get_fn)?;

            let store_set = store.clone();
            let set_fn = lua.create_function(move |lua, (key, value): (String, LuaValue)| {
                let json: JsonValue = lua.from_value(value).unwrap_or(JsonValue::Null);
                store_set.borrow_mut().insert(key, json);
                Ok(())
            })?;
            store_tbl.set("set", set_fn)?;
            module_tbl.set("store", store_tbl)?;

            *registered.borrow_mut() = true;
            let key = lua.create_registry_value(module_tbl.clone())?;
            *module_table_key.borrow_mut() = Some(key);

            Ok(module_tbl)
        })?;
        bread.set("module", module_fn)?;
        Ok(())
    }

    fn install_logging(lua: &Lua, bread: &Table, cmd_tx: mpsc::Sender<IoCommand>) -> Result<()> {
        for (name, method) in [
            ("log", "module_host.log"),
            ("warn", "module_host.warn"),
            ("error", "module_host.error"),
        ] {
            let cmd_tx = cmd_tx.clone();
            let f = lua.create_function(move |_, message: String| {
                let _ = call(&cmd_tx, method, json!({ "message": message }), RPC_TIMEOUT);
                Ok(())
            })?;
            bread.set(name, f)?;
        }
        Ok(())
    }

    fn install_json(lua: &Lua, bread: &Table) -> Result<()> {
        let json_tbl = lua.create_table()?;
        let decode_fn = lua.create_function(|lua, s: String| {
            match serde_json::from_str::<JsonValue>(&s) {
                Ok(v) => Ok((json_to_lua(lua, &v)?, LuaValue::Nil)),
                Err(e) => Ok((LuaValue::Nil, LuaValue::String(lua.create_string(&e.to_string())?))),
            }
        })?;
        json_tbl.set("decode", decode_fn)?;
        bread.set("json", json_tbl)?;
        Ok(())
    }

    fn install_events(
        lua: &Lua,
        bread: &Table,
        cmd_tx: mpsc::Sender<IoCommand>,
        handlers: Rc<RefCell<HashMap<String, RegistryKey>>>,
    ) -> Result<()> {
        for (name, once) in [("on", false), ("once", true)] {
            let cmd_tx = cmd_tx.clone();
            let handlers = handlers.clone();
            let method = if once { "module_host.once" } else { "module_host.on" };
            let f = lua.create_function(move |lua, (pattern, callback): (String, Function)| {
                let result = call(&cmd_tx, method, json!({ "pattern": pattern }), RPC_TIMEOUT)
                    .map_err(LuaError::external)?;
                let id = result
                    .get("subscription_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| LuaError::external("module_host.on: missing subscription_id"))?
                    .to_string();
                let key = lua.create_registry_value(callback)?;
                handlers.borrow_mut().insert(id.clone(), key);
                Ok(id)
            })?;
            bread.set(name, f)?;
        }

        let cmd_tx_off = cmd_tx.clone();
        let handlers_off = handlers.clone();
        let off_fn = lua.create_function(move |_, id: String| {
            let _ = call(&cmd_tx_off, "module_host.off", json!({ "id": id }), RPC_TIMEOUT);
            handlers_off.borrow_mut().remove(&id);
            Ok(())
        })?;
        bread.set("off", off_fn)?;
        Ok(())
    }

    fn install_timers(
        lua: &Lua,
        bread: &Table,
        cmd_tx: mpsc::Sender<IoCommand>,
        handlers: Rc<RefCell<HashMap<String, RegistryKey>>>,
    ) -> Result<()> {
        for (name, method, param_key) in [
            ("after", "module_host.after", "delay_ms"),
            ("every", "module_host.every", "interval_ms"),
        ] {
            let cmd_tx = cmd_tx.clone();
            let handlers = handlers.clone();
            let f = lua.create_function(move |lua, (delay_ms, callback): (u64, Function)| {
                let result = call(&cmd_tx, method, json!({ param_key: delay_ms }), RPC_TIMEOUT)
                    .map_err(LuaError::external)?;
                let id = result
                    .get("timer_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| LuaError::external(format!("{method}: missing timer_id")))?
                    .to_string();
                let key = lua.create_registry_value(callback)?;
                handlers.borrow_mut().insert(id.clone(), key);
                Ok(id)
            })?;
            bread.set(name, f)?;
        }

        let cmd_tx_cancel = cmd_tx.clone();
        let handlers_cancel = handlers.clone();
        let cancel_fn = lua.create_function(move |_, id: String| {
            let _ = call(&cmd_tx_cancel, "module_host.cancel", json!({ "id": id }), RPC_TIMEOUT);
            handlers_cancel.borrow_mut().remove(&id);
            Ok(())
        })?;
        bread.set("cancel", cancel_fn)?;
        Ok(())
    }

    fn install_emit(lua: &Lua, bread: &Table, cmd_tx: mpsc::Sender<IoCommand>) -> Result<()> {
        let emit_fn = lua.create_function(move |lua, (event, data): (String, Option<LuaValue>)| {
            let data_json: JsonValue = match data {
                Some(v) => lua.from_value(v).unwrap_or(JsonValue::Null),
                None => json!({}),
            };
            call(
                &cmd_tx,
                "module_host.emit",
                json!({ "event": event, "data": data_json }),
                RPC_TIMEOUT,
            )
            .map(|_| ())
            .map_err(LuaError::external)
        })?;
        bread.set("emit", emit_fn)?;
        Ok(())
    }

    /// `bread.state.get(path)`, gated on `state.read`. Only the `get`
    /// shorthand is bridged here — `.monitors()`/`.active_workspace()`/etc.
    /// convenience wrappers and `state.watch` (a standing subscription, a
    /// materially different capability — see `PermissionKind::StateWatch`'s
    /// doc comment in `bread-shared`) are deferred; see `Documentation.md`'s
    /// Workstream G section for the full list of what's bridged vs. not.
    fn install_state(
        lua: &Lua,
        bread: &Table,
        cmd_tx: mpsc::Sender<IoCommand>,
        granted: &HashSet<PermissionKind>,
    ) -> Result<()> {
        if !granted.contains(&PermissionKind::StateRead) {
            return Ok(());
        }
        let state_tbl = lua.create_table()?;
        let get_fn = lua.create_function(move |lua, key: String| {
            let result = call(&cmd_tx, "module_host.state_get", json!({ "key": key }), RPC_TIMEOUT)
                .map_err(LuaError::external)?;
            match result.get("value") {
                Some(v) => json_to_lua(lua, v),
                None => Ok(LuaValue::Nil),
            }
        })?;
        state_tbl.set("get", get_fn)?;
        bread.set("state", state_tbl)?;
        Ok(())
    }

    fn install_fs(
        lua: &Lua,
        bread: &Table,
        cmd_tx: mpsc::Sender<IoCommand>,
        granted: &HashSet<PermissionKind>,
    ) -> Result<()> {
        if !granted.contains(&PermissionKind::FsRead) && !granted.contains(&PermissionKind::FsWrite) {
            return Ok(());
        }
        let fs_tbl = lua.create_table()?;
        if granted.contains(&PermissionKind::FsRead) {
            let cmd_tx = cmd_tx.clone();
            let read_fn = lua.create_function(move |_, path: String| {
                let result = call(&cmd_tx, "module_host.fs_read", json!({ "path": path }), RPC_TIMEOUT)
                    .map_err(LuaError::external)?;
                Ok(result
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()))
            })?;
            fs_tbl.set("read", read_fn)?;
        }
        if granted.contains(&PermissionKind::FsWrite) {
            let cmd_tx = cmd_tx.clone();
            let write_fn = lua.create_function(move |_, (path, content): (String, String)| {
                call(
                    &cmd_tx,
                    "module_host.fs_write",
                    json!({ "path": path, "content": content }),
                    RPC_TIMEOUT,
                )
                .map(|_| ())
                .map_err(LuaError::external)
            })?;
            fs_tbl.set("write", write_fn)?;
        }
        bread.set("fs", fs_tbl)?;
        Ok(())
    }

    fn install_exec(
        lua: &Lua,
        bread: &Table,
        cmd_tx: mpsc::Sender<IoCommand>,
        granted: &HashSet<PermissionKind>,
    ) -> Result<()> {
        if !granted.contains(&PermissionKind::Exec) {
            return Ok(());
        }
        let cmd_tx_exec = cmd_tx.clone();
        let exec_fn = lua.create_function(move |_, cmd: String| {
            call(&cmd_tx_exec, "module_host.exec", json!({ "cmd": cmd }), RPC_TIMEOUT)
                .map(|_| ())
                .map_err(LuaError::external)
        })?;
        bread.set("exec", exec_fn)?;

        let exec_capture_fn = lua.create_function(move |_, (cmd, opts): (String, Option<Table>)| {
            let timeout_ms: u64 = opts
                .as_ref()
                .and_then(|o| o.get("timeout_ms").ok())
                .unwrap_or(2000);
            let call_timeout = RPC_TIMEOUT + Duration::from_millis(timeout_ms);
            let result = call(
                &cmd_tx,
                "module_host.exec_capture",
                json!({ "cmd": cmd, "timeout_ms": timeout_ms }),
                call_timeout,
            )
            .map_err(LuaError::external)?;
            let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let stdout = result
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok((ok, stdout))
        })?;
        bread.set("exec_capture", exec_capture_fn)?;
        Ok(())
    }

    /// Load and execute the module's `init.lua`, then verify it actually
    /// called `bread.module(...)` — mirrors `breadd`'s own
    /// `load_module`/`load_scoped_lua_file` contract exactly (see
    /// `breadd/src/lua/mod.rs`).
    pub fn load_entry(&self, entry_path: &std::path::Path) -> Result<()> {
        let src = std::fs::read_to_string(entry_path)
            .map_err(|e| anyhow!("failed to read {}: {e}", entry_path.display()))?;
        self.lua
            .load(&src)
            .set_name(entry_path.to_string_lossy().as_ref())
            .exec()
            .map_err(|e| anyhow!(e.to_string()))?;

        if !*self.registered.borrow() {
            return Err(anyhow!("module did not call bread.module(...)"));
        }
        self.run_on_load()
    }

    fn run_on_load(&self) -> Result<()> {
        let key_ref = self.module_table_key.borrow();
        let Some(key) = key_ref.as_ref() else {
            return Ok(());
        };
        let module_tbl: Table = self
            .lua
            .registry_value(key)
            .map_err(|e| anyhow!(e.to_string()))?;
        let hook: Option<Function> = module_tbl.get("on_load").ok();
        drop(key_ref);
        if let Some(hook) = hook {
            hook.call::<_, ()>(())
                .map_err(|e| anyhow!("{} on_load failed: {e}", self.module_name))?;
        }
        Ok(())
    }

    pub fn dispatch_event(&self, subscription_id: &str, event: &BreadEvent) {
        let func = self.lookup(subscription_id);
        if let Some(func) = func {
            if let Err(e) = self.call_event_handler(&func, event) {
                error!(subscription_id, error = %e, "module-host: event handler error");
            }
        }
    }

    pub fn dispatch_timer(&self, timer_id: &str) {
        let func = self.lookup(timer_id);
        if let Some(func) = func {
            if let Err(e) = func.call::<_, ()>(()) {
                error!(timer_id, error = %e, "module-host: timer handler error");
            }
        }
    }

    fn lookup(&self, id: &str) -> Option<Function<'_>> {
        let handlers = self.handlers.borrow();
        let key = handlers.get(id)?;
        self.lua.registry_value::<Function>(key).ok()
    }

    fn call_event_handler(&self, func: &Function, event: &BreadEvent) -> mlua::Result<()> {
        let data = json_to_lua(&self.lua, &event.data)?;
        let evt_tbl = self.lua.create_table()?;
        evt_tbl.set("event", event.event.clone())?;
        evt_tbl.set("data", data)?;
        evt_tbl.set("timestamp", event.timestamp)?;
        evt_tbl.set("id", event.id.clone())?;
        if let Some(caused_by) = &event.caused_by {
            evt_tbl.set("caused_by", caused_by.clone())?;
        }
        func.call::<_, ()>(evt_tbl)
    }
}
