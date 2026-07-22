-- bluetooth-toggle-widget — a real one-click Bluetooth power toggle in the
-- hamburger popover's tray section, showing power state and connected
-- device count. breadbar's native bar only shows a passive BT icon; this
-- adds an actual control surface for it.
--
-- Demonstrates: WidgetPlacement "tray", a bread.every-polled read of
-- bread.bluetooth.devices()/.powered(), and bread.bar.widget_clicked
-- driving a real action (bread.bluetooth.power) rather than just display.
--
-- Drop-in: copy into ~/.config/bread/modules/. Zero configuration.

local M = bread.module({ name = "bluetooth-toggle-widget", version = "1.0.0" })

local function widget_root()
    local powered = bread.bluetooth.powered()
    local devices = bread.bluetooth.devices() or {}
    local connected = 0
    for _, d in ipairs(devices) do
        if d.connected then
            connected = connected + 1
        end
    end

    local text, style
    if powered == nil then
        text, style = "BT n/a", { color = "dim" }
    elseif not powered then
        text, style = "BT off", { color = "dim" }
    elseif connected > 0 then
        text, style = "BT (" .. connected .. ")", { color = "accent", weight = "bold" }
    else
        text, style = "BT on", { color = "fg" }
    end

    return {
        type = "label",
        text = text,
        style = style,
        on_click = "toggle",
    }
end

function M.on_load()
    bread.widget.register({
        id = "toggle",
        placement = "tray",
        tooltip = "Click to toggle Bluetooth power",
        root = widget_root(),
    })

    bread.every(5000, function()
        bread.widget.update("toggle", { root = widget_root() })
    end)

    bread.on("bread.bar.widget_clicked", function(e)
        if e.data.widget_id == "bluetooth-toggle-widget.toggle" and e.data.action == "toggle" then
            bread.bluetooth.power(not bread.bluetooth.powered())
            -- Give BlueZ a moment to apply before refreshing the label.
            bread.after(500, function()
                bread.widget.update("toggle", { root = widget_root() })
            end)
        end
    end)
end

return M
