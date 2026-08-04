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

// ---------------------------------------------------------------------------
// Capability-scoped module API (Workstream D)
// ---------------------------------------------------------------------------

/// Core regression test from the capability-manifest report: a third-party
/// module whose manifest grants only `state.read` can call
/// `bread.state.get(...)`, but `bread.fs` and `bread.exec` must be
/// genuinely *absent* from the `bread` table it sees — `nil`, not merely
/// permission-denied when called.
#[tokio::test]
async fn scoped_module_sees_only_granted_state_read_permission() -> Result<()> {
    let manifest = r#"
name = "scoped-test"
version = "1.0.0"
description = "test"
author = "test"
source = "test"
installed_at = ""

[[permissions]]
type = "state.read"
path = "monitors"
"#;
    let module_lua = r#"
local M = bread.module({ name = "scoped-test", version = "1.0.0" })

function M.on_load()
    local ok = pcall(bread.state.get, "monitors")
    M.store.set("state_get_ok", ok)
    M.store.set("fs_present", bread.fs ~= nil)
    M.store.set("exec_present", bread.exec ~= nil)
    M.store.set("exec_capture_present", bread.exec_capture ~= nil)
    M.store.set("bluetooth_present", bread.bluetooth ~= nil)
    -- Baseline must still work from inside a scoped module.
    M.store.set("json_present", bread.json ~= nil)
    M.store.set("log_present", bread.log ~= nil)
end

return M
"#;

    let harness = TestHarness::spawn_with_module("scoped-test", Some(manifest), module_lua)?;
    harness.wait_until_ready().await?;

    let modules = harness
        .send_request("state.get", json!({"key": "modules"}))
        .await?;
    let entry = modules
        .as_array()
        .and_then(|arr| arr.iter().find(|m| m.get("name").and_then(Value::as_str) == Some("scoped-test")))
        .cloned()
        .ok_or_else(|| anyhow!("scoped-test module not found in modules state; dump: {modules}"))?;

    assert_eq!(
        entry.get("status").and_then(Value::as_str),
        Some("loaded"),
        "module failed to load: {entry}"
    );

    let store = entry
        .get("store")
        .ok_or_else(|| anyhow!("no store on module status: {entry}"))?;
    assert_eq!(store.get("state_get_ok"), Some(&json!(true)));
    assert_eq!(
        store.get("fs_present"),
        Some(&json!(false)),
        "bread.fs must be absent (nil) without an fs.read/fs.write permission"
    );
    assert_eq!(
        store.get("exec_present"),
        Some(&json!(false)),
        "bread.exec must be absent (nil) without an exec permission"
    );
    assert_eq!(store.get("exec_capture_present"), Some(&json!(false)));
    assert_eq!(store.get("bluetooth_present"), Some(&json!(false)));
    assert_eq!(store.get("json_present"), Some(&json!(true)), "baseline bread.json must still be present");
    assert_eq!(store.get("log_present"), Some(&json!(true)), "baseline bread.log must still be present");

    assert_eq!(
        entry.get("ungated"),
        Some(&json!(false)),
        "a module with a manifest that declares permissions must not be flagged ungated"
    );

    harness.shutdown();
    Ok(())
}

/// A third-party module installed with no `bread.module.toml` manifest at
/// all (the pre-existing/legacy case) keeps full, unscoped `bread.*`
/// access — but is surfaced as `ungated` in module status, which is exactly
/// what `bread doctor` reads to print its "no permissions declared"
/// warning.
#[tokio::test]
async fn module_with_no_manifest_keeps_full_access_but_is_flagged_ungated() -> Result<()> {
    let module_lua = r#"
local M = bread.module({ name = "legacy-test", version = "1.0.0" })

function M.on_load()
    M.store.set("fs_present", bread.fs ~= nil)
    M.store.set("exec_present", bread.exec ~= nil)
    M.store.set("bluetooth_present", bread.bluetooth ~= nil)
end

return M
"#;

    let harness = TestHarness::spawn_with_module("legacy-test", None, module_lua)?;
    harness.wait_until_ready().await?;

    let modules = harness
        .send_request("state.get", json!({"key": "modules"}))
        .await?;
    let entry = modules
        .as_array()
        .and_then(|arr| arr.iter().find(|m| m.get("name").and_then(Value::as_str) == Some("legacy-test")))
        .cloned()
        .ok_or_else(|| anyhow!("legacy-test module not found in modules state; dump: {modules}"))?;

    assert_eq!(
        entry.get("status").and_then(Value::as_str),
        Some("loaded"),
        "module failed to load: {entry}"
    );

    let store = entry
        .get("store")
        .ok_or_else(|| anyhow!("no store on module status: {entry}"))?;
    assert_eq!(
        store.get("fs_present"),
        Some(&json!(true)),
        "no manifest declared -> backward compat full access, bread.fs must be present"
    );
    assert_eq!(store.get("exec_present"), Some(&json!(true)));
    assert_eq!(store.get("bluetooth_present"), Some(&json!(true)));

    assert_eq!(
        entry.get("ungated"),
        Some(&json!(true)),
        "a module with no permissions manifest must be flagged ungated for `bread doctor`"
    );

    harness.shutdown();
    Ok(())
}

/// An explicit `permissions = []` (present but empty) is a deliberate
/// "baseline only" declaration, distinct from no manifest at all: it must
/// scope the module down for real (no fs/exec/etc.) but must *not* trip the
/// `ungated` doctor warning, since the author made a conscious choice.
#[tokio::test]
async fn explicit_empty_permissions_is_scoped_but_not_flagged_ungated() -> Result<()> {
    let manifest = r#"
name = "empty-perms-test"
version = "1.0.0"
description = "test"
author = "test"
source = "test"
installed_at = ""
permissions = []
"#;
    let module_lua = r#"
local M = bread.module({ name = "empty-perms-test", version = "1.0.0" })

function M.on_load()
    M.store.set("fs_present", bread.fs ~= nil)
    M.store.set("state_present", bread.state ~= nil)
end

return M
"#;

    let harness = TestHarness::spawn_with_module("empty-perms-test", Some(manifest), module_lua)?;
    harness.wait_until_ready().await?;

    let modules = harness
        .send_request("state.get", json!({"key": "modules"}))
        .await?;
    let entry = modules
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|m| m.get("name").and_then(Value::as_str) == Some("empty-perms-test"))
        })
        .cloned()
        .ok_or_else(|| anyhow!("empty-perms-test module not found in modules state; dump: {modules}"))?;

    assert_eq!(entry.get("status").and_then(Value::as_str), Some("loaded"));
    let store = entry.get("store").unwrap();
    assert_eq!(store.get("fs_present"), Some(&json!(false)));
    assert_eq!(store.get("state_present"), Some(&json!(false)));
    assert_eq!(
        entry.get("ungated"),
        Some(&json!(false)),
        "an explicit empty permissions list is a deliberate declaration, not 'undeclared'"
    );

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

    /// Like `spawn_with_init`, but also installs one third-party,
    /// directory-based module (`<modules_dir>/<name>/{bread.module.toml,
    /// init.lua}`) — the same on-disk shape `bread modules install`
    /// produces — before starting the daemon. `manifest_toml` is written
    /// verbatim as `bread.module.toml`; pass `None` to install the module
    /// with no manifest file at all (the legacy/backward-compat case).
    fn spawn_with_module(
        module_name: &str,
        manifest_toml: Option<&str>,
        module_init_lua: &str,
    ) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let runtime_dir = temp.path().join("runtime");
        let config_home = temp.path().join("config");
        let home = temp.path().join("home");
        fs::create_dir_all(&runtime_dir)?;
        fs::create_dir_all(&config_home)?;
        fs::create_dir_all(&home)?;

        let bread_cfg = config_home.join("bread");
        let module_dir = bread_cfg.join("modules").join(module_name);
        fs::create_dir_all(&module_dir)?;

        fs::write(
            bread_cfg.join("init.lua"),
            "bread.on('bread.system.startup', function() end)\n",
        )?;
        if let Some(manifest) = manifest_toml {
            fs::write(module_dir.join("bread.module.toml"), manifest)?;
        }
        fs::write(module_dir.join("init.lua"), module_init_lua)?;

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
