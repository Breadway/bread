//! Unix-socket client for breadd.
//!
//! Two background loops: one holds `events.subscribe` open (with replay
//! priming and reconnect-with-backoff), and one polls the introspection
//! methods into a single [`Snapshot`]. All socket I/O stays off the GTK main
//! thread; the UI drains channels on a `glib` timeout.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bread_shared::BreadEvent;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::model::{
    Health, Module, Snapshot, SubscriptionEntry, TraceEntry, WidgetSummary, Workflow,
};

const REQUEST_TIMEOUT: Duration = Duration::from_millis(1500);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const TRACE_WINDOW_MS: u64 = 120_000;
const REPLAY_WINDOW_MS: u64 = 120_000;

#[derive(Debug)]
pub enum StreamUpdate {
    Connected(bool),
    Events(Vec<BreadEvent>),
    Error(String),
}

/// Send one request and return its `result`. Unlike bread-utils' fire-and-
/// forget client this surfaces the daemon's error string, which the emit
/// dialog needs to show the reserved-domain rejection verbatim.
pub fn request(method: &str, params: Value) -> Result<Value> {
    let socket = bread_shared::resolve_socket_path();
    let mut stream =
        UnixStream::connect(&socket).with_context(|| format!("connect {}", socket.display()))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let line =
        serde_json::to_string(&json!({ "id": "scope", "method": method, "params": params }))?;
    writeln!(stream, "{line}")?;
    stream.flush()?;

    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    if response.trim().is_empty() {
        return Err(anyhow!("daemon closed the connection without a reply"));
    }
    let value: Value = serde_json::from_str(&response)?;
    if let Some(err) = value.get("error").and_then(Value::as_str) {
        return Err(anyhow!(err.to_string()));
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

fn parse<T: DeserializeOwned + Default>(value: Value) -> T {
    serde_json::from_value(value).unwrap_or_default()
}

fn fetch_snapshot() -> Snapshot {
    let Ok(health_value) = request("health", json!({})) else {
        return Snapshot::default();
    };
    let health: Health = parse(health_value);
    let modules: Vec<Module> = request("modules.list", json!({}))
        .map(parse)
        .unwrap_or_default();
    let subscriptions: Vec<SubscriptionEntry> = request("subscriptions.list", json!({}))
        .map(parse)
        .unwrap_or_default();
    let traces: Vec<TraceEntry> = request("events.trace", json!({ "since_ms": TRACE_WINDOW_MS }))
        .map(parse)
        .unwrap_or_default();
    let state = request("state.dump", json!({})).unwrap_or(Value::Null);
    let workflows: Vec<Workflow> = request("workflows.list", json!({}))
        .map(parse)
        .unwrap_or_default();
    let widgets: Vec<WidgetSummary> = request("widgets.list", json!({}))
        .map(parse)
        .unwrap_or_default();

    Snapshot {
        daemon_up: true,
        health: Some(health),
        modules,
        subscriptions,
        traces,
        state,
        workflows,
        widgets,
    }
}

pub struct Poller {
    stop: Arc<AtomicBool>,
    _join: thread::JoinHandle<()>,
}

impl Poller {
    pub fn spawn(tx: Sender<Snapshot>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let join = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let snapshot = fetch_snapshot();
                if tx.send(snapshot).is_err() {
                    break;
                }
                // Sleep in short slices so shutdown is prompt.
                let mut waited = Duration::ZERO;
                while waited < POLL_INTERVAL && !thread_stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(100));
                    waited += Duration::from_millis(100);
                }
            }
        });
        Self { stop, _join: join }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Stream {
    stop: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<UnixStream>>>,
    _join: thread::JoinHandle<()>,
}

impl Stream {
    pub fn spawn(tx: Sender<StreamUpdate>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(None));
        let thread_stop = stop.clone();
        let thread_current = current.clone();
        let join = thread::spawn(move || {
            let mut attempt = 0u32;
            while !thread_stop.load(Ordering::Relaxed) {
                match stream_once(&tx, &thread_stop, &thread_current) {
                    Ok(()) => attempt = 0,
                    Err(err) => {
                        if thread_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        eprintln!("breadscope stream: {err}");
                        let _ = tx.send(StreamUpdate::Error(err.to_string()));
                        attempt = (attempt + 1).min(5);
                    }
                }
                *thread_current.lock().unwrap_or_else(|p| p.into_inner()) = None;
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(400u64 * (1 << attempt)));
            }
        });
        Self {
            stop,
            stream: current,
            _join: join,
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(stream) = self
            .stream
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stream_once(
    tx: &Sender<StreamUpdate>,
    stop: &AtomicBool,
    current: &Mutex<Option<UnixStream>>,
) -> Result<()> {
    let socket = bread_shared::resolve_socket_path();
    let stream =
        UnixStream::connect(&socket).with_context(|| format!("connect {}", socket.display()))?;
    let read_stream = stream.try_clone()?;
    *current.lock().unwrap_or_else(|p| p.into_inner()) = Some(stream.try_clone()?);

    // Prime from the daemon's ring first, then subscribe on the same
    // connection. The window is short and duplicates are removed by event id.
    let mut writer = stream;
    let replay = json!({
        "id": "replay",
        "method": "events.replay",
        "params": { "since_ms": REPLAY_WINDOW_MS }
    });
    writeln!(writer, "{}", serde_json::to_string(&replay)?)?;
    writer.flush()?;

    let mut reader = BufReader::new(read_stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let envelope: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let replay = envelope.get("result").cloned().unwrap_or(Value::Null);
    match serde_json::from_value::<Vec<BreadEvent>>(replay) {
        Ok(events) => {
            if !events.is_empty() {
                let _ = tx.send(StreamUpdate::Events(events));
            }
        }
        Err(err) => {
            let _ = tx.send(StreamUpdate::Error(format!("replay decode failed: {err}")));
        }
    }

    let subscribe = json!({
        "id": "subscribe",
        "method": "events.subscribe",
        "params": {}
    });
    writeln!(writer, "{}", serde_json::to_string(&subscribe)?)?;
    writer.flush()?;

    line.clear();
    reader.read_line(&mut line)?;
    let ack: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    if let Some(err) = ack.get("error").and_then(Value::as_str) {
        return Err(anyhow!(err.to_string()));
    }
    let _ = tx.send(StreamUpdate::Connected(true));

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Err(anyhow!("daemon closed the event stream"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<BreadEvent>(trimmed) {
            // Hand each event over immediately. Event rates are low (even a
            // busy desktop is a few per second), and batching without a
            // flush timer would leave the UI stale until 64 events arrived.
            let _ = tx.send(StreamUpdate::Events(vec![event]));
        }
    }
}

/// One-shot helpers used by the emit dialog and the trace toggle.
pub fn set_trace(enabled: bool) -> Result<Value> {
    request("trace.set", json!({ "enabled": enabled }))
}

pub fn emit(event: &str, data: Value) -> Result<Value> {
    request("emit", json!({ "event": event, "data": data }))
}

/// Drain a channel without blocking, for use from a glib timeout.
pub fn try_drain<T>(rx: &Receiver<T>, limit: usize) -> Vec<T> {
    let mut out = Vec::new();
    while out.len() < limit {
        match rx.try_recv() {
            Ok(item) => out.push(item),
            Err(_) => break,
        }
    }
    out
}
