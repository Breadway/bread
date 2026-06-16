-- low-battery-warning — notify once when the battery runs low.
--
-- Drop-in: copy into ~/.config/bread/modules/ (auto-discovered; no init.lua
-- edit needed). Zero configuration.

local M = bread.module({ name = "low-battery-warning", version = "1.0.0" })

-- Latch so we warn once per low-battery episode, not on every poll.
local warned = false

function M.on_load()
    bread.on("bread.power.battery.low", function(event)
        if warned then return end
        warned = true
        local pct = event.data.battery_percent or "?"
        bread.notify("Battery low (" .. pct .. "%). Plug in soon.", {
            urgency = "critical",
            title   = "Battery",
            timeout = 10000,
        })
    end)

    -- Reset once back on AC so the next low episode warns again.
    bread.on("bread.power.ac.connected", function()
        warned = false
    end)
end

return M
