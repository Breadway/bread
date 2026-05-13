use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::sleep;

#[tokio::test]
async fn ping_and_state_dump_work() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let ping = harness.send_request("ping", json!({})).await?;
    assert_eq!(ping.get("ok").and_then(Value::as_bool), Some(true));

    let health = harness.send_request("health", json!({})).await?;
    assert_eq!(health.get("ok").and_then(Value::as_bool), Some(true));
    assert!(health.get("version").and_then(Value::as_str).is_some());
    assert!(health.get("uptime_ms").and_then(Value::as_u64).is_some());

    let dump = harness.send_request("state.dump", json!({})).await?;
    assert!(dump.get("devices").is_some());
    assert!(dump.get("profile").is_some());

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn unknown_method_returns_error() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness.send_request("not.a.real.method", json!({})).await;
    assert!(result.is_err(), "expected error for unknown method");
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("unknown method"),
        "expected 'unknown method', got: {msg}"
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn profile_activate_updates_state() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness
        .send_request("profile.activate", json!({"name": "battery"}))
        .await?;
    assert_eq!(
        result.get("active").and_then(Value::as_str),
        Some("battery")
    );

    let dump = harness.send_request("state.dump", json!({})).await?;
    assert_eq!(
        dump.get("profile")
            .and_then(|v| v.get("active"))
            .and_then(Value::as_str),
        Some("battery")
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn profile_activate_without_name_errors() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness.send_request("profile.activate", json!({})).await;
    assert!(result.is_err());
    let msg = result.err().unwrap().to_string();
    assert!(msg.contains("missing profile name"), "got: {msg}");

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_without_event_errors() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness.send_request("emit", json!({})).await;
    assert!(result.is_err());

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn state_get_returns_specific_subtree() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let modules = harness
        .send_request("state.get", json!({"key": "modules"}))
        .await?;
    assert!(modules.is_array(), "expected modules to be an array");

    let active = harness
        .send_request("state.get", json!({"key": "profile.active"}))
        .await?;
    assert!(
        active.as_str().is_some(),
        "expected profile.active to be a string, got: {active:?}"
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn state_get_missing_key_returns_error() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness
        .send_request("state.get", json!({"key": "does.not.exist"}))
        .await;
    assert!(result.is_err());

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn modules_list_returns_array() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness.send_request("modules.list", json!({})).await?;
    assert!(result.is_array());

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn modules_reload_succeeds() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness.send_request("modules.reload", json!({})).await?;
    assert_eq!(result.get("ok").and_then(Value::as_bool), Some(true));
    assert!(result.get("duration_ms").is_some());

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn sync_status_uninitialized_when_no_config() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness.send_request("sync.status", json!({})).await?;
    assert_eq!(
        result.get("initialized").and_then(Value::as_bool),
        Some(false)
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn sync_status_reports_initialized_with_config() -> Result<()> {
    let harness = TestHarness::spawn_with_sync_config("myhost", "git@example.com:user/repo.git")?;
    harness.wait_until_ready().await?;

    let result = harness.send_request("sync.status", json!({})).await?;
    assert_eq!(
        result.get("initialized").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        result.get("machine").and_then(Value::as_str),
        Some("myhost")
    );
    assert_eq!(
        result.get("remote").and_then(Value::as_str),
        Some("git@example.com:user/repo.git")
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn events_replay_returns_buffered_events() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    // Emit a couple of events.
    harness
        .send_request("emit", json!({"event": "bread.replay.a", "data": {}}))
        .await?;
    harness
        .send_request("emit", json!({"event": "bread.replay.b", "data": {}}))
        .await?;

    // Small delay so the events make it into the buffer.
    sleep(Duration::from_millis(100)).await;

    let result = harness
        .send_request("events.replay", json!({"since_ms": 10_000}))
        .await?;
    let arr = result.as_array().expect("replay result should be array");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|e| e.get("event").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"bread.replay.a"));
    assert!(names.contains(&"bread.replay.b"));

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn event_stream_filter_excludes_non_matching_events() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let stream = UnixStream::connect(harness.socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let subscribe = json!({
        "id": "sub-x",
        "method": "events.subscribe",
        "params": {
            "filter": "bread.match.*"
        }
    });
    write_half
        .write_all(format!("{}\n", serde_json::to_string(&subscribe)?).as_bytes())
        .await?;

    let mut reader = BufReader::new(read_half).lines();
    // Consume the ack line.
    reader.next_line().await?;

    // Emit one matching and one non-matching event.
    harness
        .send_request("emit", json!({"event": "bread.nomatch.x", "data": {}}))
        .await?;
    harness
        .send_request("emit", json!({"event": "bread.match.yes", "data": {}}))
        .await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut matched = false;
    while Instant::now() < deadline {
        let Some(line) = reader.next_line().await? else {
            break;
        };
        let event: Value = serde_json::from_str(&line)?;
        let name = event.get("event").and_then(Value::as_str).unwrap_or("");
        assert!(
            !name.starts_with("bread.nomatch"),
            "filter let through non-matching event: {name}"
        );
        if name == "bread.match.yes" {
            matched = true;
            break;
        }
    }
    assert!(matched, "did not receive matching event through filter");

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn multiple_concurrent_clients_each_get_response() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;
    let socket = harness.socket_path().to_path_buf();

    let mut handles = Vec::new();
    for i in 0..8 {
        let socket = socket.clone();
        handles.push(tokio::spawn(async move {
            let stream = UnixStream::connect(&socket).await?;
            let (read_half, mut write_half) = stream.into_split();
            let req = json!({"id": i.to_string(), "method": "ping", "params": {}});
            write_half
                .write_all(format!("{}\n", serde_json::to_string(&req)?).as_bytes())
                .await?;
            let mut lines = BufReader::new(read_half).lines();
            let line = lines.next_line().await?.ok_or_else(|| anyhow!("eof"))?;
            let parsed: Value = serde_json::from_str(&line)?;
            assert_eq!(
                parsed.get("id").and_then(Value::as_str),
                Some(i.to_string().as_str())
            );
            Ok::<(), anyhow::Error>(())
        }));
    }
    for h in handles {
        h.await??;
    }

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn events_stream_receives_emitted_events() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let stream = UnixStream::connect(harness.socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let subscribe = json!({
        "id": "sub-1",
        "method": "events.subscribe",
        "params": {
            "filter": "bread.system.*"
        }
    });
    write_half
        .write_all(format!("{}\n", serde_json::to_string(&subscribe)?).as_bytes())
        .await?;

    let mut reader = BufReader::new(read_half).lines();

    let ack = reader
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("missing subscribe ack"))?;
    let ack_json: Value = serde_json::from_str(&ack)?;
    assert_eq!(
        ack_json
            .get("result")
            .and_then(|v| v.get("subscribed"))
            .and_then(Value::as_bool),
        Some(true)
    );

    harness
        .send_request(
            "emit",
            json!({
                "event": "bread.system.test",
                "data": { "ok": true }
            }),
        )
        .await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = false;
    while Instant::now() < deadline {
        let Some(line) = reader.next_line().await? else {
            break;
        };
        let event: Value = serde_json::from_str(&line)?;
        if event.get("event").and_then(Value::as_str) == Some("bread.system.test") {
            got = true;
            break;
        }
    }

    assert!(got, "did not receive emitted event on stream");
    harness.shutdown();
    Ok(())
}

struct TestHarness {
    _temp: TempDir,
    child: Child,
    socket_path: PathBuf,
}

impl TestHarness {
    fn spawn() -> Result<Self> {
        Self::spawn_inner(None)
    }

    fn spawn_with_sync_config(machine: &str, remote_url: &str) -> Result<Self> {
        Self::spawn_inner(Some((machine.to_string(), remote_url.to_string())))
    }

    fn spawn_inner(sync_config: Option<(String, String)>) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let runtime_dir = temp.path().join("runtime");
        let config_home = temp.path().join("config");
        let home = temp.path().join("home");
        fs::create_dir_all(&runtime_dir)?;
        fs::create_dir_all(&config_home)?;
        fs::create_dir_all(&home)?;

        let bread_cfg = config_home.join("bread");
        fs::create_dir_all(bread_cfg.join("modules"))?;

        fs::write(
            bread_cfg.join("init.lua"),
            "bread.on('bread.system.startup', function() end)\n",
        )?;

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
"#,
        )?;

        if let Some((machine, remote_url)) = sync_config {
            let sync_toml = format!(
                r#"
[remote]
url = "{remote_url}"
branch = "main"

[machine]
name = "{machine}"
tags = []
"#
            );
            fs::write(bread_cfg.join("sync.toml"), sync_toml)?;
        }

        let socket_path = runtime_dir.join("bread").join("breadd.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_breadd"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        Ok(Self {
            _temp: temp,
            child,
            socket_path,
        })
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    async fn wait_until_ready(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if self.socket_path.exists() {
                let ping = self.send_request("ping", json!({})).await;
                if ping.is_ok() {
                    return Ok(());
                }
            }
            sleep(Duration::from_millis(100)).await;
        }

        Err(anyhow!("daemon did not become ready in time"))
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let stream = UnixStream::connect(self.socket_path()).await?;
        let (read_half, mut write_half) = stream.into_split();

        let req = json!({
            "id": "1",
            "method": method,
            "params": params,
        });
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

    fn shutdown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
