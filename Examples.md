# Bread Examples

These examples show how to translate existing Hyprland automation into Bread's event-driven Lua runtime.

Each snippet is designed to be drop-in friendly for a `~/.config/bread/modules/*.lua` file. Start with a new module file and `require` it from `~/.config/bread/init.lua`.

## Example 1: Porting keyboard_and_display_watcher.sh (system script)

Source inspiration: `~/.config/hypr/scripts/system/keyboard_and_display_watcher.sh`.

This example covers two parts that port cleanly to Bread:

- Start/stop the Redox layout viewer when the keyboard appears
- Start/stop a display sync service when an external monitor appears

```lua
-- ~/.config/bread/modules/redox_and_display.lua
local M = bread.module({ name = "redox_and_display", version = "1.0.0" })

local PREVIEW_CMD = "/home/breadway/redox-layout-viewer/target/release/redox-layout-viewer"
local APP_NAME = "redox-layout-vi"

local function start_viewer()
    bread.exec("pgrep -f '" .. APP_NAME .. "' >/dev/null || " .. PREVIEW_CMD .. " >/dev/null 2>&1 &")
end

local function stop_viewer()
    bread.exec("pkill -f '" .. APP_NAME .. "' >/dev/null 2>&1 || true")
end

local function is_redox(event)
    -- Inspect event.data.raw once to find stable identifiers in your environment.
    -- Typical udev fields include id_vendor, id_model, id_vendor_id, id_model_id, and name.
    local raw = event.data and event.data.raw or {}
    local name = tostring(raw.name or "")
    local vendor = tostring(raw.id_vendor or "")
    local model = tostring(raw.id_model or "")

    return name:lower():find("redox", 1, true)
        or vendor:lower():find("redox", 1, true)
        or model:lower():find("redox", 1, true)
end

local external_monitors = 0

local function update_display_service()
    if external_monitors > 0 then
        bread.exec("systemctl --user start hypr-display-sync.service")
    else
        bread.exec("systemctl --user stop hypr-display-sync.service")
    end
end

function M.on_load()
    bread.on("bread.device.keyboard.connected", function(event)
        if is_redox(event) then
            start_viewer()
        end
    end)

    bread.on("bread.device.keyboard.disconnected", function(event)
        if is_redox(event) then
            stop_viewer()
        end
    end)

    bread.on("bread.monitor.connected", function(event)
        local name = event.data and (event.data.name or event.data.raw) or ""
        -- ignore internal panel (eDP-1) and count only externals
        if not tostring(name):match("eDP%-1") then
            external_monitors = external_monitors + 1
            update_display_service()
        end
    end)

    bread.on("bread.monitor.disconnected", function(event)
        local name = event.data and (event.data.name or event.data.raw) or ""
        if not tostring(name):match("eDP%-1") then
            external_monitors = math.max(0, external_monitors - 1)
            update_display_service()
        end
    end)
end

return M
```

Notes:

- Use `bread.log(event.data.raw)` once to see your exact udev fields for matching.
- This drops polling and relies on udev/Hyprland events.

## Example 2: Porting autostart.lua

Source inspiration: `~/.config/hypr/scripts/system/autostart.lua`.

```lua
-- ~/.config/bread/modules/autostart.lua
local M = bread.module({ name = "autostart", version = "1.0.0" })

local home = os.getenv("HOME") or "/home/breadway"
local startup_commands = {
    "wal -R",
    home .. "/colorshell/build/colorshell",
    "awww-daemon",
    "awww restore",
    home .. "/.config/hypr/scripts/system/keyboard_and_display_watcher.sh",
    home .. "/.config/hypr/watch_hypr_scripts.sh",
    "systemctl --user daemon-reload",
    "systemctl --user start hypr-display-sync.service",
    "systemctl --user start hyprpolkitagent",
    "dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP",
    "/usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1",
    "flatpak run dev.deedles.Trayscale",
    "wificonf init",
    "pkill -f hyprpaper",
}

function M.on_load()
    bread.once("bread.system.startup", function()
        for _, cmd in ipairs(startup_commands) do
            bread.exec(cmd)
        end
    end)
end

return M
```

## Example 3: Porting display/monitors.lua

Source inspiration: `~/.config/hypr/scripts/display/monitors.lua`.

This uses Bread events and Hyprland keywords to update monitor layout when external displays change.

```lua
-- ~/.config/bread/modules/monitors.lua
local M = bread.module({ name = "monitors", version = "1.0.0" })

local function apply_internal_mode(has_external)
    local mode = has_external and "1920x1080@60" or "1920x1200@60"
    bread.hyprland.keyword("monitor", "eDP-1, " .. mode .. ", 0x0, 1")
end

local function apply_external()
    bread.hyprland.keyword("monitor", "DP-3, 1920x1080@60, auto, 1, mirror, eDP-1")
end

local externals = 0
local function update()
    apply_internal_mode(externals > 0)
    if externals > 0 then
        apply_external()
    end
end

function M.on_load()
    bread.on("bread.monitor.connected", function(event)
        local name = tostring((event.data and (event.data.name or event.data.raw)) or "")
        if not name:match("eDP%-1") then
            externals = externals + 1
            update()
        end
    end)

    bread.on("bread.monitor.disconnected", function(event)
        local name = tostring((event.data and (event.data.name or event.data.raw)) or "")
        if not name:match("eDP%-1") then
            externals = math.max(0, externals - 1)
            update()
        end
    end)

    bread.once("bread.system.startup", function()
        update()
    end)
end

return M
```

## Example 4: Multi-step automation workflows

The examples above are all single-event reactions: one trigger, one handler. `bread.wait`/`bread.spawn` (coroutine-based waits) and the `bread.workflow` table build on those to let a module walk through several ordered steps — with a timeout on any wait, and an overall deadline for the whole thing — rather than each step having to re-derive "am I still in the middle of handling the last event."

Full source: `examples/modules/dock-workflow.lua`.

```lua
-- ~/.config/bread/modules/dock-workflow.lua
local M = bread.module({ name = "dock-workflow", version = "1.0.0" })

bread.workflow.define("dock-connected", function()
    bread.workflow.step("applying layout")
    bread.hyprland.keyword("monitor", "HDMI-A-1, preferred, 1920x0, 1")

    bread.workflow.step("waiting for monitor")
    -- wait_any resolves on whichever of these patterns fires first, or
    -- returns nil after the timeout — it does not block forever.
    local event = bread.wait_any(
        { "bread.monitor.connected", "bread.hyprland.event" },
        { timeout = 5000 }
    )
    if not event then
        error("monitor did not appear in time")
    end

    bread.workflow.step("activating profile")
    bread.profile.activate("docked")

    bread.workflow.step("waiting for workspace")
    bread.wait("bread.workspace.changed", { timeout = 3000 })

    bread.workflow.step("notifying")
    bread.notify("Dock connected", { title = "bread" })
end)

function M.on_load()
    bread.on("bread.device.dock.connected", function()
        -- The deadline covers the whole run, independent of each step's
        -- own wait timeout — a safety net against something hanging.
        bread.workflow.start("dock-connected", { deadline = 15000 })
    end)
end

return M
```

Walking through what each piece buys you over a plain `bread.on` handler:

- **`bread.workflow.define(name, fn)` / `.start(name, opts)`** — registers the body under a name, then runs it as a `bread.spawn`ed coroutine. `opts.deadline` (ms) marks the run `timed_out` in the registry if it hasn't reached a terminal state in time — this is independent of, and layered on top of, any per-step `timeout` inside the body. `opts.args` is passed through as the single argument to the body function, if you need to parameterize a run.
- **`bread.workflow.step(label)`** — call it from inside the running body to record "currently here." It doesn't change control flow at all; it exists purely so `bread.workflow.status("dock-connected")` (or the CLI/dashboard) can answer "is this stuck, and where?" instead of a black box between start and finish.
- **`bread.wait_any(patterns, opts)`** — like `bread.wait`, but resolves on the first of several patterns (useful when you're not sure which specific event a compositor/adapter will actually emit for a given transition — see the two candidate patterns above). `bread.wait_all(patterns, opts)` is the complementary primitive: it resolves once every listed pattern has fired at least once (or the timeout elapses), returning a table keyed by pattern.
- **Errors are captured, not lost.** If the body raises (as it does above when the monitor never appears), the workflow's registry entry moves to `failed` with the message attached — visible via `bread.workflow.status(name)` or the `workflows.list` IPC method — rather than only ever showing up as a one-line log the moment it happened.

Check on a running (or finished) workflow via the IPC method directly (there's no dedicated `bread` subcommand for this yet — see `workflows.list` in the [IPC protocol dictionary](Documentation.md#dictionary-ipc-protocol)):

```bash
echo '{"id":"1","method":"workflows.list","params":{}}' | nc -U -q0 "$XDG_RUNTIME_DIR/bread/breadd.sock"
```

## Example 5: A live widget in breadbar

The examples above all react to something; they don't put anything on
screen. `bread.widget` *(Since: v1.3)* does — a module declares a small node
tree and breadbar (or any sibling app that renders `bread.widget.*`) shows
it in one of five fixed layout slots, live-updated from Lua.

Full source: `examples/modules/cpu-temp-widget.lua`.

```lua
-- ~/.config/bread/modules/cpu-temp-widget.lua
local M = bread.module({ name = "cpu-temp-widget", version = "1.0.0" })

local TEMP_PATH = "/sys/class/hwmon/hwmon6/temp1_input"
local HOT_THRESHOLD_C = 80

local function read_temp_c()
    local raw = bread.fs.read(TEMP_PATH)
    return raw and (tonumber(raw) / 1000) or nil
end

local function widget_root(temp_c)
    local text = temp_c and string.format("%.0f°C", temp_c) or "—"
    local hot = temp_c ~= nil and temp_c >= HOT_THRESHOLD_C
    return {
        type = "box",
        children = {
            {
                type = "label",
                text = text,
                style = hot and { color = "red", weight = "bold" } or { color = "dim" },
            },
            {
                type = "progress",
                value = temp_c and math.min(temp_c / 100, 1.0) or 0,
                style = hot and { color = "red" } or nil,
            },
        },
    }
end

function M.on_load()
    bread.widget.register({
        id = "cpu-temp",
        placement = "left_of_stats",
        tooltip = "CPU package temperature (Tctl)",
        root = widget_root(read_temp_c()),
    })

    bread.every(5000, function()
        bread.widget.update("cpu-temp", { root = widget_root(read_temp_c()) })
    end)
end

return M
```

Walking through what each piece buys you:

- **`root` is a small typed tree, not markup.** `box`/`label`/`icon`/`progress` map directly onto GTK primitives, so any renderer can draw it without interpreting a DSL — see [Widgets](Documentation.md#widgets-since-v13) for the full node reference and the size/depth caps.
- **`bread.widget.update(id, { root = ... })` replaces the whole tree.** There's no node-level patching — for something this small, rebuilding the tree on every tick (here, every 5s) is simpler than diffing, and it's cheap enough that it doesn't matter.
- **`style` is a bounded, typed vocabulary, not a style string.** `color = "red"` here maps to one fixed CSS class the rendering app defines, resolved from the real pywal-derived palette — see [Widgets §style](Documentation.md#style-since-v14) for the full field list. There's also a freeform `class` escape hatch, but a module can't inject arbitrary CSS through either path.
- **Clicks come back as events, not callbacks.** A node's `on_click` value isn't invoked directly — the rendering app emits `bread.bar.widget_clicked` with `{ widget_id, action }`, and your module reacts with a normal `bread.on` handler. See `examples/modules/bluetooth-toggle-widget.lua` for a widget that uses this to drive a real action (`bread.bluetooth.power`) instead of just displaying something — or `examples/modules/focus-mode-widget.lua` for one that drives `bread.profile.activate` and stays in sync when the profile changes from somewhere else entirely (the CLI, another module), not just from its own click.
- **Placement is one of five fixed slots** (`tray`, `left_of_clock`, `right_of_clock`, `right_of_workspaces`, `left_of_stats`) — see `examples/modules/active-window-widget.lua` for `right_of_workspaces` driven by `bread.state.watch` instead of a timer.
- **A widget doesn't have to read hardware.** `examples/modules/workflow-status-widget.lua` polls `bread.workflow.list()` instead — the same engine from Example 4 — and sets `visible = false` to disappear entirely when there's nothing to report, rather than showing a stale or empty readout.

Check what's currently registered over IPC (there's no dedicated `bread` subcommand for this yet — see `widgets.list` in the [IPC protocol dictionary](Documentation.md#dictionary-ipc-protocol)):

```bash
echo '{"id":"1","method":"widgets.list","params":{}}' | nc -U -q0 "$XDG_RUNTIME_DIR/bread/breadd.sock"
```

## Tips for porting your own scripts

- Start by logging the event payload: `bread.log(event.data.raw)`
- Replace polling loops with event subscriptions
- Use `bread.exec` for shell commands and systemd operations
- Use `bread.state.watch` for data that already lives in the runtime state
