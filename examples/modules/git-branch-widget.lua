-- git-branch-widget — shows "<repo> <branch>" for whichever git repo the
-- currently focused terminal's *active tab* is sitting in, yellow when the
-- worktree is dirty. Hides entirely when focus isn't on a terminal, or the
-- focused tab isn't inside a git repo.
--
-- This is a plain Lua module doing its own OS-level legwork end to end —
-- no dedicated Rust adapter behind it. It combines four general-purpose
-- primitives that all already exist (or were added alongside this module
-- as small, non-kitty-specific additions): bread.hyprland.active_window()
-- for the focused window's class + pid, bread.fs.exists to probe for a
-- listening socket, bread.exec_capture to run `kitty @ ls` and read its
-- output, and bread.json.decode to parse it.
--
-- Why kitty remote control instead of /proc: a kitty *window* can host
-- several *tabs*, each a separate child shell process, and the kernel has
-- no notion of "which pty is currently displayed" — that's purely internal
-- kitty state. Walking /proc can find the window's child processes but
-- can't tell which one you're actually looking at. Kitty's own `kitty @ ls`
-- tracks focus precisely at the OS-window/tab/window level, so asking it
-- directly is the only way to get this exactly right for multi-tab windows.
--
-- Prerequisite — add to ~/.config/kitty/kitty.conf:
--   allow_remote_control socket-only
--   listen_on unix:/tmp/kitty-bread-{kitty_pid}
-- `{kitty_pid}` makes the socket path unique per kitty process, so this
-- works whether or not you run kitty in single-instance mode, and however
-- many separate kitty processes you have open — this module derives the
-- exact socket to ask from the focused window's own pid. Kitty only picks
-- up `listen_on` on (re)start, not a config reload, so existing kitty
-- windows need to be restarted once after adding this.
--
-- Drop-in: copy into ~/.config/bread/modules/. Needs `git` and the kitty
-- remote-control config above. Assumes the terminal's WM_CLASS is "kitty"
-- (edit TERMINAL_CLASS below for another terminal, if it has an equivalent
-- remote-control/introspection story).

local M = bread.module({ name = "git-branch-widget", version = "1.0.0" })

local TERMINAL_CLASS = "kitty"

local function shell_quote(s)
    return "'" .. s:gsub("'", "'\\''") .. "'"
end

-- The exact cwd of the focused tab in the focused kitty window, or nil if
-- focus isn't on kitty, that kitty process hasn't been restarted since the
-- listen_on config was added, or nothing came back focused (shouldn't
-- happen for a window Hyprland itself says is focused, but `kitty @ ls`
-- reflects kitty's own state, not Hyprland's, so treat it as fallible).
local function focused_tab_cwd()
    local win = bread.hyprland.active_window()
    if type(win) ~= "table" or win.class ~= TERMINAL_CLASS or not win.pid then
        return nil
    end

    local socket_path = "/tmp/kitty-bread-" .. win.pid
    if not bread.fs.exists(socket_path) then
        return nil
    end

    local ok, output = bread.exec_capture("kitty @ --to unix:" .. socket_path .. " ls")
    if not ok then
        return nil
    end

    local os_windows = bread.json.decode(output)
    if type(os_windows) ~= "table" then
        return nil
    end

    for _, osw in ipairs(os_windows) do
        if osw.is_focused then
            for _, tab in ipairs(osw.tabs or {}) do
                if tab.is_focused then
                    for _, w in ipairs(tab.windows or {}) do
                        if w.is_focused then
                            return w.cwd
                        end
                    end
                end
            end
        end
    end

    return nil
end

local function git_info(cwd)
    local quoted = shell_quote(cwd)

    local ok, toplevel = bread.exec_capture("git -C " .. quoted .. " rev-parse --show-toplevel")
    if not ok then
        return nil
    end
    toplevel = toplevel:gsub("%s+$", "")
    local repo = toplevel:match("([^/]+)/?$") or toplevel

    local repo_quoted = shell_quote(toplevel)
    local branch_ok, branch = bread.exec_capture("git -C " .. repo_quoted .. " rev-parse --abbrev-ref HEAD")
    if not branch_ok then
        return nil
    end
    branch = branch:gsub("%s+$", "")

    local _, status = bread.exec_capture("git -C " .. repo_quoted .. " status --porcelain")
    local dirty = status:match("%S") ~= nil

    return { repo = repo, branch = branch, dirty = dirty }
end

local function widget_root(info)
    if not info then
        return { type = "label", text = "" }
    end
    return {
        type = "label",
        text = info.repo .. " " .. info.branch,
        style = { color = info.dirty and "yellow" or "dim" },
    }
end

local function update()
    local cwd = focused_tab_cwd()
    local info = cwd and git_info(cwd) or nil

    bread.widget.update("branch", {
        visible = info ~= nil,
        tooltip = info and ("git: " .. info.repo .. "@" .. info.branch .. (info.dirty and " (dirty)" or "")) or "",
        root = widget_root(info),
    })
end

function M.on_load()
    local cwd = focused_tab_cwd()
    local info = cwd and git_info(cwd) or nil

    bread.widget.register({
        id = "branch",
        placement = "right_of_clock",
        visible = info ~= nil,
        tooltip = info and ("git: " .. info.repo .. "@" .. info.branch) or "",
        root = widget_root(info),
    })

    -- Event-driven for instant updates on focus change, plus a poll to
    -- catch a branch switch inside the same still-focused tab (e.g. `git
    -- checkout` run without ever changing window focus), which produces no
    -- focus event at all.
    bread.on("bread.hyprland.window.focused", update)
    bread.on("bread.hyprland.window.focus.changed", update)
    bread.every(2000, update)
end

return M
