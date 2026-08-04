# Bread Documentation

## Contents

- [Overview](#overview)
- [API Stability & Versioning](#api-stability--versioning)
- [Getting started](#getting-started)
- [Your first module](#your-first-module)
- [Run, reload, and watch](#run-reload-and-watch)
- [Modules: install and manage](#modules-install-and-manage)
- [Debugging tips](#debugging-tips)
- [Dictionary: Lua API](#dictionary-lua-api)
  - [Workflows](#workflows-since-v12)
  - [Widgets](#widgets-since-v13)
  - [Bluetooth](#bluetooth)
- [Dictionary: Built-in modules](#dictionary-built-in-modules)
- [Dictionary: Event reference](#dictionary-event-reference)
- [Namespaces](#namespaces)
- [Integrating a bread* app](#integrating-a-bread-app)
- [Dictionary: Runtime state schema](#dictionary-runtime-state-schema)
- [Dictionary: IPC protocol](#dictionary-ipc-protocol)

## Overview

Bread is a reactive automation fabric for Linux desktops. The daemon (`breadd`) normalizes external signals into semantic events, maintains runtime state, and dispatches events to Lua modules that implement automation.

- **Daemon** (`breadd`) — long-running Rust process; source of truth for runtime state
- **Lua runtime** — dedicated thread inside the daemon; automation logic lives here
- **CLI** (`bread`) — talks to the daemon over a Unix socket

Adapters currently supported: Hyprland compositor IPC, Linux udev/netlink, UPower/sysfs power, rtnetlink/sysfs network, BlueZ Bluetooth, shell precmd/preexec hooks (terminal), git hooks + a dirty-state poller, project-root filesystem watches, `systemd --user` unit state, Podman container events, and SSH/remote session detection. Sibling `bread*` applications (breadclip, breadpad, and others across the BOS ecosystem) integrate through the same pipeline under a reserved `bread.<app>.*` namespace — see [Namespaces](#namespaces).

If you are new to Bread, start with the quick walkthrough below, then jump to the full dictionary when you need exact API details.

## API Stability & Versioning

The Lua API surface, the IPC method set, the event-name vocabulary, and the runtime-state schema documented in this file are collectively **Bread Automation API v1**. This is what "locking in the schema" means operationally:

- **Additive-only within a major version.** New bindings, new events, new state fields, and new optional IPC params may be added in a minor release. Existing binding signatures, event names, event `data` shapes, state field meanings, and IPC method contracts do not change or disappear within v1.
- **Deprecation window.** Anything slated for removal is marked `Deprecated` in this file for at least one minor release cycle and continues to function until the next major version (v2).
- **Since markers.** Additions made after the v1.0 baseline are marked inline with `*Since: vX.Y*`. Anything documented in this file without a marker is part of the v1.0 baseline.
- **Version discovery.** The current API version is returned as `api_version` in the `health` IPC response (see [Dictionary: IPC protocol](#dictionary-ipc-protocol)), so a client — the CLI, a Lua module, or a sibling `bread*` app — can assert compatibility at connect time rather than discovering a mismatch mid-session.

This matters because the moment sibling apps and community modules depend on this vocabulary, it becomes a contract that can break people. Treat this file, not `README.md` or `CLAUDE.md`, as the single source of truth — those files intentionally point back here rather than keeping their own copies, after a duplicated Lua API section in `README.md` was found to have already drifted from reality.

## Getting started

### 1) Create a minimal config

- Daemon config: `~/.config/bread/breadd.toml` (all values optional)
- Lua entry point: `~/.config/bread/init.lua`
- Lua modules: `~/.config/bread/modules/`

### 2) Minimal `init.lua`

```lua
bread.on("bread.system.startup", function(event)
    bread.profile.activate("default")
    bread.log("bread started on " .. bread.machine.name())
end)
```

### 3) Start the daemon

```bash
systemctl --user start breadd

# Or directly:
breadd
```

### 4) Check that it's running

```bash
bread ping
bread doctor
```

## Your first module

Create a file at `~/.config/bread/modules/hello.lua`. It is discovered and loaded automatically after `init.lua`.

```lua
local M = bread.module({ name = "hello", version = "0.1.0" })

function M.on_load()
    bread.log("hello from bread on " .. bread.machine.name())

    bread.on("bread.device.*", function(event)
        bread.log("device event: " .. event.event)
    end)
end

return M
```

Key rules:

- Every module must call `bread.module` exactly once at the top level.
- Register subscriptions inside `M.on_load` so they are cleaned up properly on hot reload.
- Use `bread.log` early to verify handlers are firing.

## Run, reload, and watch

```bash
# Hot-reload the Lua runtime after editing config
bread reload

# Watch for file changes and reload automatically
bread reload --watch
```

If any module fails to load, `bread reload` prints the error with a full Lua stack trace. The daemon stays running — fix the file and reload again.

## Modules: install and manage

Modules are Lua packages installed to `~/.config/bread/modules/`. The CLI manages the install lifecycle.

Modules install from a **local directory only**. They run with full
`bread.exec()` privileges and are not sandboxed; remote installation was
removed so that reviewing third-party code stays an explicit, manual step. To
use a module published on a git host, clone it yourself, review it, then
install from the checkout.

```bash
# Clone and review, then install from the local checkout
git clone https://github.com/someuser/bread-wifi ~/src/bread-wifi
bread modules install ~/src/bread-wifi

# List installed modules and their daemon status
bread modules list

# Show full manifest for one module
bread modules info bread-wifi

# Remove a module
bread modules remove bread-wifi
bread modules remove bread-wifi --yes   # skip confirmation
```

Each installed module has a `bread.module.toml` manifest:

```toml
name = "wifi"
version = "1.0.0"
description = "WiFi management for Bread"
author = "someuser"
source = "/home/you/src/bread-wifi"
installed_at = "2026-01-01T00:00:00Z"
```

## Debugging tips

- Run `bread events` to see live normalized events.
- Run `bread state` to see full runtime state as JSON.
- Run `bread doctor` to check adapter and module health.
- Log event payloads with `bread.log(tostring(event.data))`.
- Use `RUST_LOG=debug breadd` for verbose daemon output.

---

## Dictionary: Lua API

Every API is exposed through the `bread` global table.

### Module declaration

Every module must call `bread.module` exactly once at the top level.

```lua
local M = bread.module({
    name    = "my.module",
    version = "0.1.0",
    after   = { "bread.devices" },   -- optional: load after this module
})

return M
```

If a module does not call `bread.module`, it fails to load and is marked as a load error.

### Events

#### `bread.on(pattern, fn) -> id`
Subscribe to matching events. Returns a numeric subscription ID.

```lua
local id = bread.on("bread.device.*", function(event)
    -- event.event   → the full event name string
    -- event.data    → table of event-specific fields
    -- event.source  → adapter that produced it ("Udev", "Hyprland", etc.)
    bread.log(event.event)
end)
```

#### `bread.once(pattern, fn) -> id`
Subscribe once. The handler is removed after the first match.

#### `bread.filter(pattern, fn, opts) -> id`
Subscribe with a predicate. `opts` must contain a `filter` function:

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
Unsubscribe an event handler or state watch by ID.

#### `bread.emit(event, data)`
Emit a custom event into the system pipeline. Useful for cross-module communication.

#### `bread.wait(pattern, opts) -> event | nil`
Coroutine-only helper that suspends until a matching event arrives.

```lua
bread.spawn(function()
    local event = bread.wait("bread.device.dock.connected", { timeout = 5000 })
    if event then
        bread.log("dock arrived")
    end
end)
```

#### `bread.spawn(fn)`
Spawn a coroutine and surface errors if it fails. Required for using `bread.wait`.

#### `bread.wait_any(patterns, opts) -> event | nil` *(Since: v1.2)*
Coroutine-only. Like `bread.wait`, but resolves on the first of several patterns to match; returns `nil` after `opts.timeout` if none do.

```lua
bread.spawn(function()
    local event = bread.wait_any(
        { "bread.monitor.connected", "bread.hyprland.event" },
        { timeout = 5000 }
    )
    if event then
        bread.log("a monitor-related event arrived")
    end
end)
```

#### `bread.wait_all(patterns, opts) -> table` *(Since: v1.2)*
Coroutine-only. Resolves once every listed pattern has fired at least once, or `opts.timeout` elapses. Returns a table keyed by pattern → event; on timeout, the table additionally has `timed_out = true` and contains whichever patterns had already fired.

### Workflows *(Since: v1.2)*

Multi-step automations built on `bread.spawn`/`bread.wait` (and `wait_any`/`wait_all`), with status introspectable from outside the running coroutine — via Lua (`bread.workflow.status`/`.list`) or over IPC (`workflows.list`). See [Examples.md](Examples.md#example-4-multi-step-automation-workflows) for a full worked example.

#### `bread.workflow.define(name, fn)`
Register a workflow body under `name`. `fn` receives one argument: whatever `opts.args` was passed to `.start()` (or `nil`).

#### `bread.workflow.start(name, opts)`
Run the workflow registered as `name` (spawned as a coroutine, same mechanics as `bread.spawn`). `opts` (optional):

| Key | Type | Description |
|-----|------|-------------|
| `deadline` | ms | If the workflow hasn't reached a terminal state by then, its status becomes `timed_out`. Independent of any per-`wait` timeout inside the body — a safety net for the whole run, not a replacement for step-level timeouts. |
| `args` | any | Passed through as the sole argument to the workflow body function. |

Starting a workflow under a name that's already running **replaces** its registry entry — this is a live-status registry, not a run history.

#### `bread.workflow.step(label)`
Call from inside a running workflow body to record "currently here." Purely observational — it does not affect control flow. Errors if called outside a running workflow body.

#### `bread.workflow.status(name) -> table | nil`
Returns the current status for `name`, or `nil` if no workflow with that name has ever been started. Shape:

```json
{
  "name": "dock-connected",
  "state": "running",
  "step": "waiting for monitor",
  "started_at": 1710000000000,
  "updated_at": 1710000001500,
  "error": null
}
```

`state` is one of `running`, `done`, `failed`, `timed_out`. `error` is set (the captured Lua error message) only when `state` is `failed`.

#### `bread.workflow.list() -> table`
Returns an array of every workflow's current status, in the same shape as `bread.workflow.status`.

### Widgets *(Since: v1.3)*

Declarative, live-updating widgets rendered by sibling `bread*` apps (breadbar) in their own bar/popover free space. A widget is a small tree of typed nodes — `box`, `label`, `icon`, `progress` — not raw markup: this keeps rendering generic across every consuming app and keeps a node's appearance confined to a bounded, typed `style` vocabulary the renderer already knows about (see `style` below), with no style/CSS injection surface from Lua.

Widgets are registered per-module and are re-registered fresh on every hot reload (the whole registry is cleared right before the Lua VM resets, same as `bread.module`'s per-reload re-execution) — call `bread.widget.register` at module top level or in `on_load`, not somewhere that only runs once ever.

#### `bread.widget.register(spec) -> ok, err`
Registers (or replaces, if `spec.id` already exists for this module) a widget. `spec`:

| Key | Type | Description |
|-----|------|-------------|
| `id` | string | Local id, unique within your module. Stored/addressed elsewhere as `"<module>.<id>"`. |
| `placement` | string | One of `tray`, `left_of_clock`, `right_of_clock`, `right_of_workspaces`, `left_of_stats` — which fixed slot in the consuming app's layout this widget renders into. |
| `order` | number | Optional, default `0`. Sort priority within a placement; lower sorts first. |
| `visible` | bool | Optional, default `true`. |
| `tooltip` | string | Optional. |
| `root` | node | The render tree (see Node types below). |

Returns `true` on success, or `false, err` if `root` fails validation (tree too deep, too many nodes, or an invalid `class`), `root` contains a `style` field with a value outside its enum (a deserialization error, reported the same way), or `bread.widget.register` was called outside a module.

##### Node types

Every node accepts an optional `style` (a bounded, typed vocabulary — see below; this is the primary way to control a node's appearance), an optional `class` (a small freeform escape hatch, see [Style vs. class](#style-vs-class) below), and an optional `on_click` (any Lua value, passed through opaquely — see Click events below).

| `type` | Fields |
|--------|--------|
| `box` | `orientation` (`"horizontal"` \| `"vertical"`, default horizontal), `spacing`, `children` (array of nodes) |
| `label` | `text` |
| `icon` | `name` (bundled icon) or `path` (arbitrary SVG file) — exactly one; `size` |
| `progress` | `value` (0.0–1.0) |

A tree is capped at depth 4 (root counts as depth 1) and 50 total nodes — comfortably enough for a status readout, not enough to build a full custom UI.

```lua
bread.widget.register({
    id = "weather",
    placement = "left_of_stats",
    tooltip = "Sydney: Partly cloudy",
    root = {
        type = "box",
        children = {
            { type = "icon", name = "cloud" },
            { type = "label", text = "22°C", style = { color = "dim" }, on_click = "refresh" },
        },
    },
})
```

##### `style` *(Since: v1.4)*

`style` is a bounded, typed vocabulary for a node's appearance — every field is a small closed enum, not a string, so a typo is a `bread.widget.register` validation failure at registration time, not a silently-ignored CSS class. There is deliberately **no raw CSS/style-string field** anywhere in this API: a module can only ever pick from the fixed set below, never inject arbitrary style.

| Field | Type | Values |
|-------|------|--------|
| `color` | string | `fg`, `dim` (muted foreground), `accent`, `red`, `green`, `yellow`, `blue`, `pink`, `teal` |
| `weight` | string | `normal`, `bold` |
| `size` | string | `xs`, `sm`, `md`, `lg`, `xl` — text size in px (10/12/14/16/20); `sm`/`md` match the bread design system's own secondary/base font sizes |
| `align` | string | `start`, `center`, `end` |
| `background` | string | `none`, `surface`, `card` (surface + rounded corners + padding) |
| `radius` | string | `none`, `sm`, `md`, `full` (pill) |
| `padding` | string | `none`, `xs`, `sm`, `md` |

Every field is optional and independent — set only what you need. Colors, font sizes, radii, and padding all reuse the exact same palette, font, and spacing scale every other `bread*` GUI (breadbar, bos-settings, breadpad, ...) is themed from, so a widget recolors with the rest of the desktop when pywal's palette changes instead of drifting out of sync.

```lua
{ type = "label", text = "LOW BATTERY", style = { color = "yellow", weight = "bold" } }
```

##### Style vs. `class`

`class` still exists as an escape hatch for a CSS class the *consuming app's own stylesheet* happens to define (restricted to `^[a-zA-Z][a-zA-Z0-9_-]{0,63}$`) — useful if you're targeting a specific app you know the internals of, but undiscoverable and app-specific otherwise. As of this writing, breadbar's stylesheet only gives real meaning to `dim` this way (fades a node to 60% opacity) — everything else a module needs (color, weight, size, alignment, background, radius, padding) should go through `style` instead, which every renderer is expected to understand identically.

#### `bread.widget.update(id, patch) -> ok, err`
Patches an already-registered widget (local `id`, not the fully-qualified form). Any of `root`, `tooltip`, `visible`, `order` may be given; omitted fields are left as-is. `root`, when given, replaces the whole tree — there is no node-level patching. Returns `false, "no such widget"` if `id` isn't registered.

```lua
bread.widget.update("weather", {
    root = { type = "box", children = { { type = "label", text = "23°C" } } },
})
```

#### `bread.widget.remove(id) -> bool`
Removes a widget registered by the calling module. Returns whether anything was removed.

#### `bread.widget.list() -> table`
Returns an array of every widget the calling module currently has registered.

##### Click events

A clicked node's `on_click` value doesn't travel back through `breadd` directly — the rendering app (breadbar) emits `bread.bar.widget_clicked` with `{ widget_id, action }` (`action` being whatever you put in `on_click`), because a rendering app may only publish inside its own `bread.<app_id>.*` namespace (see [Namespaces](#namespaces)). React to it like any other event, filtering on `widget_id`:

```lua
bread.on("bread.bar.widget_clicked", function(e)
    if e.data.widget_id == "weather.weather" then
        -- e.data.action == "refresh"
    end
end)
```

### State

#### `bread.state.get(path)`
Read a state subtree by dotted path.

```lua
local monitors = bread.state.get("monitors")
local online   = bread.state.get("network.online")
```

#### Typed shorthands

```lua
bread.state.monitors()
bread.state.active_workspace()
bread.state.active_window()
bread.state.devices()
bread.state.power()
bread.state.network()
bread.state.profile()
```

#### `bread.state.watch(path, fn) -> id`
Watch a state path for changes. The callback receives `(new_value, old_value)`.

```lua
bread.state.watch("power.ac_connected", function(new_val, old_val)
    if new_val then
        bread.notify("AC connected")
    end
end)
```

### Profiles

#### `bread.profile.activate(name)`
Activate a named profile. Emits `bread.profile.activated` over IPC.

### Execution

#### `bread.exec(cmd)`
Run a shell command. Fire-and-forget (async, does not block Lua).

#### `bread.exec_capture(cmd, opts) -> ok, stdout`
Run a shell command and return its result: `ok` is whether it exited zero,
`stdout` is its captured standard output. Unlike `bread.exec`, this blocks
the calling Lua callback until the command exits (or the timeout below
elapses), so it's only appropriate for fast, local commands — e.g.
`git -C <dir> rev-parse --abbrev-ref HEAD`, not anything that hits the
network or waits on user input.

```lua
local ok, branch = bread.exec_capture("git -C " .. dir .. " rev-parse --abbrev-ref HEAD")
if ok then
    branch = branch:gsub("%s+$", "")  -- trailing newline
end
```

Options:

| Key | Type | Default |
|-----|------|---------|
| `timeout_ms` | number | `2000` |

On timeout or spawn failure, returns `false, ""`.

### Notifications

#### `bread.notify(message, opts)`
Send a desktop notification via `notify-send`.

Options:

| Key | Type | Default |
|-----|------|---------|
| `title` | string | `"bread"` |
| `urgency` | string | from config |
| `timeout` | ms | from config |
| `icon` | string | none |

Calling `bread.notify` emits `bread.notify.sent` with `{ title, message, urgency }`.

### Timers

#### `bread.after(delay_ms, fn) -> id`
Run once after a delay.

#### `bread.every(interval_ms, fn) -> id`
Run on a repeating interval.

#### `bread.cancel(id)`
Cancel a timer created by `after` or `every`. Timers are also cancelled automatically on reload.

### Utilities

#### `bread.debounce(delay_ms, fn) -> wrapped_fn`
Returns a wrapper that fires only after `delay_ms` of quiet time.

```lua
local fn = bread.debounce(200, function(event)
    reconfigure_monitors()
end)
bread.on("bread.monitor.**", fn)
```

#### `bread.log(msg)` / `bread.warn(msg)` / `bread.error(msg)`
Logging helpers. Accept any Lua value (coerced via `tostring`).

### Machine and filesystem

#### `bread.machine.name() -> string`
Returns the system hostname. If an external tool has written a
`~/.config/bread/sync.toml` with a `[machine].name`, that value takes
precedence (bread reads the file if present but does not create it).

#### `bread.machine.tags() -> string[]`
Returns `[machine].tags` from `~/.config/bread/sync.toml` if that file
exists, otherwise `{}`.

#### `bread.machine.has_tag(tag) -> bool`
Returns true if the machine has the given tag.

#### `bread.fs.write(path, content)`
Write a file. Creates parent directories as needed. `~` is expanded.

#### `bread.fs.read(path) -> string | nil`
Read a file. Returns `nil` if the file does not exist. `~` is expanded.

#### `bread.fs.exists(path) -> bool`
Returns true if the path exists. `~` is expanded.

#### `bread.fs.readlink(path) -> string | nil`
Read a symlink's target. Returns `nil` if the path doesn't exist or isn't a
symlink. Distinct from `bread.fs.read`, which opens and reads file
*contents* — for something like `/proc/<pid>/cwd`, the payload is the link
target itself, not a file to read.

#### `bread.fs.expand(path) -> string`
Expand `~` to the home directory.

#### `bread.json.decode(str) -> table | nil`
Parse a JSON string into a Lua table. Returns `nil` on malformed input.
Pairs naturally with `bread.exec_capture` for consuming JSON output from a
CLI (e.g. `kitty @ ls`).

### Hyprland

The `bread.hyprland` namespace provides compositor bindings.

```lua
-- Dispatch a Hyprland command
bread.hyprland.dispatch("workspace", "2")
bread.hyprland.dispatch("exec", "kitty")

-- Set a keyword
bread.hyprland.keyword("monitor", "HDMI-A-1, 2560x1440, 0x0, 1")

-- Query compositor state (returns deserialized Lua tables)
local win        = bread.hyprland.active_window()
local monitors   = bread.hyprland.monitors()
local workspaces = bread.hyprland.workspaces()
local clients    = bread.hyprland.clients()

-- Subscribe to raw Hyprland events (bypasses normalization)
bread.hyprland.on_raw("activewindow", function(raw)
    -- raw payload includes: kind, raw (original string), data
end)
```

### Bluetooth

The `bread.bluetooth` namespace provides control over the local Bluetooth adapter and its paired devices via BlueZ D-Bus. All functions degrade gracefully when BlueZ is unavailable — control functions log a warning and return `nil`, query functions return `nil`.

#### `bread.bluetooth.power(enabled)`
Power the Bluetooth adapter on (`true`) or off (`false`). Fire-and-forget.

#### `bread.bluetooth.powered() -> bool | nil`
Returns the current power state of the adapter, or `nil` if unavailable.

```lua
if bread.bluetooth.powered() then
    bread.log("Bluetooth is on")
end
```

#### `bread.bluetooth.connect(address)`
Connect to a paired device by MAC address. Fire-and-forget — the result is delivered as a `bread.device.connected` event when the connection succeeds.

```lua
bread.bluetooth.connect("AA:BB:CC:DD:EE:FF")
```

#### `bread.bluetooth.disconnect(address)`
Disconnect from a device by MAC address. Fire-and-forget — delivered as `bread.device.disconnected`.

#### `bread.bluetooth.scan(enabled)`
Start (`true`) or stop (`false`) device discovery.

#### `bread.bluetooth.devices() -> table | nil`
Returns all devices known to BlueZ as an array of tables. Returns `nil` if BlueZ is unavailable.

```lua
local devs = bread.bluetooth.devices()
if devs then
    for _, dev in ipairs(devs) do
        bread.log(dev.name .. " " .. dev.address
            .. (dev.connected and " [connected]" or ""))
    end
end
```

Each device table:

| Field | Type | Description |
|-------|------|-------------|
| `address` | string | Bluetooth MAC address, e.g. `"AA:BB:CC:DD:EE:FF"` |
| `name` | string | Device name from BlueZ (Alias or Name property) |
| `connected` | bool | Whether the device is currently connected |
| `paired` | bool | Whether the device is paired |

#### Example: auto-connect headphones on AC power

```lua
local M = bread.module({ name = "headphones", version = "1.0.0" })
local HEADPHONES = "AA:BB:CC:DD:EE:FF"

function M.on_load()
    bread.state.watch("power.ac_connected", function(ac)
        if ac then
            bread.bluetooth.power(true)
            bread.bluetooth.connect(HEADPHONES)
        end
    end)
end

return M
```

#### Example: turn off Bluetooth on battery

```lua
bread.state.watch("power.ac_connected", function(ac)
    bread.bluetooth.power(ac)
end)
```

### Module lifecycle hooks

All hooks are optional.

```lua
function M.on_load()
    -- Called after the module loads. Register subscriptions here.
end

function M.on_reload()
    -- Called after a hot reload completes across all modules.
end

function M.on_unload()
    -- Called before the Lua instance is dropped.
end

function M.on_error(err)
    -- Called when a subscription handler in this module throws.
    -- Return true to keep the subscription alive, false to cancel it.
    return true
end
```

### Module storage

Survives hot reload; does not survive daemon restart.

```lua
M.store.set("last_profile", "docked")
local value = M.store.get("last_profile")
```

Storage is scoped per module and is not shared across modules.

---

## Dictionary: Built-in modules

Built-ins are loaded before user modules. Disable them via `[modules].disable` in the daemon config.

### `bread.monitors`

High-level declarative monitor event handlers.

```lua
local monitors = require("bread.monitors")

monitors.layout("dock", function()
    bread.exec("~/.config/bread/scripts/layout-dock.sh")
end)

monitors.on({
    when     = "connected",
    monitors = { "HDMI-A-1" },
    run      = monitors.apply("dock"),
})
```

| Function | Description |
|----------|-------------|
| `M.on(opts)` | Register a monitor workflow. `opts`: `when`, `monitors` (optional list), `run` (function or shell string) |
| `M.layout(name, fn)` | Register a named layout function |
| `M.apply(name) -> fn` | Returns a function that calls the named layout |

`when` is one of `connected`, `disconnected`, `changed`.

### `bread.devices`

Device connection rules with name-based matching. This module handles hardware hotplug events from USB devices, monitors, and other peripherals.

Device names are defined in `~/.config/bread/devices.lua` — the daemon resolves the name before dispatching events, so modules can match on stable user-defined names rather than raw hardware identifiers.

```lua
local devices = require("bread.devices")

devices.on({
    when   = "connected",
    device = "keyboard",
    run    = function(event)
        bread.exec("xset r rate 200 40")
    end,
})

devices.on({
    when   = "connected",
    device = "dock",
    run    = "~/.config/bread/scripts/dock-connected.sh"
})

devices.on({
    when = "disconnected",
    name = "CalDigit",  -- pattern-matched against event.data.name
    run  = function(event)
        bread.log("Dock disconnected: " .. event.data.name)
    end,
})
```

#### Functions

| Function | Description |
|----------|-------------|
| `M.on(opts)` | Register a device rule. See options below. |

#### Device rule options

```lua
devices.on({
    when   = "connected",              -- required: "connected" or "disconnected"
    device = "keyboard",              -- optional: device name from devices.lua
    name   = "Keychron",             -- optional: substring matched against device name
    run    = function(event) ... end  -- required: function or shell string
})
```

- `when` (required): One of `connected` or `disconnected`.
- `device` (optional): Device name as defined in `devices.lua`. If specified, the rule only fires for devices with that name.
- `name` (optional): Pattern that must be found in `event.data.name` (case-insensitive substring). Can be combined with `device` (both must match).
- `run` (required): Function or shell string to run when the rule matches.

The callback receives the full device event:
```lua
{
  event = "bread.device.dock.connected",
  data = {
    id = "/sys/...",
    device = "dock",       -- name resolved from devices.lua
    name = "CalDigit TS4", -- raw device name from udev
    subsystem = "usb",
    vendor_id = "0x35f5",
    product_id = "0x0104",
    raw = { ... }          -- full udev properties
  }
}
```

#### Example: Keyboard configuration on connect

```lua
devices.on({
    when   = "connected",
    device = "keyboard",
    run    = function(event)
        bread.log("Keyboard connected: " .. event.data.name)
        bread.exec("xset r rate 200 40")
    end,
})
```

#### Example: Dock-specific setup

```lua
-- devices.lua defines: { device = "dock", vendor_id = "35f5" }

devices.on({
    when   = "connected",
    device = "dock",
    run    = function(event)
        bread.log("Dock connected")
        bread.exec("~/.config/bread/scripts/dock-connected.sh")
    end,
})

devices.on({
    when   = "disconnected",
    device = "dock",
    run    = function(event)
        bread.log("Dock disconnected")
        bread.exec("~/.config/bread/scripts/dock-disconnected.sh")
    end,
})
```

### `bread.workspaces`

Workspace-to-monitor assignment and app pinning.

```lua
local workspaces = require("bread.workspaces")

workspaces.assign("1", "HDMI-A-1")
workspaces.pin({ app = "Firefox", workspace = "2" })
```

| Function | Description |
|----------|-------------|
| `M.assign(workspace, monitor)` | Assign a workspace to a monitor |
| `M.pin(opts)` | Pin an app class to a workspace. `opts`: `app`, `workspace` |
| `M.apply_assignments()` | Apply all registered assignments via Hyprland dispatch |

### `bread.binds`

Runtime keybind management via Hyprland.

```lua
local binds = require("bread.binds")

binds.add({
    mods     = { "SUPER" },
    key      = "Return",
    dispatch = "exec",
    args     = "kitty",
})
```

| Function | Description |
|----------|-------------|
| `M.add(opts)` | Add a keybind. `opts`: `mods`, `key`, `dispatch`, `args` |
| `M.remove(key)` | Remove a keybind by key |
| `M.replace(key, opts)` | Remove and re-add a keybind |

---

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

| Pattern | Matches |
|---------|---------|
| `bread.device.dock.connected` | Exact match only |
| `bread.device.*` | One segment wildcard (does not cross `.`) |
| `bread.device.**` | Any depth under `bread.device` |
| `bread.monitor.?` | Single character within one segment |

### Normalized events

#### System

| Event | Data |
|-------|------|
| `bread.system.startup` | `{}` |

#### Devices (udev / Bluetooth)

| Event | Data |
|-------|------|
| `bread.device.connected` | `{ id, device, name, vendor, vendor_id, product_id, subsystem, raw }` |
| `bread.device.disconnected` | same |
| `bread.device.<device>.connected` | `{ id, device }` |
| `bread.device.<device>.disconnected` | `{ id, device }` |

`device` is the name resolved from `~/.config/bread/devices.lua`. Devices that match no rule use `"unknown"`. The generic `bread.device.connected` event carries the full payload including `raw` udev properties; the named companion event carries only `id` and `device`.

Both USB/udev devices and Bluetooth devices emit `bread.device.connected` / `bread.device.disconnected`. They can be distinguished by `event.data.subsystem`:

| `subsystem` | Source | Unique identifier field |
|-------------|--------|------------------------|
| `"usb"`, `"input"`, etc. | udev | `vendor_id` + `product_id` |
| `"bluetooth"` | BlueZ | `address` (MAC address) |

#### Bluetooth (BlueZ)

| Event | Data |
|-------|------|
| `bread.device.connected` | `{ id, device, name, address, subsystem: "bluetooth", raw }` |
| `bread.device.disconnected` | same |
| `bread.bluetooth.device.paired` | `{ id, name, address, subsystem: "bluetooth", raw }` |
| `bread.bluetooth.device.unpaired` | `{ id, address, subsystem: "bluetooth", raw }` |

`bread.bluetooth.device.paired` fires when BlueZ first learns about a device (new pairing or adapter restart). It does not mean the device is connected. `bread.device.connected` fires when the device profile actually connects.

`name` may be `"unknown"` on `bread.device.connected` events emitted from `PropertiesChanged` signals, since BlueZ only includes changed properties. It is always populated on `bread.bluetooth.device.paired` and on events from the initial enumeration at startup.

#### Hyprland

*Since: v1.5 — the `bread.hyprland.*` namespaced forms below. Bread's event vocabulary is meant to be portable across a future second compositor backend; a flat `bread.workspace.*`/`bread.monitor.*`/`bread.window.*` name gave no way to tell a genuinely cross-backend event (like `bread.power.*`) apart from one that is Hyprland-specific. The 10 rows marked `Deprecated: v1.5` are unaffected functionally — they keep firing — but new automation should subscribe to their `bread.hyprland.*` sibling instead.*

Every Hyprland-sourced event below is dual-emitted: the daemon fires both the legacy flat name and its `bread.hyprland.<rest>` equivalent with identical `data`/`timestamp`/`source`, unless `[compat] legacy_hyprland_event_names = false` is set (see below), in which case only the namespaced name fires. A module that subscribes only to `bread.hyprland.*` always gets full workspace/monitor/window coverage regardless of that setting.

| Event | Data |
|-------|------|
| `bread.workspace.changed` *(Deprecated: v1.5 — use `bread.hyprland.workspace.changed`)* | raw payload |
| `bread.hyprland.workspace.changed` *(Since: v1.5)* | raw payload |
| `bread.workspace.created` *(Deprecated: v1.5 — use `bread.hyprland.workspace.created`)* | `{ workspace }` |
| `bread.hyprland.workspace.created` *(Since: v1.5)* | `{ workspace }` |
| `bread.workspace.destroyed` *(Deprecated: v1.5 — use `bread.hyprland.workspace.destroyed`)* | `{ workspace }` |
| `bread.hyprland.workspace.destroyed` *(Since: v1.5)* | `{ workspace }` |
| `bread.monitor.connected` *(Deprecated: v1.5 — use `bread.hyprland.monitor.connected`)* | raw payload |
| `bread.hyprland.monitor.connected` *(Since: v1.5)* | raw payload |
| `bread.monitor.disconnected` *(Deprecated: v1.5 — use `bread.hyprland.monitor.disconnected`)* | raw payload |
| `bread.hyprland.monitor.disconnected` *(Since: v1.5)* | raw payload |
| `bread.window.focus.changed` *(Deprecated: v1.5 — use `bread.hyprland.window.focus.changed`)* | raw payload |
| `bread.hyprland.window.focus.changed` *(Since: v1.5)* | raw payload |
| `bread.window.focused` *(Deprecated: v1.5 — use `bread.hyprland.window.focused`)* | `{ address }` |
| `bread.hyprland.window.focused` *(Since: v1.5)* | `{ address }` |
| `bread.window.opened` *(Deprecated: v1.5 — use `bread.hyprland.window.opened`)* | `{ address, workspace, class, title }` |
| `bread.hyprland.window.opened` *(Since: v1.5)* | `{ address, workspace, class, title }` |
| `bread.window.closed` *(Deprecated: v1.5 — use `bread.hyprland.window.closed`)* | `{ address }` |
| `bread.hyprland.window.closed` *(Since: v1.5)* | `{ address }` |
| `bread.window.moved` *(Deprecated: v1.5 — use `bread.hyprland.window.moved`)* | `{ address, workspace }` |
| `bread.hyprland.window.moved` *(Since: v1.5)* | `{ address, workspace }` |
| `bread.hyprland.event` | `{ kind, raw, data }` (unhandled kinds — already namespaced, not part of this migration) |

##### Compatibility: `[compat]` config

```toml
[compat]
legacy_hyprland_event_names = true   # default during the deprecation window
```

Set to `false` to suppress the 10 legacy flat names above and emit only their `bread.hyprland.*` equivalents. This defaults to `true` for now; per the [API Stability & Versioning](#api-stability--versioning) deprecation-window policy, the default will flip to `false` in a later release once the window closes. Removing the legacy names entirely is a further, separate follow-up — see the note in `DEPRECATIONS.md`.

#### Power

| Event | Data |
|-------|------|
| `bread.power.ac.connected` | `{ ac_connected, battery_percent }` |
| `bread.power.ac.disconnected` | `{ ac_connected, battery_percent }` |
| `bread.power.battery.low` | `{ battery_percent }` |
| `bread.power.battery.very_low` | `{ battery_percent }` |
| `bread.power.battery.critical` | `{ battery_percent }` |
| `bread.power.battery.full` | `{ battery_percent }` |
| `bread.power.changed` | `{ ac_connected, battery_percent }` |

#### Network

| Event | Data |
|-------|------|
| `bread.network.connected` | `{ online, interfaces }` |
| `bread.network.disconnected` | `{ online, interfaces }` |

#### System events

| Event | Data |
|-------|------|
| `bread.profile.activated` | `{ name }` |
| `bread.notify.sent` | `{ title, message, urgency }` |
| `bread.state.changed.<path>` | emitted by state watches |

#### Widgets *(Since: v1.3)*

Emitted by `breadd` itself on every `bread.widget.*` mutation — see [Widgets](#widgets-since-v13). `data` is the full `WidgetSpec` for `registered`/`updated`; just `{ id }` for `removed`.

| Event | Data |
|-------|------|
| `bread.widget.registered` | `{ id, module, placement, order, visible, tooltip, root, updated_at }` |
| `bread.widget.updated` | same shape as `registered` |
| `bread.widget.removed` | `{ id }` |
| `bread.widget.cleared` | `{}` — fired once at the end of every module reload (`bread reload`), whether or not the widget set actually changed. The registry itself is wiped and re-populated as modules re-run; this is a "go re-fetch" signal for consumers that only react to `bread.widget.*` events, so a module that stops registering widgets (e.g. gets disabled) is noticed even though nothing else fires. |

#### Terminal (shell precmd/preexec hooks)

Requires `bread hooks install shell` and sourcing the generated script from your shell rc — see the CLI reference. Fires via the `bread-emit` helper, not the daemon reaching out.

| Event | Data |
|-------|------|
| `bread.terminal.command.started` | `{ cmd, cwd }` |
| `bread.terminal.command.finished` | `{ cmd, cwd, exit_code, duration_ms }` |
| `bread.terminal.cwd.changed` | `{ cwd, prev_cwd }` |

Terminal events are exempt from the daemon's event dedup window (running the same command twice in quick succession is legitimate, not noise).

#### Git (hooks + dirty-state poller)

`bread.git.commit.created`/`bread.git.branch.changed` come from git hooks installed via `bread hooks install git` (current repo only; never overwrites an existing hook). `bread.git.state.*`/`bread.git.ahead_behind.changed` come from an in-daemon poller over configured project roots (`[adapters.git] roots = [...]` in `breadd.toml`) and never fire for the same transition a hook already reported.

| Event | Data |
|-------|------|
| `bread.git.commit.created` | `{ repo, sha, branch, message }` |
| `bread.git.branch.changed` | `{ repo, branch, previous_ref }` |
| `bread.git.state.dirty` | `{ repo }` |
| `bread.git.state.clean` | `{ repo }` |
| `bread.git.ahead_behind.changed` | `{ repo, ahead, behind, branch }` |

#### Filesystem / project detection

Scoped to configured project roots (`[adapters.filesystem] roots = [...]`), not the whole filesystem. `.git`/`node_modules` are always silent; `target`/`dist`/`build` are silent for edits but reported on new-file creation as `build_artifact.created`.

| Event | Data |
|-------|------|
| `bread.project.detected` | `{ root, markers }` (markers: any of `.git`, `Cargo.toml`, `package.json`, `go.mod`) |
| `bread.project.file.changed` | `{ path, project_root }` |
| `bread.project.build_artifact.created` | `{ path, project_root }` |

#### Systemd (`systemd --user` units)

Only units named in `[adapters.systemd] units = [...]` are watched — subscribing to every user unit is noisy.

| Event | Data |
|-------|------|
| `bread.service.started` | `{ unit }` |
| `bread.service.stopped` | `{ unit }` |
| `bread.service.failed` | `{ unit, result }` (`result` may be `null`) |

#### Podman (containers)

Degrades to simply not emitting if the `podman` binary isn't installed — no daemon startup dependency on it.

| Event | Data |
|-------|------|
| `bread.container.started` | `{ id, name, image }` |
| `bread.container.stopped` | `{ id, name }` |
| `bread.container.health.changed` | `{ id, name, health }` |

#### Remote (SSH session detection)

Rides the same shell-hook transport as Terminal events (`bread hooks install shell`).

| Event | Data |
|-------|------|
| `bread.remote.session.started` | `{ host }` |
| `bread.remote.session.ended` | `{ host }` |

---

## Namespaces

*Since: v1.1 — the `AdapterSource::App` variant and the known-apps registry (`bread_shared::apps::KNOWN_APPS`). No sibling app emits through this path yet as of this writing except the breadclip pilot (see its own `EVENTS.md` once that lands); the daemon-side plumbing and the convention itself are what v1.1 adds.*

*Since: v1.3 — breadbar is now an active `bread-client` consumer under the `bar` app id (already present in `KNOWN_APPS`): it emits `bread.bar.widget_clicked` for widget clicks (see [Widgets](#widgets-since-v13)) and reads `bread.widget.*` to render the [Dictionary: Runtime state schema](#dictionary-runtime-state-schema)'s `widgets` field.*

Two dotted-name segments are reserved, permanent parts of the schema — not one-off conventions:

- **`bread.<app>.*`** — inbound events published *by* a sibling `bread*` application about its own state (e.g. `bread.clip.copied`). An app may only publish within its own segment; the daemon enforces this at the IPC boundary (a socket client claiming a `source` of an app id it doesn't own is rejected the same way spoofing `power`/`hyprland` is rejected today).
- **`bread.command.<app>.<verb>`** — outbound commands *to* a sibling application (e.g. `bread.command.clip.clear`). Any module or app may publish; only the target app subscribes. This reuses the existing event bus in both directions — there is no separate request/response protocol.
- The second dotted segment is drawn from a small known-apps registry (`bread_shared::apps::KNOWN_APPS`); daemon-internal domains (`terminal`, `git`, `hyprland`, `device`, `power`, `network`, `service`, `container`, `project`, `remote`, `system`, `profile`, `notify`, `command`, `workflow`) are reserved and cannot be claimed as app ids.
- **Commands are best-effort.** Publishing `bread.command.<app>.<verb>` with no subscriber (the app isn't installed or isn't running) is a silent no-op — there is nothing to special-case, and no error is raised. An app that acts on a command *should* emit a corresponding `bread.<app>.<verb>.done` (or `.failed`) confirmation; a module that needs to know a command was actually honored must `bread.wait`/`bread.wait_any` on that confirmation with a timeout rather than assume success. There is no mandatory request/response correlation layer — most commands are legitimately fire-and-forget, and building one would contradict the "no listener, no-op" degradation property.
- **`bread.exec("<cli> ...")`** remains the zero-infrastructure fallback for triggering a sibling app that has a synchronous CLI and no need for a structured response.

---

## Integrating a bread\* app

This is the checklist for adding a new sibling `bread*` application to the fabric — it's deliberately short, because the whole design goal of the name-based app registry (over one `AdapterSource` enum variant per app) is that this never requires a daemon change beyond step 1. **breadclip is the reference implementation** — see its own `EVENTS.md` for a worked example of every step below.

1. **Register your app id.** Add it to `KNOWN_APPS` in `bread-shared/src/lib.rs` (a one-line, one-word-per-app list) — this is the only change to the `bread` repo itself a new integration needs.
2. **Depend on `bread-utils` with the `bread-client` feature.** In your app's daemon (the long-running piece, if you have one — a short-lived CLI tool can use `bread-emit` instead, see below), add `bread-utils = { ..., features = ["bread-client"] }` and use `bread_utils::bread_client::BreadClient`:
   - `BreadClient::connect(app_id)` — cheap, cannot fail (there is no persistent connection to fail at construction time).
   - `client.emit(event, data)` — publish within your own `bread.<app_id>.*` namespace. Each call is its own short-lived connection (fire-and-forget, like `bread-emit`) — safe to call from a short-lived per-event process invocation, not just from inside a long-running loop.
   - `client.subscribe("bread.command.<app_id>.**", |event| { ... })` — receive commands addressed to you, on a background thread with its own reconnect/backoff loop.
3. **If you don't have a persistent daemon at all** (just a CLI tool invoked occasionally), skip `bread-client` entirely and shell out to `bread-emit` instead (see `bread-emit`'s own `--help`) — it's built for exactly that case (occasional callers that can't justify holding a socket open).
4. **Emit confirmations for commands you honor.** `bread.<app_id>.<verb>.done` or `.failed` after acting on a `bread.command.<app_id>.<verb>` — optional, but it's what lets a Lua workflow `bread.wait`/`bread.wait_any` for the real outcome instead of assuming success the moment it publishes a command.
5. **Write an `EVENTS.md`** in your app's own repo cataloguing every event you publish and every command verb you honor, with `data` shapes — the per-app companion to this file. Be honest about what's *not* implemented yet rather than stubbing a verb that does nothing (see breadclip's `EVENTS.md` for how it documents `pin`/`select` as deliberately deferred, not silently dropped).
6. **Make it opt-out, not opt-in-only, and fail silent.** Your app should work exactly the same whether breadd is installed or not — connecting/emitting/subscribing must never block, error, or crash your app just because the daemon is absent. `BreadClient` is built this way already (dropped no-op on a failed `emit`, transparent reconnect on `subscribe`); if you roll your own transport instead, keep that property.

---

## Dictionary: Runtime state schema

`bread state` and `bread.state.get("")` return the full `RuntimeState`:

```json
{
  "monitors": [
    { "name": "HDMI-A-1", "connected": true, "resolution": null, "position": null }
  ],
  "workspaces": [
    { "id": "1", "monitor": "HDMI-A-1" }
  ],
  "active_workspace": "1",
  "active_window": "0x...",
  "devices": {
    "connected": [
      {
        "id": "/sys/...",
        "name": "CalDigit TS4",
        "device": "dock",
        "subsystem": "usb",
        "vendor_id": "0x35f5",
        "product_id": "0x0104"
      }
    ]
  },
  "network": {
    "interfaces": { "eth0": { "up": true } },
    "online": true
  },
  "power": {
    "ac_connected": true,
    "battery_percent": 87,
    "battery_low": false
  },
  "profile": {
    "active": "default",
    "history": [],
    "profiles": {}
  },
  "modules": [
    {
      "name": "bread.monitors",
      "status": "loaded",
      "last_error": null,
      "builtin": true,
      "store": {}
    }
  ],
  "workflows": [
    {
      "name": "dock-connected",
      "state": "running",
      "step": "waiting for monitor",
      "started_at": 1710000000000,
      "updated_at": 1710000001500,
      "error": null
    }
  ],
  "widgets": [
    {
      "id": "weather.weather",
      "module": "weather",
      "placement": "left_of_stats",
      "order": 0,
      "visible": true,
      "tooltip": "Sydney: Partly cloudy",
      "root": {
        "type": "box",
        "orientation": "horizontal",
        "children": [
          { "type": "icon", "name": "cloud" },
          { "type": "label", "text": "22°C" }
        ]
      },
      "updated_at": 1710000001500
    }
  ]
}
```

`modules[].status` values: `loaded`, `load_error`, `not_found`, `degraded`, `disabled`. `workflows[].state` values: `running`, `done`, `failed`, `timed_out` *(Since: v1.2 — see [Workflows](#workflows-since-v12))*. `widgets[].placement` values: `tray`, `left_of_clock`, `right_of_clock`, `right_of_workspaces`, `left_of_stats` *(Since: v1.3 — see [Widgets](#widgets-since-v13))*.

---

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

| Method | Params | Description |
|--------|--------|-------------|
| `ping` | — | Connectivity check |
| `health` | — | Version, uptime, PID, adapter status, `api_version` |
| `state.get` | `key` (dotted path) | Read a value from `RuntimeState` |
| `state.dump` | — | Return the full `RuntimeState` as JSON |
| `modules.list` | — | List all loaded modules and their status |
| `modules.reload` | — | Hot-reload the Lua runtime |
| `profile.list` | — | List defined profiles |
| `profile.activate` | `name` | Switch active profile |
| `events.subscribe` | — | Upgrade to streaming mode; pushes events line by line |
| `events.replay` | `since_ms` | Replay buffered events from the last N ms |
| `emit` | `event`, `data`, optional `source`, `kind` | Inject an event. Without `source`, builds a `BreadEvent` directly tagged `System` (legacy path). With `source` set to `terminal`/`git`/`remote`, or a registered sibling-app id (see [Namespaces](#namespaces)), builds a real `RawEvent` (requires `kind` too) that goes through the normalizer like any adapter. Any other `source` value is rejected — this is the anti-spoofing boundary that stops a socket client from forging e.g. `power`/`hyprland` events. |
| `workflows.list` | — | List running/completed workflow instances and their step/status *(Since: v1.2)* |
| `widgets.list` | — | List all registered widgets across every module *(Since: v1.3)* |

The `health` response's `api_version` field lets a client — the CLI, a Lua module via `bread.exec`, or a `bread-client`-linked sibling app — assert compatibility with this document's versioned schema at connect time (see [API Stability & Versioning](#api-stability--versioning)).
