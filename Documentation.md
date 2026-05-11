# Bread Documentation

## Contents

- [Overview](#overview)
- [Getting started](#getting-started)
- [Your first module](#your-first-module)
- [Run, reload, and watch](#run-reload-and-watch)
- [Debugging tips](#debugging-tips)
- [Dictionary: Lua API](#dictionary-lua-api)
- [Dictionary: Built-in modules](#dictionary-built-in-modules)
- [Dictionary: Event reference](#dictionary-event-reference)
- [Dictionary: Runtime state schema](#dictionary-runtime-state-schema)
- [Dictionary: IPC protocol](#dictionary-ipc-protocol)

## Overview

Bread is a reactive automation fabric for Linux desktops. The daemon (`breadd`) normalizes external signals into semantic events, maintains runtime state, and dispatches events to Lua modules that implement automation.

- Daemon: long-running Rust process, source of truth for runtime state
- Lua runtime: dedicated thread inside the daemon; automation logic lives here
- CLI: talks to the daemon over a Unix socket

If you are new to Bread, start with the quick walkthrough below, then jump to the full dictionary when you need exact API details.

## Getting started

### 1) Create a minimal config

- Daemon config: `~/.config/bread/breadd.toml`
- Lua entry point: `~/.config/bread/init.lua`
- Lua modules: `~/.config/bread/modules/`

### 2) Minimal `init.lua`

```lua
require("modules.devices")
require("modules.workspaces")

bread.on("bread.system.startup", function()
    bread.profile.activate("default")
end)
```

### 3) Add your first module

Create a Lua file under your modules directory and load it from `init.lua`.

## Your first module

```lua
local M = bread.module({ name = "hello", version = "0.1.0" })

function M.on_load()
    bread.log("hello from bread")

    bread.on("bread.device.*", function(event)
        bread.log(event.event)
    end)
end

return M
```

Why this shape?

- Every module must call `bread.module` once.
- `on_load` is a good place to register subscriptions.
- Use `bread.log` early to verify handlers are firing.

## Run, reload, and watch

- Start the daemon, then use `bread reload` after editing Lua.
- `bread reload --watch` will keep reloading on changes.
- See [Examples.md](Examples.md) for real-world ports.

## Debugging tips

- Log event payloads with `bread.log(event.data.raw)` when matching devices.
- Use `bread.events` in the CLI to see live normalized events.
- Use `bread state` to see runtime state as JSON.

## Lua module system

### Entry point and module scanning

- `init.lua` is executed first.
- Modules are discovered by scanning `~/.config/bread/modules/` for `.lua` files.
- Every module must call `bread.module` exactly once at top-level.
- Modules are ordered by the `after` dependency list.

### Module declaration

```lua
local M = bread.module({
    name = "my.module",
    version = "0.1.0",
    after = { "bread.devices" },
})

return M
```

If a module does not call `bread.module`, it fails to load and is marked as a load error.

### Require loader

`require("bread.<path>")` resolves to a Lua file under the module path. For example:

```lua
local utils = require("bread.lib.utils")
```

This loads `~/.config/bread/modules/lib/utils.lua` if it exists. Non-`bread.*` `require` calls fall back to standard Lua behavior.

### Lifecycle hooks

Modules may export any of the following hooks. All are optional.

```lua
function M.on_load()
    -- register subscriptions, initialize module state
end

function M.on_reload()
    -- called after a hot reload completes
end

function M.on_unload()
    -- called before the Lua instance is dropped
end

function M.on_error(err)
    -- called when a handler throws
    -- return true to keep the subscription, false to cancel it
    return true
end
```

### Module storage

Each module has a scoped key-value store that survives reloads:

```lua
M.store.set("last_profile", "docked")
local value = M.store.get("last_profile")
```

The store lives in the daemon runtime state and is not shared across modules.

## Dictionary: Lua API

Every API is exposed through the `bread` global table.

### Events

#### `bread.on(pattern, fn) -> id`
Subscribe to matching events. Returns a numeric subscription ID.

#### `bread.once(pattern, fn) -> id`
Subscribe once. The handler is removed after the first match.

#### `bread.filter(pattern, fn, opts) -> id`
Subscribe with a predicate filter. `opts` must contain `filter`:

```lua
bread.filter("bread.device.*", function(event)
    bread.exec("xset r rate 200 40")
end, {
    filter = function(event)
        return event.data and event.data.class == "keyboard"
    end,
})
```

#### `bread.off(id)`
Unsubscribe an event or state watch by ID.

#### `bread.emit(event, data)`
Emit a custom event into the system pipeline.

#### `bread.wait(pattern, opts) -> event | nil`
Coroutine-only helper that waits for a matching event.

```lua
bread.spawn(function()
    local event = bread.wait("bread.device.dock.connected", { timeout = 5000 })
    if event then
        bread.log("dock arrived")
    end
end)
```

#### `bread.spawn(fn)`
Spawn a coroutine and surface errors if the coroutine fails.

### State

#### `bread.state.get(path)`
Read a state subtree by dotted path (e.g. `"network.online"`).

#### Convenience helpers

- `bread.state.monitors()`
- `bread.state.active_workspace()`
- `bread.state.active_window()`
- `bread.state.devices()`
- `bread.state.power()`
- `bread.state.network()`
- `bread.state.profile()`

#### `bread.state.watch(path, fn) -> id`
Watch a state path. The callback receives `(new, old)`.

```lua
bread.state.watch("power.ac_connected", function(new_val, old_val)
    if new_val then
        bread.exec("notify-send 'AC connected'")
    end
end)
```

### Profiles

#### `bread.profile.activate(name)`
Update the active profile. The CLI also emits `bread.profile.activated` over IPC; the Lua API does not emit this event on its own.

### Execution

#### `bread.exec(cmd)`
Runs `cmd` in a `sh -lc` shell. Fire-and-forget (async).

### Notifications

#### `bread.notify(message, opts)`
Sends a desktop notification via `notify-send`.

Options:

- `title` (string, default: `"bread"`)
- `urgency` (string, default from config)
- `timeout` (ms, default from config)
- `icon` (string, optional)

Calling `bread.notify` emits `bread.notify.sent` with `{ title, message, urgency }`.

### Timers

#### `bread.after(delay_ms, fn) -> id`
Run once after delay.

#### `bread.every(interval_ms, fn) -> id`
Run repeatedly on an interval.

#### `bread.cancel(id)`
Cancel a timer created by `after` or `every`.

### Utilities

#### `bread.debounce(delay_ms, fn) -> wrapped_fn`
Returns a wrapper that only fires after quiet time.

#### `bread.log(msg)` / `bread.warn(msg)` / `bread.error(msg)`
Log helpers that accept any Lua value.

### Hyprland

The `bread.hyprland` namespace provides compositor bindings:

- `bread.hyprland.dispatch(cmd, args)`
- `bread.hyprland.keyword(key, value)`
- `bread.hyprland.active_window()`
- `bread.hyprland.monitors()`
- `bread.hyprland.workspaces()`
- `bread.hyprland.clients()`
- `bread.hyprland.on_raw(kind, fn) -> id`

`bread.hyprland.on_raw` filters raw Hyprland events by `kind` and delivers the full event payload (including the original raw string).

## Dictionary: Built-in modules

Built-ins are enabled by default. Disable them via `[modules].disable` in the config.

### `bread.monitors`

```lua
local monitors = require("bread.monitors")

monitors.layout("dock", function()
    bread.exec("~/.config/bread/scripts/layout-dock.sh")
end)

monitors.on({
    when = "connected",
    monitors = { "HDMI-A-1" },
    run = monitors.apply("dock"),
})
```

- `monitors.on({ when, monitors, run })`
- `monitors.layout(name, fn)`
- `monitors.apply(name) -> fn`

`when` is one of `connected`, `disconnected`, `changed`. `run` may be a function or a shell command string.

### `bread.devices`

```lua
local devices = require("bread.devices")

devices.register("Keychron", "keyboard")

devices.on({
    when = "connected",
    class = "keyboard",
    run = function(event)
        bread.exec("xset r rate 200 40")
    end,
})
```

- `devices.on({ when, class, name, run })`
- `devices.register(pattern, class)`

`class` may be `dock`, `keyboard`, `mouse`, `tablet`, `display`, `storage`, `audio`, `unknown`.

### `bread.workspaces`

```lua
local workspaces = require("bread.workspaces")

workspaces.assign("1", "HDMI-A-1")
workspaces.pin({ app = "Firefox", workspace = "2" })
```

- `workspaces.assign(workspace, monitor)`
- `workspaces.pin({ app, workspace })`
- `workspaces.apply_assignments()`

### `bread.binds`

```lua
local binds = require("bread.binds")

binds.add({
    mods = { "SUPER" },
    key = "Return",
    dispatch = "exec",
    args = "kitty",
})
```

- `binds.add({ mods, key, dispatch, args })`
- `binds.remove(key)`
- `binds.replace(key, opts)`

## Dictionary: Event reference

Events are delivered as a `BreadEvent`:

```json
{
  "event": "bread.device.dock.connected",
  "timestamp": 1710000000000,
  "source": "Udev",
  "data": {}
}
```

### Pattern matching

Patterns match event names with glob-style syntax:

- Exact match: `bread.device.dock.connected`
- `*` matches within a single segment (does not cross `.`)
- `**` matches across segments (recursive)
- `?` matches a single character within a segment

Examples:

```lua
bread.on("bread.device.*", handler)
bread.on("bread.device.**", handler)
bread.on("bread.monitor.?", handler)
```

### Normalized events

#### System

- `bread.system.startup` (data: `{}`)

#### Devices (udev)

- `bread.device.connected`
- `bread.device.disconnected`
- `bread.device.changed`
- `bread.device.<class>.connected`
- `bread.device.<class>.disconnected`
- `bread.device.<class>.changed`

Payload notes:

- Device events include `id` and `class`; the generic event also includes `raw`.
- `<class>` is one of: `dock`, `keyboard`, `mouse`, `tablet`, `display`, `storage`, `audio`, `unknown`.

#### Hyprland

- `bread.workspace.changed` (raw payload)
- `bread.workspace.created` (`{ "workspace": "..." }`)
- `bread.workspace.destroyed` (`{ "workspace": "..." }`)
- `bread.monitor.connected` (raw payload)
- `bread.monitor.disconnected` (raw payload)
- `bread.window.focus.changed` (raw payload)
- `bread.window.focused` (`{ "address": "..." }`)
- `bread.window.opened` (`{ "address", "workspace", "class", "title" }`)
- `bread.window.closed` (`{ "address": "..." }`)
- `bread.window.moved` (`{ "address", "workspace" }`)
- `bread.hyprland.event` (raw payload for unhandled kinds)

Raw Hyprland payloads contain `kind`, `raw`, and `data` fields.

#### Power

- `bread.power.ac.connected`
- `bread.power.ac.disconnected`
- `bread.power.battery.low`
- `bread.power.battery.very_low`
- `bread.power.battery.critical`
- `bread.power.battery.full`
- `bread.power.changed` (fallback)

Payload includes `ac_connected` and `battery_percent`.

#### Network

- `bread.network.connected`
- `bread.network.disconnected`

Payload includes `online` and `interfaces`.

#### Other system events

- `bread.profile.activated` (emitted by IPC profile activation)
- `bread.notify.sent` (emitted by `bread.notify`)
- `bread.state.changed.<path>` (emitted when a state watch fires)

## Dictionary: Runtime state schema

`bread.state.get("")` returns the full `RuntimeState`:

```json
{
  "monitors": [ { "name": "HDMI-A-1", "connected": true } ],
  "workspaces": [ { "id": "1", "monitor": "HDMI-A-1" } ],
  "active_workspace": "1",
  "active_window": "Firefox",
  "devices": { "connected": [] },
  "network": { "interfaces": {}, "online": false },
  "power": { "ac_connected": false, "battery_percent": null, "battery_low": false },
  "profile": { "active": "default", "history": [], "profiles": {} },
  "modules": [ { "name": "bread.devices", "status": "loaded", "last_error": null, "builtin": true, "store": {} } ]
}
```

## Dictionary: IPC protocol

The daemon exposes a Unix socket at `$XDG_RUNTIME_DIR/bread/breadd.sock`. Messages are newline-delimited JSON.

Request:

```json
{ "id": "1", "method": "state.get", "params": { "key": "monitors" } }
```

Response:

```json
{ "id": "1", "result": [ { "name": "HDMI-A-1", "connected": true } ] }
```

Available methods:

- `ping`
- `health`
- `state.get`
- `state.dump`
- `modules.list`
- `modules.reload`
- `profile.list`
- `profile.activate`
- `events.subscribe`
- `events.replay`
- `emit`

`events.subscribe` upgrades the socket to streaming mode and sends events as they occur.
