//! Shared types for the Bread automation fabric.
//!
//! This crate defines the canonical event types ([`RawEvent`], [`BreadEvent`])
//! and the [`AdapterSource`] enum that both the daemon (`breadd`) and CLI
//! (`bread-cli`) depend on. Keeping these types in a separate crate guarantees
//! that adapters, the state engine, IPC clients, and the Lua bindings all
//! agree on a single wire format.

use serde::{Deserialize, Serialize};

pub mod apps;
pub mod glob;
pub mod widget;

/// Identifies which adapter produced an event.
///
/// The state engine uses this to choose a normalization strategy and the
/// IPC layer surfaces it so subscribers can filter by origin.
///
/// Not `Copy`: the [`App`](AdapterSource::App) variant carries an owned
/// `String` (a sibling `bread*` app id), so callers that used to copy a
/// `AdapterSource` by value now `.clone()` it — see `breadd/src/core/normalizer.rs`
/// for the (small, compiler-driven) set of call sites this touches.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
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
    /// Shell precmd/preexec hooks (terminal command lifecycle, cwd changes).
    Terminal,
    /// Git hooks (commit/branch) and the in-daemon dirty-state poller.
    Git,
    /// Project-root file watches (via `notify`/inotify).
    Filesystem,
    /// systemd --user unit state, via the session D-Bus.
    Systemd,
    /// Podman container lifecycle, via `podman events`.
    Podman,
    /// SSH/remote session detection, via the shell hook.
    Remote,
    /// A sibling `bread*` application (breadclip, breadpad, ...), identified
    /// by its registered app id (see [`apps::KNOWN_APPS`]). Confined to the
    /// `bread.<app>.*` event namespace — enforced at the IPC boundary via
    /// [`apps::validate_app_namespace`], not by this type itself.
    App(String),
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
    /// Unique id for this specific event instance, assigned at construction.
    ///
    /// *Since: v1.5* — enables causality tracking (`caused_by`) across chains
    /// of Lua modules that re-emit events from inside `bread.on` handlers.
    pub id: String,
    /// The `id` of the event whose Lua handler emitted this event via
    /// `bread.emit()`, if any.
    ///
    /// `None` for events that originate outside any Lua handler invocation
    /// (adapter-normalized events, IPC `emit`, daemon-internal sends like
    /// `bread.system.startup` / `bread.profile.activated`). Populated only
    /// when the event was constructed by `bread.emit()` while a subscriber
    /// callback was synchronously running — see the "current dispatch id"
    /// mechanism on `breadd`'s Lua engine.
    ///
    /// *Since: v1.5*
    pub caused_by: Option<String>,
}

impl BreadEvent {
    /// Construct a new event with `timestamp` set to the current wall-clock,
    /// a freshly generated `id`, and `caused_by` unset.
    pub fn new(event: impl Into<String>, source: AdapterSource, data: serde_json::Value) -> Self {
        Self::with_timestamp(event, now_unix_ms(), source, data)
    }

    /// Construct a new event with an explicit `timestamp`, preserving the
    /// originating signal's observed time instead of "now". Used by the
    /// normalizer, which carries `RawEvent::timestamp` through unchanged.
    ///
    /// Like [`BreadEvent::new`], this always assigns a fresh `id` and leaves
    /// `caused_by` unset — callers that need to thread causality set
    /// `caused_by` on the returned value themselves.
    pub fn with_timestamp(
        event: impl Into<String>,
        timestamp: u64,
        source: AdapterSource,
        data: serde_json::Value,
    ) -> Self {
        Self {
            event: event.into(),
            timestamp,
            source,
            data,
            id: new_event_id(),
            caused_by: None,
        }
    }
}

/// Generate a fresh unique id for a [`BreadEvent`].
///
/// Every construction path (`BreadEvent::new`, `BreadEvent::with_timestamp`,
/// and any remaining struct-literal construction) calls this so every event
/// gets a stable identity to hang `caused_by` chains off of.
pub fn new_event_id() -> String {
    uuid::Uuid::new_v4().to_string()
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

#[derive(Deserialize, Default)]
struct DaemonSection {
    #[serde(default)]
    socket_path: String,
}

#[derive(Deserialize, Default)]
struct SocketPathConfig {
    #[serde(default)]
    daemon: DaemonSection,
}

/// Resolve breadd's Unix socket path exactly as `breadd::core::config::Config::socket_path`
/// resolves its own: an explicit `daemon.socket_path` in `~/.config/bread/breadd.toml` wins,
/// otherwise `$XDG_RUNTIME_DIR/bread/breadd.sock`, falling back to `/tmp/bread/breadd.sock`.
///
/// Shared by every socket client that lives outside the daemon itself (`bread-emit`, and
/// `bread-client` in `bread-ecosystem/bread-utils`) so they can't drift from how the daemon
/// actually resolves its own socket — before this existed, `bread-emit` carried its own
/// hand-rolled copy of this exact logic.
pub fn resolve_socket_path() -> std::path::PathBuf {
    if let Some(home) = dirs::home_dir() {
        let config_path = home.join(".config/bread/breadd.toml");
        if let Ok(contents) = std::fs::read_to_string(&config_path) {
            if let Ok(cfg) = toml::from_str::<SocketPathConfig>(&contents) {
                if !cfg.daemon.socket_path.is_empty() {
                    return expand_path(&cfg.daemon.socket_path);
                }
            }
        }
    }

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(runtime_dir)
        .join("bread")
        .join("breadd.sock")
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
        assert_eq!(
            serde_json::to_string(&AdapterSource::Terminal).unwrap(),
            "\"terminal\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Git).unwrap(),
            "\"git\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Filesystem).unwrap(),
            "\"filesystem\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Systemd).unwrap(),
            "\"systemd\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Podman).unwrap(),
            "\"podman\""
        );
        assert_eq!(
            serde_json::to_string(&AdapterSource::Remote).unwrap(),
            "\"remote\""
        );
    }

    #[test]
    fn adapter_source_app_serializes_as_externally_tagged_object() {
        assert_eq!(
            serde_json::to_string(&AdapterSource::App("clip".to_string())).unwrap(),
            "{\"app\":\"clip\"}"
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
            AdapterSource::Terminal,
            AdapterSource::Git,
            AdapterSource::Filesystem,
            AdapterSource::Systemd,
            AdapterSource::Podman,
            AdapterSource::Remote,
            AdapterSource::App("clip".to_string()),
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
            id: "test-id-1".to_string(),
            caused_by: Some("test-id-0".to_string()),
        };
        let raw = serde_json::to_string(&original).unwrap();
        let decoded: BreadEvent = serde_json::from_str(&raw).unwrap();

        assert_eq!(decoded.event, original.event);
        assert_eq!(decoded.timestamp, original.timestamp);
        assert_eq!(decoded.source, original.source);
        assert_eq!(decoded.data, original.data);
        assert_eq!(decoded.id, original.id);
        assert_eq!(decoded.caused_by, original.caused_by);
    }

    #[test]
    fn bread_event_new_assigns_unique_id_and_no_cause() {
        let a = BreadEvent::new("bread.test.a", AdapterSource::System, json!({}));
        let b = BreadEvent::new("bread.test.b", AdapterSource::System, json!({}));
        assert!(!a.id.is_empty());
        assert_ne!(a.id, b.id, "each constructed event should get a unique id");
        assert_eq!(a.caused_by, None);
    }

    #[test]
    fn bread_event_with_timestamp_preserves_timestamp_and_assigns_id() {
        let event = BreadEvent::with_timestamp(
            "bread.test.c",
            42,
            AdapterSource::Udev,
            json!({ "x": 1 }),
        );
        assert_eq!(event.timestamp, 42);
        assert!(!event.id.is_empty());
        assert_eq!(event.caused_by, None);
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
