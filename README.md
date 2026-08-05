# Bread

**A reactive automation fabric for Linux desktops.**

Bread is a modular desktop automation runtime built around a single idea: your desktop should behave like a programmable system, not a collection of disconnected config files.

Instead of scattering behavior across shell scripts, compositor configs, udev rules, and ad-hoc daemons, Bread centralizes runtime awareness into a coherent layer that can observe, interpret, and react to system state dynamically.

> **Status:** Early development. The daemon (`breadd`) is stable. The Lua automation API is active and feature-complete for daily use.

---

## How it works

Bread runs a long-lived daemon (`breadd`) that:

1. Ingests raw signals from your compositor, hardware, and OS
2. Normalizes them into stable, semantic events (`bread.device.dock.connected`, `bread.hyprland.monitor.connected`, etc.)
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
packaging/       Arch PKGBUILD and systemd user service
```

The daemon is structured in four layers:

- **Adapters** — interface with Hyprland IPC, udev, power state, network interfaces, and Bluetooth (BlueZ)
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
- BlueZ (for Bluetooth device events and control)

---

## Installation

### From source

```bash
git clone https://git.breadway.dev/Breadway/bread.git
cd bread
```

Run the install script — it builds, symlinks `breadd` and `bread` into `~/.local/bin` (override with `BIN_DIR=…`), installs the systemd user service, and starts the daemon:

```bash
bash scripts/install.sh
```

Or step by step (system-wide install):

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

[adapters.bluetooth]
enabled = true

[events]
dedup_window_ms = 100

[compat]
legacy_hyprland_event_names = true   # dual-emits bread.hyprland.* alongside legacy flat names; see Documentation.md

[notifications]
default_timeout_ms = 5000
default_urgency = "normal"
notify_send_path = "notify-send"

[modules]
builtin = true    # load built-in modules (monitors, devices, workspaces, binds, rules)
disable = []      # list of built-in module names to disable
```

For the common "when event X happens, do Y" case, you don't need Lua at
all — drop rules straight into `~/.config/bread/rules.toml` and skip
`init.lua` entirely:

```toml
# ~/.config/bread/rules.toml
[[rule]]
on = "device.dock.connected"
run = "~/.config/bread/scripts/dock-connected.sh"

[[rule]]
on = "power.ac.disconnected"
notify = "Unplugged"
```

It's optional and purely additive alongside `init.lua` — see
[Getting started in Documentation.md](Documentation.md#getting-started) for
the full schema (`run` vs `exec` vs `notify`, wildcard `on` patterns, and
how a malformed rule surfaces via `bread doctor`).

For anything beyond a single action per event, your automation lives in
`~/.config/bread/init.lua`. Modules placed in `~/.config/bread/modules/` are
auto-loaded after `init.lua`:

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
bread doctor --json                   # Output raw JSON

# Lua runtime
bread reload                          # Hot-reload all Lua modules
bread reload --watch                  # Watch config dir and reload on changes

# State and events
bread state                           # Dump full runtime state as JSON
bread state network                   # Read a single path from state
bread state --json                    # Output raw JSON
bread events                          # Stream live normalized events
bread events bread.device.*           # Stream filtered events
bread events --since 60               # Replay events from the last 60 seconds
bread events --fields event,data      # Limit output to specific fields
bread events --json                   # Output raw JSON
bread events --tree                   # Render as a causality tree (caused_by) instead of a flat stream
bread emit <event>                    # Manually fire an event (for testing)

# Profiles
bread profile-list                    # List defined profiles
bread profile-activate <name>         # Activate a named profile

# Modules
bread modules list                    # List installed modules and daemon status
bread modules install /local/path     # Install from a local module directory
bread modules remove <name>           # Remove an installed module (--yes skips confirmation)
bread modules info <name>             # Show full manifest and daemon status
bread modules audit <name>            # Scan a module's Lua source and suggest a [[permissions]] block

# Hooks
bread hooks install-shell [shell]     # Install precmd/preexec/chpwd shell hooks (auto-detects $SHELL)
bread hooks install-git               # Install git hooks (post-commit/checkout/merge) in the current repo
```

---

## Module system

Modules are Lua files (or directories) installed to `~/.config/bread/modules/`. Each module must declare itself with `bread.module()` and have a `bread.module.toml` manifest.

### Installing modules

Modules install from a local directory only. By default a module runs
in-process with full, ungated `bread.exec()` privileges — the same trust
model as before — so to use a module published on a git host, clone it
yourself and review the Lua before installing from the local checkout.
A module can opt into a smaller, enforced footprint by declaring
`[[permissions]]` in its manifest: it then runs out-of-process under an
OS-level (Landlock) sandbox limited to exactly what it declared, with a
`bread` table that only exposes the granted namespaces. See
[Capability-scoped modules](Documentation.md#capability-scoped-modules-since-v15) and
[Out-of-process module sandboxing](Documentation.md#out-of-process-module-sandboxing-since-v16)
for the full permission taxonomy and what's enforced at the kernel level
versus what isn't yet.

```bash
git clone https://github.com/someuser/bread-wifi ~/src/bread-wifi
# review ~/src/bread-wifi, then:
bread modules install ~/src/bread-wifi
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
source = "/home/you/src/bread-wifi"
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

## Event reference, Lua API, and IPC protocol

These are fully documented in [`Documentation.md`](Documentation.md) — the single canonical reference (event catalogue, per-function Lua API, runtime-state schema, and IPC protocol), versioned as **Bread Automation API v1**. This README no longer keeps a parallel copy, to avoid the two drifting apart.

- [Dictionary: Event reference](Documentation.md#dictionary-event-reference)
- [Dictionary: Lua API](Documentation.md#dictionary-lua-api)
- [Dictionary: Runtime state schema](Documentation.md#dictionary-runtime-state-schema)
- [Dictionary: IPC protocol](Documentation.md#dictionary-ipc-protocol)
- [API Stability & Versioning](Documentation.md#api-stability--versioning)

---

## Contributing

Bread is early-stage software. Contributions, issues, and feedback are welcome.

The daemon (`breadd`) is the most stable part of the codebase. Active development is happening across the Lua API and module system.

---

## License

MIT — see [LICENSE](LICENSE).
