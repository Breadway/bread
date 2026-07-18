use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::adapters::Adapter;

/// Watches `podman events --format json` for container lifecycle changes and
/// forwards them as [`RawEvent`]s.
///
/// This is the first adapter in the codebase wrapping a child process rather
/// than a socket/D-Bus/netlink connection, so the process-lifecycle handling
/// here (kill-on-drop, treating any exit as an error so the supervisor
/// retries) is bespoke rather than following an existing pattern.
#[derive(Clone, Debug)]
pub struct PodmanAdapter;

impl PodmanAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PodmanAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for PodmanAdapter {
    fn name(&self) -> &'static str {
        "podman"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        info!("podman adapter starting");

        // kill_on_drop(true) ensures that if this future is cancelled (e.g. the
        // supervisor tears the adapter down on daemon shutdown, or `tokio::select!`
        // in Manager::spawn_adapter races it against the shutdown signal), the
        // `podman events` child is killed rather than left running as an orphan
        // with its stdout pipe silently discarded.
        let mut child = match Command::new("podman")
            .args(["events", "--format", "json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!("podman binary not found; will retry on backoff");
                return Err(anyhow!("podman binary not found: {e}"));
            }
            Err(e) => {
                return Err(anyhow!("failed to spawn podman events: {e}"));
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("podman events: child had no stdout"))?;
        let mut lines = BufReader::new(stdout).lines();

        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => {
                    // EOF: the child's stdout closed, meaning the process exited.
                    // Never surface this as Ok(()) — the supervisor treats a clean
                    // `run()` return as "stop forever," but a dead `podman events`
                    // process is exactly the kind of thing we want retried.
                    let _ = child.kill().await;
                    return Err(anyhow!("podman events exited"));
                }
                Err(e) => {
                    let _ = child.kill().await;
                    return Err(anyhow!("podman events read error: {e}"));
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            let value: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    debug!("podman events: skipping unparseable line: {e}");
                    continue;
                }
            };

            if let Some((kind, payload)) = map_podman_event(&value) {
                if tx
                    .send(RawEvent {
                        source: AdapterSource::Podman,
                        kind,
                        payload,
                        timestamp: now_unix_ms(),
                    })
                    .await
                    .is_err()
                {
                    let _ = child.kill().await;
                    return Err(anyhow!("podman adapter: downstream channel closed"));
                }
            }
        }
    }
}

/// Parses a single `podman events --format json` line and maps it to a
/// `(kind, payload)` pair for `RawEvent`, or `None` if the event should be
/// ignored.
///
/// Only `Type == "container"` events are handled. Action mapping:
/// - `start` -> `container.started`
/// - `died` -> `container.stopped`
/// - `stop` / `remove` -> ignored (see de-dup note below)
/// - `health_status` -> `container.health_status`
/// - anything else -> ignored
///
/// De-dup choice: podman commonly emits `died` followed by `stop` (and
/// sometimes `remove`) for a single container exit. Emitting on all three
/// would fire `container.stopped` multiple times for one real-world
/// transition, which is worse for Lua module authors (who'd need to
/// de-duplicate themselves) than missing the rare case where a container is
/// stopped without ever having been in a running state that produced `died`.
/// `died` fires in the overwhelmingly common paths (normal exit, kill, crash),
/// so it's used as the sole trigger for `container.stopped` and `stop`/`remove`
/// are dropped.
fn map_podman_event(value: &Value) -> Option<(String, Value)> {
    let event_type = value.get("Type").and_then(|v| v.as_str())?;
    if event_type != "container" {
        return None;
    }

    let action = value.get("Action").and_then(|v| v.as_str())?;

    let actor = value.get("Actor");
    let id = actor
        .and_then(|a| a.get("ID"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let attributes = actor.and_then(|a| a.get("Attributes"));
    let name = attributes
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let image = attributes
        .and_then(|a| a.get("image"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    match action {
        "start" => Some((
            "container.started".to_string(),
            json!({
                "id": id,
                "name": name,
                "image": image,
            }),
        )),
        "died" => Some((
            "container.stopped".to_string(),
            json!({
                "id": id,
                "name": name,
            }),
        )),
        "stop" | "remove" => {
            // Intentionally ignored — see de-dup note on map_podman_event above.
            None
        }
        "health_status" => {
            let health = attributes
                .and_then(|a| a.get("health_status"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            Some((
                "container.health_status".to_string(),
                json!({
                    "id": id,
                    "name": name,
                    "health": health,
                }),
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container_event(action: &str, extra_attrs: Value) -> Value {
        let mut attributes = json!({
            "name": "my-container",
            "image": "docker.io/library/nginx:latest",
        });
        if let (Some(attrs_obj), Some(extra_obj)) =
            (attributes.as_object_mut(), extra_attrs.as_object())
        {
            for (k, v) in extra_obj {
                attrs_obj.insert(k.clone(), v.clone());
            }
        }

        json!({
            "Type": "container",
            "Action": action,
            "Actor": {
                "ID": "abc123fullid",
                "Attributes": attributes,
            },
            "Status": action,
            "time": 1_700_000_000,
        })
    }

    #[test]
    fn maps_start_event() {
        let event = container_event("start", json!({}));
        let (kind, payload) = map_podman_event(&event).expect("should map start event");
        assert_eq!(kind, "container.started");
        assert_eq!(payload["id"], "abc123fullid");
        assert_eq!(payload["name"], "my-container");
        assert_eq!(payload["image"], "docker.io/library/nginx:latest");
    }

    #[test]
    fn maps_died_event() {
        let event = container_event("died", json!({}));
        let (kind, payload) = map_podman_event(&event).expect("should map died event");
        assert_eq!(kind, "container.stopped");
        assert_eq!(payload["id"], "abc123fullid");
        assert_eq!(payload["name"], "my-container");
    }

    #[test]
    fn ignores_stop_event_to_avoid_double_emit_with_died() {
        let event = container_event("stop", json!({}));
        assert!(map_podman_event(&event).is_none());
    }

    #[test]
    fn ignores_remove_event() {
        let event = container_event("remove", json!({}));
        assert!(map_podman_event(&event).is_none());
    }

    #[test]
    fn maps_health_status_event() {
        let event = container_event("health_status", json!({ "health_status": "healthy" }));
        let (kind, payload) = map_podman_event(&event).expect("should map health_status event");
        assert_eq!(kind, "container.health_status");
        assert_eq!(payload["id"], "abc123fullid");
        assert_eq!(payload["name"], "my-container");
        assert_eq!(payload["health"], "healthy");
    }

    #[test]
    fn ignores_unknown_action() {
        let event = container_event("exec_die", json!({}));
        assert!(map_podman_event(&event).is_none());
    }

    #[test]
    fn ignores_non_container_type() {
        let event = json!({
            "Type": "network",
            "Action": "start",
            "Actor": { "ID": "netid", "Attributes": {} },
        });
        assert!(map_podman_event(&event).is_none());
    }

    #[test]
    fn missing_fields_fall_back_to_unknown() {
        let event = json!({
            "Type": "container",
            "Action": "start",
            "Actor": { "ID": "onlyid" },
        });
        let (kind, payload) = map_podman_event(&event).expect("should still map");
        assert_eq!(kind, "container.started");
        assert_eq!(payload["id"], "onlyid");
        assert_eq!(payload["name"], "unknown");
        assert_eq!(payload["image"], "unknown");
    }
}
