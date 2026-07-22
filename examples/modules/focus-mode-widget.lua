-- focus-mode-widget — a widget that does something, not just shows
-- something: click to toggle a "focus" bread profile, which mutes audio
-- output as an observable effect. Stays in sync if the profile changes
-- from elsewhere too — the CLI (`bread profile-activate default`), another
-- module, another widget — not just from its own click, by reacting to
-- bread.profile.activated rather than tracking its own local state.
--
-- Demonstrates: a widget as an action launcher wired to bread's actual
-- profile primitive (bread.profile.activate), not just a passive readout;
-- staying in sync with state that can change from other sources; combining
-- bread.exec with a profile switch for a real, checkable effect.
--
-- Drop-in: copy into ~/.config/bread/modules/. Needs `wpctl` (pipewire —
-- already a dependency of breadbar's own volume slider, so if the bar's
-- volume control works, this will too).

local M = bread.module({ name = "focus-mode-widget", version = "1.0.0" })

local FOCUS_PROFILE = "focus"
local DEFAULT_PROFILE = "default"

local function is_focused()
    return bread.state.profile().active == FOCUS_PROFILE
end

local function widget_root()
    return {
        type = "label",
        text = "Focus",
        style = is_focused() and { color = "accent", weight = "bold" } or { color = "dim" },
        on_click = "toggle",
    }
end

function M.on_load()
    bread.widget.register({
        id = "toggle",
        placement = "left_of_clock",
        tooltip = "Click to toggle Focus mode (mutes audio, activates the 'focus' profile)",
        root = widget_root(),
    })

    -- Not just self.click -> self.update: any profile change, from any
    -- source, is reflected here. Try `bread profile-activate default` from
    -- a terminal while this is in "focus" state to see it flip on its own.
    bread.on("bread.profile.activated", function()
        bread.widget.update("toggle", { root = widget_root() })
    end)

    bread.on("bread.bar.widget_clicked", function(e)
        if e.data.widget_id ~= "focus-mode-widget.toggle" or e.data.action ~= "toggle" then
            return
        end
        if is_focused() then
            bread.profile.activate(DEFAULT_PROFILE)
            bread.exec("wpctl set-mute @DEFAULT_AUDIO_SINK@ 0")
            bread.notify("Focus mode off", { title = "bread" })
        else
            bread.profile.activate(FOCUS_PROFILE)
            bread.exec("wpctl set-mute @DEFAULT_AUDIO_SINK@ 1")
            bread.notify("Focus mode on — audio muted", { title = "bread" })
        end
    end)
end

return M
