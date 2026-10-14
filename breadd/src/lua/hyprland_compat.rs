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

use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use tracing::warn;

/// Largest request `request_until` will put on the socket.
pub(crate) const MAX_REQUEST_BYTES: usize = 32 * 1024;
/// Largest reply `request_until` will buffer before giving up.
pub(crate) const MAX_REPLY_BYTES: usize = 1024 * 1024;
/// Per-request budget for callers without an operation deadline of their own
/// (the in-process `bread.hyprland` bindings).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Blocking socket calls are issued in slices this long so a cancelled
/// deadline is noticed promptly instead of after the full budget.
const SLICE: Duration = Duration::from_millis(100);
const MIN_SLICE: Duration = Duration::from_millis(1);

/// One operation's time budget. A dispatch with its eval fallback, or a
/// keyword with its read-back, shares a single `Deadline`: every request
/// asks it for the time left before it connects or writes, so nothing is
/// sent once it has expired or been cancelled.
#[derive(Clone)]
pub(crate) struct Deadline {
    at: Instant,
    cancel: Option<Arc<AtomicBool>>,
}

impl Deadline {
    pub(crate) fn after(budget: Duration) -> Self {
        Self {
            at: Instant::now() + budget,
            cancel: None,
        }
    }

    pub(crate) fn cancelled_by(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    fn remaining(&self) -> Result<Duration> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            bail!("hyprland request cancelled: the daemon is shutting down");
        }
        let left = self.at.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("hyprland request timed out");
        }
        Ok(left)
    }

    fn slice(&self) -> Result<Duration> {
        Ok(self.remaining()?.min(SLICE).max(MIN_SLICE))
    }
}

fn is_retryable(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

fn sockaddr_for(socket: &Path) -> Result<libc::sockaddr_un> {
    let bytes = socket.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is plain data; all-zero is a valid value.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        bail!("hyprland socket path is not usable: {}", socket.display());
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    Ok(addr)
}

/// `connect(2)` with the deadline applied. std's `UnixStream::connect` has no
/// timeout and blocks while the listener's backlog is full, so the socket is
/// created by hand: `SO_SNDTIMEO` bounds a blocking unix connect.
fn connect_until(socket: &Path, deadline: &Deadline) -> Result<UnixStream> {
    let addr = sockaddr_for(socket)?;

    // SAFETY: plain socket(2) call.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` is a socket we just created and own; the stream closes it.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };

    loop {
        let slice = deadline.slice()?;
        let timeval = libc::timeval {
            tv_sec: slice.as_secs() as libc::time_t,
            tv_usec: slice.subsec_micros() as libc::suseconds_t,
        };
        // SAFETY: `timeval` outlives the call and the length matches.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDTIMEO,
                (&timeval as *const libc::timeval).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: `addr` is a fully initialised sockaddr_un and the length
        // passed is its size.
        let rc = unsafe {
            libc::connect(
                fd,
                (&addr as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if rc == 0 {
            return Ok(stream);
        }
        let err = std::io::Error::last_os_error();
        if !is_retryable(err.kind()) {
            return Err(anyhow!("connecting to {}: {err}", socket.display()));
        }
    }
}

pub(crate) fn request_socket() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());

    if let Ok(instance) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        return Ok(PathBuf::from(runtime)
            .join("hypr")
            .join(instance)
            .join(".socket.sock"));
    }

    let hypr_dir = PathBuf::from(&runtime).join("hypr");
    let mut sockets: Vec<PathBuf> = std::fs::read_dir(&hypr_dir)
        .map_err(|_| anyhow!("no Hyprland instance found ({})", hypr_dir.display()))?
        .flatten()
        .map(|e| e.path().join(".socket.sock"))
        .filter(|p| p.exists())
        .collect();

    match sockets.len() {
        0 => Err(anyhow!(
            "no Hyprland instance found in {}",
            hypr_dir.display()
        )),
        1 => Ok(sockets.remove(0)),
        _ => {
            warn!(
                "multiple Hyprland instances found in {}; using the first one",
                hypr_dir.display()
            );
            Ok(sockets.remove(0))
        }
    }
}

/// One request with its own [`REQUEST_TIMEOUT`] budget.
pub(crate) fn request(request: &str) -> Result<String> {
    let socket = request_socket()?;
    request_until(&socket, request, &Deadline::after(REQUEST_TIMEOUT))
}

/// A `send` for [`dispatch`] / [`keyword`] that spends one [`REQUEST_TIMEOUT`]
/// budget across every request of the operation (legacy form, eval fallback,
/// read-back) and resolves the socket once, so all of them go to the same
/// Hyprland instance.
pub(crate) fn operation_sender() -> impl FnMut(&str) -> Result<String> {
    let deadline = Deadline::after(REQUEST_TIMEOUT);
    let mut socket: Option<PathBuf> = None;
    move |req| {
        if socket.is_none() {
            socket = Some(request_socket()?);
        }
        match &socket {
            Some(path) => request_until(path, req, &deadline),
            None => bail!("no Hyprland socket"),
        }
    }
}

/// A `send` bound to an already chosen socket and an operation `deadline`.
pub(crate) fn sender_until(
    socket: PathBuf,
    deadline: Deadline,
) -> impl FnMut(&str) -> Result<String> {
    move |req| request_until(&socket, req, &deadline)
}

/// One request/reply exchange on the Hyprland request socket. Connect, write
/// and read all stop at `deadline`; the deadline is checked before anything
/// is sent, the request is capped at [`MAX_REQUEST_BYTES`] and the reply at
/// [`MAX_REPLY_BYTES`].
pub(crate) fn request_until(socket: &Path, request: &str, deadline: &Deadline) -> Result<String> {
    if request.len() > MAX_REQUEST_BYTES {
        bail!(
            "hyprland request is {} bytes, over the {MAX_REQUEST_BYTES} byte limit",
            request.len()
        );
    }
    deadline.remaining()?;

    let mut stream = connect_until(socket, deadline)?;

    let bytes = request.as_bytes();
    let mut sent = 0;
    while sent < bytes.len() {
        stream.set_write_timeout(Some(deadline.slice()?))?;
        match stream.write(&bytes[sent..]) {
            Ok(0) => bail!("hyprland socket closed while writing the request"),
            Ok(n) => sent += n,
            Err(e) if is_retryable(e.kind()) => {}
            Err(e) => return Err(e.into()),
        }
    }

    let mut reply = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        stream.set_read_timeout(Some(deadline.slice()?))?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if reply.len() + n > MAX_REPLY_BYTES {
                    bail!("hyprland reply exceeds the {MAX_REPLY_BYTES} byte limit");
                }
                reply.extend_from_slice(&chunk[..n]);
            }
            Err(e) if is_retryable(e.kind()) => {}
            Err(e) => return Err(e.into()),
        }
    }
    String::from_utf8(reply).map_err(|_| anyhow!("hyprland reply is not valid UTF-8"))
}

/// True when a Hyprland socket reply means success.
pub(crate) fn is_ok(reply: &str) -> bool {
    reply.trim() == "ok"
}

/// Run a legacy-style dispatch (`dispatch <cmd> <args>`), falling back to the
/// equivalent `hl.dispatch(...)` through `eval` when the legacy form is not
/// accepted. `send` performs one request on the Hyprland socket.
///
/// `cmd` may also be a Lua dispatcher expression such as
/// `hl.dsp.focus({ workspace = '5' })` (with no args); Hyprland running from
/// `hyprland.lua` accepts that directly.
pub(crate) fn dispatch<F>(mut send: F, cmd: &str, args: &str) -> Result<()>
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
///
/// An `ok` reply is not trusted on its own for binds, unbinds and monitors:
/// the result is read back over the same socket (`binds -j`, `monitors all -j`)
/// and an error is returned when the state does not match.
pub(crate) fn keyword<F>(mut send: F, key: &str, value: &str) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    if key == "monitor" {
        require_output(&mut send, value)?;
    }

    let reply = send(&format!("keyword {key} {value}"))?;
    if is_ok(&reply) {
        return confirm_keyword(&mut send, key, value);
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
        return confirm_keyword(&mut send, key, value);
    }
    Err(anyhow!(
        "hyprland rejected `keyword {key} {value}`: {} (eval fallback: {})",
        reply.trim(),
        lua_reply.trim()
    ))
}

/// Read back Hyprland state after a bind, unbind or monitor keyword was
/// accepted. `j/...` is the socket form of `hyprctl <cmd> -j`.
fn confirm_keyword<F>(send: &mut F, key: &str, value: &str) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    match key {
        "bind" | "binde" | "bindl" => {
            let (mods, k) = split_bind_head(value)?;
            let binds = read_json(send, "j/binds")?;
            if has_bind(&binds, modmask(&mods)?, &k) {
                return Ok(());
            }
            bail!(
                "hyprland accepted `keyword {key} {value}` but `binds -j` does not list `{}`",
                bind_key(&mods, &k)
            )
        }
        "unbind" => {
            let (mods, k) = split_bind_head(value)?;
            let binds = read_json(send, "j/binds")?;
            if !has_bind(&binds, modmask(&mods)?, &k) {
                return Ok(());
            }
            bail!(
                "hyprland accepted `keyword unbind {value}` but `binds -j` still lists `{}`",
                bind_key(&mods, &k)
            )
        }
        _ => Ok(()),
    }
}

/// Refuse `keyword monitor` for an output that `monitors all -j` does not list,
/// because Hyprland answers `ok` for an eval that creates nothing.
fn require_output<F>(send: &mut F, value: &str) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    let name = value.split(',').next().unwrap_or("").trim();
    let monitors = read_json(send, "j/monitors all")?;
    let present = monitors
        .as_array()
        .is_some_and(|list| list.iter().any(|m| m["name"].as_str() == Some(name)));
    if present {
        Ok(())
    } else {
        bail!("keyword monitor: no output named `{name}` (monitors all -j); nothing was changed")
    }
}

fn read_json<F>(send: &mut F, request: &str) -> Result<serde_json::Value>
where
    F: FnMut(&str) -> Result<String>,
{
    let reply = send(request)?;
    serde_json::from_str(&reply).map_err(|e| {
        anyhow!(
            "hyprland replied to `{request}` with something other than JSON ({e}): {}",
            reply.trim()
        )
    })
}

/// True when `binds -j` lists `key` with the given modifier mask.
fn has_bind(binds: &serde_json::Value, mask: u32, key: &str) -> bool {
    binds.as_array().is_some_and(|list| {
        list.iter().any(|b| {
            b["key"]
                .as_str()
                .is_some_and(|k| k.eq_ignore_ascii_case(key))
                && b["modmask"].as_u64() == Some(u64::from(mask))
        })
    })
}

/// Hyprland's `modmask` bits for the modifier names bind lines use.
fn modmask(mods: &[String]) -> Result<u32> {
    mods.iter().try_fold(0u32, |acc, m| {
        let bit = match m.to_ascii_uppercase().as_str() {
            "SHIFT" => 1,
            "CAPS" => 2,
            "CTRL" | "CONTROL" => 4,
            "ALT" | "MOD1" => 8,
            "MOD2" => 16,
            "MOD3" => 32,
            "SUPER" | "WIN" | "LOGO" | "MOD4" => 64,
            "MOD5" => 128,
            other => bail!("cannot verify bind: unknown modifier `{other}`"),
        };
        Ok(acc | bit)
    })
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
pub(crate) fn legacy_dispatch_to_lua(cmd: &str, args: &str) -> Result<String> {
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
pub(crate) fn legacy_keyword_to_lua(key: &str, value: &str) -> Result<String> {
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
    fn doc_snippets_surface_refused_keyword_instead_of_succeeding() {
        let lua = mlua::Lua::new();
        lua.load(
            "warnings = 0; bread = { warn = function() warnings = warnings + 1 end, \
             hyprland = { keyword = function() error('hyprland rejected keyword') end } }",
        )
        .exec()
        .unwrap();
        lua.load(
            "local function set_monitor(spec)
                local ok = pcall(bread.hyprland.keyword, 'monitor', spec)
                if not ok then bread.warn('could not apply ' .. spec) end
            end
            set_monitor('eDP-1,1920x1080@60,0x0,1')
            set_monitor('DP-3,1920x1080@60,auto,1,mirror,eDP-1')",
        )
        .exec()
        .unwrap();
        assert_eq!(lua.globals().get::<_, i64>("warnings").unwrap(), 2);
        let workflow = "bread.hyprland.keyword('monitor', 'HDMI-A-1,preferred,1920x0,1')";
        assert!(lua.load(workflow).exec().is_err());
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

    const F9_BIND: &str = r#"[{"key":"F9","modmask":0,"dispatcher":"__lua"}]"#;
    const SUPER_Q_BIND: &str = r#"[{"key":"Q","modmask":64,"dispatcher":"__lua"}]"#;

    fn run_keyword(
        replies: Vec<&'static str>,
        key: &str,
        value: &str,
    ) -> (Result<()>, Vec<String>) {
        let (seen, replies) = fake(replies);
        let res = keyword(
            |req| {
                seen.borrow_mut().push(req.to_string());
                Ok(replies.borrow_mut().pop().unwrap().to_string())
            },
            key,
            value,
        );
        let requests = seen.borrow().clone();
        (res, requests)
    }

    #[test]
    fn bind_refused_then_eval_ok_is_confirmed_by_binds_json() {
        let (res, seen) = run_keyword(
            vec![
                "keyword can't work with non-legacy parsers. Use eval.",
                "ok",
                F9_BIND,
            ],
            "bind",
            ", F9, exec, touch /tmp/x",
        );
        res.unwrap();
        assert_eq!(seen.last().unwrap(), "j/binds");
    }

    #[test]
    fn bind_eval_ok_but_missing_from_binds_is_an_error() {
        let (res, _) = run_keyword(
            vec![
                "keyword can't work with non-legacy parsers. Use eval.",
                "ok",
                "[]",
            ],
            "bind",
            ", F9, exec, touch /tmp/x",
        );
        let err = res.unwrap_err().to_string();
        assert!(err.contains("binds -j"), "{err}");
        assert!(err.contains("F9"), "{err}");
    }

    #[test]
    fn bind_listed_with_wrong_modifiers_is_an_error() {
        let (res, _) = run_keyword(vec!["ok", SUPER_Q_BIND], "bind", ", Q, exec, kitty");
        assert!(res.is_err());
    }

    #[test]
    fn bind_accepted_by_legacy_is_still_read_back() {
        let (res, seen) = run_keyword(vec!["ok", "[]"], "bind", "SUPER, Q, exec, kitty");
        assert!(res.is_err());
        assert_eq!(seen, vec!["keyword bind SUPER, Q, exec, kitty", "j/binds"]);
    }

    #[test]
    fn bind_read_back_non_json_is_an_error() {
        let (res, _) = run_keyword(vec!["ok", "not json"], "bind", ", F9, exec, x");
        let err = res.unwrap_err().to_string();
        assert!(err.contains("JSON"), "{err}");
    }

    #[test]
    fn bind_eval_refused_reports_both_replies_without_reading_back() {
        let (res, seen) = run_keyword(
            vec!["refused legacy", "eval exploded"],
            "bind",
            ", F9, exec, x",
        );
        let err = res.unwrap_err().to_string();
        assert!(err.contains("eval exploded"), "{err}");
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn unbind_removed_passes_read_back() {
        let (res, seen) = run_keyword(vec!["ok", "[]"], "unbind", ", F9");
        res.unwrap();
        assert_eq!(seen.last().unwrap(), "j/binds");
    }

    #[test]
    fn unbind_still_listed_is_an_error() {
        let (res, _) = run_keyword(
            vec![
                "keyword can't work with non-legacy parsers. Use eval.",
                "ok",
                F9_BIND,
            ],
            "unbind",
            ", F9",
        );
        let err = res.unwrap_err().to_string();
        assert!(err.contains("still lists"), "{err}");
    }

    const MONITORS: &str = r#"[{"name":"Virtual-1","width":1280},{"name":"TEST-1","width":1920}]"#;

    #[test]
    fn monitor_for_missing_output_is_refused_before_any_change() {
        let (res, seen) = run_keyword(vec![MONITORS], "monitor", "NOPE-9,preferred,auto,1");
        let err = res.unwrap_err().to_string();
        assert!(err.contains("NOPE-9"), "{err}");
        assert_eq!(seen, vec!["j/monitors all"]);
    }

    #[test]
    fn monitor_present_output_is_applied_and_checked() {
        let (res, seen) = run_keyword(vec![MONITORS, "ok"], "monitor", "TEST-1,preferred,auto,1");
        res.unwrap();
        assert_eq!(
            seen,
            vec!["j/monitors all", "keyword monitor TEST-1,preferred,auto,1"]
        );
    }

    #[test]
    fn monitor_output_list_that_is_not_json_is_an_error() {
        let (res, seen) = run_keyword(vec!["garbage"], "monitor", "TEST-1,preferred,auto,1");
        let err = res.unwrap_err().to_string();
        assert!(err.contains("JSON"), "{err}");
        assert_eq!(seen, vec!["j/monitors all"]);
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A stand-in Hyprland request socket for tests: records every request
    //! and lets each test script what happens on a connection.
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;

    pub(crate) type Handler = Arc<dyn Fn(usize, &str, &mut UnixStream) + Send + Sync>;

    pub(crate) struct FakeSocket {
        pub(crate) path: PathBuf,
        pub(crate) requests: Arc<Mutex<Vec<String>>>,
        pub(crate) connections: Arc<std::sync::atomic::AtomicUsize>,
        stop: Arc<AtomicBool>,
        _dir: tempfile::TempDir,
    }

    impl FakeSocket {
        pub(crate) fn start(handler: Handler) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(".socket.sock");
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let (log, count, flag) = (requests.clone(), connections.clone(), stop.clone());
            std::thread::spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    let Ok((mut conn, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    let index = count.fetch_add(1, Ordering::SeqCst);
                    let (log, handler) = (log.clone(), handler.clone());
                    std::thread::spawn(move || {
                        let _ = conn.set_nonblocking(false);
                        let mut buf = vec![0u8; 128 * 1024];
                        let Ok(n) = conn.read(&mut buf) else { return };
                        let request = String::from_utf8_lossy(&buf[..n]).to_string();
                        log.lock().unwrap().push(request.clone());
                        handler(index, &request, &mut conn);
                    });
                }
            });
            Self {
                path,
                requests,
                connections,
                stop,
                _dir: dir,
            }
        }

        pub(crate) fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for FakeSocket {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    /// A listening socket that never accepts and whose backlog is full, so a
    /// further blocking `connect(2)` does not complete.
    pub(crate) struct FullBacklog {
        pub(crate) path: PathBuf,
        _fds: Vec<std::os::fd::OwnedFd>,
        _dir: tempfile::TempDir,
    }

    impl FullBacklog {
        pub(crate) fn new() -> Self {
            use std::os::fd::{FromRawFd, OwnedFd};
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(".socket.sock");
            let addr = sockaddr_for(&path).unwrap();
            let size = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
            let mut fds = Vec::new();
            // SAFETY: plain socket/bind/listen/connect calls on sockets owned
            // here; every fd is wrapped in an OwnedFd so it is closed on drop.
            unsafe {
                let listener = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
                assert!(listener >= 0);
                fds.push(OwnedFd::from_raw_fd(listener));
                let sa = (&addr as *const libc::sockaddr_un).cast();
                assert_eq!(libc::bind(listener, sa, size), 0);
                assert_eq!(libc::listen(listener, 0), 0);
                for _ in 0..16 {
                    let c = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0);
                    assert!(c >= 0);
                    fds.push(OwnedFd::from_raw_fd(c));
                    let _ = libc::connect(c, sa, size);
                }
            }
            Self {
                path,
                _fds: fds,
                _dir: dir,
            }
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::fake::*;
    use super::*;

    fn never_replies() -> Handler {
        Arc::new(|_, _, conn| {
            std::thread::sleep(Duration::from_secs(5));
            let _ = conn.write_all(b"too late");
        })
    }

    #[test]
    fn read_stall_stops_at_the_deadline() {
        let sock = FakeSocket::start(never_replies());
        let started = Instant::now();
        let err = request_until(
            &sock.path,
            "dispatch workspace 9",
            &Deadline::after(Duration::from_millis(400)),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn expired_deadline_sends_nothing() {
        let sock = FakeSocket::start(Arc::new(|_, _, _| {}));
        let deadline = Deadline::after(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(20));
        let err = request_until(&sock.path, "dispatch workspace 9", &deadline)
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "{err}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sock.connections.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn one_deadline_spans_dispatch_and_its_fallback() {
        // The legacy form is rejected after 300ms and the fallback then
        // stalls. With a 600ms operation budget the fallback may only use the
        // ~300ms that are left: a per-request budget would run to ~900ms.
        let sock = FakeSocket::start(Arc::new(|index, _, conn| {
            if index == 0 {
                std::thread::sleep(Duration::from_millis(300));
                let _ = conn.write_all(b"Invalid dispatcher");
            } else {
                std::thread::sleep(Duration::from_secs(5));
            }
        }));
        let started = Instant::now();
        let err = dispatch(
            sender_until(
                sock.path.clone(),
                Deadline::after(Duration::from_millis(600)),
            ),
            "workspace",
            "9",
        )
        .unwrap_err()
        .to_string();
        let elapsed = started.elapsed();
        assert!(err.contains("timed out"), "{err}");
        assert!(elapsed < Duration::from_millis(850), "{elapsed:?}");
        assert_eq!(sock.requests().len(), 2);
    }

    #[test]
    fn budget_spent_by_the_first_request_means_no_second_request() {
        let sock = FakeSocket::start(Arc::new(|_, _, conn| {
            std::thread::sleep(Duration::from_millis(500));
            let _ = conn.write_all(b"Invalid dispatcher");
        }));
        let err = dispatch(
            sender_until(
                sock.path.clone(),
                Deadline::after(Duration::from_millis(300)),
            ),
            "workspace",
            "9",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("timed out"), "{err}");
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(sock.requests(), vec!["dispatch workspace 9"]);
    }

    #[test]
    fn cancelling_stops_a_stalled_request_promptly() {
        let sock = FakeSocket::start(never_replies());
        let flag = Arc::new(AtomicBool::new(false));
        let deadline = Deadline::after(Duration::from_secs(30)).cancelled_by(flag.clone());
        let canceller = {
            let flag = flag.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                flag.store(true, Ordering::Relaxed);
            })
        };
        let started = Instant::now();
        let err = request_until(&sock.path, "j/monitors", &deadline)
            .unwrap_err()
            .to_string();
        canceller.join().unwrap();
        assert!(err.contains("cancelled"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn oversized_reply_is_refused_and_not_followed_up() {
        let sock = FakeSocket::start(Arc::new(|_, _, conn| {
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..(MAX_REPLY_BYTES / chunk.len() + 2) {
                if conn.write_all(&chunk).is_err() {
                    return;
                }
            }
        }));
        let err = dispatch(
            sender_until(sock.path.clone(), Deadline::after(Duration::from_secs(10))),
            "workspace",
            "9",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("byte limit"), "{err}");
        assert_eq!(
            sock.requests().len(),
            1,
            "no fallback after an oversized reply"
        );
    }

    #[test]
    fn oversized_request_is_refused_before_connecting() {
        let sock = FakeSocket::start(Arc::new(|_, _, _| {}));
        let big = "x".repeat(MAX_REQUEST_BYTES + 1);
        let err = request_until(&sock.path, &big, &Deadline::after(Duration::from_secs(5)))
            .unwrap_err()
            .to_string();
        assert!(err.contains("byte limit"), "{err}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sock.connections.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn listener_that_never_accepts_cannot_hold_the_caller() {
        let sock = FullBacklog::new();
        let started = Instant::now();
        let err = request_until(
            &sock.path,
            "j/monitors",
            &Deadline::after(Duration::from_millis(400)),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
