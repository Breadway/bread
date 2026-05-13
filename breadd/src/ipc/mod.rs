use std::collections::{HashMap, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use bread_shared::{now_unix_ms, AdapterSource, BreadEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing::{error, info, warn};

use crate::adapters::AdapterStatus;
use crate::core::state_engine::StateHandle;
use crate::lua::RuntimeHandle;

#[derive(Clone)]
pub struct Server {
    socket_path: PathBuf,
    state_handle: StateHandle,
    event_tx: broadcast::Sender<BreadEvent>,
    lua_runtime: RuntimeHandle,
    emit_tx: mpsc::UnboundedSender<BreadEvent>,
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

            let req: IpcRequest = serde_json::from_str(&line)?;
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
                let profiles = full
                    .get("profile")
                    .and_then(|v| v.get("profiles"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Ok(profiles)
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
                let Some(event) = req.params.get("event").and_then(Value::as_str) else {
                    return Err((id, "missing event name".to_string()));
                };
                let data = req.params.get("data").cloned().unwrap_or_else(|| json!({}));
                if self
                    .emit_tx
                    .send(BreadEvent::new(event, AdapterSource::System, data))
                    .is_err()
                {
                    return Err((id, "emit channel closed".to_string()));
                }
                Ok(json!({ "emitted": true }))
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
                    "uptime_ms": uptime_ms,
                    "socket": self.socket_path.to_string_lossy(),
                    "adapters": adapters,
                    "modules": modules,
                    "subscriptions": subscription_count,
                    "recent_errors": recent_errors,
                }))
            }
            "sync.status" => {
                let sync_path = bread_sync::config::bread_config_dir().join("sync.toml");
                match std::fs::read_to_string(&sync_path)
                    .ok()
                    .and_then(|s| s.parse::<toml::Value>().ok())
                {
                    Some(toml) => {
                        let machine = toml
                            .get("machine")
                            .and_then(|m| m.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let remote = toml
                            .get("remote")
                            .and_then(|r| r.get("url"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        Ok(json!({
                            "initialized": true,
                            "machine": machine,
                            "remote": remote,
                        }))
                    }
                    None => Ok(json!({ "initialized": false })),
                }
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
                if !matches_filter(&evt.event, filter) {
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

fn matches_filter(event_name: &str, pattern: &str) -> bool {
    // Delegate to the same glob logic used by the subscription table so that
    // `bread events --filter "bread.device.**"` behaves identically to
    // `bread.on("bread.device.**", ...)` in Lua.
    if pattern.ends_with(".*") {
        let prefix = &pattern[..pattern.len() - 1];
        return event_name.starts_with(prefix);
    }

    if let Some(prefix) = pattern.strip_suffix(".**") {
        if event_name == prefix || event_name.starts_with(&format!("{prefix}.")) {
            return true;
        }
        return false;
    }

    matches_glob_filter(pattern.as_bytes(), event_name.as_bytes())
}

fn matches_glob_filter(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    if pattern.len() >= 2 && pattern[0] == b'*' && pattern[1] == b'*' {
        let rest = &pattern[2..];
        if rest.is_empty() {
            return true;
        }
        for offset in 0..=text.len() {
            if matches_glob_filter(rest, &text[offset..]) {
                return true;
            }
        }
        return false;
    }

    match pattern[0] {
        b'*' => {
            let mut offset = 0;
            loop {
                if matches_glob_filter(&pattern[1..], &text[offset..]) {
                    return true;
                }
                if offset == text.len() || text[offset] == b'.' {
                    break;
                }
                offset += 1;
            }
            false
        }
        b'?' => {
            if text.is_empty() || text[0] == b'.' {
                return false;
            }
            matches_glob_filter(&pattern[1..], &text[1..])
        }
        ch => {
            if text.first().copied() != Some(ch) {
                return false;
            }
            matches_glob_filter(&pattern[1..], &text[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::matches_filter;

    #[test]
    fn filter_exact_match() {
        assert!(matches_filter("bread.window.opened", "bread.window.opened"));
        assert!(!matches_filter(
            "bread.window.opened",
            "bread.window.closed"
        ));
    }

    #[test]
    fn filter_dot_star_matches_one_segment_only() {
        assert!(matches_filter("bread.device.connected", "bread.device.*"));
        assert!(matches_filter(
            "bread.device.dock.connected",
            "bread.device.*"
        ));
        assert!(!matches_filter("bread.device", "bread.device.*"));
    }

    #[test]
    fn filter_dot_double_star_matches_zero_or_more_segments() {
        // Matches the exact prefix (zero segments after).
        assert!(matches_filter("bread.device", "bread.device.**"));
        // And matches deeper paths.
        assert!(matches_filter(
            "bread.device.dock.connected",
            "bread.device.**"
        ));
        // But not a sibling at the same depth.
        assert!(!matches_filter(
            "bread.network.connected",
            "bread.device.**"
        ));
    }

    #[test]
    fn filter_question_mark_matches_single_char_not_dot() {
        assert!(matches_filter("bread.x", "bread.?"));
        assert!(!matches_filter("bread.xy", "bread.?"));
        assert!(!matches_filter("bread.", "bread.?"));
    }

    #[test]
    fn filter_mid_pattern_star_does_not_cross_dots() {
        // A `*` in the middle of the pattern (not the `.*` suffix shortcut)
        // matches within a single segment only.
        assert!(matches_filter("bread.alpha.connected", "bread.*.connected"));
        assert!(!matches_filter(
            "bread.alpha.beta.connected",
            "bread.*.connected"
        ));
    }

    #[test]
    fn filter_dot_star_at_end_acts_as_prefix_match() {
        // `bread.*` ending the pattern is treated as a prefix match, so
        // matches everything under `bread.` regardless of depth. This is
        // consistent with the subscription table's pattern matcher.
        assert!(matches_filter("bread.alpha", "bread.*"));
        assert!(matches_filter("bread.alpha.beta", "bread.*"));
    }
}
