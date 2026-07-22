use std::collections::{HashMap, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use bread_shared::apps::{is_known_app, validate_app_namespace};
use bread_shared::{now_unix_ms, AdapterSource, BreadEvent, RawEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing::{error, info, warn};

use crate::adapters::AdapterStatus;
use crate::core::state_engine::StateHandle;
use crate::lua::RuntimeHandle;

/// The Bread Automation API version (Lua API surface + IPC methods + event
/// vocabulary + runtime-state schema), per `Documentation.md`'s "API
/// Stability & Versioning" section. Bump the minor version when adding
/// something new-but-additive (a binding, an event, an IPC param); bump the
/// major version only for a breaking change, which should not happen inside
/// this daemon's v1 lifetime per that section's stated policy.
const API_VERSION: &str = "1.4.0";

#[derive(Clone)]
pub struct Server {
    socket_path: PathBuf,
    state_handle: StateHandle,
    event_tx: broadcast::Sender<BreadEvent>,
    lua_runtime: RuntimeHandle,
    emit_tx: mpsc::UnboundedSender<BreadEvent>,
    raw_tx: mpsc::Sender<RawEvent>,
    adapter_status: Arc<RwLock<HashMap<String, AdapterStatus>>>,
    subscription_count: Arc<AtomicU64>,
    event_buffer: Arc<std::sync::Mutex<VecDeque<BreadEvent>>>,
    started_at: Instant,
    pid: u32,
}

#[derive(Debug, Deserialize)]
struct IpcRequest {
    id: String,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct IpcResponse {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Server {
    // Server::new legitimately requires all 8 fields; a builder pattern here would be
    // over-engineering for a single-call-site constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        socket_path: PathBuf,
        state_handle: StateHandle,
        event_tx: broadcast::Sender<BreadEvent>,
        lua_runtime: RuntimeHandle,
        emit_tx: mpsc::UnboundedSender<BreadEvent>,
        raw_tx: mpsc::Sender<RawEvent>,
        adapter_status: Arc<RwLock<HashMap<String, AdapterStatus>>>,
        subscription_count: Arc<AtomicU64>,
        event_buffer: Arc<std::sync::Mutex<VecDeque<BreadEvent>>>,
    ) -> Self {
        Self {
            socket_path,
            state_handle,
            event_tx,
            lua_runtime,
            emit_tx,
            raw_tx,
            adapter_status,
            subscription_count,
            event_buffer,
            started_at: Instant::now(),
            pid: process::id(),
        }
    }

    pub async fn serve(&self, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
        if let Some(parent) = self.socket_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if self.socket_path.exists() {
            fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        fs::set_permissions(&self.socket_path, fs::Permissions::from_mode(0o600))?;

        info!(socket = %self.socket_path.display(), "ipc server listening");

        // Emit the startup event after the socket is bound so that clients
        // connecting immediately after the socket appears can subscribe and receive it.
        let _ = self.emit_tx.send(BreadEvent::new(
            "bread.system.startup",
            AdapterSource::System,
            serde_json::json!({}),
        ));

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                accept = listener.accept() => {
                    let (stream, _) = accept?;
                    let server = self.clone();
                    tokio::spawn(async move {
                        if let Err(err) = server.handle_connection(stream).await {
                            warn!(error = %err, "ipc connection failed");
                        }
                    });
                }
            }
        }

        Ok(())
    }

    async fn handle_connection(&self, stream: UnixStream) -> Result<()> {
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            let req: IpcRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let err_resp = IpcResponse {
                        id: "?".to_string(),
                        result: None,
                        error: Some(format!("parse error: {e}")),
                    };
                    write_half
                        .write_all(format!("{}\n", serde_json::to_string(&err_resp)?).as_bytes())
                        .await?;
                    continue;
                }
            };
            if req.method == "events.subscribe" {
                let filter = req
                    .params
                    .get("filter")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let ok = IpcResponse {
                    id: req.id,
                    result: Some(json!({ "subscribed": true })),
                    error: None,
                };
                write_half
                    .write_all(format!("{}\n", serde_json::to_string(&ok)?).as_bytes())
                    .await?;
                self.stream_events(&mut write_half, filter).await?;
                return Ok(());
            }

            let response = match self.handle_request(req).await {
                Ok(res) => IpcResponse {
                    id: res.0,
                    result: Some(res.1),
                    error: None,
                },
                Err((id, err)) => IpcResponse {
                    id,
                    result: None,
                    error: Some(err),
                },
            };

            write_half
                .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
                .await?;
        }

        Ok(())
    }

    async fn handle_request(
        &self,
        req: IpcRequest,
    ) -> std::result::Result<(String, Value), (String, String)> {
        let id = req.id.clone();
        let result = match req.method.as_str() {
            "ping" => Ok(json!({ "ok": true })),
            "state.get" => {
                let key = req.params.get("key").and_then(Value::as_str).unwrap_or("");
                let value = self
                    .state_handle
                    .state_get(key)
                    .await
                    .ok_or_else(|| anyhow!("state path not found"));
                value.map_err(|e| e.to_string())
            }
            "state.dump" => Ok(self.state_handle.state_dump().await),
            "modules.list" => {
                let full = self.state_handle.state_dump().await;
                Ok(full.get("modules").cloned().unwrap_or_else(|| json!([])))
            }
            "workflows.list" => {
                let full = self.state_handle.state_dump().await;
                Ok(full.get("workflows").cloned().unwrap_or_else(|| json!([])))
            }
            "widgets.list" => {
                let full = self.state_handle.state_dump().await;
                Ok(full.get("widgets").cloned().unwrap_or_else(|| json!([])))
            }
            "modules.reload" => {
                let started = Instant::now();
                if let Err(err) = self.lua_runtime.reload().await {
                    return Err((id, err.to_string()));
                }
                let duration_ms = started.elapsed().as_millis();
                let modules = self
                    .state_handle
                    .state_dump()
                    .await
                    .get("modules")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                Ok(json!({
                    "ok": true,
                    "duration_ms": duration_ms,
                    "modules": modules,
                }))
            }
            "profile.list" => {
                let full = self.state_handle.state_dump().await;
                let profile = full.get("profile").cloned().unwrap_or_else(|| json!({}));
                Ok(profile)
            }
            "profile.activate" => {
                let Some(name) = req.params.get("name").and_then(Value::as_str) else {
                    return Err((id, "missing profile name".to_string()));
                };

                self.state_handle.set_profile(name.to_string());
                if self
                    .emit_tx
                    .send(BreadEvent::new(
                        "bread.profile.activated",
                        AdapterSource::System,
                        json!({ "name": name }),
                    ))
                    .is_err()
                {
                    return Err((id, "emit channel closed".to_string()));
                }
                Ok(json!({ "active": name }))
            }
            "emit" => {
                let data = req.params.get("data").cloned().unwrap_or_else(|| json!({}));

                // Sourced emit: hook-originated events (shell/git/ssh) and
                // sibling bread* app events both go through the same
                // RawEvent -> normalizer pipeline as in-process adapters,
                // instead of being tagged System. `source` is restricted to
                // the fixed hook-fed set plus registered app ids — allowing
                // arbitrary sources here would let any socket client spoof
                // e.g. a power/bluetooth event, or another app's namespace.
                if let Some(source_str) = req.params.get("source").and_then(Value::as_str) {
                    let source = match source_str {
                        "terminal" => AdapterSource::Terminal,
                        "git" => AdapterSource::Git,
                        "remote" => AdapterSource::Remote,
                        other if is_known_app(other) => AdapterSource::App(other.to_string()),
                        other => {
                            return Err((
                                id,
                                format!("source '{other}' is not externally injectable"),
                            ));
                        }
                    };
                    let Some(kind) = req.params.get("kind").and_then(Value::as_str) else {
                        return Err((id, "missing kind for sourced emit".to_string()));
                    };
                    // For a sibling-app source, `kind` is the full dotted event
                    // name (e.g. "bread.clip.copied"), not a bare suffix — it
                    // must live inside that app's own namespace.
                    if let AdapterSource::App(app) = &source {
                        if !validate_app_namespace(app, kind) {
                            return Err((
                                id,
                                format!(
                                    "event '{kind}' is not in the '{app}' namespace (must start with 'bread.{app}.')"
                                ),
                            ));
                        }
                    }
                    if self
                        .raw_tx
                        .send(RawEvent {
                            source,
                            kind: kind.to_string(),
                            payload: data,
                            timestamp: now_unix_ms(),
                        })
                        .await
                        .is_err()
                    {
                        return Err((id, "raw channel closed".to_string()));
                    }
                    Ok(json!({ "emitted": true }))
                } else {
                    let Some(event) = req.params.get("event").and_then(Value::as_str) else {
                        return Err((id, "missing event name".to_string()));
                    };
                    if self
                        .emit_tx
                        .send(BreadEvent::new(event, AdapterSource::System, data))
                        .is_err()
                    {
                        return Err((id, "emit channel closed".to_string()));
                    }
                    Ok(json!({ "emitted": true }))
                }
            }
            "health" => {
                let uptime_ms = self.started_at.elapsed().as_millis();
                let state = self.state_handle.state_dump().await;
                let modules = state.get("modules").cloned().unwrap_or_else(|| json!([]));
                let adapters = self.adapter_status.read().await.clone();
                let subscription_count = self
                    .subscription_count
                    .load(std::sync::atomic::Ordering::Relaxed);
                let recent_errors = self.lua_runtime.recent_errors();
                Ok(json!({
                    "ok": true,
                    "pid": self.pid,
                    "version": env!("CARGO_PKG_VERSION"),
                    "api_version": API_VERSION,
                    "uptime_ms": uptime_ms,
                    "socket": self.socket_path.to_string_lossy(),
                    "adapters": adapters,
                    "modules": modules,
                    "subscriptions": subscription_count,
                    "recent_errors": recent_errors,
                }))
            }
            "events.replay" => {
                let since_ms = req
                    .params
                    .get("since_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let cutoff = now_unix_ms().saturating_sub(since_ms);
                let replay: Vec<BreadEvent> = self
                    .event_buffer
                    .lock()
                    .map(|buf| {
                        buf.iter()
                            .filter(|e| e.timestamp >= cutoff)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(serde_json::to_value(replay).unwrap_or_else(|_| json!([])))
            }
            _ => Err("unknown method".to_string()),
        };

        match result {
            Ok(v) => Ok((id, v)),
            Err(err) => Err((id, err)),
        }
    }

    async fn stream_events(
        &self,
        writer: &mut tokio::net::unix::OwnedWriteHalf,
        filter: Option<String>,
    ) -> Result<()> {
        let mut rx = self.event_tx.subscribe();
        loop {
            let evt = rx.recv().await?;
            if let Some(filter) = filter.as_deref() {
                if !bread_shared::glob::matches_pattern(filter, &evt.event) {
                    continue;
                }
            }

            let line = format!("{}\n", serde_json::to_string(&evt)?);
            if let Err(err) = writer.write_all(line.as_bytes()).await {
                error!(error = %err, "failed to write event stream line");
                return Ok(());
            }
        }
    }
}

// The CLI `--filter` glob semantics used to be a second, hand-rolled copy of
// the subscription-table matcher (`matches_filter`/`matches_glob_filter`
// used to live here). That duplication is exactly what let the two paths
// drift out of sync despite the docs claiming parity. Both now delegate to
// the single implementation in `bread_shared::glob::matches_pattern`; see
// that module for the pattern-matching test suite.
