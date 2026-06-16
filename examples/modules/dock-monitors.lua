-- dock-monitors — apply a monitor layout when an external display is plugged
-- in (a "dock") and revert to the laptop panel when it's removed.
--
-- Drop-in: copy into ~/.config/bread/modules/ and edit the output names /
-- resolutions for your machine (see `hyprctl monitors`).

local monitors = require("bread.monitors")
local M = bread.module({ name = "dock-monitors", version = "1.0.0" })

-- Named layouts ----------------------------------------------------------------
monitors.layout("docked", function()
    bread.hyprland.keyword("monitor", "eDP-1, 1920x1200@60, 0x0, 1")
    bread.hyprland.keyword("monitor", "HDMI-A-1, preferred, 1920x0, 1")
end)

monitors.layout("solo", function()
    bread.hyprland.keyword("monitor", "eDP-1, preferred, 0x0, 1")
end)

-- React to the external display ------------------------------------------------
function M.on_load()
    monitors.on({ when = "connected",    monitors = { "HDMI-A-1" }, run = monitors.apply("docked") })
    monitors.on({ when = "disconnected", monitors = { "HDMI-A-1" }, run = monitors.apply("solo") })
end

return M
