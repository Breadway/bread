//! breadbug configuration and well-known paths.
//!
//! One small TOML file, `~/.config/bread/breadbug.toml`, shared by breadd
//! (which reads the exclusion list and store limits), the `breadbug` binary
//! (upload toggle, endpoint) and bos-settings (which edits it). Every key is
//! optional; a missing file means defaults, and the default is "never upload".

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default intake endpoint. Overridable in the config file.
pub const DEFAULT_ENDPOINT: &str = "https://bugs.breadway.dev";

/// Bounds on the on-disk store. All sizes are in bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StoreLimits {
    /// Size at which one log/event/marker file rotates to `<file>.1`.
    pub max_bytes_per_file: u64,
    /// Cap across the whole store; oldest data is dropped first.
    pub max_total_bytes: u64,
    /// Most distinct app directories the store will keep.
    pub max_apps: usize,
    /// A single stored line (log message, marker text) is cut to this.
    pub max_line_bytes: usize,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_bytes_per_file: 256 * 1024,
            max_total_bytes: 32 * 1024 * 1024,
            max_apps: 64,
            max_line_bytes: 4096,
        }
    }
}

/// The contents of `breadbug.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BugConfig {
    /// Master switch for sending reports. Off by default; saving a report
    /// to a file is always available regardless.
    pub upload_enabled: bool,
    /// Apps that never have logs stored or appear in reports.
    pub excluded_apps: Vec<String>,
    /// Base URL of the intake server.
    pub endpoint: String,
    /// Local store bounds.
    pub store: StoreLimits,
    /// Tell me (desktop notification) when a report I sent is fixed and an
    /// update is available. Opt-in; only receipts I hold are polled.
    pub notify_fixed: bool,
    /// Whether breadd watches app heartbeats for freezes.
    pub watchdog_enabled: bool,
    /// An app that sends heartbeats and goes silent for this long (and
    /// longer than four of its own intervals) is considered frozen.
    pub freeze_after_ms: u64,
}

impl Default for BugConfig {
    fn default() -> Self {
        Self {
            upload_enabled: false,
            excluded_apps: Vec::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            store: StoreLimits::default(),
            notify_fixed: false,
            watchdog_enabled: true,
            freeze_after_ms: 15_000,
        }
    }
}

impl BugConfig {
    /// Load from the default path; a missing file yields defaults.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Write atomically (temp file in the same directory, then rename).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("config path has no parent")?;
        fs::create_dir_all(dir)?;
        let tmp = dir.join(".breadbug.toml.tmp");
        let mut f = fs::File::create(&tmp)?;
        f.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn is_excluded(&self, app: &str) -> bool {
        self.excluded_apps.iter().any(|a| a == app)
    }
}

/// `$XDG_CONFIG_HOME/bread/breadbug.toml`.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("bread")
        .join("breadbug.toml")
}

/// `$XDG_STATE_HOME/bread/bugstore`, the on-disk store root.
pub fn store_root() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("bread")
        .join("bugstore")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_defaults_and_never_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BugConfig::load_from(&dir.path().join("nope.toml")).unwrap();
        assert!(!cfg.upload_enabled);
        assert_eq!(cfg, BugConfig::default());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.toml");
        fs::write(&p, "excluded_apps = [\"clip\"]\n[store]\nmax_apps = 3\n").unwrap();
        let cfg = BugConfig::load_from(&p).unwrap();
        assert!(cfg.is_excluded("clip"));
        assert_eq!(cfg.store.max_apps, 3);
        assert_eq!(
            cfg.store.max_total_bytes,
            StoreLimits::default().max_total_bytes
        );
        assert!(!cfg.upload_enabled);
    }

    #[test]
    fn save_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/b.toml");
        let cfg = BugConfig {
            upload_enabled: true,
            excluded_apps: vec!["search".into()],
            ..BugConfig::default()
        };
        cfg.save_to(&p).unwrap();
        assert_eq!(BugConfig::load_from(&p).unwrap(), cfg);
    }

    #[test]
    fn garbage_is_an_error_not_silent_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.toml");
        fs::write(&p, "upload_enabled = [[[").unwrap();
        assert!(BugConfig::load_from(&p).is_err());
    }
}
