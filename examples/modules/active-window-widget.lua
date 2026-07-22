-- active-window-widget — shows the focused app right next to the
-- workspace pills, live-updated via bread.state.watch (no polling, no
-- bread.on needed).
--
-- Demonstrates: WidgetPlacement "right_of_workspaces", a state-watch-driven
-- widget (as opposed to a timer or event handler), and calling
-- bread.widget.update from inside the watch callback. breadbar's own bar
-- doesn't show the focused window anywhere today — this adds that for free.
--
-- Drop-in: copy into ~/.config/bread/modules/. Zero configuration.

local M = bread.module({ name = "active-window-widget", version = "1.0.0" })

local function label_for(window)
    -- A JSON null (no window focused) doesn't necessarily arrive as Lua nil
    -- through bread.state.* — mlua's serde bridge can hand back a distinct
    -- null sentinel instead, which `not window` won't catch. Guard on the
    -- type directly so any non-string value (nil, the sentinel, ...) falls
    -- through to the placeholder rather than crashing on #window below.
    if type(window) ~= "string" or window == "" then
        return "—"
    end
    if #window > 24 then
        return window:sub(1, 24) .. "…"
    end
    return window
end

local function widget_root(window)
    return { type = "label", text = label_for(window), style = { color = "dim" } }
end

function M.on_load()
    bread.widget.register({
        id = "active-window",
        placement = "right_of_workspaces",
        tooltip = "Currently focused window",
        root = widget_root(bread.state.active_window()),
    })

    bread.state.watch("active_window", function(new_val)
        bread.widget.update("active-window", { root = widget_root(new_val) })
    end)
end

return M
