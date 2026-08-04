-- dock-workflow — a worked example of bread's workflow engine
-- (bread.workflow + bread.wait_any), not just a single-event reaction like
-- the other examples in this directory.
--
-- Scenario: when a dock is connected, apply a monitor layout, wait (with a
-- timeout) for Hyprland to actually report a new monitor, activate a
-- "docked" profile, wait for the workspace to settle, then notify. Each
-- step is recorded via bread.workflow.step() so `bread workflows` (or the
-- `workflows.list` IPC method) can show exactly where a run is — useful for
-- diagnosing a dock that isn't behaving, since you can see whether it got
-- stuck waiting for the monitor or the workspace change.
--
-- Drop-in: copy into ~/.config/bread/modules/ and adjust the dock device
-- name (see devices.lua) and profile name for your setup.

local M = bread.module({ name = "dock-workflow", version = "1.0.0" })

bread.workflow.define("dock-connected", function()
    bread.workflow.step("applying layout")
    bread.hyprland.keyword("monitor", "HDMI-A-1, preferred, 1920x0, 1")

    bread.workflow.step("waiting for monitor")
    local event = bread.wait_any(
        { "bread.hyprland.monitor.connected", "bread.hyprland.event" },
        { timeout = 5000 }
    )
    if not event then
        -- Hyprland never reported the new monitor within 5s — leave a
        -- breadcrumb rather than silently continuing as if it worked.
        bread.warn("dock-workflow: timed out waiting for monitor to appear")
        error("monitor did not appear in time")
    end

    bread.workflow.step("activating profile")
    bread.profile.activate("docked")

    bread.workflow.step("waiting for workspace")
    bread.wait("bread.hyprland.workspace.changed", { timeout = 3000 })

    bread.workflow.step("notifying")
    bread.notify("Dock connected", { title = "bread" })
end)

function M.on_load()
    bread.on("bread.device.dock.connected", function()
        -- deadline covers the whole workflow, independent of each step's
        -- own wait timeout — a safety net in case something hangs
        -- unexpectedly rather than failing cleanly.
        bread.workflow.start("dock-connected", { deadline = 15000 })
    end)
end

return M
