# Bread Documentation

## Contents

- [Overview](#overview)
- [Getting started](#getting-started)
- [Your first module](#your-first-module)
- [Run, reload, and watch](#run-reload-and-watch)
- [Modules: install and manage](#modules-install-and-manage)
- [Sync: snapshot and restore](#sync-snapshot-and-restore)
- [Debugging tips](#debugging-tips)
- [Dictionary: Lua API](#dictionary-lua-api)
  - [Bluetooth](#bluetooth)
- [Dictionary: Built-in modules](#dictionary-built-in-modules)
- [Dictionary: Event reference](#dictionary-event-reference)
- [Dictionary: Runtime state schema](#dictionary-runtime-state-schema)
- [Dictionary: IPC protocol](#dictionary-ipc-protocol)

## Overview

Bread is a reactive automation fabric for Linux desktops. The daemon (`breadd`) normalizes external signals into semantic events, maintains runtime state, and dispatches events to Lua modules that implement automation.

- **Daemon** (`breadd`) — long-running Rust process; source of truth for runtime state
- **Lua runtime** — dedicated thread inside the daemon; automation logic lives here
- **CLI** (`bread`) — talks to the daemon over a Unix socket

Adapters currently supported: Hyprland compositor IPC, Linux udev/netlink, UPower/sysfs power, rtnetlink/sysfs network, and BlueZ Bluetooth.

If you are new to Bread, start with the quick walkthrough below, then jump to the full dictionary when you need exact API details.

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

```bash
# Install from GitHub (downloads and extracts the default branch tarball)
bread modules install github:someuser/bread-wifi

# Install from a local directory
bread modules install ~/src/my-module

# Install a specific ref
bread modules install github:someuser/bread-wifi@v1.2.0

# List installed modules and their daemon status
bread modules list

# Show full manifest for one module
bread modules info bread-wifi

# Re-install all GitHub-sourced modules (pick up upstream changes)
bread modules update

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
source = "github:someuser/bread-wifi"
installed_at = "2026-01-01T00:00:00Z"
```

## Sync: snapshot and restore

Bread sync snapshots your config, dotfiles, and installed packages into a local Git repository. Use `export`/`import` to move state between machines — no git remote required.

```bash
# First-time setup (remote optional)
bread sync init
bread sync init --remote git@github.com:you/bread-config.git

# Commit local snapshot
bread sync push
bread sync push --message "before reinstall"

# Apply snapshot to this machine
bread sync pull

# Also reinstall packages from snapshot
bread sync pull --install-packages

# See what has changed
bread sync status
bread sync diff

# List known machines
bread sync machines
```

### Portable export/import

`export` creates a self-contained snapshot directory or `.tar.gz` — no git auth needed.

```bash
# Create a portable snapshot (defaults to ./bread-export-<machine>-<date>.tar.gz)
bread sync export

# Export to a specific path
bread sync export --output ~/backups/bread.tar.gz
bread sync export --output /mnt/usb/bread-snapshot/   # directory

# Apply a snapshot on another machine
bread sync import bread-export-hermes-2026-05-16.tar.gz
bread sync import /mnt/usb/bread-snapshot/

# Also install packages from the snapshot
bread sync import bread-export.tar.gz --install-packages

# Skip cloning git repos back to their original locations
bread sync import bread-export.tar.gz --no-clone-repos

# Skip confirmation prompt
bread sync import bread-export.tar.gz --yes
```

Each export snapshot includes:

| Directory | Contents |
|-----------|----------|
| `bread/` | `~/.config/bread/` (your Bread config) |
| `configs/` | Common app configs (hypr, nvim, kitty, waybar, fish, etc.) |
| `dotfiles/` | Individual files: `.gitconfig`, `.zshrc`, `.zprofile`, `.zshenv`, SSH config, etc. |
| `local-bin/` | `~/.local/bin/` scripts (non-symlink, <512 KB) |
| `local-fonts/` | `~/.local/share/fonts/` |
| `systemd/` | `~/.config/systemd/user/` units |
| `system/` | System files: udev rules, modprobe, sysctl, NetworkManager, bluetooth (root-only paths skipped unless run with sudo) |
| `packages/` | Package lists (pacman.txt, pip.txt, cargo.txt, npm.txt) |
| `machines/` | Per-machine profile with tags and last-sync time |
| `manifest.toml` | Path map for exact restoration on import |
| `restore.sh` | Shell script for manual restore (system files shown as sudo commands) |

**Git repository tracking:** at export time, all git repositories with remotes found under `~`, `~/Projects`, `~/Documents`, and `~/.config` are auto-committed and pushed. Their remote URLs and branches are recorded in the manifest. On import, `--no-clone-repos` suppresses cloning them back.

Configure sync in `~/.config/bread/sync.toml`:

```toml
[remote]
url = "git@github.com:you/bread-config.git"
branch = "main"

[machine]
name = "hermes"
tags = ["laptop", "battery"]

[packages]
enabled = true
managers = ["pacman", "pip", "cargo"]

[delegates]
include = ["~/.config/nvim", "~/.config/waybar"]
exclude = ["**/.git", "**/*.cache"]
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
Returns the machine name from `sync.toml`. Falls back to the system hostname if sync is not initialized.

#### `bread.machine.tags() -> string[]`
Returns the tags array from `sync.toml`, or `{}` if sync is not initialized.

#### `bread.machine.has_tag(tag) -> bool`
Returns true if the machine has the given tag.

#### `bread.fs.write(path, content)`
Write a file. Creates parent directories as needed. `~` is expanded.

#### `bread.fs.read(path) -> string | nil`
Read a file. Returns `nil` if the file does not exist. `~` is expanded.

#### `bread.fs.exists(path) -> bool`
Returns true if the path exists. `~` is expanded.

#### `bread.fs.expand(path) -> string`
Expand `~` to the home directory.

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

| Event | Data |
|-------|------|
| `bread.workspace.changed` | raw payload |
| `bread.workspace.created` | `{ workspace }` |
| `bread.workspace.destroyed` | `{ workspace }` |
| `bread.monitor.connected` | raw payload |
| `bread.monitor.disconnected` | raw payload |
| `bread.window.focus.changed` | raw payload |
| `bread.window.focused` | `{ address }` |
| `bread.window.opened` | `{ address, workspace, class, title }` |
| `bread.window.closed` | `{ address }` |
| `bread.window.moved` | `{ address, workspace }` |
| `bread.hyprland.event` | `{ kind, raw, data }` (unhandled kinds) |

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
  ]
}
```

`status` values: `loaded`, `load_error`, `not_found`, `degraded`, `disabled`.

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
| `health` | — | Version, uptime, PID, adapter status |
| `state.get` | `key` (dotted path) | Read a value from `RuntimeState` |
| `state.dump` | — | Return the full `RuntimeState` as JSON |
| `modules.list` | — | List all loaded modules and their status |
| `modules.reload` | — | Hot-reload the Lua runtime |
| `profile.list` | — | List defined profiles |
| `profile.activate` | `name` | Switch active profile |
| `events.subscribe` | — | Upgrade to streaming mode; pushes events line by line |
| `events.replay` | `since_ms` | Replay buffered events from the last N ms |
| `emit` | `event`, `data` | Inject a synthetic event into the pipeline |
| `sync.status` | — | Return sync init state: `{ initialized, machine?, remote? }` |
