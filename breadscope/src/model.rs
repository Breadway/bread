//! Typed mirrors of the breadd IPC payloads breadscope consumes.
//!
//! Kept deliberately tolerant (`#[serde(default)]` everywhere) so a daemon
//! that adds a field or a module with an older shape never blanks a whole
//! view. The one type not redefined here is `BreadEvent`, which comes straight
//! from `bread-shared` so the GUI sees exactly the daemon's wire shape,
//! including `id`, `source`, and `caused_by`.

use std::collections::HashMap;

use bread_shared::{AdapterSource, BreadEvent};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub api_version: String,
    #[serde(default)]
    pub uptime_ms: u64,
    #[serde(default)]
    pub socket: String,
    #[serde(default)]
    pub adapters: HashMap<String, String>,
    #[serde(default)]
    pub subscriptions: u64,
    #[serde(default)]
    pub recent_errors: Vec<ErrorEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ErrorEntry {
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Module {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub ungated: bool,
    #[serde(default)]
    pub store: HashMap<String, Value>,
}

impl Module {
    pub fn is_degraded(&self) -> bool {
        matches!(self.status.as_str(), "degraded" | "load_error")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionEntry {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub once: bool,
    #[serde(default)]
    pub filtered: bool,
    #[serde(default)]
    pub raw_kind: Option<String>,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub last_event: Option<String>,
    #[serde(default)]
    pub last_at: Option<u64>,
}

impl SubscriptionEntry {
    pub fn module_label(&self) -> &str {
        self.module.as_deref().unwrap_or("init.lua")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TraceEntry {
    #[serde(default)]
    pub event_id: String,
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub duration_us: u64,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub at: u64,
}

impl TraceEntry {
    pub fn module_label(&self) -> &str {
        self.module.as_deref().unwrap_or("init.lua")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Workflow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WidgetSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub module: String,
    #[serde(default)]
    pub placement: String,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub root: Value,
}

/// One request/reply snapshot assembled from several IPC methods.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub daemon_up: bool,
    pub health: Option<Health>,
    pub modules: Vec<Module>,
    pub subscriptions: Vec<SubscriptionEntry>,
    pub traces: Vec<TraceEntry>,
    pub state: Value,
    pub workflows: Vec<Workflow>,
    pub widgets: Vec<WidgetSummary>,
}

// ---------------------------------------------------------------------------
// presentation helpers shared by every view
// ---------------------------------------------------------------------------

pub fn source_key(source: &AdapterSource) -> String {
    match source {
        AdapterSource::Hyprland => "hyprland".into(),
        AdapterSource::Udev => "udev".into(),
        AdapterSource::Power => "power".into(),
        AdapterSource::Network => "network".into(),
        AdapterSource::System => "system".into(),
        AdapterSource::Manual => "manual".into(),
        AdapterSource::Bluetooth => "bluetooth".into(),
        AdapterSource::Terminal => "terminal".into(),
        AdapterSource::Git => "git".into(),
        AdapterSource::Filesystem => "filesystem".into(),
        AdapterSource::Systemd => "systemd".into(),
        AdapterSource::Podman => "podman".into(),
        AdapterSource::Remote => "remote".into(),
        AdapterSource::App(app) => format!("app:{app}"),
    }
}

pub fn source_label(source: &AdapterSource) -> String {
    match source {
        AdapterSource::Hyprland => "Hyprland".into(),
        AdapterSource::Udev => "Udev".into(),
        AdapterSource::Power => "Power".into(),
        AdapterSource::Network => "Network".into(),
        AdapterSource::System => "System".into(),
        AdapterSource::Manual => "Manual".into(),
        AdapterSource::Bluetooth => "Bluetooth".into(),
        AdapterSource::Terminal => "Terminal".into(),
        AdapterSource::Git => "Git".into(),
        AdapterSource::Filesystem => "Filesystem".into(),
        AdapterSource::Systemd => "Systemd".into(),
        AdapterSource::Podman => "Podman".into(),
        AdapterSource::Remote => "Remote".into(),
        AdapterSource::App(app) => format!("App({app})"),
    }
}

/// Stable accent per adapter source, matching the mockup's color language.
pub fn source_color(source: &AdapterSource) -> &'static str {
    match source {
        AdapterSource::Udev => "#61afef",
        AdapterSource::Hyprland => "#c678dd",
        AdapterSource::Power => "#e5c07b",
        AdapterSource::Network => "#56b6c2",
        AdapterSource::Bluetooth => "#7aa2f7",
        AdapterSource::System => "#98c379",
        AdapterSource::Manual => "#8d8d8d",
        AdapterSource::App(_) => "#d9a05b",
        _ => "#8d8d8d",
    }
}

/// The top-level event family after `bread.`, used as the middle column of the
/// interaction map. Custom events with no `bread.` prefix group under their
/// first segment.
pub fn event_family(event: &str) -> String {
    let mut segments = event.split('.');
    match segments.next() {
        Some("bread") => segments.next().unwrap_or("other").to_string(),
        Some(first) => first.to_string(),
        None => "other".to_string(),
    }
}

pub fn event_summary(event: &BreadEvent) -> String {
    let Some(object) = event.data.as_object() else {
        return if event.data.is_null() {
            "no payload".to_string()
        } else {
            event.data.to_string()
        };
    };
    if object.is_empty() {
        return "no payload".to_string();
    }
    object
        .iter()
        .take(3)
        .map(|(key, value)| {
            let text = if value.is_string() {
                value.as_str().unwrap_or_default().to_string()
            } else {
                value.to_string()
            };
            format!("{key}={text}")
        })
        .collect::<Vec<_>>()
        .join("  ")
}

pub fn matches_pattern(pattern: &str, name: &str) -> bool {
    bread_shared::glob::matches_pattern(pattern, name)
}

pub fn uptime_label(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

pub fn timestamp_label(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(time) => time.format("%H:%M:%S%.3f").to_string(),
        None => "--:--:--".to_string(),
    }
}

pub fn date_timestamp_label(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(time) => time.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        None => "unknown".to_string(),
    }
}

pub fn age_label(ms: u64) -> String {
    let now = bread_shared::now_unix_ms();
    let seconds = now.saturating_sub(ms) / 1000;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else {
        format!("{}h ago", seconds / 3600)
    }
}

pub fn short_id(id: &str) -> String {
    if id.len() > 10 {
        format!("{}·{}", &id[..4], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bread_shared::{AdapterSource, BreadEvent};

    fn event(name: &str, data: serde_json::Value) -> BreadEvent {
        BreadEvent::with_timestamp(name, 1_700_000_000_000, AdapterSource::Udev, data)
    }

    #[test]
    fn glob_matches_one_segment_and_any_depth() {
        assert!(matches_pattern(
            "bread.monitor.connected",
            "bread.monitor.connected"
        ));
        assert!(matches_pattern(
            "bread.monitor.*",
            "bread.monitor.connected"
        ));
        assert!(!matches_pattern(
            "bread.monitor.*",
            "bread.monitor.layout.applied"
        ));
        assert!(matches_pattern(
            "bread.monitor.**",
            "bread.monitor.layout.applied"
        ));
        assert!(matches_pattern(
            "bread.device.?.connected",
            "bread.device.a.connected"
        ));
        assert!(!matches_pattern(
            "bread.device.?.connected",
            "bread.device.ab.connected"
        ));
    }

    #[test]
    fn event_family_uses_the_domain_after_bread() {
        assert_eq!(event_family("bread.monitor.connected"), "monitor");
        assert_eq!(event_family("bread.command.bar.refresh"), "command");
        assert_eq!(event_family("custom.ping"), "custom");
        assert_eq!(event_family("single"), "single");
    }

    #[test]
    fn summary_shows_the_first_three_fields() {
        let summary = event_summary(&event(
            "bread.test",
            serde_json::json!({"alpha": "dock", "beta": 2, "gamma": true, "ignored": 4}),
        ));
        assert!(summary.contains("alpha=dock"));
        assert!(summary.contains("beta=2"));
        assert!(summary.contains("gamma=true"));
        assert!(!summary.contains("ignored"));
    }

    #[test]
    fn empty_payload_is_named_explicitly() {
        assert_eq!(
            event_summary(&event("bread.system.startup", serde_json::json!({}))),
            "no payload"
        );
    }

    #[test]
    fn source_keys_distinguish_apps_from_adapters() {
        assert_eq!(source_key(&AdapterSource::Udev), "udev");
        assert_eq!(source_key(&AdapterSource::App("bar".into())), "app:bar");
        assert_eq!(source_label(&AdapterSource::App("bar".into())), "App(bar)");
    }

    #[test]
    fn short_id_keeps_both_ends() {
        assert_eq!(
            short_id("c41d8e77-2a90-4f0e-8c33-19b6a2d4e201"),
            "c41d·e201"
        );
        assert_eq!(short_id("tiny"), "tiny");
    }
}
