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
use tokio::sync::{broadcast, mpsc, watch};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::watchdog::{process_alive, Action, Watchdog};

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
pub fn spawn_event_recorder(
    store: Arc<Store>,
    watchdog: Arc<Watchdog>,
    mut rx: broadcast::Receiver<BreadEvent>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(evt) => {
                    // Resume and display hotplug stall apps for a moment;
                    // the watchdog must not call that a freeze.
                    if evt.event.starts_with("bread.power.") || evt.event.starts_with("bread.monitor.") {
                        watchdog.grace(Instant::now());
                    }
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

/// `bug.reload`: re-read `breadbug.toml` and apply the exclusion list now
/// (newly excluded apps are wiped). Other keys are read at startup.
pub fn ipc_reload(store: Option<&Store>) -> Result<Value, String> {
    let store = store.ok_or_else(unavailable)?;
    let cfg = BugConfig::load().map_err(|e| format!("{e:#}"))?;
    store.set_excluded(&cfg.excluded_apps).map_err(|e| e.to_string())?;
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

/// Publish the bus event that matches a stored marker.
pub fn publish_marker(
    emit_tx: &mpsc::UnboundedSender<BreadEvent>,
    app: &str,
    marker: &Marker,
) {
    let kind = serde_json::to_value(marker.kind)
        .ok()
        .and_then(|v| v.as_str().map(ToString::to_string))
        .unwrap_or_default();
    let _ = emit_tx.send(BreadEvent::new(
        format!("bread.bug.{kind}"),
        bread_shared::AdapterSource::System,
        json!({ "app": app, "kind": kind, "t": marker.t }),
    ));
}

/// `bug.heartbeat`: `{app, pid, interval_ms?, stopping?}`. Feeds the freeze
/// watchdog; a heartbeat after a freeze records and publishes `recovered`.
pub fn ipc_heartbeat(
    store: Option<&Store>,
    watchdog: &Watchdog,
    emit_tx: &mpsc::UnboundedSender<BreadEvent>,
    params: &Value,
) -> Result<Value, String> {
    let app = app_param(params)?;
    if params.get("stopping").and_then(Value::as_bool) == Some(true) {
        watchdog.stopping(app);
        return Ok(json!({ "ok": true }));
    }
    let pid = params
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| "missing or invalid pid".to_string())?;
    let interval = params
        .get("interval_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis);
    if watchdog.heartbeat(app, pid, interval, Instant::now()) {
        let mut marker = Marker::new(MarkerKind::Recovered, now_unix_ms());
        marker.pid = Some(pid);
        if let Some(store) = store {
            let _ = store.record_marker(app, marker.clone());
        }
        publish_marker(emit_tx, app, &marker);
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
            let actions = watchdog.check(now, now.saturating_duration_since(last), TICK, process_alive);
            last = now;
            for action in actions {
                let Action::Freeze { app, pid, silent_for } = action else {
                    continue;
                };
                let mut marker = Marker::new(MarkerKind::Freeze, now_unix_ms());
                marker.pid = Some(pid);
                marker.message = Some(format!("no heartbeat for {}s", silent_for.as_secs()));
                if let Some(store) = &store {
                    let _ = store.record_marker(&app, marker.clone());
                }
                publish_marker(&emit_tx, &app, &marker);
            }
        }
    });
}
