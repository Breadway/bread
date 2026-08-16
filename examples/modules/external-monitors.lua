-- external-monitors — behave like a normal laptop desktop
--
-- Plug in any display (HDMI, DisplayPort, USB-C dock, a random TV) and
-- the session just works. No output names to edit.
--
--   • the laptop panel stays at its preferred (native) mode
--   • each external uses its preferred mode and refresh
--   • new screens clone the laptop (set ARRANGE = "extend" to sit to the right)
--   • closing the lid does not sleep while an external is on
--   • unplug everything and the laptop is the only display again
--
-- Drop-in: copy to ~/.config/bread/modules/ and `bread reload`.

local M = bread.module({
    name    = "external-monitors",
    version = "1.0.0",
    after   = { "bread.monitors" },
})

-- "mirror" = every external clones the laptop (presentations, TVs)
-- "extend" = extra desktop to the right
local ARRANGE = "mirror"
local SCALE   = "auto"

local INTERNAL_RE = "^eDP"
local INHIBITOR   = "/tmp/bread-lid-inhibitor.pid"

local function inhibit_lid()
    if bread.fs.exists(INHIBITOR) then return end
    bread.exec(
        "bash -c 'systemd-inhibit --what=handle-lid-switch --who=bread "
            .. "--why=external-display sleep infinity & echo $! > "
            .. INHIBITOR
            .. "'"
    )
end

local function release_lid()
    bread.exec(
        "bash -c 'kill $(cat " .. INHIBITOR .. " 2>/dev/null) 2>/dev/null; rm -f " .. INHIBITOR .. "'"
    )
end

local function is_internal(name)
    return type(name) == "string" and name:match(INTERNAL_RE) ~= nil
end

local function drm_status(name)
    for card = 0, 5 do
        local raw = bread.fs.read(string.format("/sys/class/drm/card%d-%s/status", card, name))
        if raw then
            return raw:match("^%s*(%S+)")
        end
    end
    return nil
end

local function drm_first_mode(name)
    for card = 0, 5 do
        local raw = bread.fs.read(string.format("/sys/class/drm/card%d-%s/modes", card, name))
        if raw then
            local w, h = raw:match("(%d+)x(%d+)")
            if w then
                return tonumber(w), tonumber(h)
            end
        end
    end
    return 1920, 1080
end

local function list_connectors()
    local names = {}
    local ok, out = bread.exec_capture("ls /sys/class/drm", { timeout_ms = 500 })
    if not ok or not out then
        return names
    end
    for ent in out:gmatch("[^%s]+") do
        local name = ent:match("^card%d+%-(.+)$")
        if name and not name:match("^Writeback") then
            names[#names + 1] = name
        end
    end
    table.sort(names)
    return names
end

local function connected()
    local internal, externals = nil, {}
    for _, name in ipairs(list_connectors()) do
        if drm_status(name) == "connected" then
            if is_internal(name) then
                internal = internal or name
            else
                externals[#externals + 1] = name
            end
        end
    end
    return internal or "eDP-1", externals
end

-- BOS Hyprland talks Lua (`hl.monitor`). Stock Hyprland uses the
-- `monitor=` keyword. Try eval first, then keyword.
local function apply_monitor(opts)
    local extra = ""
    if opts.mirror and opts.mirror ~= "" then
        extra = string.format(", mirror = %q", opts.mirror)
    end
    local expr = string.format(
        "hl.monitor({ output = %q, mode = %q, position = %q, scale = %q%s })",
        opts.output,
        opts.mode or "preferred",
        opts.position or "0x0",
        opts.scale or SCALE,
        extra
    )
    local resp = bread.hyprland.eval(expr)
    if type(resp) == "string" and resp:match("error") then
        local spec = string.format(
            "%s, %s, %s, %s",
            opts.output,
            opts.mode or "preferred",
            opts.position or "0x0",
            opts.scale or SCALE
        )
        if opts.mirror and opts.mirror ~= "" then
            spec = spec .. ", mirror, " .. opts.mirror
        end
        bread.hyprland.keyword("monitor", spec)
    end
end

local function apply(internal, externals)
    apply_monitor({
        output   = internal,
        mode     = "preferred",
        position = "0x0",
        scale    = SCALE,
    })

    if ARRANGE == "mirror" then
        for _, name in ipairs(externals) do
            apply_monitor({
                output   = name,
                mode     = "preferred",
                position = "0x0",
                scale    = SCALE,
                mirror   = internal,
            })
        end
        return
    end

    local x = select(1, drm_first_mode(internal)) or 1920
    for _, name in ipairs(externals) do
        apply_monitor({
            output   = name,
            mode     = "preferred",
            position = x .. "x0",
            scale    = SCALE,
        })
        local w = select(1, drm_first_mode(name)) or 1920
        x = x + w
    end
end

function M.on_load()
    local last = nil
    local applied = false

    local function evaluate()
        local internal, externals = connected()
        local sig = internal .. "|" .. table.concat(externals, ",")
        if sig == last then
            return
        end
        last = sig

        if #externals == 0 then
            if applied then
                apply_monitor({
                    output   = internal,
                    mode     = "preferred",
                    position = "0x0",
                    scale    = SCALE,
                })
                release_lid()
                applied = false
            end
            return
        end

        apply(internal, externals)
        inhibit_lid()
        applied = true
        bread.log("[external-monitors] " .. internal .. " + " .. table.concat(externals, ", "))
    end

    local settle = bread.debounce(1500, evaluate)

    bread.on("bread.hyprland.monitor.connected", function(event)
        local name = event.data and event.data.name
        if name and not is_internal(name) then
            bread.notify("Display connected: " .. name, { urgency = "low" })
        end
        settle()
    end)

    bread.on("bread.hyprland.monitor.disconnected", function()
        settle()
    end)

    bread.on("bread.device.**", function(event)
        local sub = event.data and event.data.subsystem
        if sub == "drm" then
            settle()
        end
    end)

    bread.hyprland.on_raw("configreloaded", function()
        last = nil
        evaluate()
    end)

    bread.every(3000, evaluate)
    settle()
end

return M
