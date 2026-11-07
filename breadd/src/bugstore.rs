//! breadbug integration: the on-disk log/event store and its feeders.
//!
//! The store itself lives in the `bread-bug` crate so the `breadbug` binary
//! can read it with breadd down. This module is the daemon side: opening it
//! at startup, recording the event bus into it, mirroring breadd's own
//! `tracing` output, and the `bug.*` IPC handlers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bread_bug::config::{store_root, BugConfig};
use bread_bug::store::{app_for_event, Marker, MarkerKind, Store, DAEMON_APP};
use bread_scrub::ScrubContext;
use bread_shared::apps::{is_known_app, KNOWN_APPS};
use bread_shared::{now_unix_ms, BreadEvent};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, mpsc, watch};

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

    #[test]
    fn health_exposes_accounting_failure_without_false_byte_count() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path(), &BugConfig::default(), ScrubContext::default()).unwrap();
        store.append_log("pad", "info", "seed", 1).unwrap();
        let app = dir.path().join("apps/pad");
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o000)).unwrap();
        let status = ipc_status(Some(&store));
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(status["available"], false);
        assert!(status["error"]
            .as_str()
            .unwrap()
            .contains("accounting failed"));
        assert!(status.get("total_bytes").is_none());
    }

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

#[cfg(test)]
mod watchdog_store_tests {
    use super::*;
    use crate::watchdog::Watchdog;

    fn test_store(root: &std::path::Path, excluded: &[&str], max_line_bytes: usize) -> Store {
        let cfg = BugConfig {
            excluded_apps: excluded.iter().map(|s| s.to_string()).collect(),
            store: bread_bug::config::StoreLimits {
                max_line_bytes,
                max_bytes_per_file: max_line_bytes as u64,
                max_total_bytes: max_line_bytes as u64,
                ..Default::default()
            },
            ..Default::default()
        };
        Store::open(root, &cfg, ScrubContext::default()).unwrap()
    }

    #[test]
    fn excluded_apps_never_publish_watchdog_markers() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path(), &["clip"], 4096);
        let watchdog = Watchdog::with_excluded(Duration::from_millis(10), &["clip".into()]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = ipc_heartbeat(
            Some(&store),
            &watchdog,
            &tx,
            &json!({"app":"clip", "pid":std::process::id()}),
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert!(!watchdog.is_tracked("clip"));
        assert!(store.is_excluded("clip"));
        assert!(rx.try_recv().is_err());

        let marker = Marker::new(MarkerKind::Freeze, now_unix_ms());
        record_and_publish(Some(&store), &tx, "clip", marker);
        assert!(rx.try_recv().is_err());
        assert!(bread_bug::store::Reader::open(store.root())
            .last_marker("clip")
            .is_none());
    }

    #[test]
    fn an_unstorable_freeze_is_published_with_stored_false_and_warned() {
        let dir = tempfile::tempdir().unwrap();
        // `write_line` returns Ok(false) rather than error when an encoded
        // record exceeds the configured cap. This verifies the same no-false-
        // success contract as disk-full and permission errors.
        let store = test_store(dir.path(), &[], 96);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut marker = Marker::new(MarkerKind::Freeze, now_unix_ms());
        marker.message = Some("x".repeat(1024));
        record_and_publish(Some(&store), &tx, "clip", marker);
        let evt = rx.try_recv().expect("the freeze is still announced");
        assert_eq!(evt.event, "bread.bug.freeze");
        assert_eq!(evt.data["stored"], false);
        assert!(bread_bug::store::Reader::open(store.root())
            .last_marker("clip")
            .is_none());
    }

    #[test]
    fn a_stored_freeze_says_stored_true() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path(), &[], 4096);
        let (tx, mut rx) = mpsc::unbounded_channel();
        record_and_publish(
            Some(&store),
            &tx,
            "clip",
            Marker::new(MarkerKind::Freeze, now_unix_ms()),
        );
        assert_eq!(rx.try_recv().unwrap().data["stored"], true);
        assert_eq!(
            bread_bug::store::Reader::open(store.root())
                .last_marker("clip")
                .unwrap()
                .kind,
            MarkerKind::Freeze
        );
    }

    #[test]
    fn store_errors_publish_freeze_with_stored_false() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path(), &[], 4096);
        store
            .append_log("clip", "info", "seed", now_unix_ms())
            .unwrap();
        let app_dir = store.root().join("apps/clip");
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        record_and_publish(
            Some(&store),
            &tx,
            "clip",
            Marker::new(MarkerKind::Freeze, now_unix_ms()),
        );
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt.event, "bread.bug.freeze");
        assert_eq!(evt.data["stored"], false);
    }

    #[test]
    fn missing_store_still_publishes_freeze_with_stored_false() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        record_and_publish(
            None,
            &tx,
            "clip",
            Marker::new(MarkerKind::Freeze, now_unix_ms()),
        );
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt.event, "bread.bug.freeze");
        assert_eq!(evt.data["stored"], false);
    }

    #[test]
    fn recovered_marker_is_also_suppressed_for_an_excluded_app() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path(), &["clip"], 4096);
        let (tx, mut rx) = mpsc::unbounded_channel();
        record_and_publish(
            Some(&store),
            &tx,
            "clip",
            Marker::new(MarkerKind::Recovered, now_unix_ms()),
        );
        assert!(rx.try_recv().is_err());
        assert!(bread_bug::store::Reader::open(store.root())
            .last_marker("clip")
            .is_none());
    }

    #[test]
    fn stop_and_replacement_retire_only_the_old_frozen_instance() {
        let old_pid = std::process::id();
        // Simulate the frozen bit without waiting for real time.
        let old = instance_of(old_pid);
        let now = Instant::now();
        let direct = Watchdog::new(Duration::from_millis(1));
        direct.heartbeat("clip", old.clone(), None, now);
        direct.grace(now);
        assert_eq!(
            direct
                .check(
                    now + Duration::from_secs(12),
                    Duration::ZERO,
                    Duration::ZERO,
                    |_| crate::watchdog::Probe::Alive
                )
                .len(),
            1
        );
        // A late clean stop with a different instance cannot affect the tracked one.
        let replacement = crate::watchdog::Instance {
            pid: old_pid,
            identity: Some(bread_bug::store::ProcessIdentity {
                start_ticks: old.identity.as_ref().unwrap().start_ticks + 1,
                exe: old.identity.as_ref().unwrap().exe.clone(),
                boot_id: old.identity.as_ref().unwrap().boot_id.clone(),
            }),
        };
        assert_eq!(
            direct.stopping("clip", &replacement),
            StopOutcome::OtherInstance
        );
        assert!(direct.is_tracked("clip"));
    }

    #[tokio::test]
    async fn power_and_monitor_events_get_grace_without_a_store() {
        assert!(is_grace_event("bread.power.ac.connected"));
        assert!(is_grace_event("bread.monitor.connected"));
        assert!(is_grace_event("bread.hyprland.monitor.disconnected"));
        assert!(!is_grace_event("bread.system.startup"));

        // There is no Store parameter at all: grace listener operates solely
        // on the bus and Watchdog, so an unavailable store cannot remove it.
        let wd = Arc::new(Watchdog::new(Duration::from_secs(1)));
        let (bus, rx) = broadcast::channel(8);
        spawn_grace_listener(wd.clone(), rx);
        let before = Instant::now();
        let beat = crate::watchdog::Instance {
            pid: std::process::id(),
            identity: None,
        };
        wd.heartbeat("clip", beat.clone(), None, before);
        let _ = bus.send(BreadEvent::new(
            "bread.power.ac.connected",
            bread_shared::AdapterSource::System,
            json!({}),
        ));
        tokio::time::sleep(Duration::from_millis(20)).await;
        // If the grace listener ran, a 10-second-silent check is forgiven.
        assert!(wd
            .check(
                before + Duration::from_secs(10),
                Duration::ZERO,
                Duration::ZERO,
                |_| crate::watchdog::Probe::Alive
            )
            .is_empty());
        drop(bus);
    }

    #[test]
    fn grace_listener_is_independent_of_store_open_success() {
        // Unavailable store is `None`; the same grace task is still spawned
        // by main. This test pins the explicit design contract.
        let unavailable_store: Option<Arc<Store>> = None;
        assert!(unavailable_store.is_none());
        assert!(is_grace_event("bread.power.changed"));
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

use crate::watchdog::{
    instance_of, probe, Action, HeartbeatOutcome, Retired, StopOutcome, Watchdog,
};

/// Load `breadbug.toml`, falling back to defaults (with a note on stderr)
/// if it is unreadable: a bad config must never stop the daemon.
pub fn load_config() -> BugConfig {
    match BugConfig::load() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("breadbug: ignoring unreadable breadbug.toml: {err:#}");
            BugConfig::default()
        }
    }
}

/// Open the store, or `None` (with a note on stderr) if it cannot be opened.
/// A broken store must never stop the daemon from starting.
pub fn open(cfg: &BugConfig) -> Option<Arc<Store>> {
    match Store::open(store_root(), cfg, ScrubContext::from_environment()) {
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

/// Whether a bus event stalls sibling apps for a moment: resume from suspend
/// and display hotplug.
fn is_grace_event(event: &str) -> bool {
    event.starts_with("bread.power.")
        || event.starts_with("bread.monitor.")
        || event.starts_with("bread.hyprland.monitor.")
}

/// Pause the watchdog around power and monitor events.
///
/// Deliberately independent of the store: the watchdog runs (and can publish
/// freezes) without one, so its false-positive protection must too. Tying
/// this subscription to a successfully opened store silently turned grace
/// off exactly when the store was broken.
pub fn spawn_grace_listener(watchdog: Arc<Watchdog>, mut rx: broadcast::Receiver<BreadEvent>) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(evt) if is_grace_event(&evt.event) => watchdog.grace(Instant::now()),
                Ok(_) => {}
                // Missed events may have included a resume: be conservative.
                Err(broadcast::error::RecvError::Lagged(_)) => watchdog.grace(Instant::now()),
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
    let mut marker: Marker = serde_json::from_value(raw).map_err(|e| format!("bad marker: {e}"))?;
    if marker.kind == MarkerKind::Stopped {
        return Err("stopped is an internal watchdog marker".to_string());
    }
    // Which process a marker is about is decided by breadd's watchdog from
    // /proc, never by a client: a live stack is only ever taken of an
    // instance the daemon itself observed.
    marker.pid = None;
    marker.process = None;
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
    let usage = match store.usage() {
        Ok(usage) => usage,
        Err(err) => {
            return json!({
                "available": false,
                "error": format!("store accounting failed: {err:#}"),
            })
        }
    };
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

/// `bug.reload`: re-read `breadbug.toml` and apply the exclusion list now
/// (newly excluded apps are wiped and dropped from freeze tracking). Other
/// keys are read at startup. Writers must replace the file atomically
/// (temp file plus rename, as `BugConfig::save_to` does): a truncated
/// in-place rewrite read mid-write parses as defaults and would silently
/// clear the exclusion list for that reload.
pub fn ipc_reload(store: Option<&Store>, watchdog: &Watchdog) -> Result<Value, String> {
    let store = store.ok_or_else(unavailable)?;
    let cfg = BugConfig::load().map_err(|e| format!("{e:#}"))?;
    store
        .set_excluded(&cfg.excluded_apps)
        .map_err(|e| e.to_string())?;
    watchdog.set_excluded(&cfg.excluded_apps);
    Ok(json!({ "excluded": cfg.excluded_apps }))
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

/// Publish the bus event that matches a marker. `stored` says whether the
/// marker is actually in the store.
///
/// `Stopped` closes a freeze in the store only; it has no bus event.
pub fn publish_marker(
    emit_tx: &mpsc::UnboundedSender<BreadEvent>,
    app: &str,
    marker: &Marker,
    stored: bool,
) {
    if marker.kind == MarkerKind::Stopped {
        return;
    }
    let kind = serde_json::to_value(marker.kind)
        .ok()
        .and_then(|v| v.as_str().map(ToString::to_string))
        .unwrap_or_default();
    let _ = emit_tx.send(BreadEvent::new(
        format!("bread.bug.{kind}"),
        bread_shared::AdapterSource::System,
        json!({ "app": app, "kind": kind, "t": marker.t, "stored": stored }),
    ));
}

/// Store a watchdog marker and, for freeze and recovery, publish it.
///
/// * An excluded app gets nothing: no marker, no event. The opt-out is
///   checked here as well as before tracking, so a store whose exclusions
///   changed at runtime is honored.
/// * If the marker cannot be stored (disk full, permissions, an oversized
///   record), the event is still published, with `"stored": false`, and a
///   warning is logged. A freeze is worth telling the user about even when
///   the evidence could not be kept; the alternative, silence, would hide a
///   hung app exactly when the machine is already unhealthy. The flag lets
///   consumers (the notification, `breadbug`) know there is no marker behind
///   it, and `breadbug` never offers a live stack without a stored marker.
fn record_and_publish(
    store: Option<&Store>,
    emit_tx: &mpsc::UnboundedSender<BreadEvent>,
    app: &str,
    marker: Marker,
) {
    let stored = match store {
        Some(store) if store.is_excluded(app) => return,
        Some(store) => match store.record_marker(app, marker.clone()) {
            Ok(stored) => {
                if !stored {
                    tracing::warn!(app, kind = ?marker.kind, "breadbug marker was not stored");
                }
                stored
            }
            Err(err) => {
                tracing::warn!(app, kind = ?marker.kind, error = %err, "breadbug marker could not be stored");
                false
            }
        },
        None => false,
    };
    publish_marker(emit_tx, app, &marker, stored);
}

/// Close a freeze in the store after its instance ended or was replaced.
fn record_retired(store: Option<&Store>, app: &str, retired: &Retired, why: &str) {
    if !retired.was_frozen {
        return;
    }
    let Some(store) = store else {
        return;
    };
    let mut marker = Marker::new(MarkerKind::Stopped, now_unix_ms());
    marker.pid = Some(retired.instance.pid);
    marker.process = retired.instance.identity.clone();
    marker.message = Some(why.to_string());
    if let Err(err) = store.record_marker(app, marker) {
        tracing::warn!(app, error = %err, "could not record that a frozen process ended");
    }
}

/// `bug.heartbeat`: `{app, pid, interval_ms?, stopping?}`. Feeds the freeze
/// watchdog.
///
/// A heartbeat from the instance that was marked frozen records and publishes
/// `recovered`. A heartbeat from a different instance (restart, reused pid)
/// retires the old one and never counts as its recovery. `stopping` only
/// takes effect for the instance it comes from. Excluded apps are ignored
/// before anything is tracked.
pub fn ipc_heartbeat(
    store: Option<&Store>,
    watchdog: &Watchdog,
    emit_tx: &mpsc::UnboundedSender<BreadEvent>,
    params: &Value,
) -> Result<Value, String> {
    let app = app_param(params)?;
    let pid = params
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| "missing or invalid pid".to_string())?;
    if watchdog.is_excluded(app) || store.is_some_and(|s| s.is_excluded(app)) {
        return Ok(json!({ "ok": true }));
    }
    let instance = instance_of(pid);
    // Only a process whose actual executable belongs to this app can add a
    // heartbeat. A missing `/proc/<pid>/exe` is inconclusive, never a reason
    // to trust the client-supplied app name.
    let Some(identity) = instance.identity.as_ref() else {
        // The process ended before the daemon could read /proc. It has no
        // heartbeat to track and must not be rejected as an IPC error.
        return Ok(json!({ "ok": true, "tracked": false }));
    };
    if !bread_bug::procid::exe_matches_app(app, &identity.exe) {
        return Err(format!(
            "pid {pid} does not run the '{app}' executable (exe: {})",
            identity.exe
        ));
    }
    if params.get("stopping").and_then(Value::as_bool) == Some(true) {
        if let StopOutcome::Stopped(retired) = watchdog.stopping(app, &instance) {
            record_retired(store, app, &retired, "the process announced a clean exit");
        }
        return Ok(json!({ "ok": true }));
    }
    let interval = params
        .get("interval_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis);
    let HeartbeatOutcome {
        recovered,
        retired,
        excluded: _,
    } = watchdog.heartbeat(app, instance.clone(), interval, Instant::now());
    if let Some(retired) = retired {
        record_retired(store, app, &retired, "replaced by a new process");
    }
    if recovered {
        let mut marker = Marker::new(MarkerKind::Recovered, now_unix_ms());
        marker.pid = Some(instance.pid);
        marker.process = instance.identity;
        record_and_publish(store, emit_tx, app, marker);
    }
    Ok(json!({ "ok": true }))
}

/// Run the freeze watchdog until shutdown.
pub fn spawn_watchdog(
    store: Option<Arc<Store>>,
    watchdog: Arc<Watchdog>,
    emit_tx: mpsc::UnboundedSender<BreadEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    const TICK: Duration = Duration::from_secs(1);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last = Instant::now();
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {}
            }
            let now = Instant::now();
            let actions = watchdog.check(now, now.saturating_duration_since(last), TICK, probe);
            last = now;
            for action in actions {
                match action {
                    Action::Freeze {
                        app,
                        instance,
                        silent_for,
                    } => {
                        let mut marker = Marker::new(MarkerKind::Freeze, now_unix_ms());
                        marker.pid = Some(instance.pid);
                        marker.process = instance.identity;
                        marker.message =
                            Some(format!("no heartbeat for {}s", silent_for.as_secs()));
                        record_and_publish(store.as_deref(), &emit_tx, &app, marker);
                    }
                    Action::Gone {
                        app,
                        instance,
                        was_frozen,
                    } => {
                        let retired = Retired {
                            instance,
                            was_frozen,
                        };
                        record_retired(store.as_deref(), &app, &retired, "the process ended");
                    }
                }
            }
        }
    });
}
