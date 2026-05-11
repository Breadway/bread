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

## Tips for porting your own scripts

- Start by logging the event payload: `bread.log(event.data.raw)`
- Replace polling loops with event subscriptions
- Use `bread.exec` for shell commands and systemd operations
- Use `bread.state.watch` for data that already lives in the runtime state
