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

## Tips for porting your own scripts

- Start by logging the event payload: `bread.log(event.data.raw)`
- Replace polling loops with event subscriptions
- Use `bread.exec` for shell commands and systemd operations
- Use `bread.state.watch` for data that already lives in the runtime state
