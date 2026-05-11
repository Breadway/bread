# Bread

**A reactive automation fabric for Linux desktops.**

Bread is a modular desktop automation runtime built around a single idea: your desktop should behave like a programmable system, not a collection of disconnected config files.

Instead of scattering behavior across shell scripts, compositor configs, udev rules, and ad-hoc daemons, Bread centralizes runtime awareness into a coherent layer that can observe, interpret, and react to system state dynamically.

> **Status:** Early development. The daemon (`breadd`) is stable. The Lua automation API is under active development.

---

## How it works

Bread runs a long-lived daemon (`breadd`) that:

1. Ingests raw signals from your compositor, hardware, and OS
2. Normalizes them into stable, semantic events (`bread.device.dock.connected`, `bread.monitor.connected`, etc.)
3. Maintains a live model of your desktop state
4. Delivers those events to Lua modules that implement your automation

Your automation lives in Lua. You subscribe to events, read state, and call APIs:

```lua
bread.on("bread.device.dock.connected", function()
    bread.profile.activate("desk")
    bread.exec("waybar --config ~/.config/waybar/desk.jsonc")
end)

bread.on("bread.device.dock.disconnected", function()
    bread.profile.activate("default")
end)
```

---

## Architecture

```
breadd/          Rust daemon — event pipeline, state engine, IPC, adapter supervision
bread-cli/       CLI frontend — talks to breadd over a Unix socket
bread-shared/    Shared types — RawEvent, BreadEvent, AdapterSource
packaging/       Arch PKGBUILD and systemd user service
```

The daemon is structured in four layers:

- **Adapters** — interface with Hyprland IPC, udev, power state, and network interfaces
- **Normalizer** — transforms raw adapter signals into semantic Bread events
- **State engine** — maintains runtime state and dispatches events to subscribers
- **Lua runtime** — loads your modules, registers handlers, executes automation

---

## Requirements

- Linux (Arch recommended)
- Wayland compositor (Hyprland for full functionality)
- Rust toolchain (stable, 2021 edition)
- `udev` (standard on systemd systems)

Optional but preferred:
- UPower (for battery events via D-Bus rather than sysfs polling)
- rtnetlink (for network events; falls back to sysfs polling without it)

---

## Installation

### From source

```bash
git clone https://github.com/Breadway/bread.git
cd bread
```

Run the install script — it builds, installs to `/usr/bin`, sets up the systemd user service, and starts the daemon:

```bash
bash scripts/install.sh
```

Or do it step by step:

```bash
cargo build --release
sudo install -Dm755 target/release/breadd /usr/bin/breadd
sudo install -Dm755 target/release/bread /usr/bin/bread
```

### Arch Linux (PKGBUILD)

```bash
cd packaging/arch
makepkg -si
```

### systemd user service

```bash
mkdir -p ~/.config/systemd/user
cp packaging/systemd/breadd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now breadd
```

---

## Configuration

Bread reads from `~/.config/bread/breadd.toml`. All values are optional — the daemon runs with defaults if the file doesn't exist.

```toml
[daemon]
log_level = "info"   # trace | debug | info | warn | error

[lua]
entry_point = "~/.config/bread/init.lua"
module_path = "~/.config/bread/modules"

[adapters.hyprland]
enabled = true

[adapters.udev]
enabled = true
subsystems = ["usb", "input", "drm", "power_supply"]

[adapters.power]
enabled = true
poll_interval_secs = 30

[adapters.network]
enabled = true

[events]
dedup_window_ms = 100

[notifications]
default_timeout_ms = 5000
default_urgency = "normal"
notify_send_path = "notify-send"

[modules]
builtin = true               # load built-in modules (monitors, devices, etc.)
disable = []                 # list of built-in module names to disable
```

Your automation lives in `~/.config/bread/init.lua`:

```lua
-- ~/.config/bread/init.lua

require("modules.devices")
require("modules.workspaces")

bread.on("bread.system.startup", function()
    bread.profile.activate("default")
end)
```

---

## CLI reference

All commands communicate with the running daemon over a Unix socket at `$XDG_RUNTIME_DIR/bread/breadd.sock`.

```bash
bread reload                          # Hot-reload all Lua modules
bread reload --watch                  # Watch config dir and reload on changes
bread state                           # Dump full runtime state as JSON
bread events                          # Stream live normalized events
bread events --filter bread.device.*  # Stream filtered events
bread events --since 60               # Replay events from the last 60 seconds
bread modules                         # List loaded modules and status
bread profile-list                    # List defined profiles
bread profile-activate <name>         # Activate a named profile
bread emit <event> --data '{}'        # Manually fire an event (for testing)
bread ping                            # Check daemon connectivity
bread health                          # Daemon version, uptime, PID
bread doctor                          # Diagnose daemon and module health
```

---

## Event reference

Events follow the namespace convention `bread.<subsystem>.<noun>.<verb>`.

| Event | Trigger |
|-------|---------|
| `bread.system.startup` | Daemon fully initialized |
| `bread.device.connected` | Any device attached |
| `bread.device.disconnected` | Any device removed |
| `bread.device.changed` | Any device changed |
| `bread.device.<class>.connected` | Device attached by class |
| `bread.device.<class>.disconnected` | Device removed by class |
| `bread.device.<class>.changed` | Device changed by class |
| `bread.monitor.connected` | Display connected |
| `bread.monitor.disconnected` | Display disconnected |
| `bread.workspace.changed` | Active workspace changed |
| `bread.workspace.created` | Workspace created |
| `bread.workspace.destroyed` | Workspace destroyed |
| `bread.window.focus.changed` | Focused window changed |
| `bread.window.focused` | Focus moved (address only) |
| `bread.window.opened` | Window opened |
| `bread.window.closed` | Window closed |
| `bread.window.moved` | Window moved workspaces |
| `bread.power.ac.connected` | AC adapter plugged in |
| `bread.power.ac.disconnected` | AC adapter unplugged |
| `bread.power.battery.low` | Battery ≤ 20% |
| `bread.power.battery.very_low` | Battery ≤ 10% |
| `bread.power.battery.critical` | Battery ≤ 5% |
| `bread.power.battery.full` | Battery at 100% |
| `bread.power.changed` | Power state changed (fallback) |
| `bread.network.connected` | Network came online |
| `bread.network.disconnected` | Network went offline |
| `bread.profile.activated` | Profile switched via IPC |
| `bread.notify.sent` | Notification dispatched |
| `bread.state.changed.<path>` | State watch fired |
| `bread.hyprland.event` | Raw Hyprland event (unhandled kind) |

---

## Lua API

Full reference and usage notes live in [documentation.md](documentation.md). This section is a compact quick-reference to every API that exists today.

Practical walkthroughs and ports from existing Hyprland configs live in [Examples.md](Examples.md).

### Events

```lua
-- Subscribe to an event; returns a numeric ID
local id = bread.on("bread.monitor.connected", function(event)
    print(event.data.name)
end)

-- Unsubscribe by ID
bread.off(id)

-- Subscribe once, then auto-unsubscribe
bread.once("bread.system.startup", function(event)
    -- runs exactly once
end)

-- Subscribe with a predicate filter
bread.filter("bread.device.connected", function(event)
    return event.data.class == "keyboard"
end, function(event)
    bread.exec("xset r rate 200 40")
end)

-- Emit a custom event (for cross-module communication)
bread.emit("mymodule.something", { key = "value" })

-- Wait for an event (coroutine-only)
bread.spawn(function()
    local event = bread.wait("bread.device.dock.connected", { timeout = 5000 })
    if event then
        bread.log("dock arrived")
    end
end)
```

### State

```lua
-- Read a value from runtime state by dot-separated path
local monitors = bread.state.get("monitors")
local workspace = bread.state.get("active_workspace")
local power = bread.state.get("power")
local devices = bread.state.get("devices")

-- Watch a state key and fire on changes
bread.state.watch("active_workspace", function(new, old)
    print("workspace changed from " .. tostring(old) .. " to " .. tostring(new))
end)

-- Convenience helpers
local monitors = bread.state.monitors()
local active_ws = bread.state.active_workspace()
local active_win = bread.state.active_window()
local devices = bread.state.devices()
local power = bread.state.power()
local network = bread.state.network()
local profile = bread.state.profile()
```

### Profiles

```lua
bread.profile.activate("desk")
bread.profile.activate("default")
```

### Execution and notifications

```lua
-- Fire-and-forget: returns immediately, process runs in background
bread.exec("kitty")

-- Desktop notification
bread.notify("Dock connected", { urgency = "normal", timeout = 3000 })
```

### Timers

```lua
-- Run once after a delay (ms)
bread.after(500, function()
    bread.exec("some-delayed-command")
end)

-- Run on a repeating interval (ms); returns a timer ID
local id = bread.every(60000, function()
    bread.log("tick")
end)
bread.cancel(id)

-- Debounce a rapidly-firing handler
local fn = bread.debounce(200, function(event)
    reconfigure_monitors()
end)

-- Cancel a timer
local timer_id = bread.after(500, function() bread.exec("echo ready") end)
bread.cancel(timer_id)
```

### Logging

```lua
bread.log("Module loaded")
bread.warn("Unexpected state")
bread.error("Something failed")

### Hyprland

```lua
bread.hyprland.dispatch("workspace", "2")
bread.hyprland.keyword("monitor", "HDMI-A-1, 2560x1440, 0x0, 1")

local win = bread.hyprland.active_window()
local monitors = bread.hyprland.monitors()
local workspaces = bread.hyprland.workspaces()
local clients = bread.hyprland.clients()

-- Raw Hyprland event filtering (kind matches hyprland event name)
bread.hyprland.on_raw("openwindow", function(event)
    bread.log(event.data.raw)
end)
```

### Modules

```lua
local M = bread.module({ name = "my.module", version = "0.1.0", after = { "bread.devices" } })

function M.on_load()
    bread.on("bread.device.*", function(event)
        bread.log(event.event)
    end)
end

function M.on_unload()
    bread.log("unloaded")
end

M.store.set("last_seen", os.time())
local last = M.store.get("last_seen")

return M
```
```

---

## IPC protocol

The daemon exposes a Unix socket at `$XDG_RUNTIME_DIR/bread/breadd.sock`. The protocol is newline-delimited JSON — useful for scripting or building tooling outside the CLI.

Request:
```json
{ "id": "1", "method": "state.get", "params": { "key": "monitors" } }
```

Response:
```json
{ "id": "1", "result": [ { "name": "HDMI-A-1", "connected": true } ] }
```

Available methods: `ping`, `health`, `state.get`, `state.dump`, `modules.list`, `modules.reload`, `profile.list`, `profile.activate`, `events.subscribe`, `events.replay`, `emit`.

`events.subscribe` upgrades the connection to a streaming mode — the daemon pushes events line by line until the client disconnects.

---

## Contributing

Bread is early-stage software. Contributions, issues, and feedback are welcome.

The daemon (`breadd`) is the most stable part of the codebase. The Lua API surface is where most active development is happening.

---

## License

MIT — see [LICENSE](LICENSE).
