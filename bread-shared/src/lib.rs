//! Shared types for the Bread automation fabric.
//!
//! This crate defines the canonical event types ([`RawEvent`], [`BreadEvent`])
//! and the [`AdapterSource`] enum that both the daemon (`breadd`) and CLI
//! (`bread-cli`) depend on. Keeping these types in a separate crate guarantees
//! that adapters, the state engine, IPC clients, and the Lua bindings all
//! agree on a single wire format.

use serde::{Deserialize, Serialize};

/// Identifies which adapter produced an event.
///
/// The state engine uses this to choose a normalization strategy and the
/// IPC layer surfaces it so subscribers can filter by origin.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AdapterSource {
    /// The Hyprland compositor IPC socket.
    Hyprland,
    /// The Linux udev / netlink subsystem.
    Udev,
    /// Power management (sysfs / UPower).
    Power,
    /// Network state (rtnetlink / NetworkManager).
    Network,
    /// Internal events synthesized by the daemon itself
    /// (e.g. `bread.profile.activated`, `bread.state.changed.*`).
    System,
    /// BlueZ Bluetooth stack via D-Bus.
    Bluetooth,
}

/// An unnormalized event as emitted by an adapter.
///
/// Raw events carry the adapter's native payload verbatim. The
/// [`EventNormalizer`](../breadd/core/normalizer/struct.EventNormalizer.html)
/// in `breadd` transforms `RawEvent` into one or more [`BreadEvent`]s with
/// a semantic name and structured data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    /// Which adapter produced this event.
    pub source: AdapterSource,
    /// Adapter-specific event kind (e.g. `"workspace"`, `"add"`, `"battery"`).
    pub kind: String,
    /// Adapter-specific JSON payload — not stable across versions.
    pub payload: serde_json::Value,
    /// Unix epoch milliseconds when the event was observed.
    pub timestamp: u64,
}

/// A normalized event ready for dispatch to Lua subscribers and IPC consumers.
///
/// `BreadEvent` is the public, stable contract: event names use a dotted
/// namespace (e.g. `bread.device.dock.connected`) and the `data` payload
/// follows a documented shape per event family. See `Documentation.md` for
/// the full event catalogue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreadEvent {
    /// Dotted event name, e.g. `bread.workspace.changed`.
    pub event: String,
    /// Unix epoch milliseconds when the originating signal was observed.
    pub timestamp: u64,
    /// The adapter that produced the underlying raw event.
    pub source: AdapterSource,
    /// Structured event data. The shape depends on the event family.
    pub data: serde_json::Value,
}

impl BreadEvent {
    /// Construct a new event with `timestamp` set to the current wall-clock.
    pub fn new(event: impl Into<String>, source: AdapterSource, data: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            timestamp: now_unix_ms(),
            source,
            data,
        }
    }
}

/// Current Unix epoch in milliseconds.
///
/// Falls back to `0` if the system clock is before the epoch, which keeps
/// callers infallible. Used for `BreadEvent::timestamp` and replay cutoffs.
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Expand a leading `~` or `~/` in a path string to the user's home directory.
///
/// Falls back to returning the path unchanged if `$HOME` is unset, which keeps
/// callers infallible. Shared by the daemon and CLI for resolving
/// user-supplied paths (config entries, module install sources).
pub fn expand_path(path: &str) -> std::path::PathBuf {
    use std::path::PathBuf;
    let home = std::env::var("HOME").ok();
    if path == "~" {
        if let Some(home) = home {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expand_path_leaves_non_tilde_paths_unchanged() {
        use std::path::PathBuf;
        assert_eq!(expand_path("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_path("relative/x"), PathBuf::from("relative/x"));
        assert_eq!(expand_path("./x"), PathBuf::from("./x"));
        // A `~` not in leading position is not special.
        assert_eq!(expand_path("/etc/~weird"), PathBuf::from("/etc/~weird"));
    }

    #[test]
    fn expand_path_expands_leading_tilde() {
        // Read-only env access; safe under parallel test execution.
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(expand_path("~"), std::path::PathBuf::from(&home));
            assert_eq!(
                expand_path("~/.config/bread"),
                std::path::PathBuf::from(&home).join(".config/bread")
            );
        }
    }

    #[test]
    fn adapter_source_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&AdapterSource::Hyprland).unwrap(),
            "\"hyprland\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Udev).unwrap(),
            "\"udev\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Power).unwrap(),
            "\"power\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Network).unwrap(),
            "\"network\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::System).unwrap(),
            "\"system\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Bluetooth).unwrap(),
            "\"bluetooth\""
        );
    }

    #[test]
    fn adapter_source_round_trips_through_json() {
        for source in [
            AdapterSource::Hyprland,
            AdapterSource::Udev,
            AdapterSource::Power,
            AdapterSource::Network,
            AdapterSource::System,
            AdapterSource::Bluetooth,
        ] {
            let s = serde_json::to_string(&source).unwrap();
            let back: AdapterSource = serde_json::from_str(&s).unwrap();
            assert_eq!(source, back);
        }
    }

    #[test]
    fn adapter_source_rejects_unknown_variant() {
        let result: Result<AdapterSource, _> = serde_json::from_str("\"floppy\"");
        assert!(result.is_err());
    }

    #[test]
    fn bread_event_new_sets_current_timestamp() {
        let before = now_unix_ms();
        let event = BreadEvent::new("bread.test", AdapterSource::System, json!({}));
        let after = now_unix_ms();

        assert!(event.timestamp >= before);
        assert!(event.timestamp <= after);
        assert_eq!(event.event, "bread.test");
        assert_eq!(event.source, AdapterSource::System);
    }

    #[test]
    fn bread_event_new_accepts_owned_and_borrowed_names() {
        let owned = BreadEvent::new(String::from("bread.a"), AdapterSource::System, json!(null));
        let borrowed = BreadEvent::new("bread.b", AdapterSource::System, json!(null));
        assert_eq!(owned.event, "bread.a");
        assert_eq!(borrowed.event, "bread.b");
    }

    #[test]
    fn bread_event_round_trips_through_json() {
        let original = BreadEvent {
            event: "bread.device.connected".to_string(),
            timestamp: 1_700_000_000_000,
            source: AdapterSource::Udev,
            data: json!({ "id": "usb-1-1.4", "name": "Logitech" }),
        };
        let raw = serde_json::to_string(&original).unwrap();
        let decoded: BreadEvent = serde_json::from_str(&raw).unwrap();

        assert_eq!(decoded.event, original.event);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.source, original.source);
        assert_eq!(decoded.data, original.data);
    }

    #[test]
    fn raw_event_round_trips_through_json() {
        let original = RawEvent {
            source: AdapterSource::Hyprland,
            kind: "workspace".to_string(),
            payload: json!({ "data": "2" }),
            timestamp: 42,
        };
        let raw = serde_json::to_string(&original).unwrap();
        let decoded: RawEvent = serde_json::from_str(&raw).unwrap();

        assert_eq!(decoded.kind, original.kind);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.source, original.source);
        assert_eq!(decoded.payload, original.payload);
    }

    #[test]
    fn now_unix_ms_is_monotonically_non_decreasing_across_calls() {
        let a = now_unix_ms();
        let b = now_unix_ms();
        assert!(b >= a, "now_unix_ms went backwards: {a} -> {b}");
    }

    #[test]
    fn adapter_source_is_hashable_and_eq() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(AdapterSource::Hyprland);
        set.insert(AdapterSource::Hyprland);
        set.insert(AdapterSource::Udev);
        set.insert(AdapterSource::Bluetooth);
        assert_eq!(set.len(), 3);
        assert!(set.contains(&AdapterSource::Hyprland));
    }
}
