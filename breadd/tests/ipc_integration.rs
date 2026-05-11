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
