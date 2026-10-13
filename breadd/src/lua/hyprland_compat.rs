//! Glue between `bread.hyprland.dispatch` / `bread.hyprland.keyword` and a
//! running Hyprland, whichever way that Hyprland is configured.
//!
//! Hyprland is stock upstream, configured either with `hyprland.conf` or with
//! `hyprland.lua`. The two differ in what the IPC socket accepts:
//!
//! - With `hyprland.conf`, `dispatch workspace 5` and `keyword bind ...` work.
//! - With `hyprland.lua`, `dispatch` takes a Lua expression (so the legacy form
//!   is a syntax error) and `keyword` is refused with "keyword can't work with
//!   non-legacy parsers. Use eval." Both come back as ordinary reply text, not
//!   as a socket error, so the old code returned that text to Lua as if it had
//!   succeeded.
//!
//! The functions here try the legacy request first (so a plain `hyprland.conf`
//! keeps working unchanged), and when Hyprland does not answer `ok` they
//! translate the legacy command into the equivalent `hl.*` call and send it
//! through `eval`. Any reply other than `ok` ends up as an error.
//!
//! Every field name used in the generated Lua was checked against the typed
//! stubs: `~/.config/hypr/hyprland-api.lua` (BOS) and the stub that ships with
//! Hyprland, `/usr/share/hypr/stubs/hl.meta.lua`. The one exception is noted
//! on the address-targeted window move below.

use anyhow::{anyhow, bail, Result};

/// True when a Hyprland socket reply means success.
pub(super) fn is_ok(reply: &str) -> bool {
    reply.trim() == "ok"
}

/// Run a legacy-style dispatch (`dispatch <cmd> <args>`), falling back to the
/// equivalent `hl.dispatch(...)` through `eval` when the legacy form is not
/// accepted. `send` performs one request on the Hyprland socket.
///
/// `cmd` may also be a Lua dispatcher expression such as
/// `hl.dsp.focus({ workspace = '5' })` (with no args); Hyprland running from
/// `hyprland.lua` accepts that directly.
pub(super) fn dispatch<F>(mut send: F, cmd: &str, args: &str) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    let legacy = join_cmd(cmd, args);
    let reply = send(&format!("dispatch {legacy}"))?;
    if is_ok(&reply) {
        return Ok(());
    }

    let expr = match legacy_dispatch_to_lua(cmd, args) {
        Ok(expr) => expr,
        Err(why) => bail!(
            "hyprland rejected `dispatch {legacy}`: {} (no hyprland.lua equivalent: {why})",
            reply.trim()
        ),
    };
    let lua_reply = send(&format!("eval hl.dispatch({expr})"))?;
    if is_ok(&lua_reply) {
        return Ok(());
    }
    Err(anyhow!(
        "hyprland rejected `dispatch {legacy}`: {} (eval fallback: {})",
        reply.trim(),
        lua_reply.trim()
    ))
}

/// Run a legacy-style `keyword <key> <value>`, falling back to the equivalent
/// `hl.config` / `hl.monitor` / `hl.bind` / `hl.unbind` call through `eval`.
pub(super) fn keyword<F>(mut send: F, key: &str, value: &str) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    let reply = send(&format!("keyword {key} {value}"))?;
    if is_ok(&reply) {
        return Ok(());
    }

    let stmt = match legacy_keyword_to_lua(key, value) {
        Ok(stmt) => stmt,
        Err(why) => bail!(
            "hyprland rejected `keyword {key} {value}`: {} (no hyprland.lua equivalent: {why})",
            reply.trim()
        ),
    };
    let lua_reply = send(&format!("eval {stmt}"))?;
    if is_ok(&lua_reply) {
        return Ok(());
    }
    Err(anyhow!(
        "hyprland rejected `keyword {key} {value}`: {} (eval fallback: {})",
        reply.trim(),
        lua_reply.trim()
    ))
}

fn join_cmd(cmd: &str, args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        cmd.trim().to_string()
    } else {
        format!("{} {args}", cmd.trim())
    }
}

/// Quote `s` as a single-quoted Lua string literal.
fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Translate a legacy dispatcher and its arguments into a Lua expression that
/// evaluates to a dispatcher (or a function `hl.dispatch` / `hl.bind` accept).
///
/// A `cmd` that already is Lua (starts with `hl.`) is passed through.
pub(super) fn legacy_dispatch_to_lua(cmd: &str, args: &str) -> Result<String> {
    let cmd = cmd.trim();
    let args = args.trim();
    if cmd.starts_with("hl.") {
        return Ok(join_cmd(cmd, args));
    }

    let need_args = || -> Result<()> {
        if args.is_empty() {
            bail!("`{cmd}` needs an argument");
        }
        Ok(())
    };

    match cmd {
        "workspace" => {
            need_args()?;
            Ok(format!("hl.dsp.focus({{ workspace = {} }})", lua_str(args)))
        }
        "focusworkspaceoncurrentmonitor" => {
            need_args()?;
            Ok(format!(
                "hl.dsp.focus({{ workspace = {}, on_current_monitor = true }})",
                lua_str(args)
            ))
        }
        "movetoworkspace" => move_to_workspace(args, true),
        "movetoworkspacesilent" => move_to_workspace(args, false),
        "moveworkspacetomonitor" => {
            let mut parts = args.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some(ws), Some(mon)) => Ok(format!(
                    "hl.dsp.workspace.move({{ monitor = {}, workspace = {} }})",
                    lua_str(mon),
                    lua_str(ws)
                )),
                _ => bail!("`moveworkspacetomonitor` needs `<workspace> <monitor>`"),
            }
        }
        "focusmonitor" => {
            need_args()?;
            Ok(format!("hl.dsp.focus({{ monitor = {} }})", lua_str(args)))
        }
        "focuswindow" => {
            need_args()?;
            Ok(format!("hl.dsp.focus({{ window = {} }})", lua_str(args)))
        }
        "movefocus" => {
            need_args()?;
            Ok(format!("hl.dsp.focus({{ direction = {} }})", lua_str(args)))
        }
        "movewindow" => {
            need_args()?;
            Ok(format!(
                "hl.dsp.window.move({{ direction = {} }})",
                lua_str(args)
            ))
        }
        "dpms" => {
            // `dpms <on|off|toggle> [monitor]`
            let mut parts = args.split_whitespace();
            let action = parts.next().unwrap_or("toggle");
            if !matches!(action, "on" | "off" | "toggle") {
                bail!("`dpms` action must be on, off or toggle");
            }
            match parts.next() {
                Some(mon) => Ok(format!(
                    "hl.dsp.dpms({{ action = {}, monitor = {} }})",
                    lua_str(action),
                    lua_str(mon)
                )),
                None => Ok(format!("hl.dsp.dpms({{ action = {} }})", lua_str(action))),
            }
        }
        "exec" => {
            need_args()?;
            Ok(format!("hl.dsp.exec_cmd({})", lua_str(args)))
        }
        "killactive" => Ok("hl.dsp.window.close()".to_string()),
        "togglefloating" => Ok("hl.dsp.window.float({ action = 'toggle' })".to_string()),
        "pseudo" => Ok("hl.dsp.window.pseudo({ action = 'toggle' })".to_string()),
        "fullscreen" => match args {
            "" => Ok("hl.dsp.window.fullscreen({ action = 'toggle' })".to_string()),
            "0" => Ok(
                "hl.dsp.window.fullscreen({ action = 'toggle', mode = 'fullscreen' })".to_string(),
            ),
            "1" => Ok(
                "hl.dsp.window.fullscreen({ action = 'toggle', mode = 'maximized' })".to_string(),
            ),
            other => bail!("`fullscreen` mode must be 0 or 1, got `{other}`"),
        },
        "togglespecialworkspace" => Ok(format!(
            "hl.dsp.workspace.toggle_special({})",
            lua_str(args)
        )),
        "submap" => {
            need_args()?;
            Ok(format!("hl.dsp.submap({})", lua_str(args)))
        }
        "layoutmsg" => {
            need_args()?;
            Ok(format!("hl.dsp.layout({})", lua_str(args)))
        }
        "exit" => Ok("hl.dsp.exit()".to_string()),
        other => bail!("dispatcher `{other}` has no known translation; pass a Lua dispatcher such as `hl.dsp.focus({{ workspace = '5' }})` instead"),
    }
}

/// `movetoworkspace[silent] <workspace>[,<window>]`.
///
/// Upstream Hyprland 0.56.2's hlWindowMove captures the optional `window`
/// selector directly. Do not focus first: silent moves must preserve focus,
/// and a missing selector must never fall back to an unrelated active window.
fn move_to_workspace(args: &str, follow: bool) -> Result<String> {
    if args.is_empty() {
        bail!("movetoworkspace needs a workspace");
    }
    let (ws, window) = match args.split_once(',') {
        Some((ws, window)) => (ws.trim(), Some(window.trim())),
        None => (args.trim(), None),
    };
    if ws.is_empty() {
        bail!("movetoworkspace needs a workspace");
    }
    let follow_field = if follow { "" } else { ", follow = false" };
    let window_field = match window {
        None | Some("") => String::new(),
        Some(window) => format!(", window = {}", lua_str(window)),
    };
    let mv = format!(
        "hl.dsp.window.move({{ workspace = {}{follow_field}{window_field} }})",
        lua_str(ws)
    );
    match window {
        None | Some("") => Ok(mv),
        Some(window) => Ok(format!(
            "function() assert(hl.get_window({}), 'window selector did not match'); \
             local result = hl.dispatch({mv}); assert(result and result.ok, \
             result and result.error or 'window move failed'); return result end",
            lua_str(window)
        )),
    }
}

/// Translate a legacy `keyword <key> <value>` into a Lua statement.
pub(super) fn legacy_keyword_to_lua(key: &str, value: &str) -> Result<String> {
    let key = key.trim();
    let value = value.trim();
    match key {
        "bind" | "binde" | "bindl" => bind_to_lua(key, value),
        "unbind" => {
            let (mods, k) = split_bind_head(value)?;
            Ok(format!("hl.unbind({})", lua_str(&bind_key(&mods, &k))))
        }
        "monitor" => monitor_to_lua(value),
        k if k.contains(':') => config_to_lua(k, value),
        other => bail!(
            "keyword `{other}` has no known translation; use bread.hyprland.eval with an hl.* call"
        ),
    }
}

fn split_bind_head(value: &str) -> Result<(Vec<String>, String)> {
    let mut parts = value.splitn(3, ',');
    let mods = parts.next().unwrap_or("").trim();
    let key = parts.next().map(str::trim).unwrap_or("");
    if key.is_empty() {
        bail!("bind needs `<mods>, <key>, ...`");
    }
    let mods: Vec<String> = mods
        .split(|c: char| c.is_whitespace() || c == '_')
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .collect();
    Ok((mods, key.to_string()))
}

/// `SUPER + SHIFT + Q`, the form `hl.bind` takes (BOS keybinds.lua joins
/// modifiers and key with ` + `).
fn bind_key(mods: &[String], key: &str) -> String {
    if mods.is_empty() {
        key.to_string()
    } else {
        format!("{} + {key}", mods.join(" + "))
    }
}

fn bind_to_lua(kind: &str, value: &str) -> Result<String> {
    let (mods, key) = split_bind_head(value)?;
    let mut parts = value.splitn(4, ',');
    let _ = parts.next();
    let _ = parts.next();
    let dispatcher = parts.next().map(str::trim).unwrap_or("");
    let args = parts.next().map(str::trim).unwrap_or("");
    if dispatcher.is_empty() {
        bail!("bind needs a dispatcher");
    }
    let expr = legacy_dispatch_to_lua(dispatcher, args)?;
    let opts = match kind {
        "binde" => ", { repeating = true }",
        "bindl" => ", { locked = true }",
        _ => "",
    };
    Ok(format!(
        "hl.bind({}, {expr}{opts})",
        lua_str(&bind_key(&mods, &key))
    ))
}

/// `name, mode, position, scale[, mirror, other]`, or `name, disable`.
fn monitor_to_lua(value: &str) -> Result<String> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    let output = parts.first().copied().unwrap_or("");
    if parts.len() < 2 {
        bail!("monitor needs `<name>, <mode>, <position>, <scale>`");
    }
    if parts[1] == "disable" || parts[1] == "disabled" {
        return Ok(format!(
            "hl.monitor({{ output = {}, disabled = true }})",
            lua_str(output)
        ));
    }
    if parts.len() < 4 {
        bail!("monitor needs `<name>, <mode>, <position>, <scale>`");
    }
    let mut fields = format!(
        "output = {}, mode = {}, position = {}, scale = {}",
        lua_str(output),
        lua_str(parts[1]),
        lua_str(parts[2]),
        lua_str(parts[3]),
    );
    let mut extra = parts[4..].iter();
    while let Some(name) = extra.next() {
        let Some(val) = extra.next() else {
            bail!("monitor option `{name}` has no value");
        };
        match *name {
            "mirror" => fields.push_str(&format!(", mirror = {}", lua_str(val))),
            other => bail!("monitor option `{other}` is not translated; use hl.monitor through bread.hyprland.eval"),
        }
    }
    Ok(format!("hl.monitor({{ {fields} }})"))
}

/// `section:option value` (or `a:b:option value`) into `hl.config`.
fn config_to_lua(key: &str, value: &str) -> Result<String> {
    let path: Vec<&str> = key.split(':').collect();
    if path.iter().any(|p| p.is_empty()) {
        bail!("`{key}` is not a config option");
    }
    let mut expr = lua_value(value);
    for seg in path.iter().rev() {
        expr = format!("{{ {seg} = {expr} }}");
    }
    Ok(format!("hl.config({expr})"))
}

/// Render a legacy config value as a Lua literal: booleans and numbers stay
/// bare, anything else is quoted.
fn lua_value(value: &str) -> String {
    match value {
        "true" | "yes" | "on" => "true".to_string(),
        "false" | "no" | "off" => "false".to_string(),
        v if v
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
            && v.parse::<f64>().is_ok() =>
        {
            v.to_string()
        }
        v => lua_str(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn dispatch_workspace_translates_to_focus() {
        assert_eq!(
            legacy_dispatch_to_lua("workspace", "5").unwrap(),
            "hl.dsp.focus({ workspace = '5' })"
        );
    }

    #[test]
    fn dispatch_lua_form_passes_through() {
        assert_eq!(
            legacy_dispatch_to_lua("hl.dsp.focus({ workspace = '5' })", "").unwrap(),
            "hl.dsp.focus({ workspace = '5' })"
        );
    }

    #[test]
    fn dispatch_dpms_with_and_without_monitor() {
        assert_eq!(
            legacy_dispatch_to_lua("dpms", "off").unwrap(),
            "hl.dsp.dpms({ action = 'off' })"
        );
        assert_eq!(
            legacy_dispatch_to_lua("dpms", "on DP-1").unwrap(),
            "hl.dsp.dpms({ action = 'on', monitor = 'DP-1' })"
        );
        assert!(legacy_dispatch_to_lua("dpms", "sideways").is_err());
    }

    #[test]
    fn dispatch_moveworkspacetomonitor() {
        assert_eq!(
            legacy_dispatch_to_lua("moveworkspacetomonitor", "3 HDMI-A-1").unwrap(),
            "hl.dsp.workspace.move({ monitor = 'HDMI-A-1', workspace = '3' })"
        );
        assert!(legacy_dispatch_to_lua("moveworkspacetomonitor", "3").is_err());
    }

    #[test]
    fn dispatch_movetoworkspace_variants() {
        assert_eq!(
            legacy_dispatch_to_lua("movetoworkspace", "2").unwrap(),
            "hl.dsp.window.move({ workspace = '2' })"
        );
        assert_eq!(
            legacy_dispatch_to_lua("movetoworkspacesilent", "2").unwrap(),
            "hl.dsp.window.move({ workspace = '2', follow = false })"
        );
        let targeted = legacy_dispatch_to_lua("movetoworkspacesilent", "2,address:0xabc").unwrap();
        assert_eq!(
            targeted,
            "function() assert(hl.get_window('address:0xabc'), 'window selector did not match'); \
             local result = hl.dispatch(hl.dsp.window.move({ workspace = '2', follow = false, window = 'address:0xabc' })); \
             assert(result and result.ok, result and result.error or 'window move failed'); return result end"
        );
    }

    #[test]
    fn targeted_moves_check_selector_and_dispatch_result_without_focusing() {
        let lua = mlua::Lua::new();
        lua.load("moves = 0; hl = { dsp = { window = { move = function(opts) assert(opts.window == 'address:0xabc'); assert(opts.follow == false); return opts end } }, get_window = function() return matched end, dispatch = function() moves = moves + 1; return { ok = succeeded, error = 'fixture failure' } end }").exec().unwrap();
        let expr = legacy_dispatch_to_lua("movetoworkspacesilent", "2,address:0xabc").unwrap();
        let run = format!("return ({expr})()");
        lua.load("matched = false; succeeded = true")
            .exec()
            .unwrap();
        assert!(lua.load(&run).eval::<mlua::Value>().is_err());
        assert_eq!(lua.globals().get::<_, i64>("moves").unwrap(), 0);
        lua.load("matched = true; succeeded = false")
            .exec()
            .unwrap();
        assert!(lua.load(&run).eval::<mlua::Value>().is_err());
        lua.load("succeeded = true").exec().unwrap();
        assert!(lua.load(&run).eval::<mlua::Value>().is_ok());
        assert!(!expr.contains("focus"));
    }

    #[test]
    fn dispatch_exec_escapes_quotes_and_backslashes() {
        assert_eq!(
            legacy_dispatch_to_lua("exec", r"notify-send 'it\'s'").unwrap(),
            r"hl.dsp.exec_cmd('notify-send \'it\\\'s\'')"
        );
    }

    #[test]
    fn dispatch_unknown_and_missing_args_are_errors() {
        assert!(legacy_dispatch_to_lua("nonesuch", "x").is_err());
        assert!(legacy_dispatch_to_lua("workspace", "").is_err());
        assert!(legacy_dispatch_to_lua("fullscreen", "7").is_err());
    }

    #[test]
    fn keyword_monitor_translates() {
        assert_eq!(
            legacy_keyword_to_lua("monitor", "HDMI-A-1, 2560x1440, 0x0, 1").unwrap(),
            "hl.monitor({ output = 'HDMI-A-1', mode = '2560x1440', position = '0x0', scale = '1' })"
        );
        assert_eq!(
            legacy_keyword_to_lua("monitor", "DP-3, 1920x1080@60, auto, 1, mirror, eDP-1")
                .unwrap(),
            "hl.monitor({ output = 'DP-3', mode = '1920x1080@60', position = 'auto', scale = '1', mirror = 'eDP-1' })"
        );
        assert_eq!(
            legacy_keyword_to_lua("monitor", "eDP-1, disable").unwrap(),
            "hl.monitor({ output = 'eDP-1', disabled = true })"
        );
        assert!(legacy_keyword_to_lua("monitor", "eDP-1, preferred").is_err());
        assert!(legacy_keyword_to_lua("monitor", "eDP-1, preferred, auto, 1, vrr, 1").is_err());
    }

    #[test]
    fn keyword_config_nests_sections_and_types_values() {
        assert_eq!(
            legacy_keyword_to_lua("general:gaps_in", "5").unwrap(),
            "hl.config({ general = { gaps_in = 5 } })"
        );
        assert_eq!(
            legacy_keyword_to_lua("decoration:blur:enabled", "false").unwrap(),
            "hl.config({ decoration = { blur = { enabled = false } } })"
        );
        assert_eq!(
            legacy_keyword_to_lua("general:layout", "dwindle").unwrap(),
            "hl.config({ general = { layout = 'dwindle' } })"
        );
        assert_eq!(
            legacy_keyword_to_lua("cursor:zoom_factor", "2.0").unwrap(),
            "hl.config({ cursor = { zoom_factor = 2.0 } })"
        );
    }

    #[test]
    fn keyword_bind_and_unbind() {
        assert_eq!(
            legacy_keyword_to_lua("bind", "SUPER SHIFT, Q, exec, kitty --single-instance").unwrap(),
            "hl.bind('SUPER + SHIFT + Q', hl.dsp.exec_cmd('kitty --single-instance'))"
        );
        assert_eq!(
            legacy_keyword_to_lua("bind", ", XF86AudioMute, killactive,").unwrap(),
            "hl.bind('XF86AudioMute', hl.dsp.window.close())"
        );
        assert_eq!(
            legacy_keyword_to_lua("binde", "SUPER, right, movefocus, r").unwrap(),
            "hl.bind('SUPER + right', hl.dsp.focus({ direction = 'r' }), { repeating = true })"
        );
        assert_eq!(
            legacy_keyword_to_lua("unbind", "SUPER, Q").unwrap(),
            "hl.unbind('SUPER + Q')"
        );
        assert!(legacy_keyword_to_lua("bind", "SUPER, Q").is_err());
        assert!(legacy_keyword_to_lua("bind", "SUPER, Q, nonesuch, x").is_err());
    }

    #[test]
    fn keyword_unknown_is_error() {
        assert!(legacy_keyword_to_lua("windowrule", "float, class:foo").is_err());
    }

    /// Replays canned replies and records every request.
    fn fake(replies: Vec<&'static str>) -> (RefCell<Vec<String>>, RefCell<Vec<&'static str>>) {
        let mut r = replies;
        r.reverse();
        (RefCell::new(Vec::new()), RefCell::new(r))
    }

    #[test]
    fn dispatch_uses_legacy_when_accepted() {
        let (seen, replies) = fake(vec!["ok"]);
        let res = dispatch(
            |req| {
                seen.borrow_mut().push(req.to_string());
                Ok(replies.borrow_mut().pop().unwrap().to_string())
            },
            "workspace",
            "2",
        );
        assert!(res.is_ok());
        assert_eq!(*seen.borrow(), vec!["dispatch workspace 2"]);
    }

    #[test]
    fn dispatch_falls_back_to_eval_when_legacy_refused() {
        let (seen, replies) = fake(vec!["')' expected near '5'", "ok"]);
        let res = dispatch(
            |req| {
                seen.borrow_mut().push(req.to_string());
                Ok(replies.borrow_mut().pop().unwrap().to_string())
            },
            "workspace",
            "5",
        );
        assert!(res.is_ok());
        assert_eq!(
            *seen.borrow(),
            vec![
                "dispatch workspace 5",
                "eval hl.dispatch(hl.dsp.focus({ workspace = '5' }))"
            ]
        );
    }

    #[test]
    fn dispatch_surfaces_failure_of_both_forms() {
        let (_, replies) = fake(vec!["legacy bad", "lua bad"]);
        let err = dispatch(
            |_| Ok(replies.borrow_mut().pop().unwrap().to_string()),
            "workspace",
            "5",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("legacy bad"), "{err}");
        assert!(err.contains("lua bad"), "{err}");
    }

    #[test]
    fn dispatch_without_translation_reports_the_reply() {
        let err = dispatch(|_| Ok("nope".to_string()), "nonesuch", "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("no hyprland.lua equivalent"), "{err}");
    }

    #[test]
    fn dispatch_lua_form_is_sent_as_is() {
        let (seen, replies) = fake(vec!["ok"]);
        dispatch(
            |req| {
                seen.borrow_mut().push(req.to_string());
                Ok(replies.borrow_mut().pop().unwrap().to_string())
            },
            "hl.dsp.focus({ workspace = '5' })",
            "",
        )
        .unwrap();
        assert_eq!(
            *seen.borrow(),
            vec!["dispatch hl.dsp.focus({ workspace = '5' })"]
        );
    }

    #[test]
    fn keyword_refused_reply_falls_back_to_eval() {
        let (seen, replies) = fake(vec![
            "keyword can't work with non-legacy parsers. Use eval.",
            "ok",
        ]);
        keyword(
            |req| {
                seen.borrow_mut().push(req.to_string());
                Ok(replies.borrow_mut().pop().unwrap().to_string())
            },
            "general:gaps_in",
            "5",
        )
        .unwrap();
        assert_eq!(
            *seen.borrow(),
            vec![
                "keyword general:gaps_in 5",
                "eval hl.config({ general = { gaps_in = 5 } })"
            ]
        );
    }

    #[test]
    fn keyword_io_error_is_propagated() {
        let err = keyword(|_| Err(anyhow!("socket gone")), "general:gaps_in", "5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("socket gone"));
    }
}
