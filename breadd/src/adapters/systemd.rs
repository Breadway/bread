use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use futures_util::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, Message, MessageStream};

use super::Adapter;

const MANAGER_DEST: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_IFACE: &str = "org.freedesktop.systemd1.Manager";
const UNIT_IFACE: &str = "org.freedesktop.systemd1.Unit";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

/// Watches an allowlist of `systemd --user` units on the session bus and emits
/// start/stop/failure lifecycle events.
///
/// Only units named in the allowlist are tracked — subscribing to every user
/// unit's transitions is noisy (timers, transient scopes, etc. fire
/// constantly), so we filter down to what the user's config explicitly named.
#[derive(Clone, Debug)]
pub struct SystemdAdapter {
    units: Vec<String>,
}

impl SystemdAdapter {
    pub fn new(units: Vec<String>) -> Self {
        Self { units }
    }
}

#[async_trait]
impl Adapter for SystemdAdapter {
    fn name(&self) -> &'static str {
        "systemd"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        info!("systemd adapter starting");

        let conn = Connection::session()
            .await
            .map_err(|e| anyhow!("systemd session bus unavailable: {e}"))?;

        // Job/property signals aren't delivered until a client asks the manager
        // to start tracking them.
        conn.call_method(
            Some(MANAGER_DEST),
            MANAGER_PATH,
            Some(MANAGER_IFACE),
            "Subscribe",
            &(),
        )
        .await
        .map_err(|e| anyhow!("systemd Manager.Subscribe failed: {e}"))?;

        // Resolve each allowlisted unit name to its object path up front, so
        // PropertiesChanged messages (which arrive addressed by path, not name)
        // can be matched back to a unit without a lookup on every message. A
        // unit that fails to resolve (not currently loaded, typo'd name, etc.)
        // is skipped rather than failing the whole adapter — it simply won't be
        // watched for `unit.failed` until the adapter restarts.
        let mut path_to_unit: HashMap<String, String> = HashMap::new();
        for unit in &self.units {
            match get_unit_path(&conn, unit).await {
                Ok(path) => {
                    debug!("systemd resolved unit '{unit}' -> {path}");
                    path_to_unit.insert(path, unit.clone());
                }
                Err(e) => {
                    warn!("systemd: could not resolve unit '{unit}' (not loaded?): {e}");
                }
            }
        }

        let mut stream = MessageStream::from(&conn);
        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => {
                    if let Some(event) =
                        handle_message(&conn, &message, &self.units, &path_to_unit).await
                    {
                        if tx.send(event).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Err(e) => debug!("systemd stream error: {e}"),
            }
        }

        Ok(())
    }
}

/// Resolve a unit name to its `/org/freedesktop/systemd1/unit/...` object path
/// via `Manager.GetUnit`.
async fn get_unit_path(conn: &Connection, unit_name: &str) -> Result<String> {
    let msg = conn
        .call_method(
            Some(MANAGER_DEST),
            MANAGER_PATH,
            Some(MANAGER_IFACE),
            "GetUnit",
            &(unit_name,),
        )
        .await?;
    let path: OwnedObjectPath = msg.body()?;
    Ok(path.as_str().to_string())
}

/// Read the current `ActiveState` property (`"active"`, `"inactive"`,
/// `"failed"`, ...) off a resolved unit object path.
async fn query_active_state(conn: &Connection, unit_path: &str) -> Option<String> {
    let msg = conn
        .call_method(
            Some(MANAGER_DEST),
            unit_path,
            Some(PROPS_IFACE),
            "Get",
            &(UNIT_IFACE, "ActiveState"),
        )
        .await
        .ok()?;
    let value: OwnedValue = msg.body().ok()?;
    serde_json::to_value(&value)
        .ok()?
        .as_str()
        .map(|s| s.to_string())
}

async fn handle_message(
    conn: &Connection,
    message: &Message,
    units: &[String],
    path_to_unit: &HashMap<String, String>,
) -> Option<RawEvent> {
    let header = message.header().ok()?;
    let interface = header.interface().ok()??.as_str().to_string();
    let member = header.member().ok()??.as_str().to_string();
    let path = header
        .path()
        .ok()
        .flatten()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();

    // Start/stop: a job affecting one of our allowlisted units has completed.
    // JobRemoved alone doesn't say whether the job was a start or a stop (or
    // what it settled on), so we re-query ActiveState once the job is done to
    // find out what actually happened. "failed" is deliberately left to the
    // PropertiesChanged branch below, so each transition is only emitted once.
    if interface == MANAGER_IFACE && member == "JobRemoved" {
        let (_id, _job_path, unit_name, _result): (u32, OwnedObjectPath, String, String) =
            message.body().ok()?;
        if !units.iter().any(|u| u == &unit_name) {
            return None;
        }
        let unit_path = get_unit_path(conn, &unit_name).await.ok()?;
        let state = query_active_state(conn, &unit_path).await?;
        let kind = active_state_to_kind(&state)?;
        return Some(RawEvent {
            source: AdapterSource::Systemd,
            kind: kind.to_string(),
            payload: json!({ "unit": unit_name }),
            timestamp: now_unix_ms(),
        });
    }

    // Failed: ActiveState flipped to "failed" on a unit we resolved at startup.
    // This covers unit failures that occur without an explicit job completing
    // from our point of view (e.g. a crash detected asynchronously).
    if interface == PROPS_IFACE && member == "PropertiesChanged" {
        let unit_name = path_to_unit.get(&path)?;
        let (iface, changed, _invalidated): (String, HashMap<String, OwnedValue>, Vec<String>) =
            message.body().ok()?;
        if iface != UNIT_IFACE {
            return None;
        }
        let changed_json = serde_json::to_value(&changed).ok()?;
        if !is_failed_transition(&changed_json) {
            return None;
        }
        let result = failure_result(&changed_json);
        return Some(RawEvent {
            source: AdapterSource::Systemd,
            kind: "unit.failed".to_string(),
            payload: json!({ "unit": unit_name, "result": result }),
            timestamp: now_unix_ms(),
        });
    }

    None
}

/// Map a unit's `ActiveState` to the lifecycle event kind it represents.
/// `"failed"` is intentionally excluded — that transition is reported via the
/// dedicated `PropertiesChanged` branch instead, so it isn't double-reported
/// once here and once there. Intermediate states (`activating`, `deactivating`,
/// `reloading`) aren't a resting state yet, so they're ignored too.
fn active_state_to_kind(state: &str) -> Option<&'static str> {
    match state {
        "active" => Some("unit.started"),
        "inactive" | "dead" => Some("unit.stopped"),
        _ => None,
    }
}

/// Whether a decoded `PropertiesChanged` payload represents a transition into
/// the `failed` active state.
fn is_failed_transition(changed: &serde_json::Value) -> bool {
    changed
        .get("ActiveState")
        .and_then(|v| v.as_str())
        .map(|s| s == "failed")
        .unwrap_or(false)
}

/// Extract the `Result` property (e.g. `"exit-code"`, `"timeout"`) from a
/// decoded `PropertiesChanged` payload, if it was included in this batch.
fn failure_result(changed: &serde_json::Value) -> Option<String> {
    changed
        .get("Result")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_state_to_kind_maps_active_to_started() {
        assert_eq!(active_state_to_kind("active"), Some("unit.started"));
    }

    #[test]
    fn active_state_to_kind_maps_inactive_and_dead_to_stopped() {
        assert_eq!(active_state_to_kind("inactive"), Some("unit.stopped"));
        assert_eq!(active_state_to_kind("dead"), Some("unit.stopped"));
    }

    #[test]
    fn active_state_to_kind_excludes_failed() {
        // Failed transitions are reported via PropertiesChanged instead, so
        // JobRemoved handling must not also emit for this state.
        assert_eq!(active_state_to_kind("failed"), None);
    }

    #[test]
    fn active_state_to_kind_ignores_transitional_states() {
        assert_eq!(active_state_to_kind("activating"), None);
        assert_eq!(active_state_to_kind("deactivating"), None);
        assert_eq!(active_state_to_kind("reloading"), None);
    }

    #[test]
    fn is_failed_transition_detects_failed_active_state() {
        let changed = json!({ "ActiveState": "failed", "SubState": "failed" });
        assert!(is_failed_transition(&changed));
    }

    #[test]
    fn is_failed_transition_ignores_other_active_states() {
        let changed = json!({ "ActiveState": "active" });
        assert!(!is_failed_transition(&changed));
    }

    #[test]
    fn is_failed_transition_ignores_unrelated_property_changes() {
        // A PropertiesChanged batch that doesn't touch ActiveState at all
        // (e.g. just MemoryCurrent ticking) must not be treated as a failure.
        let changed = json!({ "MemoryCurrent": 12345 });
        assert!(!is_failed_transition(&changed));
    }

    #[test]
    fn failure_result_extracts_reason_when_present() {
        let changed = json!({ "ActiveState": "failed", "Result": "exit-code" });
        assert_eq!(failure_result(&changed), Some("exit-code".to_string()));
    }

    #[test]
    fn failure_result_is_none_when_absent() {
        let changed = json!({ "ActiveState": "failed" });
        assert_eq!(failure_result(&changed), None);
    }
}
