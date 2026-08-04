use std::collections::HashMap;
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

// ---------------------------------------------------------------------------
// Capability-scoped module API (Workstream D)
// ---------------------------------------------------------------------------

/// Core regression test from the capability-manifest report: a third-party
/// module whose manifest grants only `state.read` can call
/// `bread.state.get(...)`, but `bread.fs` and `bread.exec` must be
/// genuinely *absent* from the `bread` table it sees — `nil`, not merely
/// permission-denied when called.
///
/// *Since Workstream G*: a module that declares `[[permissions]]` (any,
/// including an explicit empty list — see
/// `explicit_empty_permissions_is_scoped_but_not_flagged_ungated` below)
/// now runs out-of-process in a real `bread-module-host` child instead of
/// in-process with a scoped Lua `_ENV` (see `breadd/src/lua/mod.rs`'s
/// `load_module`) — the presence/absence check this test exists for still
/// holds, just enforced by what `bread-module-host`'s own `ModuleHostLua`
/// constructs the `bread` table from (see `bread-module-host/src/
/// lua_env.rs`) instead of `build_scoped_env`. `M.store.set(...)` no
/// longer works as the result-reporting channel here, since an
/// out-of-process module's `bread.module().store` is process-local (not
/// synced back to `breadd`'s `RuntimeState` — a documented gap, see
/// `Documentation.md`'s Workstream G section) — `bread.emit(...)` is used
/// instead, which *does* cross the process boundary via the RPC bridge.
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

bread.on("test.trigger", function()
    local ok = pcall(bread.state.get, "monitors")
    bread.emit("test.scoped_result", {
        state_get_ok = ok,
        fs_present = bread.fs ~= nil,
        exec_present = bread.exec ~= nil,
        exec_capture_present = bread.exec_capture ~= nil,
        bluetooth_present = bread.bluetooth ~= nil,
        -- Baseline must still work from inside a scoped module.
        json_present = bread.json ~= nil,
        log_present = bread.log ~= nil,
    })
end)

return M
"#;

    let harness = TestHarness::spawn_with_module("scoped-test", Some(manifest), module_lua)?;
    harness.wait_until_ready().await?;
    harness.wait_for_module_loaded("scoped-test").await?;

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
    assert_eq!(
        entry.get("ungated"),
        Some(&json!(false)),
        "a module with a manifest that declares permissions must not be flagged ungated"
    );

    let store = harness.trigger_and_await_result("test.scoped_result").await?;
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
    // `wait_until_ready` only proves the IPC socket is accepting
    // connections — module loading runs concurrently on the Lua engine's
    // own thread (see `lua::spawn_runtime`), so without this the check
    // below races module load completion even for an in-process module.
    // Usually fast enough not to matter, but flaky under this suite's
    // heavier concurrent process load (see Workstream G's
    // `module_host_sandbox.rs` tests, which run real sandboxed child
    // processes alongside this one).
    harness.wait_for_module_loaded("legacy-test").await?;

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
///
/// *Since Workstream G*: `Some(vec![])` also opts this module into the
/// out-of-process sandboxed path (same as any other declared
/// `[[permissions]]`), and — as in the test above — results come back via
/// `bread.emit` on a `test.trigger` handler rather than `M.store`. See that
/// test's doc comment for the full explanation.
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

bread.on("test.trigger", function()
    bread.emit("test.empty_perms_result", {
        fs_present = bread.fs ~= nil,
        state_present = bread.state ~= nil,
    })
end)

return M
"#;

    let harness = TestHarness::spawn_with_module("empty-perms-test", Some(manifest), module_lua)?;
    harness.wait_until_ready().await?;
    harness.wait_for_module_loaded("empty-perms-test").await?;

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
    assert_eq!(
        entry.get("ungated"),
        Some(&json!(false)),
        "an explicit empty permissions list is a deliberate declaration, not 'undeclared'"
    );

    let store = harness.trigger_and_await_result("test.empty_perms_result").await?;
    assert_eq!(store.get("fs_present"), Some(&json!(false)));
    assert_eq!(store.get("state_present"), Some(&json!(false)));

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

/// A chain of 3 handlers, each reacting to the previous one's emitted event
/// entirely inside the Lua runtime (no test-side pumping between links):
/// module A handles the external trigger and emits X, module B subscribes
/// to X and emits Y, module C subscribes to Y and emits Z. This exercises
/// the "current dispatch id" mechanism in `breadd::lua::LuaEngine` end to
/// end: `caused_by` must thread through every hop of the chain, not just a
/// single emit.
#[tokio::test]
async fn event_causality_chain_threads_caused_by_across_handlers() -> Result<()> {
    let harness = TestHarness::spawn_with_init(
        r#"
        -- module A: reacts to the external trigger, emits X
        bread.on("bread.chain.trigger", function(event)
            bread.emit("bread.chain.x", {})
        end)

        -- module B: reacts to X, emits Y
        bread.on("bread.chain.x", function(event)
            bread.emit("bread.chain.y", {})
        end)

        -- module C: reacts to Y, emits Z
        bread.on("bread.chain.y", function(event)
            bread.emit("bread.chain.z", {})
        end)
        "#,
    )?;
    harness.wait_until_ready().await?;

    let stream = UnixStream::connect(harness.socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let subscribe = json!({
        "id": "sub-chain",
        "method": "events.subscribe",
        "params": { "filter": "bread.chain.*" }
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

    // The trigger itself comes in over IPC's unsourced `emit`, outside any
    // Lua handler — its `caused_by` must be None. Everything downstream
    // (X, Y, Z) is emitted by `bread.emit()` from inside a running handler.
    harness
        .send_request("emit", json!({ "event": "bread.chain.trigger", "data": {} }))
        .await?;

    let mut events: HashMap<String, Value> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while events.len() < 4 && Instant::now() < deadline {
        let Some(line) = reader.next_line().await? else {
            break;
        };
        let event: Value = serde_json::from_str(&line)?;
        let name = event
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if name.starts_with("bread.chain.") {
            events.insert(name, event);
        }
    }

    let trigger = events
        .get("bread.chain.trigger")
        .expect("missing trigger event");
    let x = events.get("bread.chain.x").expect("missing X event");
    let y = events.get("bread.chain.y").expect("missing Y event");
    let z = events.get("bread.chain.z").expect("missing Z event");

    let trigger_id = trigger
        .get("id")
        .and_then(Value::as_str)
        .expect("trigger event missing id")
        .to_string();
    let x_id = x
        .get("id")
        .and_then(Value::as_str)
        .expect("X event missing id")
        .to_string();
    let y_id = y
        .get("id")
        .and_then(Value::as_str)
        .expect("Y event missing id")
        .to_string();
    let z_id = z
        .get("id")
        .and_then(Value::as_str)
        .expect("Z event missing id")
        .to_string();

    assert_eq!(
        trigger.get("caused_by").and_then(Value::as_str),
        None,
        "IPC-originated trigger event should have no caused_by"
    );
    assert_eq!(
        x.get("caused_by").and_then(Value::as_str),
        Some(trigger_id.as_str()),
        "X should be caused_by the trigger event's id"
    );
    assert_eq!(
        y.get("caused_by").and_then(Value::as_str),
        Some(x_id.as_str()),
        "Y should be caused_by X's id, not the original trigger"
    );
    assert_eq!(
        z.get("caused_by").and_then(Value::as_str),
        Some(y_id.as_str()),
        "Z should be caused_by Y's id"
    );

    let ids: std::collections::HashSet<&str> =
        [trigger_id.as_str(), x_id.as_str(), y_id.as_str(), z_id.as_str()]
            .into_iter()
            .collect();
    assert_eq!(
        ids.len(),
        4,
        "every event in the chain should have a distinct id"
    );

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

#[tokio::test]
async fn rules_toml_absent_is_a_no_op() -> Result<()> {
    // No rules.toml written at all — the daemon must still start cleanly
    // and `bread.rules` (always present as a built-in) must come up
    // `loaded` with nothing registered, not `load_error` or `not_found`.
    let harness = TestHarness::spawn()?;
    harness.wait_until_ready().await?;

    let health = harness.send_request("health", json!({})).await?;
    let modules = health
        .get("modules")
        .and_then(Value::as_array)
        .expect("modules array");
    let rules_mod = modules
        .iter()
        .find(|m| m.get("name").and_then(Value::as_str) == Some("bread.rules"))
        .expect("bread.rules should always be registered as a built-in");
    assert_eq!(
        rules_mod.get("status").and_then(Value::as_str),
        Some("loaded")
    );
    assert!(rules_mod.get("last_error").and_then(Value::as_str).is_none());

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn rules_toml_well_formed_all_action_kinds_work_end_to_end() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let assets = tempfile::tempdir()?;
    // Deliberately includes a space to prove `run` shell-quotes the whole
    // path rather than letting `sh -c` word-split it into "dock" + "sh".
    let script_path = assets.path().join("dock connected.sh");
    let run_marker = assets.path().join("run-marker");
    let exec_marker = assets.path().join("exec-marker");

    fs::write(
        &script_path,
        format!("#!/bin/sh\ntouch '{}'\n", run_marker.display()),
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    // Non-reserved-domain event names ("testrule.*" rather than the
    // realistic "device.*"/"power.*" shown in rules.toml's own docs):
    // Workstream A closed the IPC `emit` method's no-source path to reject
    // any event name claiming a reserved, adapter-owned domain (see
    // `bread_shared::apps::RESERVED_DOMAINS` — "device" and "power" are both
    // in it), so a real `bread.device.dock.connected` can no longer be
    // manually injected over the socket the way this test needs to. What's
    // under test here is rules.toml -> bread.on() -> action wiring, which is
    // exercised identically regardless of which domain the event name lives
    // in, so a custom domain sidesteps that (correct, unrelated) restriction
    // without weakening it.
    let rules_toml = format!(
        r#"
[[rule]]
on = "testrule.dock.connected"
run = "{run_path}"

[[rule]]
on = "testrule.ac.disconnected"
notify = "Unplugged"

[[rule]]
on = "testrule.keyboard.connected"
exec = "touch '{exec_path}'"
"#,
        run_path = script_path.display(),
        exec_path = exec_marker.display(),
    );

    let harness = TestHarness::spawn_with_init_and_rules(
        "bread.on('bread.system.startup', function() end)\n",
        Some(&rules_toml),
    )?;
    harness.wait_until_ready().await?;

    // rules.toml parsed and registered cleanly — no load_error.
    let health = harness.send_request("health", json!({})).await?;
    let modules = health
        .get("modules")
        .and_then(Value::as_array)
        .expect("modules array");
    let rules_mod = modules
        .iter()
        .find(|m| m.get("name").and_then(Value::as_str) == Some("bread.rules"))
        .expect("bread.rules present");
    assert_eq!(
        rules_mod.get("status").and_then(Value::as_str),
        Some("loaded"),
        "unexpected bread.rules status: {rules_mod:?}"
    );

    // `run`: fire the matching event and expect the script to have run.
    harness
        .send_request(
            "emit",
            json!({"event": "bread.testrule.dock.connected", "data": {}}),
        )
        .await?;
    wait_for_file(&run_marker).await?;

    // `exec`: same, via a raw shell command instead of a script file.
    harness
        .send_request(
            "emit",
            json!({"event": "bread.testrule.keyboard.connected", "data": {}}),
        )
        .await?;
    wait_for_file(&exec_marker).await?;

    // `notify`: bread.notify() always emits bread.notify.sent regardless of
    // whether a real notify-send binary is available, so that's the
    // deterministic way to observe it fired with the right message.
    harness
        .send_request(
            "emit",
            json!({"event": "bread.testrule.ac.disconnected", "data": {}}),
        )
        .await?;
    sleep(Duration::from_millis(150)).await;
    let replay = harness
        .send_request("events.replay", json!({"since_ms": 10_000}))
        .await?;
    let sent = replay
        .as_array()
        .expect("replay array")
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("bread.notify.sent"))
        .expect("bread.notify.sent should have been emitted");
    assert_eq!(
        sent.get("data")
            .and_then(|d| d.get("message"))
            .and_then(Value::as_str),
        Some("Unplugged")
    );

    harness.shutdown();
    Ok(())
}

#[tokio::test]
async fn rules_toml_malformed_rule_surfaces_doctor_visible_error() -> Result<()> {
    // Rule #0 is fine; rule #1 has no action key at all.
    let rules_toml = r#"
[[rule]]
on = "device.dock.connected"
exec = "true"

[[rule]]
on = "power.ac.disconnected"
"#;

    let harness = TestHarness::spawn_with_init_and_rules(
        "bread.on('bread.system.startup', function() end)\n",
        Some(rules_toml),
    )?;
    harness.wait_until_ready().await?;

    let health = harness.send_request("health", json!({})).await?;
    let modules = health
        .get("modules")
        .and_then(Value::as_array)
        .expect("modules array");
    let rules_mod = modules
        .iter()
        .find(|m| m.get("name").and_then(Value::as_str) == Some("bread.rules"))
        .expect("bread.rules present");
    assert_eq!(
        rules_mod.get("status").and_then(Value::as_str),
        Some("load_error"),
        "malformed rule should surface as load_error: {rules_mod:?}"
    );
    let last_error = rules_mod
        .get("last_error")
        .and_then(Value::as_str)
        .expect("last_error should be set");
    assert!(
        last_error.contains("rule #1"),
        "expected the bad rule's index in the error, got: {last_error}"
    );
    assert!(
        last_error.contains("must set exactly one"),
        "expected the validation message, got: {last_error}"
    );

    // The daemon itself must not have crashed — still reachable.
    let ping = harness.send_request("ping", json!({})).await?;
    assert_eq!(ping.get("ok").and_then(Value::as_bool), Some(true));

    harness.shutdown();
    Ok(())
}

/// Polls for `path` to exist, or times out after 5 seconds — `bread.exec`
/// is fire-and-forget (spawn_blocking + `sh -c`), so its side effects land
/// asynchronously relative to the IPC call that triggered them.
async fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow!(
        "expected file was not created in time: {}",
        path.display()
    ))
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
        Self::spawn_with_init_and_rules(init_lua, None)
    }

    /// Like `spawn_with_init`, but also writes `rules.toml` (when `Some`)
    /// into the same synthetic `~/.config/bread` before starting the
    /// daemon, and exposes the synthetic `$HOME` so tests can point a
    /// `run = "..."` rule at a script file they've written under it.
    fn spawn_with_init_and_rules(init_lua: &str, rules_toml: Option<&str>) -> Result<Self> {
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

        if let Some(rules_toml) = rules_toml {
            fs::write(bread_cfg.join("rules.toml"), rules_toml)?;
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

    /// Poll `modules.list`/`state.get "modules"`-equivalent status until
    /// `name` reaches `Loaded` (or `LoadError`, which is treated as a test
    /// failure). Out-of-process modules (Workstream G:
    /// `decl.permissions.is_some()`, see `breadd/src/lua/mod.rs`) report
    /// their load outcome asynchronously — a passing `wait_until_ready`
    /// only proves the daemon's IPC socket itself is up, not that any
    /// particular module has finished spawning/connecting/authenticating/
    /// running its `init.lua` yet.
    async fn wait_for_module_loaded(&self, name: &str) -> Result<()> {
        // Comfortably exceeds module_host::READY_TIMEOUT (breadd's own
        // spawn-side wait) so this test-side poll doesn't give up before
        // breadd itself would.
        let deadline = Instant::now() + Duration::from_secs(55);
        while Instant::now() < deadline {
            let modules = self.send_request("state.get", json!({"key": "modules"})).await?;
            if let Some(arr) = modules.as_array() {
                for m in arr {
                    if m.get("name").and_then(Value::as_str) == Some(name) {
                        match m.get("status").and_then(Value::as_str) {
                            Some("loaded") => return Ok(()),
                            Some("load_error") => {
                                return Err(anyhow!("module '{name}' failed to load: {m}"))
                            }
                            _ => {}
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!("module '{name}' did not reach Loaded within timeout"))
    }

    /// Subscribe to `result_event`, send a `test.trigger` manual emit to
    /// kick off whatever Lua handler is waiting on it, and return the
    /// triggered event's `data`. See `breadd/tests/module_host_sandbox.rs`'s
    /// module doc comment for why this trigger-based pattern exists at all:
    /// a module reporting its result from `on_load` directly would race the
    /// daemon's own startup sequence, since `tokio::sync::broadcast` (what
    /// `events.subscribe` reads from) never replays history to a subscriber
    /// that joins after a send already happened.
    async fn trigger_and_await_result(&self, result_event: &str) -> Result<Value> {
        let stream = UnixStream::connect(self.socket_path()).await?;
        let (read_half, mut write_half) = stream.into_split();
        let subscribe = json!({
            "id": "sub-1",
            "method": "events.subscribe",
            "params": { "filter": result_event },
        });
        write_half
            .write_all(format!("{}\n", serde_json::to_string(&subscribe)?).as_bytes())
            .await?;
        let mut reader = BufReader::new(read_half).lines();
        let _ack = reader.next_line().await?;

        self.send_request("emit", json!({ "event": "test.trigger", "data": {} }))
            .await?;

        let line = timeout(Duration::from_secs(10), reader.next_line())
            .await
            .map_err(|_| anyhow!("timed out waiting for {result_event}"))??
            .ok_or_else(|| anyhow!("connection closed before {result_event} arrived"))?;
        let event: Value = serde_json::from_str(&line)?;
        event
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow!("{result_event} missing data"))
    }

    fn shutdown(self) {
        // Drop (below) does the actual killing.
        drop(self);
    }
}

impl Drop for TestHarness {
    /// A test that fails partway through (an `?`-propagated error, a
    /// failed `assert!` unwinding) must not leak a live `breadd` process —
    /// worse, since Workstream G, a leaked `breadd` can itself have spawned
    /// `bread-module-host` children under a real Landlock sandbox, which
    /// don't exit on their own once the parent socket's other end goes
    /// away instantly (they notice on their next read and exit, but that's
    /// not instant). Without this, a single failing test in this file
    /// leaves orphaned processes for every *other* concurrently-running
    /// test to contend with for CPU/scheduler time — turning one flaky
    /// failure into cascading slowdowns/timeouts across the whole suite
    /// (observed directly while developing Workstream G's tests).
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
