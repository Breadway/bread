# Bread — Architecture & Vision
### A Reactive Automation Fabric for Linux Desktops

---

## Overview

Bread is a modular desktop automation fabric for Linux systems, built around a single guiding principle:

> The desktop should behave like a programmable runtime, not a collection of disconnected configuration files.

Most advanced Linux setups are a patchwork — compositor config here, a udev rule there, a handful of shell scripts duct-taped together with cron jobs and `~/.profile` hacks. They work until they don't, they're hard to reason about, and they share no understanding of what the system is actually doing at runtime.

Bread replaces that patchwork with a coherent layer: a long-running Rust daemon that tracks system state, normalizes hardware and compositor events into semantic signals, and exposes a Lua API for writing automation that actually knows what's going on.

**Bread provides:**

- A reactive runtime daemon (`breadd`) written in Rust
- A Lua-driven automation and configuration layer
- A normalized event model that abstracts raw Linux signals
- A unified runtime state interface for advanced desktop workflows
- A first-class CLI for introspection, debugging, and live control

---

## Core Philosophy

### The Problem

A modern Linux power-user desktop typically involves:

- Compositor configuration (Hyprland, Sway, etc.)
- Monitor hotplug scripts
- udev rules for input devices and USB
- Workspace layout logic
- Keybinding layers
- Network state hooks
- Power management scripts
- Application launchers and session managers
- Status bar integrations
- Machine-specific environment hacks

Each of these subsystems is implemented independently. None of them share a common understanding of runtime state. If your dock connects, your monitor script doesn't know your workspace manager ran, your workspace manager doesn't know your keybindings changed, and your keybindings don't know you're now in "desk mode." Everything is blind to everything else.

### The Solution

Bread introduces a shared runtime daemon as the connective tissue between these systems. Rather than each subsystem operating in isolation, Bread:

1. **Ingests** raw signals from the OS, compositor, and hardware
2. **Normalizes** them into semantic desktop events
3. **Maintains** a canonical view of runtime state
4. **Exposes** that state and those events to Lua automation modules

The goal is not to replace the kernel, systemd, Hyprland, or your package manager. Bread exists *between* the operating system, the compositor, connected hardware, and the user — as an orchestration and automation fabric.

---

## Architecture

Bread is organized into four primary layers that process system signals from raw input to user behavior.

```
┌─────────────────────────────────────────────────────────────┐
│                        Lua Modules                          │
│          (automation, bindings, profiles, behavior)         │
├─────────────────────────────────────────────────────────────┤
│                    Lua Runtime API                          │
│        (bread.on, bread.state, bread.hypr, bread.exec)      │
├─────────────────────────────────────────────────────────────┤
│                  Runtime State Engine                       │
│     (event normalization, state tracking, subscriptions)    │
├──────────────────┬──────────────────┬───────────────────────┤
│  Hyprland IPC    │   udev / kernel  │   System interfaces   │
│    Adapter       │     Adapter      │      (net, power)      │
└──────────────────┴──────────────────┴───────────────────────┘
```

### Layer 1: Runtime Adapters

Adapters are the boundary between Bread and the outside world. Each adapter interfaces with a specific external system and translates its raw output into a form the daemon can process.

Adapters handle:

- **Hyprland IPC** — workspace changes, monitor events, focus changes, window lifecycle
- **udev** — device attachment and removal (keyboards, mice, docks, USB peripherals)
- **Power state** — battery level, AC adapter state, suspend/resume
- **Monitor topology** — hotplug detection, EDID, display arrangement
- **Network interfaces** — link state changes, connection events

Adapters contain no automation logic. Their only job is ingestion and forwarding.

### Layer 2: Runtime State Engine

The state engine is the core of `breadd`. It is the canonical source of truth for everything Bread knows about the system at any given moment.

Responsibilities:

- **Normalize** raw adapter events into semantic Bread events
- **Track** runtime topology (monitors, workspaces, devices, network, power)
- **Manage** module subscriptions and event dispatch
- **Coordinate** Lua module execution and hot reload
- **Expose** structured state queries over IPC

The normalization step is one of Bread's defining features. Raw Linux signals are often low-level, fragmented, and hardware-specific. The state engine transforms them into stable, meaningful events that modules can rely on.

**Example: USB-C dock connection**

Raw udev event:
```json
{ "source": "udev", "action": "add", "device": "/dev/input/event12", "subsystem": "usb" }
```

Normalized Bread event:
```json
{ "event": "bread.device.dock.connected", "device": "USB-C Dock", "timestamp": 1718000000 }
```

Modules never see the raw event. They only see the semantic one.

### Layer 3: Lua Automation Runtime

The Lua runtime is where user behavior lives. Modules subscribe to events from the state engine and implement desktop automation using Bread's API.

```lua
bread.on("bread.device.dock.connected", function(event)
    bread.profile.activate("desk")
    bread.hypr.dispatch("workspace 1")
    bread.exec("kitty")
    bread.notify("Desk mode active")
end)
```

The runtime provides:

- Event subscriptions with optional filtering predicates
- Desktop APIs (Hyprland, exec, notifications, state access)
- Profile management (named environment contexts)
- Utility helpers (timers, debounce, logging)
- Live reload support — modules can be reloaded without restarting the daemon

Modules interact with the system exclusively through Bread's APIs. The daemon handles all low-level coordination; modules express intent.

### Layer 4: CLI Interface

The CLI (`bread`) is the operator interface for the daemon. It provides runtime introspection, debugging, and control without requiring a GUI.

```bash
bread reload              # Hot-reload all Lua modules
bread state               # Dump current runtime state
bread events              # Stream live normalized events
bread modules             # List loaded modules and status
bread profile list        # Show available profiles
bread profile activate desk  # Switch active profile
bread doctor              # Diagnose configuration and daemon health
bread emit <event>        # Manually fire an event (for testing)
```

The `bread emit` command is particularly useful during module development — it lets you trigger any event without having to physically plug in hardware.

---

## Technology Stack

| Component | Language | Rationale |
|-----------|----------|-----------|
| Runtime daemon (`breadd`) | Rust | Safety, performance, predictable concurrency |
| Module system | Lua | Rapid iteration, live reload, user extensibility |
| User configuration | Lua | Unified with module layer; no separate config DSL |
| Automation runtime | Lua | Behavioral layer of the desktop |
| IPC transport | JSON over Unix sockets | Simple, debuggable, language-agnostic |
| Hyprland integration | Native Lua + IPC | Leverages Hyprland's native Lua config support |
| CLI frontend | Rust binary | Fast startup, direct daemon communication |

### Why Rust + Lua

Bread intentionally separates runtime infrastructure from user behavior. This split is fundamental to the architecture.

**Rust** owns everything that must be reliable: the daemon lifecycle, event ingestion, normalization, IPC, subscription management, module loading, concurrency, and hot reload orchestration. Rust's safety guarantees and performance characteristics make it the right choice for a long-running daemon that must never crash or leak.

**Lua** owns everything that must be flexible: configuration, automation logic, bindings, event handlers, and desktop behavior. Lua enables rapid iteration, live reload, and a low barrier for user customization. Bread treats Lua as the behavioral scripting layer of the desktop — expressive, dynamic, and immediately reloadable.

The two layers communicate through a well-defined API boundary. Lua calls into Rust-backed functions; Rust dispatches events into the Lua runtime. Neither layer bleeds into the other's concerns.

---

## Event Model

Events are the primary communication mechanism in Bread. Understanding the event model is essential to understanding how automation works.

### Event Structure

All normalized Bread events share a common envelope:

```json
{
    "event": "bread.monitor.connected",
    "timestamp": 1718000000,
    "source": "udev",
    "data": {
        "name": "HDMI-A-1",
        "resolution": "2560x1440",
        "position": "right"
    }
}
```

### Event Namespacing

Events follow a dot-separated namespace convention: `bread.<subsystem>.<noun>.<verb>`.

| Namespace | Events |
|-----------|--------|
| `bread.device.*` | `dock.connected`, `keyboard.connected`, `mouse.removed` |
| `bread.monitor.*` | `connected`, `disconnected`, `layout.changed` |
| `bread.workspace.*` | `changed`, `created`, `destroyed` |
| `bread.power.*` | `ac.connected`, `battery.low`, `suspend`, `resume` |
| `bread.network.*` | `connected`, `disconnected`, `interface.up` |
| `bread.profile.*` | `activated`, `deactivated` |
| `bread.system.*` | `startup`, `shutdown`, `reload` |

### Filtered Subscriptions

Modules can subscribe to broad event patterns or use predicate filters for precision:

```lua
-- Subscribe to all device events
bread.on("bread.device.*", function(event)
    bread.log("Device event: " .. event.event)
end)

-- Subscribe with a filter predicate
bread.on("bread.device.keyboard.connected", function(event)
    if event.data.name == "Keychron K2" then
        bread.exec("xset r rate 200 40")
    end
end)

-- One-shot subscription (fires once, then unregisters)
bread.once("bread.system.startup", function()
    bread.profile.activate("default")
end)
```

### Custom Events

Modules can emit custom events, allowing cross-module communication without direct coupling:

```lua
-- In module A: emit a custom event
bread.emit("myconfig.mode.gaming", { fps_target = 144 })

-- In module B: react to it
bread.on("myconfig.mode.gaming", function(event)
    bread.exec("gamemode -r")
end)
```

---

## Profile System

Profiles are a first-class primitive in Bread. A profile is a named desktop context — a coherent set of behaviors, bindings, and configurations that apply when certain conditions are met.

```lua
bread.profile.define("desk", {
    description = "USB-C dock connected, external monitors active",

    on_activate = function()
        bread.hypr.keyword("monitor HDMI-A-1,2560x1440,0x0,1")
        bread.hypr.keyword("monitor eDP-1,preferred,2560x0,1.5")
        bread.exec("waybar --config ~/.config/waybar/desk.jsonc")
        bread.notify("Desk mode")
    end,

    on_deactivate = function()
        bread.hypr.keyword("monitor HDMI-A-1,disabled")
        bread.exec("pkill waybar && waybar")
        bread.notify("Laptop mode")
    end
})

-- Automatically activate based on hardware state
bread.on("bread.device.dock.connected", function()
    bread.profile.activate("desk")
end)

bread.on("bread.device.dock.disconnected", function()
    bread.profile.activate("default")
end)
```

Profiles can be stacked, nested, or switched manually via the CLI. They give automation a clear semantic structure — rather than writing ad-hoc scripts for each scenario, you define what "desk mode" means once and trigger it from anywhere.

---

## Module System

Bread is fully modular. All automation, integrations, and desktop behavior live in Lua modules loaded by the daemon.

### Module Structure

```
~/.config/bread/modules/
├── devices.lua          # Hardware device handling
├── workspaces.lua       # Workspace layout logic
├── profiles/
│   ├── desk.lua         # Desk profile definition
│   └── travel.lua       # Travel profile definition
└── apps/
    └── dev-session.lua  # Development environment setup
```

### Module Metadata

Modules declare their identity and dependencies in a metadata block:

```lua
return {
    name = "devices",
    version = "1.0.0",
    description = "Hardware device automation",

    depends = {
        "hypr",
        "notifications"
    },

    on_load = function()
        bread.log("Device module loaded")
    end,

    on_unload = function()
        -- Clean up subscriptions, timers, etc.
    end
}
```

### Module Lifecycle

The daemon manages the full module lifecycle:

1. **Discovery** — scan `~/.config/bread/modules/` for Lua files
2. **Dependency resolution** — topological sort based on `depends`
3. **Loading** — initialize each module's Lua environment
4. **Event wiring** — register all `bread.on` subscriptions
5. **Hot reload** — on `bread reload`, unload and reload modules in dependency order without restarting the daemon

Modules are currently trusted and unrestricted. Security sandboxing is not a V1 goal but is noted as a future consideration.

---

## Lua Runtime API

### Event API

```lua
-- Subscribe to an event
bread.on("bread.monitor.connected", function(event)
    print(event.data.name)
end)

-- Subscribe once
bread.once("bread.system.startup", function() end)

-- Emit a custom event
bread.emit("mymodule.something.happened", { key = "value" })

-- Unsubscribe by handle
local handle = bread.on("bread.device.*", handler)
handle:cancel()
```

### State API

```lua
-- Read runtime state
local monitors = bread.state.get("monitors")
local workspace = bread.state.get("workspace.active")
local devices = bread.state.get("devices.connected")
local profile = bread.state.get("profile.active")

-- Watch a state value for changes
bread.state.watch("workspace.active", function(new, old)
    print("Switched from workspace " .. old .. " to " .. new)
end)
```

### Hyprland API

```lua
-- Dispatch Hyprland commands
bread.hypr.dispatch("workspace 2")
bread.hypr.dispatch("movetoworkspace 3")

-- Set Hyprland keywords (monitor config, etc.)
bread.hypr.keyword("monitor HDMI-A-1,preferred,0x0,1")
bread.hypr.keyword("general:gaps_out = 10")

-- Query Hyprland state
local clients = bread.hypr.clients()
local monitors = bread.hypr.monitors()
```

### Utility API

```lua
-- Execute a process
bread.exec("kitty")
bread.exec("notify-send 'Hello'")

-- Send a desktop notification
bread.notify("Dock connected", { urgency = "normal", timeout = 3000 })

-- Logging
bread.log("Module initialized")
bread.warn("Something unexpected happened")

-- Timers
local timer = bread.after(500, function()   -- run once after 500ms
    bread.exec("some-delayed-command")
end)

local interval = bread.every(60000, function()  -- run every 60s
    bread.state.refresh("network")
end)

-- Debounce (useful for rapid hardware events)
local handler = bread.debounce(200, function(event)
    reconfigure_monitors()
end)
```

---

## Runtime State

The daemon maintains a live, structured model of the desktop at all times. This is what makes Bread's automation context-aware rather than purely reactive to isolated events.

**Tracked state includes:**

| Domain | State |
|--------|-------|
| Displays | Connected monitors, resolution, position, refresh rate |
| Workspaces | Active workspace, workspace list, window assignments |
| Devices | Connected keyboards, mice, docks, USB peripherals |
| Network | Interface state, active connections |
| Power | Battery level, charging state, AC status |
| Profiles | Active profile, profile history |
| Hyprland | Active window, client list, monitor config |
| Modules | Loaded modules, load status, error state |

State is live — it reflects the current system, not a snapshot. Modules read state synchronously; the daemon updates it as events arrive.

---

## Hot Reload

Hot reload is a core design requirement, not an afterthought.

The daemon persists across reloads. Only the Lua layer reloads:

- Lua modules
- Configuration
- Bindings and event handlers
- Profile definitions
- Hyprland integration scripts

Reload is triggered by `bread reload` or by file system watch (if enabled). The daemon:

1. Calls `on_unload` for each loaded module in reverse dependency order
2. Clears all event subscriptions and timers registered by Lua
3. Re-evaluates all Lua module files
4. Calls `on_load` for each module in dependency order
5. Re-registers all `bread.on` subscriptions

The result: you can edit a module, run `bread reload`, and see the effect immediately — without losing daemon state, without restarting Hyprland, and without interrupting your session.

---

## Hyprland Integration

Bread V1 is Hyprland-first. The architecture is compositor-agnostic in design but Hyprland is the exclusive target for V1.

This is a deliberate choice. Hyprland now supports Lua configuration natively, which means Bread's Lua layer integrates directly into compositor configuration rather than working around it. Bread becomes the orchestration layer that surrounds and augments Hyprland.

**Bread does not replace Hyprland.** Hyprland handles:
- Window management
- Compositor rendering
- Keybinding dispatch (at the base level)
- Layout algorithms

Bread handles:
- Semantic event interpretation
- Hardware-aware workspace automation
- Cross-subsystem orchestration
- Live behavioral scripting

The two systems are complementary. A Hyprland config without Bread is static. Bread without Hyprland has no compositor to orchestrate.

---

## Filesystem Layout

```
~/.config/bread/
├── init.lua              # Entry point — loads modules, sets defaults
├── modules/              # User and community modules
│   ├── devices.lua
│   ├── workspaces.lua
│   └── profiles/
│       ├── desk.lua
│       └── travel.lua
├── environments/         # Named environment definitions (future)
├── state/                # Persisted runtime state (optional)
├── generated/            # Daemon-generated config fragments
├── runtime/              # Active runtime sockets and PIDs
└── cache/                # Module cache, compiled chunks
```

`init.lua` is the single entry point. It imports modules, defines global behavior, and wires up the initial profile:

```lua
-- ~/.config/bread/init.lua

require("modules.devices")
require("modules.workspaces")
require("modules.profiles.desk")
require("modules.profiles.travel")

bread.on("bread.system.startup", function()
    bread.profile.activate("default")
    bread.log("Bread initialized")
end)
```

---

## Example Workflow: Dock Connect / Disconnect

This end-to-end example shows how Bread's layers work together.

**User connects a USB-C dock.**

1. udev fires a raw device add event
2. The udev adapter ingests it and forwards it to the state engine
3. The state engine recognizes the device signature as a known dock
4. Runtime state updates: `devices.dock = { connected: true, name: "USB-C Dock" }`
5. Normalized event fires: `bread.device.dock.connected`
6. The `devices` module receives the event
7. `bread.profile.activate("desk")` is called
8. The desk profile's `on_activate` fires:
   - Monitor layout is configured via `bread.hypr.keyword`
   - Development applications launch via `bread.exec`
   - A notification is sent via `bread.notify`
9. Desk mode is active

**User disconnects the dock.**

1. udev fires a raw device remove event
2. State engine updates, fires `bread.device.dock.disconnected`
3. `bread.profile.activate("default")` is called
4. The desk profile's `on_deactivate` fires
5. External monitors are disabled, laptop layout is restored
6. Mobile mode is active

The entire workflow is event-driven, stateful, and expressed entirely in Lua. The user never writes a udev rule, a shell script, or a one-off systemd service.

---

## V1 Scope

V1 is intentionally narrow. The goal is a complete, working, well-designed foundation — not a feature-complete platform.

### Included in V1

**Runtime**
- Rust daemon (`breadd`)
- JSON IPC over Unix sockets
- Event subscription and dispatch
- Runtime state engine
- Hot reload

**Adapters**
- Hyprland IPC
- udev (device hotplug)
- Monitor topology (hotplug, EDID)
- Power state (battery, AC)
- Basic network interface state

**Lua Layer**
- Module system with dependency resolution
- Full runtime API (`bread.on`, `bread.state`, `bread.exec`, `bread.notify`, `bread.hypr`)
- Profile system
- Timers, debounce, logging utilities
- Live reload

**CLI**
- `bread reload` — hot-reload modules
- `bread state` — dump runtime state
- `bread events` — stream live events
- `bread modules` — list modules and status
- `bread profile` — manage profiles
- `bread emit` — manually fire events (for development)
- `bread doctor` — diagnose configuration and daemon health

### Explicitly Excluded from V1

To preserve focus and architectural integrity, V1 does not include:

- Provisioning or dotfile management
- Package management or module marketplace
- Cloud sync or distributed state
- GUI frontends or system tray integration
- Compositor abstraction (non-Hyprland support)
- Security sandboxing for modules
- Non-Arch Linux support
- Non-Wayland support
- Reconciliation or declarative config engine

These are not permanent exclusions — they are deferred to preserve the quality and coherence of V1.

---

## Error Handling & Reliability

A desktop automation daemon must be robust. Bread's reliability strategy:

**Lua errors are isolated.** A panic in a module's event handler does not crash the daemon. Errors are caught, logged, and reported via `bread doctor`. The daemon continues running.

**Adapter failures are non-fatal.** If the Hyprland IPC socket disappears (compositor restart), the Hyprland adapter reconnects with exponential backoff. Other adapters continue functioning.

**Hot reload is atomic.** If a module fails to load during reload (syntax error, missing dependency), the reload aborts and the previous module state is preserved. A partial reload never leaves the daemon in an inconsistent state.

**State is eventually consistent.** The daemon does not guarantee that state reads are perfectly synchronized with the physical system at every millisecond. It guarantees that state converges to truth as events arrive. For desktop automation, this is sufficient.

---

## Design Goals

Bread prioritizes these properties above all else:

- **Runtime introspection** — you can always ask Bread what it knows
- **Event-driven** — behavior is triggered by state changes, not polling
- **Modular** — no monolithic config; composable automation units
- **Live reconfiguration** — reload without restarting anything
- **Hardware-aware** — first-class understanding of device topology
- **Operator-focused tooling** — great CLI, great debugging experience
- **Predictable** — events have stable names; state has stable structure; APIs don't break

## Non-Goals

Bread is not:

- A desktop environment
- A window manager or compositor
- A package manager or provisioning system
- An init system or service manager
- A shell replacement
- A Linux distribution
- A monolithic platform

Bread exists as an automation and orchestration fabric layered on top of existing, well-designed Linux tools. It makes those tools work together — it does not replace them.

---

## Long-Term Vision

Bread's V1 is a foundation. The long-term vision is:

> A programmable automation fabric for Linux desktops — where the desktop is an observable, scriptable, reactive runtime that adapts to the user's context in real time.

Future directions under consideration:

- **Broader compositor support** — Sway, niri, others
- **Environment abstractions** — portable desktop profiles that work across machines
- **Declarative runtime layers** — optional reconciliation for users who prefer that model
- **REPL / runtime console** — live Lua evaluation against the daemon state
- **Provisioning tooling** — machine bootstrap and dotfile orchestration
- **Synchronization** — state and config sync across devices
- **Module ecosystem** — community modules and a discovery mechanism

The core philosophy does not change: Linux desktops should behave like observable, programmable runtime systems.

---

## Summary

Bread transforms Linux desktop automation from a fragmented collection of shell scripts, isolated configs, and disconnected runtime hacks into a coherent reactive runtime — powered by Rust, scripted through Lua, and driven by semantic desktop state.

It is designed for users who want their desktop to behave less like a static configuration and more like a programmable operating environment: one that knows what hardware is connected, what profile is active, what the compositor is doing, and what to do about all of it.
