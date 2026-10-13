//! breadbug integration: the on-disk log/event store and its feeders.
//!
//! The store itself lives in the `bread-bug` crate so the `breadbug` binary
//! can read it with breadd down. This module is the daemon side: opening it
//! at startup, recording the event bus into it, mirroring breadd's own
//! `tracing` output, and the `bug.*` IPC handlers.

use std::sync::Arc;

use bread_bug::config::{store_root, BugConfig};
use bread_bug::store::{app_for_event, Marker, Store, DAEMON_APP};
use bread_scrub::ScrubContext;
use bread_shared::apps::{is_known_app, KNOWN_APPS};
use bread_shared::{now_unix_ms, BreadEvent};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, watch};

// Explicit unit names, rather than trusting a journal-supplied app id.
const APP_UNITS: &[(&str, &str)] = &[
    ("breadclipd.service", "clip"),
    ("breadclip.service", "clip"),
    ("breadpad.service", "pad"),
    ("breadbar.service", "bar"),
    ("breadbox.service", "box"),
    ("breadlock.service", "lock"),
    ("breadmon.service", "mon"),
    ("breadpaper.service", "paper"),
    ("breadsearch.service", "search"),
    ("breadshot.service", "shot"),
    ("breadarr.service", "arr"),
    ("breadcrumbs.service", "crumbs"),
    ("breadhelp.service", "help"),
    ("bakery.service", "bakery"),
    ("breadcastd.service", "cast"),
    ("breadcloud.service", "cloud"),
];

fn ingest_journal_line(store: &Store, line: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let Some(unit) = value.get("_SYSTEMD_USER_UNIT").and_then(Value::as_str) else {
        return;
    };
    let Some((_, app)) = APP_UNITS.iter().find(|(name, _)| *name == unit) else {
        return;
    };
    let Some(message) = value.get("MESSAGE").and_then(Value::as_str) else {
        return;
    };
    let level = match value.get("PRIORITY").and_then(Value::as_str) {
        Some("0" | "1" | "2" | "3") => "error",
        Some("4") => "warn",
        Some("7") => "debug",
        _ => "info",
    };
    let t = value
        .get("__REALTIME_TIMESTAMP")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<u64>().ok())
        .map(|us| us / 1000)
        .unwrap_or_else(now_unix_ms);
    let _ = store.append_log(app, level, message, t);
}

#[cfg(test)]
mod journal_tests {
    use super::*;

    #[tokio::test]
    async fn journal_child_streams_records_and_stops_on_shutdown() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fixture-journalctl");
        std::fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' '{\"_SYSTEMD_USER_UNIT\":\"breadclipd.service\",\"MESSAGE\":\"remote 203.0.113.42\"}'\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Arc::new(
            Store::open(
                dir.path().join("store"),
                &BugConfig::default(),
                ScrubContext::default(),
            )
            .unwrap(),
        );
        let (tx, rx) = watch::channel(false);
        let task = journal_recorder(store.clone(), rx, executable);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let snapshot = bread_bug::store::Reader::open(store.root())
                    .snapshot("clip", 10, 0)
                    .unwrap();
                if !snapshot.logs.is_empty() {
                    assert_eq!(snapshot.logs[0].msg, "remote <IP:1>");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn journal_records_are_routed_scrubbed_and_exclusions_honored() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BugConfig {
            excluded_apps: vec!["cast".into()],
            ..BugConfig::default()
        };
        let store = Store::open(dir.path(), &cfg, ScrubContext::default()).unwrap();
        ingest_journal_line(&store, br#"{"_SYSTEMD_USER_UNIT":"breadclipd.service","MESSAGE":"remote 203.0.113.42","PRIORITY":"3","__REALTIME_TIMESTAMP":"123000"}"#);
        ingest_journal_line(
            &store,
            br#"{"_SYSTEMD_USER_UNIT":"breadcastd.service","MESSAGE":"excluded"}"#,
        );
        ingest_journal_line(
            &store,
            br#"{"_SYSTEMD_USER_UNIT":"unrelated.service","MESSAGE":"not ours"}"#,
        );
        ingest_journal_line(&store, b"invalid json");
        let reader = bread_bug::store::Reader::open(dir.path());
        let snapshot = reader.snapshot("clip", 10, 0).unwrap();
        assert_eq!(snapshot.logs.len(), 1);
        assert_eq!(snapshot.logs[0].msg, "remote <IP:1>");
        assert_eq!(snapshot.logs[0].t, 123);
        assert_eq!(snapshot.logs[0].level, "ERROR");
        assert_eq!(reader.apps(), vec!["clip"]);
        let disk = std::fs::read_to_string(dir.path().join("apps/clip/log.jsonl")).unwrap();
        assert!(!disk.contains("203.0.113.42"));
    }
}

/// Follow relevant user units. Only scrubbed records reach the disk store;
/// journal input and oversized lines are bounded in memory, never spooled.
pub fn spawn_journal_recorder(store: Arc<Store>, shutdown: watch::Receiver<bool>) {
    journal_recorder(store, shutdown, "journalctl".into());
}

fn journal_recorder(
    store: Arc<Store>,
    mut shutdown: watch::Receiver<bool>,
    executable: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                break;
            }
            let mut command = tokio::process::Command::new(&executable);
            command
                .args([
                    "--user",
                    "--follow",
                    "--lines=0",
                    "--output=json",
                    "--no-pager",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            for (unit, _) in APP_UNITS {
                command.arg(format!("--unit={unit}"));
            }
            if let Ok(mut child) = command.spawn() {
                if let Some(mut stdout) = child.stdout.take() {
                    let mut chunk = [0; 4096];
                    let mut line = Vec::new();
                    let mut oversized = false;
                    loop {
                        let read = tokio::select! {
                            _ = shutdown.changed() => { break; }
                            read = stdout.read(&mut chunk) => read,
                        };
                        let Ok(n) = read else { break };
                        if n == 0 {
                            break;
                        }
                        for byte in &chunk[..n] {
                            if *byte == b'\n' {
                                if !oversized {
                                    ingest_journal_line(&store, &line);
                                }
                                line.clear();
                                oversized = false;
                            } else if !oversized {
                                if line.len() == 65536 {
                                    line.clear();
                                    oversized = true;
                                } else {
                                    line.push(*byte);
                                }
                            }
                        }
                    }
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
        }
    })
}
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Open the store, or `None` (with a note on stderr) if it cannot be opened.
/// A broken store must never stop the daemon from starting.
pub fn open() -> Option<Arc<Store>> {
    let cfg = match BugConfig::load() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("breadbug: ignoring unreadable breadbug.toml: {err:#}");
            BugConfig::default()
        }
    };
    match Store::open(store_root(), &cfg, ScrubContext::from_environment()) {
        Ok(store) => Some(Arc::new(store)),
        Err(err) => {
            eprintln!("breadbug: store disabled: {err:#}");
            None
        }
    }
}

/// Record every bus event (name and allowlisted fields only) into the store,
/// filed under the sibling app that owns the namespace or under `breadd`.
pub fn spawn_event_recorder(store: Arc<Store>, mut rx: broadcast::Receiver<BreadEvent>) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(evt) => {
                    let app = app_for_event(&evt.event, KNOWN_APPS);
                    let _ = store.append_event(&app, &evt.event, &evt.data, evt.timestamp);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// A `tracing` layer that mirrors breadd's own log events into the store.
pub struct BugLayer {
    store: Arc<Store>,
}

impl BugLayer {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

impl<S: Subscriber> Layer<S> for BugLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut v = MessageVisitor::default();
        event.record(&mut v);
        let mut line = v.message;
        for f in v.fields {
            line.push(' ');
            line.push_str(&f);
        }
        let meta = event.metadata();
        // Errors are dropped on purpose: logging a logging failure would
        // recurse straight back into this layer.
        let _ = self.store.append_log(
            DAEMON_APP,
            &meta.level().to_string(),
            &format!("{}: {line}", meta.target()),
            now_unix_ms(),
        );
    }
}

fn app_param(params: &Value) -> Result<&str, String> {
    let app = params
        .get("app")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing app".to_string())?;
    check_app_name(app)
}

fn check_app_name(app: &str) -> Result<&str, String> {
    if app == DAEMON_APP || is_known_app(app) {
        Ok(app)
    } else {
        Err(format!("'{app}' is not a registered bread app"))
    }
}

fn unavailable() -> String {
    "breadbug store is unavailable".to_string()
}

/// `bug.log`: `{app, level?, msg, t?}`.
pub fn ipc_log(store: Option<&Store>, params: &Value) -> Result<Value, String> {
    let store = store.ok_or_else(unavailable)?;
    let app = app_param(params)?;
    let msg = params
        .get("msg")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing msg".to_string())?;
    let level = params
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or("info");
    let t = params
        .get("t")
        .and_then(Value::as_u64)
        .unwrap_or_else(now_unix_ms);
    let stored = store
        .append_log(app, level, msg, t)
        .map_err(|e| e.to_string())?;
    Ok(json!({ "stored": stored }))
}

/// `bug.logs`: `{app, lines: [{level?, msg, t?}, ...]}`.
pub fn ipc_logs(store: Option<&Store>, params: &Value) -> Result<Value, String> {
    let store = store.ok_or_else(unavailable)?;
    let app = app_param(params)?;
    let lines = params
        .get("lines")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing lines".to_string())?;
    let mut stored = 0u64;
    for line in lines.iter().take(1000) {
        let Some(msg) = line.get("msg").and_then(Value::as_str) else {
            continue;
        };
        let level = line.get("level").and_then(Value::as_str).unwrap_or("info");
        let t = line
            .get("t")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_unix_ms);
        if store
            .append_log(app, level, msg, t)
            .map_err(|e| e.to_string())?
        {
            stored += 1;
        }
    }
    Ok(json!({ "stored": stored }))
}

/// `bug.marker`: `{app, marker: {kind, t?, app_version?, build_id?, message?,
/// location?, signal?, frames?}}`. Returns the marker kind so the caller can
/// emit the matching bus event.
pub fn ipc_marker(store: Option<&Store>, params: &Value) -> Result<(String, Marker, bool), String> {
    let store = store.ok_or_else(unavailable)?;
    let app = app_param(params)?;
    let mut raw = params
        .get("marker")
        .cloned()
        .ok_or_else(|| "missing marker".to_string())?;
    if let Some(obj) = raw.as_object_mut() {
        obj.entry("t").or_insert_with(|| json!(now_unix_ms()));
    }
    let marker: Marker = serde_json::from_value(raw).map_err(|e| format!("bad marker: {e}"))?;
    let stored = store
        .record_marker(app, marker.clone())
        .map_err(|e| e.to_string())?;
    Ok((app.to_string(), marker, stored))
}

/// `bug.status`: store usage plus the apps that have data.
pub fn ipc_status(store: Option<&Store>) -> Value {
    let Some(store) = store else {
        return json!({ "available": false });
    };
    let usage = store.usage().ok().unwrap_or_default();
    let reader = bread_bug::store::Reader::open(store.root());
    let apps: Vec<Value> = reader
        .apps()
        .into_iter()
        .map(|app| {
            let last = reader.last_marker(&app);
            json!({
                "app": app,
                "last_marker": last.as_ref().map(|m| json!({ "kind": m.kind, "t": m.t })),
            })
        })
        .collect();
    json!({
        "available": true,
        "total_bytes": usage.total_bytes,
        "apps": apps,
    })
}

/// `bug.wipe`: `{app?}`; no app wipes the whole store.
pub fn ipc_wipe(store: Option<&Store>, params: &Value) -> Result<Value, String> {
    let store = store.ok_or_else(unavailable)?;
    match params.get("app").and_then(Value::as_str) {
        Some(app) => {
            let app = check_app_name(app)?;
            store.wipe_app(app).map_err(|e| e.to_string())?;
        }
        None => store.wipe_all().map_err(|e| e.to_string())?,
    }
    Ok(json!({ "wiped": true }))
}
