-- workflow-status-widget — surfaces bread's workflow engine (bread.workflow,
-- see Examples.md's "Example 4" and dock-workflow.lua in this directory) in
-- the bar, which had zero visibility anywhere in the UI before this. Shows
-- whichever non-done workflow was most recently updated, with its current
-- step if it's set one via bread.workflow.step(); hides entirely when
-- nothing is running/failed/timed_out, so it stays out of the way until
-- there's actually something to look at.
--
-- Demonstrates: WidgetPlacement "tray", polling an existing bread subsystem
-- (bread.workflow.list()) instead of raw hardware or a single module's own
-- state, and a widget that disappears (visible = false) rather than
-- showing a stale or empty readout.
--
-- Drop-in: copy into ~/.config/bread/modules/. Zero configuration — it
-- reflects whatever workflow any other loaded module starts, including
-- dock-workflow.lua in this same directory.

local M = bread.module({ name = "workflow-status-widget", version = "1.0.0" })

local function most_relevant()
    local workflows = bread.workflow.list()
    local best = nil
    for _, w in ipairs(workflows) do
        if w.state ~= "done" and (not best or w.updated_at > best.updated_at) then
            best = w
        end
    end
    return best
end

local function widget_update()
    local w = most_relevant()
    if not w then
        -- bread.widget.update leaves an omitted field unchanged, not
        -- cleared — a Lua table can't distinguish "tooltip = nil" from
        -- "no tooltip key at all", so an explicit "" is what actually wipes
        -- the previous tooltip instead of leaving it stale under a hidden
        -- widget (harmless in practice since GTK won't show a tooltip on
        -- an invisible widget, but `bread state widgets` would otherwise
        -- report it forever).
        return { visible = false, tooltip = "", root = { type = "label", text = "" } }
    end

    local color = "dim"
    if w.state == "failed" or w.state == "timed_out" then
        color = "red"
    elseif w.state == "running" then
        color = "accent"
    end

    local text = w.name
    if w.step then
        text = text .. ": " .. w.step
    end

    return {
        visible = true,
        tooltip = "Workflow " .. w.name .. " — " .. w.state,
        root = { type = "label", text = text, style = { color = color } },
    }
end

function M.on_load()
    local u = widget_update()
    bread.widget.register({
        id = "status",
        placement = "tray",
        visible = u.visible,
        tooltip = u.tooltip,
        root = u.root,
    })

    bread.every(3000, function()
        local next_u = widget_update()
        bread.widget.update("status", {
            visible = next_u.visible,
            tooltip = next_u.tooltip,
            root = next_u.root,
        })
    end)
end

return M
