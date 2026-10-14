//! Shared harness for the module-host Hyprland bridge test targets
//! (`module_host_hyprland_wire.rs`, `module_host_hyprland_deadlines.rs`).
//!
//! A real `breadd` is spawned against a fake Hyprland request socket. Its
//! module hosts are `examples/fake_module_host.rs`, which breadd spawns inside
//! the normal sandbox and which authenticates over the real one-time token
//! handshake, then sends exactly the wire requests a test scripts. That makes
//! the tests speak the real authenticated protocol, including requests the
//! real `bread-module-host` would never send (forged fields, replays).
//! `XDG_RUNTIME_DIR` and `HYPRLAND_INSTANCE_SIGNATURE` are overridden, so the
//! user's compositor is never reachable.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

pub const SIGNATURE: &str = "fake-instance";

/// Build the fake module host example once and return its path.
pub fn fake_host_bin() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let output = Command::new(cargo)
            .args([
                "build",
                "-p",
                "breadd",
                "--example",
                "fake_module_host",
                "--locked",
                "--message-format=json",
            ])
            .output()
            .expect("failed to run cargo build for fake_module_host");
        assert!(
            output.status.success(),
            "building fake_module_host failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|msg| msg["reason"] == "compiler-artifact")
            .filter(|msg| msg["target"]["name"] == "fake_module_host")
            .filter_map(|msg| msg["executable"].as_str().map(PathBuf::from))
            .next_back()
            .expect("cargo reported no fake_module_host executable")
    })
    .clone()
}

/// What the fake compositor does with one connection after reading its
/// request: write bytes, sleep, or hold the connection.
pub type Behaviour = Arc<dyn Fn(&str, &mut UnixStream) + Send + Sync>;

pub fn reply(text: &'static str) -> Behaviour {
    Arc::new(move |_, conn| {
        let _ = conn.write_all(text.as_bytes());
    })
}

/// Stand-in for Hyprland's request socket. One thread per connection, so a
/// stalled connection never blocks another. Every request is recorded with
/// the time it arrived.
pub struct Compositor {
    pub requests: Arc<Mutex<Vec<(Instant, String)>>>,
    stop: Arc<AtomicBool>,
}

impl Compositor {
    pub fn start(runtime_dir: &Path, behaviour: Behaviour) -> Self {
        let dir = runtime_dir.join("hypr").join(SIGNATURE);
        fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join(".socket.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, flag) = (requests.clone(), stop.clone());
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let Ok((mut conn, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let (log, behaviour) = (log.clone(), behaviour.clone());
                std::thread::spawn(move || {
                    let _ = conn.set_nonblocking(false);
                    let mut buf = vec![0u8; 128 * 1024];
                    let Ok(n) = conn.read(&mut buf) else { return };
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    log.lock().unwrap().push((Instant::now(), request.clone()));
                    behaviour(&request, &mut conn);
                });
            }
        });
        Self { requests, stop }
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, r)| r.clone())
            .collect()
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A listening Hyprland socket that never accepts, with its backlog already
/// full, so a blocking `connect(2)` to it does not complete.
pub struct NeverAccepts {
    _fds: Vec<OwnedFd>,
}

impl NeverAccepts {
    pub fn start(runtime_dir: &Path) -> Self {
        let dir = runtime_dir.join("hypr").join(SIGNATURE);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".socket.sock");
        let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
        // SAFETY: sockaddr_un is plain data; all-zero is valid.
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        assert!(bytes.len() < addr.sun_path.len());
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
            *dst = *src as libc::c_char;
        }
        let size = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let sa = (&addr as *const libc::sockaddr_un).cast();
        let mut fds = Vec::new();
        // SAFETY: plain socket calls on fds that are immediately owned.
        unsafe {
            let listener = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            assert!(listener >= 0);
            fds.push(OwnedFd::from_raw_fd(listener));
            assert_eq!(libc::bind(listener, sa, size), 0);
            assert_eq!(libc::listen(listener, 0), 0);
            for _ in 0..16 {
                let c = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0);
                assert!(c >= 0);
                fds.push(OwnedFd::from_raw_fd(c));
                let _ = libc::connect(c, sa, size);
            }
        }
        Self { _fds: fds }
    }
}

pub struct ModuleSpec {
    pub name: &'static str,
    /// Manifest permission types, e.g. `["hyprland"]` or `["state.read"]`.
    pub permissions: &'static [&'static str],
    /// Merged into the hello params by the fake host (identity forgery).
    pub hello_extra: Value,
}

impl ModuleSpec {
    pub fn granted(name: &'static str) -> Self {
        Self {
            name,
            permissions: &["hyprland"],
            hello_extra: Value::Null,
        }
    }

    pub fn denied(name: &'static str) -> Self {
        Self {
            name,
            permissions: &["state.read"],
            hello_extra: Value::Null,
        }
    }

    pub fn with_hello(mut self, extra: Value) -> Self {
        self.hello_extra = extra;
        self
    }
}

pub struct Harness {
    _temp: TempDir,
    child: Child,
    pub socket_path: PathBuf,
    pub runtime_dir: PathBuf,
    module_dirs: HashMap<&'static str, PathBuf>,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Harness {
    /// Spawn `breadd` with one fake-host module per spec. `listen` runs once
    /// the runtime dir exists, before `breadd` starts, and returns whatever
    /// must stay alive for the compositor side (kept until the test ends).
    pub fn spawn<T>(modules: &[ModuleSpec], listen: impl FnOnce(&Path) -> T) -> (Self, T) {
        let host_bin = fake_host_bin();
        let temp = tempfile::tempdir().unwrap();
        let runtime_dir = temp.path().join("runtime");
        let config_home = temp.path().join("config");
        let home = temp.path().join("home");
        for dir in [&runtime_dir, &config_home, &home] {
            fs::create_dir_all(dir).unwrap();
        }
        let compositor = listen(&runtime_dir);

        let bread_cfg = config_home.join("bread");
        fs::create_dir_all(bread_cfg.join("modules")).unwrap();
        fs::write(
            bread_cfg.join("init.lua"),
            "bread.on('bread.system.startup', function() end)\n",
        )
        .unwrap();
        let mut module_dirs = HashMap::new();
        for spec in modules {
            let dir = bread_cfg.join("modules").join(spec.name);
            fs::create_dir_all(&dir).unwrap();
            let mut manifest = format!("name = \"{}\"\n", spec.name);
            for perm in spec.permissions {
                manifest.push_str(&format!("\n[[permissions]]\ntype = \"{perm}\"\n"));
            }
            fs::write(dir.join("bread.module.toml"), manifest).unwrap();
            // The daemon only registers a module whose entry file declares
            // itself; the fake host never runs this file.
            fs::write(
                dir.join("init.lua"),
                format!(
                    "local M = bread.module({{ name = \"{}\", version = \"1.0.0\" }})\nreturn M\n",
                    spec.name
                ),
            )
            .unwrap();
            fs::write(dir.join("hello.json"), spec.hello_extra.to_string()).unwrap();
            module_dirs.insert(spec.name, dir);
        }
        fs::write(
            bread_cfg.join("breadd.toml"),
            r#"
[daemon]
log_level = "error"

[lua]
entry_point = "~/.config/bread/init.lua"
module_path = "~/.config/bread/modules"

[modules]
builtin = false

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

        let socket_path = runtime_dir.join("bread").join("breadd.sock");
        let mut child = Command::new(env!("CARGO_BIN_EXE_breadd"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("HOME", &home)
            .env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE)
            .env("BREAD_MODULE_HOST_BIN", &host_bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if std::env::var_os("HYPR_TEST_STDERR").is_some() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .unwrap();

        let lines = Arc::new(Mutex::new(Vec::new()));
        let stdout = child.stdout.take().unwrap();
        let sink = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });

        (
            Self {
                _temp: temp,
                child,
                socket_path,
                runtime_dir,
                module_dirs,
                lines,
            },
            compositor,
        )
    }

    fn wait_lines(
        &self,
        prefix: &str,
        names: &[&str],
        within: Duration,
    ) -> HashMap<String, String> {
        let deadline = Instant::now() + within;
        loop {
            let found: HashMap<String, String> = self
                .lines
                .lock()
                .unwrap()
                .iter()
                .filter_map(|line| line.strip_prefix(prefix))
                .filter_map(|rest| rest.trim_start().split_once(' '))
                .map(|(name, rest)| (name.to_string(), rest.to_string()))
                .collect();
            if names.iter().all(|n| found.contains_key(*n)) {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {prefix} from {names:?}; saw {:?}",
                found.keys().collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Wait until every named fake host has authenticated and reported
    /// loaded. Returns each host's one-time token (already consumed).
    pub fn wait_ready(&self, names: &[&str]) -> HashMap<String, String> {
        self.wait_lines("FAKEHOST_READY", names, Duration::from_secs(90))
            .into_iter()
            .map(|(name, token)| (name, token.trim().to_string()))
            .collect()
    }

    /// Write the script for `name` (does not start it).
    pub fn set_script(&self, name: &str, steps: Vec<(&str, Value)>) {
        let steps: Vec<Value> = steps
            .into_iter()
            .map(|(label, request)| json!({ "label": label, "request": request }))
            .collect();
        fs::write(
            self.module_dirs[name].join("script.json"),
            json!({ "steps": steps }).to_string(),
        )
        .unwrap();
    }

    /// Let the named hosts run their scripts, as close together as possible.
    pub fn go(&self, names: &[&str]) {
        for name in names {
            fs::write(self.module_dirs[*name].join("go"), "").unwrap();
        }
    }

    /// Wait for each host to finish its script and return what it observed.
    pub fn wait_done(&self, names: &[&str], within: Duration) -> HashMap<String, Value> {
        self.wait_lines("FAKEHOST_DONE", names, within)
            .into_iter()
            .map(|(name, json)| (name, serde_json::from_str(&json).unwrap()))
            .collect()
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// SIGTERM the daemon and report how it exited, if it did within `within`.
    pub fn terminate(&mut self, within: Duration) -> Option<ExitStatus> {
        // SAFETY: plain kill(2) on our own child.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
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

impl Drop for Harness {
    fn drop(&mut self) {
        // SAFETY: plain kill(2) on our own child.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

/// One plain (not module-host) client connection: send each line, collect one
/// response line per request, giving up on a response after `per_reply`.
pub fn raw_exchange(socket: &Path, lines: &[Value], per_reply: Duration) -> Vec<Option<Value>> {
    let stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(per_reply)).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut out = Vec::new();
    for line in lines {
        if writer.write_all(format!("{line}\n").as_bytes()).is_err() {
            out.push(None);
            continue;
        }
        let mut reply = String::new();
        match reader.read_line(&mut reply) {
            Ok(n) if n > 0 => out.push(serde_json::from_str(&reply).ok()),
            _ => out.push(None),
        }
    }
    out
}

pub fn request(id: &str, method: &str, params: Value) -> Value {
    json!({ "id": id, "method": method, "params": params })
}

pub fn error_of(response: &Value) -> String {
    response["error"].as_str().unwrap_or("").to_string()
}
