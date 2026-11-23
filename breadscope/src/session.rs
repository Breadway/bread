//! Persisted window preferences.
//!
//! Kept deliberately small and local: view, filter, source chip, ordering,
//! follow mode, time windows, and window size. Nothing about the daemon or any
//! module is stored here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub causal: Option<bool>,
    #[serde(default)]
    pub follow_tail: Option<bool>,
    #[serde(default)]
    pub window_minutes: Option<u64>,
    #[serde(default)]
    pub flow_window_minutes: Option<u64>,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
}

fn session_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("breadscope").join("session.json"))
}

pub fn load() -> Session {
    let Some(path) = session_path() else {
        return Session::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(session: &Session) {
    let Some(path) = session_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(text) = serde_json::to_string_pretty(session) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[test]
    fn session_round_trips_through_json() {
        let session = Session {
            view: Some("flow".into()),
            filter: Some("bread.device.**".into()),
            source: Some("udev".into()),
            causal: Some(false),
            follow_tail: Some(true),
            window_minutes: Some(5),
            flow_window_minutes: Some(15),
            width: Some(1400),
            height: Some(900),
        };
        let text = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(back.view.as_deref(), Some("flow"));
        assert_eq!(back.window_minutes, Some(5));
        assert_eq!(back.causal, Some(false));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let back: Session =
            serde_json::from_str(r#"{"view":"modules","future_field":42}"#).unwrap();
        assert_eq!(back.view.as_deref(), Some("modules"));
    }
}
