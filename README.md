# Bread

**A reactive automation fabric for Linux desktops.**

Bread is a modular desktop automation runtime built around a single idea: your desktop should behave like a programmable system, not a collection of disconnected config files.

Instead of scattering behavior across shell scripts, compositor configs, udev rules, and ad-hoc daemons, Bread centralizes runtime awareness into a coherent layer that can observe, interpret, and react to system state dynamically.

> **Status:** Early development. The daemon (`breadd`) is stable. The Lua automation API is active and feature-complete for daily use.

---

## How it works

Bread runs a long-lived daemon (`breadd`) that:

1. Ingests raw signals from your compositor, hardware, and OS
2. Normalizes them into stable, semantic events (`bread.device.dock.connected`, `bread.monitor.connected`, etc.)
3. Maintains a live model of your desktop state
4. Delivers those events to Lua modules that implement your automation

Your automation lives in Lua. You subscribe to events, read state, and call APIs:

```lua
local M = bread.module({ name = "dock", version = "1.0.0" })

bread.on("bread.device.dock.connected", function(event)
    bread.profile.activate("desk")
    bread.exec("waybar --config ~/.config/waybar/desk.jsonc")
    bread.notify("Dock connected", { urgency = "low" })
end)

bread.on("bread.device.dock.disconnected", function(event)
    bread.profile.activate("default")
end)

return M
```

---

## Architecture

```
breadd/          Rust daemon — event pipeline, state engine, IPC, adapter supervision
bread-cli/       CLI frontend — talks to breadd over a Unix socket
bread-shared/    Shared types — RawEvent, BreadEvent, AdapterSource
bread-sync/      Sync engine — snapshot and restore system state via a Git remote
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

Or step by step:

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
builtin = true    # load built-in modules (monitors, devices, workspaces, binds)
disable = []      # list of built-in module names to disable
```

Your automation lives in `~/.config/bread/init.lua`. Modules placed in `~/.config/bread/modules/` are auto-loaded after `init.lua`:

```lua
-- ~/.config/bread/init.lua

bread.on("bread.system.startup", function(event)
    bread.profile.activate("default")
end)
```

---

## CLI reference

All commands communicate with the running daemon over a Unix socket at `$XDG_RUNTIME_DIR/bread/breadd.sock`.

```bash
# Daemon
bread ping                            # Check daemon connectivity
bread health                          # Daemon version, uptime, PID
bread doctor                          # Diagnose daemon and module health

# Lua runtime
bread reload                          # Hot-reload all Lua modules
bread reload --watch                  # Watch config dir and reload on changes

# State and events
bread state                           # Dump full runtime state as JSON
bread events                          # Stream live normalized events
bread events bread.device.*           # Stream filtered events
bread events --since 60               # Replay events from the last 60 seconds
bread emit <event>                    # Manually fire an event (for testing)

# Profiles
bread profile-list                    # List defined profiles
bread profile-activate <name>         # Activate a named profile

# Modules
bread modules list                    # List installed modules and daemon status
bread modules install github:user/repo  # Install from GitHub
bread modules install /local/path     # Install from a local directory
bread modules remove <name>           # Remove an installed module
bread modules update [name]           # Re-install one or all GitHub-sourced modules
bread modules info <name>             # Show full manifest and daemon status

# Sync
bread sync init                       # Initialize sync for this machine
bread sync push                       # Snapshot and push current state to remote
bread sync pull                       # Pull and apply latest state from remote
bread sync pull --install-packages    # Also install packages from snapshot
bread sync status                     # Show what has changed since last push
bread sync diff                       # Show file-level diff vs last commit
bread sync diff --remote              # Show diff vs remote
bread sync machines                   # List known machines from sync repo
```

---

## Module system

Modules are Lua files (or directories) installed to `~/.config/bread/modules/`. Each module must declare itself with `bread.module()` and have a `bread.module.toml` manifest.

### Installing modules

```bash
# From GitHub (downloads latest release tarball)
bread modules install github:someuser/bread-wifi

# From a local path
bread modules install ~/src/my-module

# From a specific ref
bread modules install github:someuser/bread-wifi@v1.2.0
```

### Writing a module

A module directory looks like:

```
~/.config/bread/modules/
└── wifi/
    ├── bread.module.toml    ← required manifest
    └── init.lua             ← entry point
```

`bread.module.toml`:
```toml
name = "wifi"
version = "1.0.0"
description = "WiFi management for Bread"
author = "someuser"
source = "github:someuser/bread-wifi"
installed_at = "2026-01-01T00:00:00Z"
```

`init.lua`:
```lua
local M = bread.module({ name = "wifi", version = "1.0.0" })

bread.on("bread.network.connected", function(event)
    bread.log("Network up: " .. (event.data.interface or "unknown"))
end)

return M
```

---

## Sync system

Bread sync snapshots your entire setup — Bread config, arbitrary dotfiles, and package lists — and stores it in a Git repository. Pull it on another machine to restore.

```bash
# First-time setup
bread sync init --remote git@github.com:you/bread-config.git

# Push current state
bread sync push

# On another machine: pull and apply
bread sync pull

# Check what's pending
bread sync status
```

Configure what gets synced in `~/.config/bread/sync.toml`:

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

The sync repo stores:

```
sync-repo/
├── bread/          ← ~/.config/bread/ snapshot
├── configs/        ← delegate paths (nvim, waybar, etc.)
├── machines/       ← per-machine profiles
└── packages/       ← package snapshots (pacman.txt, pip.txt, etc.)
```

---

## Event reference

Events follow the namespace convention `bread.<subsystem>.<noun>.<verb>`.

| Event | Trigger |
|-------|---------|
| `bread.system.startup` | Daemon fully initialized |
| `bread.device.connected` | Any device attached |
| `bread.device.disconnected` | Any device removed |
| `bread.device.<device>.connected` | Named device attached (name from `devices.lua`) |
| `bread.device.<device>.disconnected` | Named device removed |
| `bread.monitor.connected` | Display connected |
| `bread.monitor.disconnected` | Display disconnected |
| `bread.workspace.changed` | Active workspace changed |
| `bread.window.focus.changed` | Focused window changed |
| `bread.window.opened` | Window opened |
| `bread.window.closed` | Window closed |
| `bread.power.ac.connected` | AC adapter plugged in |
| `bread.power.ac.disconnected` | AC adapter unplugged |
| `bread.power.battery.low` | Battery ≤ 20% |
| `bread.power.battery.very_low` | Battery ≤ 10% |
| `bread.power.battery.critical` | Battery ≤ 5% |
| `bread.power.battery.full` | Battery at 100% |
| `bread.network.connected` | Network interface came online |
| `bread.network.disconnected` | Network interface went offline |
| `bread.profile.activated` | Profile switched |
| `bread.notify.sent` | Desktop notification dispatched |

---

## Lua API

### Modules

Every module file must declare itself. The declaration is used for dependency ordering and status tracking.

```lua
local M = bread.module({
    name    = "my-module",
    version = "1.0.0",
    after   = { "bread.devices" },   -- load after this module
})

-- ... module body ...

return M
```

### Events

```lua
-- Subscribe to events; returns a subscription ID
local id = bread.on("bread.monitor.connected", function(event)
    -- event.event   → "bread.monitor.connected"
    -- event.data    → table of event-specific fields
    -- event.source  → adapter that produced it
    bread.log(event.event)
end)

-- Unsubscribe by ID
bread.off(id)

-- Subscribe once, auto-unsubscribe after first delivery
bread.once("bread.system.startup", function(event)
    bread.profile.activate("default")
end)

-- Subscribe with a filter predicate
bread.filter("bread.device.connected", function(event)
    return event.data.device == "keyboard"
end, function(event)
    bread.exec("xset r rate 200 40")
end)

-- Emit a custom event (for cross-module communication)
bread.emit("mymodule.something", { key = "value" })
```

Pattern matching supports `*` (single segment), `**` (any depth), and `?` (single character):
```lua
bread.on("bread.device.*", handler)   -- matches bread.device.dock.connected
bread.on("bread.device.**", handler)  -- matches any depth under bread.device
```

### State

```lua
-- Read from runtime state by dot-separated path
local monitors = bread.state.get("monitors")
local online   = bread.state.get("network.online")

-- Typed shorthands
local monitors  = bread.state.monitors()
local workspace = bread.state.active_workspace()
local window    = bread.state.active_window()
local devices   = bread.state.devices()
local power     = bread.state.power()
local network   = bread.state.network()
local profile   = bread.state.profile()

-- Watch a state path for changes
bread.state.watch("power.ac_connected", function(new_val, old_val)
    if new_val then
        bread.notify("AC connected")
    end
end)
```

### Profiles

```lua
bread.profile.activate("desk")
bread.profile.activate("default")
```

### Execution and notifications

```lua
-- Fire-and-forget shell command
bread.exec("kitty")

-- Desktop notification (uses notify-send)
bread.notify("Title", { urgency = "normal", timeout = 3000, icon = "dialog-info" })
bread.notify("Simple message")   -- title defaults to "bread"
```

### Timers

```lua
-- Run once after a delay (ms)
local id = bread.after(500, function()
    bread.exec("some-delayed-command")
end)

-- Run on a repeating interval (ms)
local id = bread.every(60000, function()
    bread.log("tick")
end)

-- Cancel either kind
bread.cancel(id)

-- Debounce a rapidly-firing handler
local fn = bread.debounce(200, function(event)
    reconfigure_monitors()
end)
bread.on("bread.monitor.*", fn)
```

### Wait (inside coroutines)

```lua
-- Yield until a matching event arrives
local event = bread.wait("bread.device.dock.connected", { timeout = 5000 })
if event then
    -- dock arrived within 5 seconds
end
```

### Machine and filesystem

```lua
-- Machine identity (from sync.toml, falls back to hostname)
local name = bread.machine.name()
local tags = bread.machine.tags()       -- array of strings
local ok   = bread.machine.has_tag("laptop")

-- Filesystem helpers (~ is expanded)
bread.fs.write("~/.config/some/file", "content")
local content = bread.fs.read("~/.config/some/file")   -- nil if not found
local exists  = bread.fs.exists("~/some/path")
local abs     = bread.fs.expand("~/some/path")
```

### Logging

```lua
bread.log("Module loaded")     -- info level
bread.warn("Unexpected state") -- warn level
bread.error("Something failed") -- error level
```

### Hyprland bindings

```lua
-- Dispatch a Hyprland command
bread.hyprland.dispatch("workspace", "2")
bread.hyprland.dispatch("exec", "kitty")

-- Set a keyword
bread.hyprland.keyword("monitor", "HDMI-A-1, 2560x1440, 0x0, 1")

-- Query compositor state
local win        = bread.hyprland.active_window()
local monitors   = bread.hyprland.monitors()
local workspaces = bread.hyprland.workspaces()
local clients    = bread.hyprland.clients()

-- Subscribe to raw Hyprland events (bypass normalization)
bread.hyprland.on_raw("activewindow", function(raw)
    -- raw is the unparsed string from Hyprland's event socket
end)
```

### Module-scoped storage

Survives hot reload; does not survive daemon restart.

```lua
M.store.set("last_profile", "docked")
local p = M.store.get("last_profile")  -- "docked"
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

Available methods:

| Method | Description |
|--------|-------------|
| `ping` | Connectivity check |
| `health` | Version, uptime, PID, adapter status |
| `state.get` | Read a value from `RuntimeState` by dotted key path |
| `state.dump` | Return the full `RuntimeState` as JSON |
| `modules.list` | List all loaded modules and their status |
| `modules.reload` | Hot-reload the Lua runtime |
| `profile.list` | List defined profiles |
| `profile.activate` | Switch active profile |
| `events.subscribe` | Upgrade connection to streaming mode |
| `events.replay` | Replay buffered events from the last N ms |
| `emit` | Inject a synthetic event into the pipeline |
| `sync.status` | Return sync initialization state and machine info |

`events.subscribe` upgrades the connection to streaming mode — the daemon pushes events line by line until the client disconnects.

---

## Contributing

Bread is early-stage software. Contributions, issues, and feedback are welcome.

The daemon (`breadd`) is the most stable part of the codebase. Active development is happening across the Lua API, module system, and sync subsystem.

---

## License

MIT — see [LICENSE](LICENSE).
