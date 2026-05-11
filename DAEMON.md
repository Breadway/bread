# breadd — Daemon Architecture
### The Bread Runtime Daemon

---

## Overview

`breadd` is the long-running Rust daemon at the center of Bread. It is the canonical source of truth for all desktop runtime state: what hardware is connected, what the compositor is doing, what profile is active, and what events have occurred.

Everything else in Bread — Lua modules, the CLI, profile logic, automation behavior — exists as a consumer of what `breadd` tracks and exposes. The daemon is the foundation.

`breadd` does not implement automation. It makes automation possible.

---

## Responsibilities

At a high level, `breadd` is responsible for six things:

1. **Adapter management** — spawn and supervise connections to external systems (Hyprland IPC, udev, power, network)
2. **Event ingestion** — receive raw signals from adapters and push them into the pipeline
3. **Event normalization** — transform raw signals into stable, semantic Bread events
4. **State maintenance** — keep a live, structured model of the desktop
5. **Subscription dispatch** — deliver normalized events to Lua module subscribers
6. **IPC** — expose runtime state and control to the CLI and external consumers

The daemon does not decide what to do when events occur. That is Lua's job. The daemon decides what is true about the system, and tells Lua about it.

---

## Process Model

`breadd` is a single long-running process started at login (via systemd user service or similar). It runs for the duration of the session.

```
breadd (main process)
├── Adapter threads
│   ├── HyprlandAdapter      (async task — IPC socket reader)
│   ├── UdevAdapter           (async task — netlink listener)
│   ├── PowerAdapter          (async task — sysfs / UPower watcher)
│   └── NetworkAdapter        (async task — netlink / D-Bus watcher)
├── State Engine             (async task — central coordinator)
├── Lua Runtime              (dedicated thread — Lua is not Send)
├── IPC Server               (async task — Unix socket listener)
└── Watcher                  (async task — config file watcher, optional)
```

The daemon uses Tokio as its async runtime. Most work is non-blocking and event-driven. The Lua runtime runs on a dedicated OS thread because Lua's C bindings are not `Send`-safe; it communicates with the async side through a bounded channel.

---

## Internal Event Pipeline

Every signal that enters `breadd` flows through the same pipeline before it reaches a Lua module:

```
External System
      │
      ▼
  Adapter
  (raw ingestion)
      │
      │  RawEvent
      ▼
  Normalizer
  (semantic interpretation)
      │
      │  BreadEvent
      ▼
  State Engine
  (state update + fan-out)
      │
      ├──► State Store (updated)
      │
      └──► Subscription Dispatcher
                │
                │  BreadEvent (per subscriber)
                ▼
           Lua Runtime
           (module handlers)
```

No step is skipped. Raw events never reach Lua directly. Lua never reads from sysfs or a compositor socket directly. The pipeline enforces clean separation between "what the system said" and "what it means."

---

## Core Data Structures

### RawEvent

A `RawEvent` is what an adapter produces. It is uninterpreted — it contains only what the external system reported.

```rust
pub struct RawEvent {
    pub source: AdapterSource,     // Hyprland | Udev | Power | Network
    pub kind: String,              // raw event type string from the source
    pub payload: serde_json::Value, // raw data, source-specific shape
    pub timestamp: u64,            // unix milliseconds
}
```

### BreadEvent

A `BreadEvent` is what the normalizer produces. It is stable, versioned, and typed.

```rust
pub struct BreadEvent {
    pub event: String,             // "bread.device.dock.connected"
    pub timestamp: u64,
    pub source: AdapterSource,
    pub data: serde_json::Value,   // normalized, structured payload
}
```

The `event` field follows the namespace convention `bread.<subsystem>.<noun>.<verb>`. This string is stable across Bread versions; modules can rely on it without breaking.

### RuntimeState

The `RuntimeState` is the daemon's live model of the desktop. It is updated atomically as events arrive.

```rust
pub struct RuntimeState {
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: Option<WorkspaceId>,
    pub active_window: Option<WindowId>,
    pub devices: DeviceTopology,
    pub network: NetworkState,
    pub power: PowerState,
    pub profile: ProfileState,
    pub modules: Vec<ModuleStatus>,
}
```

State is stored behind an `Arc<RwLock<RuntimeState>>`. Readers (IPC, Lua state queries) take a read lock briefly. The state engine holds the write lock only during update. Contention is minimal because updates are infrequent relative to query frequency.

---

## Adapters

Each adapter is an independent async task. It owns its connection to an external system and is responsible for reconnection if that connection is lost.

### Adapter Trait

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, tx: Sender<RawEvent>) -> Result<()>;
    async fn on_connect(&self) {}
    async fn on_disconnect(&self) {}
}
```

Each adapter runs its `run` loop indefinitely, pushing `RawEvent`s into the shared channel. Failures inside `run` trigger a reconnect cycle with exponential backoff — the adapter never terminates the daemon.

### HyprlandAdapter

Connects to Hyprland's event socket (`$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`). Reads newline-delimited event strings and forwards them as `RawEvent`s.

Handles:
- `monitoradded` / `monitorremoved`
- `workspace` / `workspacev2`
- `activewindow` / `activewindowv2`
- `openwindow` / `closewindow`
- `focusedmon`

Reconnects if the socket disappears (compositor restart). Buffers events during reconnect to avoid losing the first few signals after the compositor comes back.

### UdevAdapter

Uses `tokio-udev` to listen on a netlink socket for kernel device events. Monitors all subsystems relevant to desktop hardware:

- `usb` — docks, peripherals, hubs
- `input` — keyboards, mice, tablets
- `drm` — display connectors
- `power_supply` — batteries, chargers

Unlike most adapters, `UdevAdapter` also performs an initial enumeration on startup so the state engine has a full picture of currently-connected hardware before any hotplug events arrive.

### PowerAdapter

Reads battery and AC state from `/sys/class/power_supply/`. Polls on a configurable interval (default: 30s) and emits events on meaningful state transitions:

- AC plugged / unplugged
- Battery level crossing thresholds (20%, 10%, 5%)
- Battery full

Also subscribes to UPower over D-Bus when available for faster event delivery.

### NetworkAdapter

Monitors network interface state via netlink. Emits events when interfaces transition between up and down states, and when the system gains or loses default-route connectivity.

---

## Normalizer

The normalizer sits between the adapter channel and the state engine. It is a pure function: given a `RawEvent`, produce zero or more `BreadEvent`s.

```rust
pub trait Normalizer: Send + Sync {
    fn normalize(&self, raw: &RawEvent) -> Vec<BreadEvent>;
}
```

Each adapter source has its own normalizer implementation. Normalization is where domain knowledge lives: knowing that a udev `add` event on a `usb` device with certain vendor/product IDs means "dock connected" rather than "generic USB device."

### Device Classification

The `UdevNormalizer` maintains a device classifier that maps hardware identifiers to semantic device types:

```rust
pub enum DeviceClass {
    Dock,
    Keyboard,
    Mouse,
    Tablet,
    Display,
    Storage,
    Audio,
    Unknown,
}
```

Classification is based on udev properties (`ID_INPUT_KEYBOARD`, `ID_USB_CLASS`, subsystem, driver name). Unknown devices are classified as `Unknown` and still emit a generic `bread.device.connected` event — they are never silently dropped.

### Event Deduplication

The normalizer tracks recent events and suppresses duplicates within a configurable window (default: 100ms). This prevents rapid-fire hardware oscillation (e.g., a dock that briefly disconnects and reconnects during power negotiation) from flooding the event bus with spurious events.

---

## State Engine

The state engine is the coordinator. It receives `BreadEvent`s from the normalizer, updates `RuntimeState`, and dispatches events to subscribers.

```rust
pub struct StateEngine {
    state: Arc<RwLock<RuntimeState>>,
    subscriptions: Arc<RwLock<SubscriptionTable>>,
    lua_tx: Sender<LuaMessage>,
}
```

On each event:

1. Acquire write lock on `RuntimeState`
2. Apply the state update corresponding to the event
3. Release write lock
4. Look up matching subscriptions in `SubscriptionTable`
5. For each match, send a `LuaMessage::Event` to the Lua runtime channel

State updates are synchronous and must be fast. No I/O, no blocking, no external calls inside the update path.

### Subscription Table

The `SubscriptionTable` maps event patterns to subscriber IDs. Patterns support exact matches and wildcard suffix matching (`bread.device.*`).

```rust
pub struct SubscriptionTable {
    entries: Vec<Subscription>,
}

pub struct Subscription {
    pub id: SubscriptionId,
    pub pattern: EventPattern,
    pub once: bool,
}
```

Matching is O(n) over the subscription list. For typical module counts (tens of subscriptions), this is negligible. If subscription counts grow into the thousands, an index structure would be warranted — but that is not a V1 concern.

---

## Lua Runtime

The Lua runtime runs on a dedicated OS thread. It owns the `mlua` `Lua` instance and processes messages from the async side through a `tokio::sync::mpsc` channel.

```rust
pub enum LuaMessage {
    Event(BreadEvent),
    Reload,
    Exec(String),
    StateQuery { key: String, reply: oneshot::Sender<serde_json::Value> },
    Shutdown,
}
```

### Module Loading

On startup (and on reload), the Lua runtime:

1. Creates a fresh `Lua` instance (on reload, the old one is dropped)
2. Registers all built-in `bread.*` API functions
3. Evaluates `~/.config/bread/init.lua`
4. Resolves module dependency order
5. Loads each module in order, calling `on_load` if defined
6. Registers all `bread.on` subscriptions with the state engine

### Error Isolation

Lua errors during event handler execution are caught with `pcall`. The error message and stack trace are logged. The handler is removed from the subscription table if it is a `once` subscription; otherwise it remains registered and will be called again on the next matching event.

Errors during module load are fatal to that module but not to the daemon. The failed module is marked as `LoadError` in module state; remaining modules continue loading.

### Lua ↔ Rust Boundary

All calls across the Lua/Rust boundary go through `mlua`'s safe API. Rust functions registered as Lua globals return `mlua::Result` and handle their own error mapping. Panics inside registered functions are caught by mlua and converted to Lua errors — they do not unwind into the Lua thread.

---

## IPC

`breadd` exposes a Unix domain socket at `$XDG_RUNTIME_DIR/bread/breadd.sock`. The protocol is newline-delimited JSON.

### Request / Response

```json
{ "id": "1", "method": "state.get", "params": { "key": "monitors" } }
```

```json
{ "id": "1", "result": [ { "name": "HDMI-A-1", "resolution": "2560x1440" } ] }
```

### Methods

| Method | Description |
|--------|-------------|
| `state.get` | Read a value from RuntimeState by key path |
| `state.dump` | Return full RuntimeState as JSON |
| `events.subscribe` | Subscribe to a stream of BreadEvents (persistent connection) |
| `modules.list` | List loaded modules and their status |
| `modules.reload` | Trigger a hot reload |
| `profile.list` | List defined profiles |
| `profile.activate` | Activate a named profile |
| `emit` | Inject a synthetic BreadEvent |
| `ping` | Health check |

### Event Streaming

`events.subscribe` upgrades the connection to a streaming mode. The daemon pushes `BreadEvent` JSON objects line-by-line as they occur. The CLI's `bread events` command uses this to implement its live event stream. The connection remains open until the client disconnects.

### IPC Security

The socket is created with `0600` permissions, owned by the user. No authentication is performed — any process running as the same user can connect. This is intentional for V1 and consistent with how tools like Hyprland and sway handle their IPC sockets.

---

## Hot Reload

Hot reload is a first-class feature of `breadd`. The daemon persists; the Lua layer restarts.

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
      ├── call on_unload() on each module (reverse dependency order)
      ├── cancel all active timers and intervals
      ├── send subscription cancellations to SubscriptionTable
      ├── drop Lua instance (all state cleared)
      ├── create new Lua instance
      ├── re-register built-in API
      ├── re-load init.lua and all modules
      └── re-register subscriptions with SubscriptionTable
      │
      ▼
StateEngine: resume event dispatch
      │
      ▼
IPC: reload complete response
```

If any module fails to load during reload, the reload aborts. The previous Lua instance cannot be restored (it was dropped), so the daemon enters a degraded state: no Lua handlers active, but the daemon itself remains running and IPC-accessible. The CLI reports the error and the user can fix the Lua and reload again.

This tradeoff (no rollback on failed reload) is intentional for V1. Rollback would require snapshotting the previous Lua state before initiating reload, which adds complexity. The user experience is acceptable: a syntax error in a module gives a clear error message via `bread reload`, and the daemon stays alive.

---

## Startup Sequence

```
1. Parse config (breadd.toml or default)
2. Initialize logging (tracing subscriber)
3. Create RuntimeState (empty)
4. Create SubscriptionTable (empty)
5. Bind IPC socket
6. Spawn adapter tasks:
   a. UdevAdapter (enumerate existing devices → populate initial state)
   b. HyprlandAdapter (connect to compositor socket)
   c. PowerAdapter (read initial battery state)
   d. NetworkAdapter (read initial interface state)
7. Spawn StateEngine task
8. Spawn Lua runtime thread
9. Send Lua runtime: load init.lua
10. Lua loads modules, registers subscriptions
11. StateEngine fires bread.system.startup event
12. Daemon enters steady-state event loop
```

Step 6a (UdevAdapter enumeration) is synchronous before other adapters start. This ensures that when Lua modules first run, `bread.state.get("devices")` returns an accurate picture of what's already connected rather than an empty list.

---

## Configuration

`breadd` reads from `~/.config/bread/breadd.toml`. All values have defaults; the file is optional.

```toml
[daemon]
log_level = "info"           # trace | debug | info | warn | error
socket_path = ""             # default: $XDG_RUNTIME_DIR/bread/breadd.sock

[lua]
entry_point = "~/.config/bread/init.lua"
module_path = "~/.config/bread/modules"

[adapters.hyprland]
enabled = true
reconnect_delay_ms = 500
reconnect_max_attempts = 10

[adapters.udev]
enabled = true
subsystems = ["usb", "input", "drm", "power_supply"]

[adapters.power]
enabled = true
poll_interval_secs = 30

[adapters.network]
enabled = true

[events]
dedup_window_ms = 100        # suppress duplicate events within this window
```

---

## Observability

### Logging

`breadd` uses `tracing` for structured logging. Log level is configurable. At `debug` level, every `RawEvent` and `BreadEvent` is logged with full payloads. At `info` level, only significant lifecycle events and errors are logged.

### `bread doctor`

The `bread doctor` command queries the daemon over IPC and produces a diagnostic report:

- Daemon version and uptime
- IPC socket status
- Adapter connection status (connected / disconnected / reconnecting)
- Module load status (loaded / error / not found)
- Active subscriptions count
- Recent errors (last 10 Lua errors with stack traces)
- RuntimeState summary

### `bread events`

Streams the live `BreadEvent` log to the terminal. Supports optional pattern filtering:

```bash
bread events                          # all events
bread events --filter "bread.device.*"  # device events only
bread events --filter "bread.monitor.*" # monitor events only
```

---

## Failure Modes & Recovery

| Failure | Behavior |
|---------|----------|
| Hyprland socket unavailable | HyprlandAdapter retries with backoff; other adapters unaffected |
| Compositor restart | HyprlandAdapter detects disconnect, reconnects when socket reappears |
| Lua syntax error on reload | Reload aborts; daemon enters degraded mode; reports error via IPC |
| Lua runtime error in handler | Error caught and logged; handler remains registered; daemon continues |
| IPC client disconnect | Connection cleaned up; no effect on daemon |
| udev socket error | UdevAdapter logs error and retries; events may be missed during outage |
| Panic in Rust async task | Task restarts via supervisor; logged as critical error |

The daemon is designed to never require a full restart due to a recoverable failure. The only cases that warrant a daemon restart are: daemon binary update, unrecoverable OS-level error, or explicit user action.

---

## Summary

`breadd` is a narrow, focused daemon. It does not automate. It does not configure the compositor. It does not manage packages or provision machines.

It does one thing well: maintain a live, coherent model of the desktop runtime and deliver that model — as structured state and semantic events — to the Lua automation layer that acts on it.

Everything complex lives in Lua. Everything reliable lives in `breadd`.
