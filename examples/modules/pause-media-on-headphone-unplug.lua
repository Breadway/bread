-- pause-media-on-headphone-unplug — pause playback when headphones disconnect,
-- so sound doesn't suddenly blast out of the speakers.
--
-- Drop-in: copy into ~/.config/bread/modules/. Requires `playerctl`.

local M = bread.module({ name = "pause-media-on-headphone-unplug", version = "1.0.0" })

local function looks_like_headphones(name)
    if not name then return false end
    name = name:lower()
    return name:find("head") ~= nil
        or name:find("earbud") ~= nil
        or name:find("airpod") ~= nil
        or name:find("buds") ~= nil
end

function M.on_load()
    bread.on("bread.device.disconnected", function(event)
        if looks_like_headphones(event.data.name) then
            bread.exec("playerctl pause")
        end
    end)
end

return M
