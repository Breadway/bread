//! BREAD-42: `bread.hyprland` for out-of-process (manifest) modules.
//!
//! A real `breadd` and real sandboxed `bread-module-host` children are
//! spawned. The compositor is a fake: a Unix socket at
//! `$XDG_RUNTIME_DIR/hypr/<signature>/.socket.sock` inside a throwaway runtime
//! dir, which is where `breadd` looks (it talks to the Hyprland request socket
//! directly and never runs `hyprctl`, so a fake `hyprctl` on PATH would not be
//! exercised). `HYPRLAND_INSTANCE_SIGNATURE` and `XDG_RUNTIME_DIR` are both
//! overridden for every spawned process, so the host compositor is never
//! reachable from these tests.

mod common;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

const SIGNATURE: &str = "fake-instance";

/// A stand-in Hyprland request socket. Records every request it receives and
/// answers from a fixed script. Each connection carries one request and one
/// reply, like the real socket.
struct FakeHyprland {
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl FakeHyprland {
    fn start(runtime_dir: &Path) -> Result<Self> {
        let dir = runtime_dir.join("hypr").join(SIGNATURE);
        fs::create_dir_all(&dir)?;
        let listener = UnixListener::bind(dir.join(".socket.sock"))?;
        listener.set_nonblocking(true)?;

        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, flag) = (requests.clone(), stop.clone());
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let Ok((mut conn, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let _ = conn.set_nonblocking(false);
                let mut buf = [0u8; 4096];
                let Ok(n) = conn.read(&mut buf) else { continue };
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let reply = Self::reply(&request);
                log.lock().unwrap().push(request);
                let _ = conn.write_all(reply.as_bytes());
            }
        });

        Ok(Self { requests, stop })
    }

    /// What a `hyprland.lua` Hyprland answers: legacy `keyword` is refused
    /// (as ordinary reply text), legacy `dispatch` is a syntax error, and
    /// `eval` runs Lua.
    fn reply(request: &str) -> String {
        match request {
            "dispatch workspace 3" => "ok".into(),
            "dispatch workspace 9" => "Invalid dispatcher".into(),
            "eval hl.dispatch(hl.dsp.focus({ workspace = '9' }))" => {
                "error: no such workspace 9".into()
            }
            "keyword general:gaps_in 5" => {
                "keyword can't work with non-legacy parsers. Use eval.".into()
            }
            "eval hl.config({ general = { gaps_in = 5 } })" => "ok".into(),
            "eval bogus(" => "error: syntax error near end of input".into(),
            "j/monitors" => r#"[{"name":"DP-1"}]"#.into(),
            "j/activewindow" => r#"{"class":"kitty"}"#.into(),
            other => format!("unexpected request: {other}"),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeHyprland {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Harness {
    _temp: TempDir,
    child: Child,
    socket_path: PathBuf,
    hyprland: FakeHyprland,
}

impl Harness {
    fn spawn(modules: &[(&str, &str, &str)]) -> Result<Self> {
        let module_host_bin = common::ensure_module_host_built();
        let temp = tempfile::tempdir()?;
        let runtime_dir = temp.path().join("runtime");
        let config_home = temp.path().join("config");
        let home = temp.path().join("home");
        fs::create_dir_all(&runtime_dir)?;
        fs::create_dir_all(&config_home)?;
        fs::create_dir_all(&home)?;

        let hyprland = FakeHyprland::start(&runtime_dir)?;

        let bread_cfg = config_home.join("bread");
        fs::create_dir_all(bread_cfg.join("modules"))?;
        fs::write(
            bread_cfg.join("init.lua"),
            "bread.on('bread.system.startup', function() end)\n",
        )?;
        for (name, manifest, init_lua) in modules {
            let dir = bread_cfg.join("modules").join(name);
            fs::create_dir_all(&dir)?;
            fs::write(dir.join("bread.module.toml"), manifest)?;
            fs::write(dir.join("init.lua"), init_lua)?;
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
        )?;

        let socket_path = runtime_dir.join("bread").join("breadd.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_breadd"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("HOME", &home)
            .env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE)
            .env(common::BREAD_MODULE_HOST_BIN_ENV, &module_host_bin)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        Ok(Self {
            _temp: temp,
            child,
            socket_path,
            hyprland,
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (read_half, mut write_half) = stream.into_split();
        let req = json!({ "id": "1", "method": method, "params": params });
        write_half
            .write_all(format!("{}\n", serde_json::to_string(&req)?).as_bytes())
            .await?;
        let mut lines = BufReader::new(read_half).lines();
        let line = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("missing ipc response"))?;
        let parsed: Value = serde_json::from_str(&line)?;
        if let Some(err) = parsed.get("error").and_then(Value::as_str) {
            return Err(anyhow!(err.to_string()));
        }
        Ok(parsed.get("result").cloned().unwrap_or_else(|| json!({})))
    }

    async fn wait_until_ready(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            // Connectivity is not startup readiness. The daemon binds IPC
            // before the Lua thread has necessarily loaded modules; wait for
            // its explicit startup/state-engine barrier before querying
            // module status or triggering handlers.
            if self.socket_path.exists() && self.request("startup.ready", json!({})).await.is_ok() {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!("daemon did not become ready in time"))
    }

    async fn wait_for_module_loaded(&self, name: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(55);
        while Instant::now() < deadline {
            let modules = self.request("modules.list", json!({})).await?;
            for m in modules.as_array().into_iter().flatten() {
                if m.get("name").and_then(Value::as_str) != Some(name) {
                    continue;
                }
                match m.get("status").and_then(Value::as_str) {
                    Some("loaded") => return Ok(()),
                    Some("load_error") => {
                        return Err(anyhow!("module '{name}' failed: {:?}", m.get("last_error")))
                    }
                    _ => {}
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!("module '{name}' did not load in time"))
    }

    /// Fire `test.trigger` and return the `data` of the next `result_event`.
    async fn trigger_and_collect(&self, result_event: &str) -> Result<Value> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (read_half, mut write_half) = stream.into_split();
        let subscribe = json!({
            "id": "sub",
            "method": "events.subscribe",
            "params": { "filter": result_event },
        });
        write_half
            .write_all(format!("{}\n", serde_json::to_string(&subscribe)?).as_bytes())
            .await?;
        let mut reader = BufReader::new(read_half).lines();
        let _ack = reader.next_line().await?;

        self.request("emit", json!({ "event": "test.trigger", "data": {} }))
            .await?;

        let line = timeout(Duration::from_secs(20), reader.next_line())
            .await
            .map_err(|_| anyhow!("timed out waiting for {result_event}"))??
            .ok_or_else(|| anyhow!("connection closed before {result_event}"))?;
        let event: Value = serde_json::from_str(&line)?;
        event
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow!("event missing data"))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const GRANTED_MANIFEST: &str = r#"
name = "hypr-granted"

[[permissions]]
type = "hyprland"
"#;

const GRANTED_LUA: &str = r#"
local M = bread.module({ name = "hypr-granted", version = "1.0.0" })

bread.on("test.trigger", function()
    local ok_dispatch, err_dispatch = pcall(bread.hyprland.dispatch, "workspace", "3")
    local ok_refused, err_refused = pcall(bread.hyprland.dispatch, "workspace", "9")
    local ok_keyword, err_keyword = pcall(bread.hyprland.keyword, "general:gaps_in", "5")
    local eval_reply = bread.hyprland.eval("bogus(")
    local monitors = bread.hyprland.monitors()
    local window = bread.hyprland.active_window()

    bread.emit("test.hypr_granted_result", {
        has_hyprland = bread.hyprland ~= nil,
        ok_dispatch = ok_dispatch,
        err_dispatch = tostring(err_dispatch),
        ok_refused = ok_refused,
        err_refused = tostring(err_refused),
        ok_keyword = ok_keyword,
        err_keyword = tostring(err_keyword),
        eval_reply = eval_reply,
        monitor = monitors[1].name,
        window_class = window.class,
    })
end)

return M
"#;

const DENIED_MANIFEST: &str = r#"
name = "hypr-denied"

[[permissions]]
type = "state.read"
"#;

const DENIED_LUA: &str = r#"
local M = bread.module({ name = "hypr-denied", version = "1.0.0" })

bread.on("test.trigger", function()
    bread.emit("test.hypr_denied_result", {
        hyprland_is_nil = bread.hyprland == nil,
    })
end)

return M
"#;

#[tokio::test]
async fn granted_module_drives_hyprland_with_checked_replies() -> Result<()> {
    let h = Harness::spawn(&[("hypr-granted", GRANTED_MANIFEST, GRANTED_LUA)])?;
    h.wait_until_ready().await?;
    h.wait_for_module_loaded("hypr-granted").await?;

    let data = h.trigger_and_collect("test.hypr_granted_result").await?;

    assert_eq!(data["has_hyprland"], json!(true));
    assert_eq!(data["ok_dispatch"], json!(true), "{data}");
    assert_eq!(data["monitor"], json!("DP-1"));
    assert_eq!(data["window_class"], json!("kitty"));
    // eval hands back the raw reply, exactly like the in-process binding.
    assert_eq!(
        data["eval_reply"],
        json!("error: syntax error near end of input")
    );
    // keyword was refused by the legacy form and applied through eval.
    assert_eq!(data["ok_keyword"], json!(true), "{data}");

    let requests = h.hyprland.requests();
    assert!(requests.contains(&"dispatch workspace 3".to_string()));
    assert!(requests.contains(&"keyword general:gaps_in 5".to_string()));
    assert!(requests.contains(&"eval hl.config({ general = { gaps_in = 5 } })".to_string()));
    Ok(())
}

#[tokio::test]
async fn refused_reply_is_an_error_in_the_module() -> Result<()> {
    let h = Harness::spawn(&[("hypr-granted", GRANTED_MANIFEST, GRANTED_LUA)])?;
    h.wait_until_ready().await?;
    h.wait_for_module_loaded("hypr-granted").await?;

    let data = h.trigger_and_collect("test.hypr_granted_result").await?;

    assert_eq!(data["ok_refused"], json!(false), "{data}");
    let err = data["err_refused"].as_str().unwrap();
    assert!(
        err.contains("hyprland rejected `dispatch workspace 9`"),
        "{err}"
    );
    assert!(err.contains("Invalid dispatcher"), "{err}");
    assert!(err.contains("no such workspace 9"), "{err}");
    Ok(())
}

#[tokio::test]
async fn module_without_the_permission_has_no_hyprland_and_cannot_reach_it() -> Result<()> {
    let h = Harness::spawn(&[("hypr-denied", DENIED_MANIFEST, DENIED_LUA)])?;
    h.wait_until_ready().await?;
    h.wait_for_module_loaded("hypr-denied").await?;

    let data = h.trigger_and_collect("test.hypr_denied_result").await?;
    assert_eq!(data["hyprland_is_nil"], json!(true));

    // The methods exist only on an authenticated module-host connection; an
    // ordinary client cannot use them.
    for method in [
        "module_host.hyprland_dispatch",
        "module_host.hyprland_keyword",
        "module_host.hyprland_eval",
    ] {
        let err = h
            .request(
                method,
                json!({ "cmd": "workspace", "args": "3", "expr": "x" }),
            )
            .await
            .expect_err("unauthenticated hyprland method must fail");
        assert!(
            err.to_string().contains("unknown method"),
            "{method}: {err}"
        );
    }

    assert!(
        h.hyprland.requests().is_empty(),
        "nothing may reach the compositor: {:?}",
        h.hyprland.requests()
    );
    Ok(())
}
