use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

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
async fn emit_without_source_rejects_adapter_owned_event_name() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    // A no-`source` emit must not be able to impersonate a real adapter
    // event by event name alone — "power" is a reserved, adapter-owned
    // domain (see `bread_shared::apps::RESERVED_DOMAINS`).
    let result = harness
        .send_request(
            "emit",
            json!({ "event": "bread.power.ac.connected", "data": { "ac_connected": true } }),
        )
        .await;
    assert!(
        result.is_err(),
        "manual emit must not be able to claim an adapter-owned event name"
    );
    let msg = result.err().unwrap().to_string();
    assert!(msg.contains("reserved"), "got: {msg}");

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_without_source_rejects_every_adapter_owned_domain() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    // Every namespace a real adapter (or the daemon itself) publishes
    // under must be closed to the unsourced emit path, not just "power".
    for event in [
        "bread.power.ac.connected",
        "bread.network.connected",
        "bread.device.connected",
        "bread.bluetooth.device.paired",
        "bread.hyprland.event",
        "bread.workspace.changed",
        "bread.monitor.connected",
        "bread.window.opened",
        "bread.system.startup",
    ] {
        let result = harness
            .send_request("emit", json!({ "event": event, "data": {} }))
            .await;
        assert!(
            result.is_err(),
            "expected '{event}' to be rejected on the unsourced emit path"
        );
    }

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_without_source_still_allows_custom_event_names() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    // The documented `bread emit <custom-name>` debug use case (testing Lua
    // handlers without unplugging cables) must keep working for any event
    // name that isn't a reserved, adapter-owned domain.
    let result = harness
        .send_request(
            "emit",
            json!({ "event": "bread.mymodule.custom_thing", "data": { "n": 1 } }),
        )
        .await;
    assert!(result.is_ok(), "custom event name must still be emittable");
    assert_eq!(
        result.unwrap().get("emitted").and_then(Value::as_bool),
        Some(true)
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_without_source_is_tagged_manual_not_system() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let stream = UnixStream::connect(harness.socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let subscribe = json!({
        "id": "sub-manual",
        "method": "events.subscribe",
        "params": { "filter": "bread.mymodule.*" }
    });
    write_half
        .write_all(format!("{}\n", serde_json::to_string(&subscribe)?).as_bytes())
        .await?;

    let mut reader = BufReader::new(read_half).lines();
    reader
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("missing subscribe ack"))?;

    // The subscribe ack is written to the client before the server task
    // actually registers on the broadcast channel (see `handle_connection`'s
    // "events.subscribe" arm — ack first, `stream_events`/`event_tx.subscribe()`
    // second). A brief settle delay avoids racing that registration under
    // full-suite parallel load, same as the settle delay already used in
    // `workflow_reaches_done_via_wait_any_happy_path` above for the same
    // class of subscribe-then-fire race.
    sleep(Duration::from_millis(100)).await;

    harness
        .send_request(
            "emit",
            json!({ "event": "bread.mymodule.poked", "data": {} }),
        )
        .await?;

    // Bounded by an explicit timeout (not just the `Instant`-deadline-checked
    // loop used elsewhere in this file) so a real regression here fails the
    // test in 5s instead of hanging this test binary — and therefore the
    // whole `cargo test --workspace` run — forever.
    let event = timeout(Duration::from_secs(5), async {
        loop {
            let line = reader
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("event stream closed before match"))?;
            let event: Value = serde_json::from_str(&line)?;
            if event.get("event").and_then(Value::as_str) == Some("bread.mymodule.poked") {
                return Ok::<Value, anyhow::Error>(event);
            }
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for bread.mymodule.poked on the stream"))??;
    // A wire-triggered no-source emit must never be indistinguishable from
    // a daemon-internal `System` event (e.g. `bread.system.startup`,
    // `bread.profile.activated`) — it must carry the distinct `Manual` tag.
    assert_eq!(
        event.get("source").and_then(Value::as_str),
        Some("manual"),
        "no-source emit must be tagged 'manual', not 'system': {event:?}"
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_with_internal_source_is_rejected() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    // "power" is a real internal AdapterSource — a socket client must not be
    // able to spoof it via sourced emit.
    let result = harness
        .send_request(
            "emit",
            json!({ "source": "power", "kind": "ac.connected", "data": {} }),
        )
        .await;
    assert!(
        result.is_err(),
        "spoofing an internal source must be rejected"
    );
    let msg = result.err().unwrap().to_string();
    assert!(msg.contains("not externally injectable"), "got: {msg}");

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_with_unregistered_app_source_is_rejected() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let result = harness
        .send_request(
            "emit",
            json!({ "source": "notanapp", "kind": "bread.notanapp.thing", "data": {} }),
        )
        .await;
    assert!(result.is_err(), "unregistered app id must be rejected");

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_with_known_app_source_routes_through_normalizer() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let stream = UnixStream::connect(harness.socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let subscribe = json!({
        "id": "sub-app",
        "method": "events.subscribe",
        "params": { "filter": "bread.clip.**" }
    });
    write_half
        .write_all(format!("{}\n", serde_json::to_string(&subscribe)?).as_bytes())
        .await?;

    let mut reader = BufReader::new(read_half).lines();
    reader
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("missing subscribe ack"))?;

    harness
        .send_request(
            "emit",
            json!({
                "source": "clip",
                "kind": "bread.clip.copied",
                "data": { "kind": "url", "len": 42 }
            }),
        )
        .await?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received: Option<Value> = None;
    while Instant::now() < deadline {
        let Some(line) = reader.next_line().await? else {
            break;
        };
        let event: Value = serde_json::from_str(&line)?;
        if event.get("event").and_then(Value::as_str) == Some("bread.clip.copied") {
            received = Some(event);
            break;
        }
    }

    let event = received.expect("did not receive bread.clip.copied on the stream");
    assert_eq!(
        event
            .get("source")
            .and_then(|s| s.get("app"))
            .and_then(Value::as_str),
        Some("clip")
    );
    assert_eq!(
        event.get("data").and_then(|d| d.get("len")),
        Some(&json!(42))
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn emit_with_app_source_rejects_wrong_namespace() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    // "clip" is a registered app id, but the event name belongs to "pad" —
    // an app may only publish within its own namespace segment.
    let result = harness
        .send_request(
            "emit",
            json!({
                "source": "clip",
                "kind": "bread.pad.reminder.due",
                "data": {}
            }),
        )
        .await;
    assert!(
        result.is_err(),
        "cross-app namespace claim must be rejected"
    );
    let msg = result.err().unwrap().to_string();
    assert!(msg.contains("namespace"), "got: {msg}");

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
async fn daemon_survives_repeated_reloads_and_pipeline_resumes() -> Result<()> {
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    // Event emitted before any reload.
    harness
        .send_request("emit", json!({"event": "bread.reload.before", "data": {}}))
        .await?;

    // Hammer reload: each cycle drops and rebuilds the Lua VM, cancels timers,
    // and re-registers subscriptions. A wedge here (lost Lua thread, deadlocked
    // dispatch, paused-and-never-resumed pipeline) is the regression this guards
    // — the previous suite only checked a single happy-path reload.
    for _ in 0..3 {
        let r = harness.send_request("modules.reload", json!({})).await?;
        assert_eq!(r.get("ok").and_then(Value::as_bool), Some(true));
    }

    // Daemon must still answer control requests after the reload storm.
    let ping = harness.send_request("ping", json!({})).await?;
    assert_eq!(ping.get("ok").and_then(Value::as_bool), Some(true));
    let health = harness.send_request("health", json!({})).await?;
    assert_eq!(health.get("ok").and_then(Value::as_bool), Some(true));

    // The pipeline must have resumed: an event emitted *after* the reloads
    // still flows through normalization into the replay buffer.
    harness
        .send_request("emit", json!({"event": "bread.reload.after", "data": {}}))
        .await?;
    sleep(Duration::from_millis(100)).await;

    let replay = harness
        .send_request("events.replay", json!({"since_ms": 30_000}))
        .await?;
    let names: Vec<&str> = replay
        .as_array()
        .expect("replay result should be array")
        .iter()
        .filter_map(|e| e.get("event").and_then(Value::as_str))
        .collect();
    assert!(
        names.contains(&"bread.reload.after"),
        "event pipeline did not resume after reload; got {names:?}"
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
            // "system" is a reserved, adapter/daemon-owned domain (see
            // `emit_without_source_rejects_reserved_domain` below) — a
            // manual emit can no longer use it, so this test's custom
            // event lives under "custom" instead, same as it always did
            // for "match"/"nomatch"/"replay"/etc. elsewhere in this file.
            "filter": "bread.custom.*"
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
                "event": "bread.custom.test",
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
        if event.get("event").and_then(Value::as_str) == Some("bread.custom.test") {
            got = true;
            break;
        }
    }

    assert!(got, "did not receive emitted event on stream");
    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn workflow_reaches_done_via_wait_any_happy_path() -> Result<()> {
    let harness = TestHarness::spawn_with_init(
        r#"
        bread.workflow.define("test-flow", function()
            bread.workflow.step("started")
            local event = bread.wait_any({"bread.test.a", "bread.test.b"}, { timeout = 5000 })
            bread.workflow.step("waited")
            if not event then
                error("did not receive expected event")
            end
        end)

        bread.on("bread.test.trigger", function()
            bread.workflow.start("test-flow")
        end)
        "#,
    )?;
    harness.wait_until_ready().await?;

    harness
        .send_request("emit", json!({ "event": "bread.test.trigger", "data": {} }))
        .await?;
    // Give the workflow a moment to register and reach the wait_any point
    // before firing the event it's blocked on.
    sleep(Duration::from_millis(200)).await;
    harness
        .send_request("emit", json!({ "event": "bread.test.a", "data": {} }))
        .await?;

    let status = poll_workflow_status(&harness, "test-flow", "done").await?;
    assert_eq!(status.get("step").and_then(Value::as_str), Some("waited"));

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn workflow_times_out_when_deadline_exceeded() -> Result<()> {
    let harness = TestHarness::spawn_with_init(
        r#"
        bread.workflow.define("timeout-flow", function()
            bread.workflow.step("waiting")
            bread.wait("bread.test.never")
            bread.workflow.step("unreachable")
        end)

        bread.on("bread.test.trigger", function()
            bread.workflow.start("timeout-flow", { deadline = 300 })
        end)
        "#,
    )?;
    harness.wait_until_ready().await?;

    harness
        .send_request("emit", json!({ "event": "bread.test.trigger", "data": {} }))
        .await?;

    let status = poll_workflow_status(&harness, "timeout-flow", "timed_out").await?;
    assert_eq!(status.get("step").and_then(Value::as_str), Some("waiting"));

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn workflow_captures_error_on_failure() -> Result<()> {
    let harness = TestHarness::spawn_with_init(
        r#"
        bread.workflow.define("fail-flow", function()
            bread.workflow.step("about-to-fail")
            error("boom")
        end)

        bread.on("bread.test.trigger", function()
            bread.workflow.start("fail-flow")
        end)
        "#,
    )?;
    harness.wait_until_ready().await?;

    harness
        .send_request("emit", json!({ "event": "bread.test.trigger", "data": {} }))
        .await?;

    let status = poll_workflow_status(&harness, "fail-flow", "failed").await?;
    let error = status
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(error.contains("boom"), "got error: {error}");

    harness.shutdown();
    Ok(())
}

/// Polls `workflows.list` until `name` is present with `expected_state`, or
/// times out after 5 seconds. Returns the matching entry.
async fn poll_workflow_status(
    harness: &TestHarness,
    name: &str,
    expected_state: &str,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let list = harness.send_request("workflows.list", json!({})).await?;
        if let Some(entries) = list.as_array() {
            if let Some(entry) = entries
                .iter()
                .find(|e| e.get("name").and_then(Value::as_str) == Some(name))
            {
                if entry.get("state").and_then(Value::as_str) == Some(expected_state) {
                    return Ok(entry.clone());
                }
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow!(
        "workflow '{name}' did not reach state '{expected_state}' in time"
    ))
}

struct TestHarness {
    _temp: TempDir,
    child: Child,
    socket_path: PathBuf,
}

impl TestHarness {
    fn spawn() -> Result<Self> {
        Self::spawn_with_init("bread.on('bread.system.startup', function() end)\n")
    }

    fn spawn_with_init(init_lua: &str) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let runtime_dir = temp.path().join("runtime");
        let config_home = temp.path().join("config");
        let home = temp.path().join("home");
        fs::create_dir_all(&runtime_dir)?;
        fs::create_dir_all(&config_home)?;
        fs::create_dir_all(&home)?;

        let bread_cfg = config_home.join("bread");
        fs::create_dir_all(bread_cfg.join("modules"))?;

        fs::write(bread_cfg.join("init.lua"), init_lua)?;

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
