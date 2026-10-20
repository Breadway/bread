//! BREAD-43: module `on_unload` hooks must run when the daemon is stopped by
//! a signal, so `bread.binds` removes its Hyprland binds instead of leaving
//! them behind for the next start to duplicate.
//!
//! A real `breadd` runs against the fake Hyprland request socket from
//! `hypr_support`, inside a private `XDG_RUNTIME_DIR`. The compositor here is
//! stateful: it tracks binds the way Hyprland does, so `bread.binds`'s own
//! read-back (`binds -j`) passes exactly as it does against a real compositor.
//! The user's compositor is never reachable from these tests.

#![allow(dead_code)]

mod hypr_support;

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hypr_support::{raw_exchange, request, Behaviour, Compositor, SIGNATURE};
use serde_json::{json, Value};
use tempfile::TempDir;

/// Two binds, so the test sees both unbinds (or the missing second one).
const BIND_A: &str = "F9";
const BIND_B: &str = "F10";

/// A module that installs two binds from its body. It declares
/// `after = { "bread.binds" }` so it loads after the builtin binds module.
const BINDS_MODULE: &str = r#"
local M = bread.module({ name = "daemon-test-binds", version = "1.0.0", after = { "bread.binds" } })
local binds = require("bread.binds")
binds.add({ mods = { "SUPER" }, key = "F9", dispatch = "exec", args = "true" })
binds.add({ mods = { "SUPER" }, key = "F10", dispatch = "exec", args = "true" })
return M
"#;

/// Same two binds, plus an `on_unload` hook that raises. It unloads before
/// `bread.binds` (reverse load order), so `bread.binds` must still run after it.
const RAISING_HOOK_MODULE: &str = r#"
local M = bread.module({ name = "daemon-test-binds", version = "1.0.0", after = { "bread.binds" } })
local binds = require("bread.binds")
binds.add({ mods = { "SUPER" }, key = "F9", dispatch = "exec", args = "true" })
binds.add({ mods = { "SUPER" }, key = "F10", dispatch = "exec", args = "true" })
function M.on_unload()
    error("daemon-test-binds on_unload raised on purpose")
end
return M
"#;

/// An `on_unload` that never returns. Used to prove shutdown is bounded.
const STUCK_HOOK_MODULE: &str = r#"
local M = bread.module({ name = "daemon-test-stuck", version = "1.0.0", after = { "bread.binds" } })
function M.on_unload()
    while true do end
end
return M
"#;

type Bind = (u64, String);

/// Parse the `<mods>, <key>[, <dispatcher>, <args>]` part of a bind or unbind
/// keyword into (modmask, key), the same identity `binds -j` reports.
fn parse_bind(value: &str) -> Bind {
    let mut parts = value.splitn(3, ',');
    let mods = parts.next().unwrap_or("").trim();
    let key = parts.next().unwrap_or("").trim().to_string();
    let mask = if mods
        .split_whitespace()
        .any(|m| m.eq_ignore_ascii_case("SUPER"))
    {
        64
    } else {
        0
    };
    (mask, key)
}

/// A stateful fake compositor. When `fail_unbind_key` is set, every unbind
/// (keyword or eval fallback) for that key is refused, which models Hyprland
/// rejecting the unbind.
fn fake_compositor(fail_unbind_key: Option<&'static str>) -> Behaviour {
    let binds: Arc<Mutex<Vec<Bind>>> = Arc::new(Mutex::new(Vec::new()));
    Arc::new(
        move |req: &str, conn: &mut std::os::unix::net::UnixStream| {
            use std::io::Write;
            let reply = answer(req, &binds, fail_unbind_key);
            let _ = conn.write_all(reply.as_bytes());
        },
    )
}

fn answer(req: &str, binds: &Mutex<Vec<Bind>>, fail_unbind_key: Option<&str>) -> String {
    if let Some(value) = req.strip_prefix("keyword bind ") {
        binds.lock().unwrap().push(parse_bind(value));
        return "ok".to_string();
    }
    if req.contains("unbind") {
        if let Some(key) = fail_unbind_key {
            if req.contains(key) {
                return "error: refused by fake compositor".to_string();
            }
        }
    }
    if let Some(value) = req.strip_prefix("keyword unbind ") {
        let bind = parse_bind(value);
        binds.lock().unwrap().retain(|b| *b != bind);
        return "ok".to_string();
    }
    if req == "j/binds" {
        let list: Vec<Value> = binds
            .lock()
            .unwrap()
            .iter()
            .map(|(mask, key)| json!({ "key": key, "modmask": mask }))
            .collect();
        return Value::Array(list).to_string();
    }
    if req.starts_with("j/") {
        return "[]".to_string();
    }
    "ok".to_string()
}

/// A running `breadd` plus the private directories it uses. Killed on drop so
/// a failing assertion never leaks a daemon.
struct Daemon {
    _temp: TempDir,
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    /// Start `breadd` with the given third-party modules (name, init.lua).
    /// Modules have no manifest, so they load ungated, as the legacy path does.
    fn spawn(modules: &[(&str, &str)], behaviour: Behaviour) -> (Self, Compositor) {
        let temp = tempfile::tempdir().unwrap();
        let runtime_dir = temp.path().join("runtime");
        let config_home = temp.path().join("config");
        let home = temp.path().join("home");
        for dir in [&runtime_dir, &config_home, &home] {
            fs::create_dir_all(dir).unwrap();
        }
        let compositor = Compositor::start(&runtime_dir, behaviour);

        let bread_cfg = config_home.join("bread");
        fs::create_dir_all(bread_cfg.join("modules")).unwrap();
        fs::write(
            bread_cfg.join("init.lua"),
            "bread.on('bread.system.startup', function() end)\n",
        )
        .unwrap();
        for (name, source) in modules {
            let dir = bread_cfg.join("modules").join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("init.lua"), source).unwrap();
        }
        fs::write(
            bread_cfg.join("breadd.toml"),
            r#"
[daemon]
log_level = "error"

[lua]
entry_point = "~/.config/bread/init.lua"
module_path = "~/.config/bread/modules"

[adapters.hyprland]
enabled = false

[adapters.udev]
enabled = false

[adapters.power]
enabled = false

[adapters.network]
enabled = false

[adapters.podman]
enabled = false
"#,
        )
        .unwrap();

        let socket = runtime_dir.join("bread").join("breadd.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_breadd"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("HOME", &home)
            .env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        (
            Self {
                _temp: temp,
                child,
                socket,
            },
            compositor,
        )
    }

    /// Wait until `want_binds` bind requests reached the compositor and the
    /// IPC server answers a ping. A ping answer means the signal handlers in
    /// `wait_for_shutdown` are installed. Before that, SIGTERM would kill the
    /// process outright and skip the shutdown path under test.
    fn wait_ready(&self, compositor: &Compositor, want_binds: usize) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let binds = compositor
                .requests()
                .iter()
                .filter(|r| r.starts_with("keyword bind"))
                .count();
            if binds >= want_binds && self.socket.exists() {
                let replies = raw_exchange(
                    &self.socket,
                    &[request("ready", "ping", json!({}))],
                    Duration::from_secs(2),
                );
                if matches!(replies.first(), Some(Some(_))) {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not become ready with {want_binds} binds installed; requests: {:?}",
                compositor.requests()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn signal(&self, sig: libc::c_int) {
        // SAFETY: plain kill(2) on our own child.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, sig);
        }
    }

    /// Wait for the daemon to exit on its own, up to `within`.
    fn wait_exit(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // SAFETY: plain kill(2) on our own child.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

/// Start the daemon, stop it with `sig`, and return the requests the fake
/// compositor saw plus how the process exited and how long that took.
fn stop_with_signal(
    modules: &[(&str, &str)],
    behaviour: Behaviour,
    want_binds: usize,
    sig: libc::c_int,
) -> (Vec<String>, Option<ExitStatus>, Duration) {
    let (mut daemon, compositor) = Daemon::spawn(modules, behaviour);
    daemon.wait_ready(&compositor, want_binds);
    let started = Instant::now();
    daemon.signal(sig);
    let status = daemon.wait_exit(Duration::from_secs(20));
    let elapsed = started.elapsed();
    (compositor.requests(), status, elapsed)
}

fn unbound(requests: &[String], key: &str) -> bool {
    requests
        .iter()
        .any(|r| r.starts_with("keyword unbind") && r.contains(key))
}

fn assert_clean_exit(status: Option<ExitStatus>) {
    let status = status.expect("daemon did not exit after the signal");
    assert!(
        status.success(),
        "daemon should exit cleanly after shutdown, got {status}"
    );
}

/// (a) SIGTERM with two installed binds: both are unbound on the way out.
#[test]
fn sigterm_unbinds_every_bind_before_exit() {
    let (requests, status, _) = stop_with_signal(
        &[("daemon-test-binds", BINDS_MODULE)],
        fake_compositor(None),
        2,
        libc::SIGTERM,
    );
    assert_clean_exit(status);
    assert!(
        unbound(&requests, BIND_A),
        "no `keyword unbind` for {BIND_A} after SIGTERM; requests: {requests:?}"
    );
    assert!(
        unbound(&requests, BIND_B),
        "no `keyword unbind` for {BIND_B} after SIGTERM; requests: {requests:?}"
    );
}

/// (b1) The first unbind is refused by Hyprland. The second bind must still
/// be unbound: one failed remove cannot stop the rest of the loop.
#[test]
fn refused_unbind_does_not_stop_the_other_bind() {
    let (requests, status, _) = stop_with_signal(
        &[("daemon-test-binds", BINDS_MODULE)],
        fake_compositor(Some(BIND_A)),
        2,
        libc::SIGTERM,
    );
    assert_clean_exit(status);
    assert!(
        unbound(&requests, BIND_A),
        "the refused {BIND_A} unbind was never attempted; requests: {requests:?}"
    );
    assert!(
        unbound(&requests, BIND_B),
        "{BIND_B} was not unbound after {BIND_A} was refused; requests: {requests:?}"
    );
}

/// (b2) A module's `on_unload` raises. It runs first (reverse load order), and
/// `bread.binds` must still run afterwards and unbind both binds.
#[test]
fn raising_on_unload_hook_does_not_stop_later_hooks() {
    let (requests, status, _) = stop_with_signal(
        &[("daemon-test-binds", RAISING_HOOK_MODULE)],
        fake_compositor(None),
        2,
        libc::SIGTERM,
    );
    assert_clean_exit(status);
    assert!(
        unbound(&requests, BIND_A),
        "{BIND_A} not unbound after a sibling hook raised; requests: {requests:?}"
    );
    assert!(
        unbound(&requests, BIND_B),
        "{BIND_B} not unbound after a sibling hook raised; requests: {requests:?}"
    );
}

/// (c) An `on_unload` that never returns. Shutdown must still finish within
/// the bounded wait, so the daemon exits instead of hanging.
#[test]
fn stuck_on_unload_hook_cannot_hang_shutdown() {
    let (_requests, status, elapsed) = stop_with_signal(
        &[
            ("daemon-test-binds", BINDS_MODULE),
            ("daemon-test-stuck", STUCK_HOOK_MODULE),
        ],
        fake_compositor(None),
        2,
        libc::SIGTERM,
    );
    assert_clean_exit(status);
    // The stuck hook never returns, so the daemon must wait out the bound
    // (LUA_SHUTDOWN_TIMEOUT) and then exit anyway. Exiting well before the
    // bound would mean the hook was skipped, and far after it means the
    // bound is not enforced.
    assert!(
        elapsed >= Duration::from_secs(2),
        "daemon exited in {elapsed:?}, before the hook could have been given its time"
    );
    assert!(
        elapsed < Duration::from_secs(12),
        "daemon took {elapsed:?} to exit with a stuck on_unload hook; shutdown is not bounded"
    );
}

/// (d) SIGINT goes through the same shutdown path as SIGTERM.
#[test]
fn sigint_unbinds_every_bind_before_exit() {
    let (requests, status, _) = stop_with_signal(
        &[("daemon-test-binds", BINDS_MODULE)],
        fake_compositor(None),
        2,
        libc::SIGINT,
    );
    assert_clean_exit(status);
    assert!(
        unbound(&requests, BIND_A),
        "no `keyword unbind` for {BIND_A} after SIGINT; requests: {requests:?}"
    );
    assert!(
        unbound(&requests, BIND_B),
        "no `keyword unbind` for {BIND_B} after SIGINT; requests: {requests:?}"
    );
}
