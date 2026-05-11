# bread — Lua Runtime Architecture
### The Bread Scripting and Automation Layer

---

## Overview

The Lua runtime is the automation half of Bread. Where `breadd` maintains truth about the desktop, the Lua layer decides what to do about it.

Modules written in Lua subscribe to events, read state, execute shell commands, and activate profiles. The entire scripting surface is exposed through a single `bread.*` global API — stable, versioned, and designed to be hostile to accidents.

The runtime lives on a dedicated OS thread inside `breadd`. It is reachable from the async side only through a bounded message channel. Lua never touches sockets, sysfs, or compositor IPC directly. Everything flows through the daemon.

---

## Phase 1 — Runtime Core

These capabilities exist in the codebase today. Phase 1 is the foundation the Lua runtime is built on.

### Daemon Stability

`breadd` is a single long-running Rust process. It survives compositor restarts, module load errors, and Lua runtime panics. The daemon never terminates because a Lua file has a syntax error.

The Lua runtime thread is spawned once at startup:

```rust
std::thread::Builder::new()
    .name("breadd-lua".to_string())
    .spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create lua runtime thread");

        rt.block_on(async move {
            let mut engine = LuaEngine::new(config, state_handle, emit_tx)?;
            engine.reload_internal()?;

            while let Some(msg) = rx.recv().await {
                match msg { /* ... */ }
            }
        });
    })?;
```

If the initial module load fails, the daemon enters degraded mode: no Lua handlers are active, but the daemon itself remains alive. IPC stays responsive and `bread reload` can be used to recover after the user fixes their config.

### Event Ingestion

Every signal that enters `breadd` from an external system flows through a strict pipeline before it reaches Lua:

```
External System (Hyprland / udev / power / network)
      │
      ▼
  Adapter          — raw ingestion, owns the connection
      │  RawEvent
      ▼
  Normalizer       — semantic interpretation
      │  BreadEvent
      ▼
  State Engine     — state update + fan-out
      │
      ├──► RuntimeState (updated atomically)
      └──► Subscription Dispatcher
                │  BreadEvent (per subscriber)
                ▼
           Lua Runtime
           (module handlers)
```

Raw events never reach Lua directly. Lua never observes a `RawEvent` — it only ever sees a normalized `BreadEvent` with a stable namespace string like `bread.device.dock.connected`.

### Subscriptions

Modules subscribe to events by pattern. The subscription table maps pattern strings to `(SubscriptionId, is_once)` pairs. The state engine evaluates each incoming `BreadEvent` against the table and dispatches to every matching subscriber.

```rust
pub struct SubscriptionId(pub u64);
```

The Lua side registers subscriptions via `bread.on` and `bread.once`. Each call allocates a monotonically increasing `SubscriptionId`, stores the callback in the Lua registry, and registers the pattern with the state engine:

```lua
bread.on("bread.device.dock.*", function(event)
    bread.exec("~/.config/bread/scripts/dock.sh")
end)

bread.once("bread.system.startup", function(event)
    bread.profile.activate("default")
end)
```

`bread.once` subscriptions are automatically cancelled after first delivery. The handler is removed from both the Lua registry and the subscription table.

### IPC

`breadd` exposes a Unix domain socket at `$XDG_RUNTIME_DIR/bread/breadd.sock`. The protocol is newline-delimited JSON. All IPC requests that affect the Lua runtime route through the `LuaMessage` channel — IPC never touches the Lua thread directly.

Relevant IPC methods:

| Method | Description |
|--------|-------------|
| `modules.list` | List loaded modules and their status |
| `modules.reload` | Trigger a hot reload of the Lua layer |
| `emit` | Inject a synthetic `BreadEvent` into the pipeline |
| `state.get` | Read a value from `RuntimeState` by key path |
| `state.dump` | Return the full `RuntimeState` as JSON |

The `emit` method is particularly useful for testing: it allows injecting arbitrary `BreadEvent`s without needing the real hardware event that would normally produce them.

### Hot Reload

Hot reload is a first-class feature. The daemon persists; the Lua layer restarts. No process restart required.

Reload sequence:

```
bread reload (CLI)
      │
      ▼
IPC: modules.reload
      │
      ▼
StateEngine: pause event dispatch to Lua
      │
      ▼
LuaRuntime: receive Reload message
      │
      ├── cancel all active subscriptions
      ├── clear handler registry
      ├── drop Lua instance (all state cleared)
      ├── create fresh Lua instance
      ├── re-register bread.* API
      ├── re-evaluate init.lua and all modules
      └── re-register subscriptions with SubscriptionTable
      │
      ▼
StateEngine: resume event dispatch
      │
      ▼
IPC: reload complete response
```

If any module fails to load during reload, the reload aborts and the daemon enters degraded mode. There is no rollback — the previous Lua state was dropped before the reload began. This is intentional for V1. A syntax error in a module produces a clear error message from `bread reload`, and the daemon stays alive.

### State Registry

The daemon maintains a live `RuntimeState` behind an `Arc<RwLock<RuntimeState>>`. It is the authoritative record of what is true about the desktop right now.

```rust
pub struct RuntimeState {
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: Option<String>,
    pub active_window: Option<String>,
    pub devices: DeviceTopology,
    pub network: NetworkState,
    pub power: PowerState,
    pub profile: ProfileState,
    pub modules: Vec<ModuleStatus>,
}
```

Lua accesses this via `bread.state.get(path)`. The call takes a brief read lock, serializes the requested subtree to JSON, and converts it to a Lua value. Lua never holds the lock — the lock is dropped before control returns to the Lua callback:

```lua
local monitors = bread.state.get("monitors")
local power    = bread.state.get("power")
local active   = bread.state.get("active_workspace")
```

Dotted paths are supported for nested access:

```lua
local online = bread.state.get("network.online")
```

State is read-only from Lua. Lua cannot write to `RuntimeState` directly — it can only influence state indirectly by activating a profile or emitting an event that the state engine processes.

---

## Phase 2 — Lua Runtime

Phase 2 covers what is not yet built: the features required to make the Lua layer a complete, ergonomic automation platform.

### Module Loader

Currently, `breadd` loads modules by scanning `~/.config/bread/modules/` and executing every `.lua` file in sorted order. There is no concept of module identity, exports, or dependency declarations.

Phase 2 introduces a proper module system:

```
~/.config/bread/
├── init.lua             ← entry point; declares module list
└── modules/
    ├── dock.lua
    ├── display.lua
    ├── power.lua
    └── lib/
        └── utils.lua    ← shared library, loaded on require
```

**`bread.module` declaration** — each module declares itself at the top of the file:

```lua
local M = bread.module({
    name    = "dock",
    version = "1.0.0",
    after   = { "display" },   -- load after display.lua
})
```

The runtime resolves the dependency graph and loads modules in topological order. Circular dependencies are detected at load time and reported as a load error on the offending module.

**`require` support** — modules in `lib/` are loadable via `require`:

```lua
local utils = require("bread.lib.utils")
```

The module loader intercepts `require` calls that begin with `bread.` and resolves them relative to `~/.config/bread/`. Standard Lua `require` semantics apply for everything else.

**Module status tracking** — each module's load state is reflected in `RuntimeState.modules` and visible via `bread doctor` and `modules.list`:

```rust
pub enum ModuleLoadState {
    Loaded,
    LoadError,
    NotFound,
}
```

Phase 2 extends this with `Degraded` (loaded but encountered a runtime error since last reload) and `Disabled` (explicitly disabled in config).

### Lifecycle Hooks

Currently, modules have no way to run code at load time or cleanup code at unload time. Phase 2 adds four lifecycle hooks.

```lua
function M.on_load()
    -- called once when the module is first loaded
    -- register subscriptions, initialize module state
end

function M.on_reload()
    -- called after a hot reload completes
    -- re-apply any external side effects the module manages
end

function M.on_unload()
    -- called before the Lua instance is dropped
    -- cancel external resources, write state if needed
end

function M.on_error(err)
    -- called when a subscription handler in this module throws
    -- return true to keep the subscription, false to cancel it
end
```

The runtime calls hooks in a defined order:

- **Load**: `on_load` is called after the module file executes successfully, in dependency order.
- **Reload**: `on_unload` is called in reverse dependency order. After the new Lua instance is ready, `on_load` runs on every module. `on_reload` runs after all `on_load` calls complete.
- **Error**: `on_error` is called on the Lua thread immediately after a handler throws. If the module does not define `on_error`, the default behavior is to log the error and keep the subscription alive.

All hooks are optional. A module with no lifecycle hooks continues to work exactly as it does today.

### Event APIs

Phase 2 expands the event surface available to Lua modules.

**Pattern syntax** — the current subscription API matches event names against patterns using glob-style `*` wildcards. Phase 2 adds `**` for recursive matching and `?` for single-character wildcards:

```lua
bread.on("bread.device.*",    handler)  -- matches bread.device.dock.connected
bread.on("bread.device.**",   handler)  -- matches any depth under bread.device
bread.on("bread.monitor.?",   handler)  -- single-segment wildcard
```

**`bread.off`** — cancel a subscription by the ID returned from `bread.on`:

```lua
local id = bread.on("bread.power.*", handler)
-- later:
bread.off(id)
```

**`bread.wait`** — yield until a matching event arrives, with an optional timeout:

```lua
local event = bread.wait("bread.device.dock.connected", { timeout = 5000 })
if event then
    -- dock arrived within 5 seconds
end
```

`bread.wait` is syntactic sugar over a `bread.once` subscription combined with a coroutine yield. It can only be used inside a coroutine context; calling it from a top-level module body is a load error.

**`bread.filter`** — attach a predicate to a subscription. The handler is only called when the predicate returns true:

```lua
bread.on("bread.device.*", handler, {
    filter = function(event)
        return event.data.class == "dock"
    end
})
```

**Timers** — schedule callbacks without relying on an external timer process:

```lua
local id = bread.after(500, function()
    -- called once, 500ms from now
end)

local id = bread.every(30000, function()
    -- called every 30 seconds
end)

bread.cancel(id)  -- cancel either kind
```

Timers are cancelled automatically on reload. A module does not need to track its own timer IDs for cleanup.

### State Access

Phase 2 extends `bread.state` from a read-only snapshot query into a richer interface.

**Typed helpers** — convenience wrappers for the most common state subtrees:

```lua
bread.state.monitors()           -- Vec<Monitor>
bread.state.active_workspace()   -- string | nil
bread.state.active_window()      -- string | nil
bread.state.devices()            -- Vec<Device>
bread.state.power()              -- PowerState
bread.state.network()            -- NetworkState
bread.state.profile()            -- ProfileState
```

These are thin wrappers over `bread.state.get` — they add no locking overhead.

**Reactive state** — watch a state path for changes and receive a callback when it changes:

```lua
bread.state.watch("power.ac_connected", function(new_val, old_val)
    if new_val then
        bread.exec("notify-send 'AC connected'")
    end
end)
```

State watches are implemented as synthetic subscriptions: the state engine compares the watched path before and after each `RuntimeState` update and synthesizes a `bread.state.changed.<path>` event when a difference is detected. From the Lua runtime's perspective, watches are ordinary subscriptions.

**Module-scoped storage** — a key-value store persisted across reloads (but not across daemon restarts):

```lua
M.store.set("last_profile", "docked")
local p = M.store.get("last_profile")  -- "docked"
```

Storage is scoped per module. A module cannot read another module's store. The store is backed by a `HashMap<String, serde_json::Value>` in the `RuntimeState.modules` entry for that module, so it survives hot reload.

### Hyprland Bindings

Phase 2 exposes a `bread.hyprland` namespace for direct interaction with the Hyprland compositor. This is the only place in the Lua API that is compositor-specific; all other APIs are compositor-agnostic.

The bindings communicate over Hyprland's IPC request socket (`$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`), not the event socket. Calls are dispatched to a Tokio task on the async side and awaited transparently from Lua via coroutine suspension.

**Dispatch**

```lua
bread.hyprland.dispatch("workspace", "2")
bread.hyprland.dispatch("movetoworkspace", "2,address:0x...")
bread.hyprland.dispatch("exec", "kitty")
```

**Keyword**

```lua
local result = bread.hyprland.keyword("monitor", "HDMI-A-1, 2560x1440, 0x0, 1")
```

**Active window**

```lua
local win = bread.hyprland.active_window()
-- { address, title, class, workspace, monitor, ... }
```

**Monitor and workspace queries**

```lua
local monitors   = bread.hyprland.monitors()
local workspaces = bread.hyprland.workspaces()
local clients    = bread.hyprland.clients()
```

All calls return deserialized Lua tables matching Hyprland's JSON response shape. Errors from the compositor (malformed dispatch, unknown keyword) are surfaced as Lua errors catchable with `pcall`.

**Hyprland-specific events** — the existing `bread.monitor.*` and `bread.workspace.*` event namespaces already cover the most common Hyprland signals. The Phase 2 bindings add lower-level passthrough for events that do not yet have a normalized `BreadEvent` representation:

```lua
bread.hyprland.on_raw("activewindow", function(raw)
    -- raw is the unparsed string from Hyprland's event socket
end)
```

Raw subscriptions bypass normalization. They are intended for power users and for features not yet covered by the normalized event namespace. Once a raw event pattern is common enough, it graduates to a stable `BreadEvent` and the raw subscription is deprecated.

---

## Lua ↔ Rust Boundary

All calls across the boundary go through `mlua`'s safe API. Rust functions registered as Lua globals return `mlua::Result` and handle their own error mapping. Panics inside registered Rust functions are caught by mlua and converted to Lua errors — they do not unwind into the Lua thread and they do not crash the daemon.

The `LuaMessage` enum is the only channel between the async Tokio runtime and the Lua thread:

```rust
pub enum LuaMessage {
    Event {
        subscription_id: SubscriptionId,
        event: BreadEvent,
    },
    SubscriptionCancelled {
        id: SubscriptionId,
    },
    Reload {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}
```

Lua is not `Send`. The `LuaEngine` and the `Lua` instance live exclusively on the dedicated Lua OS thread. The async side communicates only by sending `LuaMessage` values through the channel — it never holds a reference to anything inside the Lua VM.

---

## Error Isolation

### Handler errors

Lua errors during event handler execution are caught with `pcall` at the Rust boundary:

```rust
fn handle_event(&self, id: SubscriptionId, event: BreadEvent) -> Result<()> {
    let callback: Function = self.lua.registry_value(reg)?;
    let event_value = self.lua.to_value(&event)?;
    if let Err(err) = callback.call::<_, ()>(event_value) {
        error!(subscription = id.0, error = %err, "lua callback failed");
    }
    Ok(())
}
```

The error is logged with the subscription ID and full Lua stack trace. The handler remains registered and will fire again on the next matching event. A persistently failing handler is the module's responsibility to cancel via `bread.off`.

Phase 2's `on_error` hook gives modules a structured way to respond to handler failures rather than relying solely on the daemon log.

### Module load errors

Errors during module load are fatal to that module but not to the daemon or to other modules. The failed module is marked `LoadError` in `RuntimeState.modules`. Remaining modules continue loading in dependency order; only modules that declared `after` the failed module are also skipped (their dependency is broken).

### Degraded mode

If the initial load or a hot reload fails such that no Lua instance is running, the daemon enters degraded mode:

- No Lua handlers are active.
- IPC remains fully operational.
- `bread reload` can be retried after the user fixes their config.
- `bread doctor` reports the load error with the full stack trace.

The daemon never requires a full restart to recover from a Lua error.

---

## `bread.*` API Surface Summary

### Phase 1 (implemented)

| Function | Description |
|----------|-------------|
| `bread.on(pattern, fn)` | Subscribe to a pattern; returns subscription ID |
| `bread.once(pattern, fn)` | Subscribe once; auto-cancelled after first delivery |
| `bread.emit(event, payload)` | Inject a synthetic `BreadEvent` |
| `bread.exec(cmd)` | Fire-and-forget shell command |
| `bread.state.get(path)` | Read a value from `RuntimeState` by dotted path |
| `bread.profile.activate(name)` | Activate a named profile |

### Phase 2 (planned)

| Function | Description |
|----------|-------------|
| `bread.off(id)` | Cancel a subscription by ID |
| `bread.wait(pattern, opts)` | Yield until a matching event arrives |
| `bread.filter(pattern, fn, opts)` | Subscribe with a predicate guard |
| `bread.after(ms, fn)` | One-shot timer |
| `bread.every(ms, fn)` | Repeating timer |
| `bread.cancel(id)` | Cancel a timer |
| `bread.state.watch(path, fn)` | React to state changes at a path |
| `bread.state.monitors()` | Typed shorthand for `bread.state.get("monitors")` |
| `bread.state.power()` | Typed shorthand for `bread.state.get("power")` |
| `bread.state.network()` | Typed shorthand for `bread.state.get("network")` |
| `bread.hyprland.dispatch(cmd, args)` | Send a Hyprland dispatch |
| `bread.hyprland.keyword(key, val)` | Set a Hyprland keyword |
| `bread.hyprland.active_window()` | Query the active window |
| `bread.hyprland.monitors()` | Query all monitors |
| `bread.hyprland.workspaces()` | Query all workspaces |
| `bread.hyprland.clients()` | Query all open clients |
| `bread.hyprland.on_raw(event, fn)` | Subscribe to a raw Hyprland event string |
| `bread.module(decl)` | Declare a module with name, version, and dependencies |
| `M.store.get(key)` | Read from module-scoped persistent storage |
| `M.store.set(key, val)` | Write to module-scoped persistent storage |

---

## Summary

The Lua runtime is where Bread becomes useful. The daemon provides a reliable, normalized view of the desktop; the Lua layer acts on it.

Phase 1 delivers the mechanical minimum: a stable thread, a working `bread.*` API, event subscriptions, state access, hot reload, and IPC. That foundation is in the codebase today.

Phase 2 builds the ergonomics: module identity, lifecycle hooks, reactive state, timers, richer event APIs, and Hyprland control bindings. Each Phase 2 feature is additive — nothing in Phase 1 needs to change to support it.

The boundary between Rust and Lua is intentionally narrow. The daemon knows nothing about what modules do. Modules know nothing about how events arrive. The `bread.*` API is the entire contract between them.
