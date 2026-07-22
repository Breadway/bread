-- cpu-temp-widget — live CPU package temperature, read straight from the
-- k10temp hwmon sysfs node via bread.fs.read.
--
-- Demonstrates: WidgetPlacement "left_of_stats", a bread.every-polled
-- widget reading real hardware state (the same category of readout
-- breadbar's native CPU%/RAM stats already do in Rust — this shows it's
-- just as easy from a drop-in Lua module), and the typed `style` vocabulary
-- (color + weight) swapping based on a threshold so the widget visually
-- flags when something's hot — no CSS, no guessing which class names the
-- rendering app happens to define.
--
-- Drop-in: copy into ~/.config/bread/modules/. TEMP_PATH is specific to
-- this machine (AMD, k10temp) — find yours with:
--   grep -l k10temp /sys/class/hwmon/hwmon*/name
-- and adjust below; a missing/unreadable path just shows "—" rather than
-- erroring, since bread.fs.read returns nil (not an error) for that case.

local M = bread.module({ name = "cpu-temp-widget", version = "1.0.0" })

local TEMP_PATH = "/sys/class/hwmon/hwmon6/temp1_input"
local HOT_THRESHOLD_C = 80

local function read_temp_c()
    local raw = bread.fs.read(TEMP_PATH)
    if not raw then
        return nil
    end
    return tonumber(raw) / 1000
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
