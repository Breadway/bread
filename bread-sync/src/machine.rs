use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::SyncConfig;

/// Machine profile persisted to `<repo>/machines/<name>.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineProfile {
    pub name: String,
    pub hostname: String,
    pub tags: Vec<String>,
    pub last_sync: String, // RFC 3339
}

impl MachineProfile {
    pub fn new(cfg: &SyncConfig) -> Result<Self> {
        let host = hostname()?;
        let name = cfg.machine.name.clone().unwrap_or_else(|| host.clone());
        Ok(Self {
            name,
            hostname: host,
            tags: cfg.machine.tags.clone(),
            last_sync: Utc::now().to_rfc3339(),
        })
    }

    /// Write profile to `<repo>/machines/<name>.toml`.
    pub fn write_to_repo(&self, repo_root: &Path) -> Result<()> {
        let machines_dir = repo_root.join("machines");
        std::fs::create_dir_all(&machines_dir)?;
        let path = machines_dir.join(format!("{}.toml", self.name));
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(&path, raw)?;
        Ok(())
    }

    /// Load from `<repo>/machines/<name>.toml`.
    pub fn load_from_repo(repo_root: &Path, name: &str) -> Result<Self> {
        let path = repo_root.join("machines").join(format!("{name}.toml"));
        let raw = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&raw)?)
    }
}

/// List all machine profiles in `<repo>/machines/`.
pub fn list_machines(repo_root: &Path) -> Vec<MachineProfile> {
    let machines_dir = repo_root.join("machines");
    let Ok(entries) = std::fs::read_dir(&machines_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
        .filter_map(|e| {
            std::fs::read_to_string(e.path())
                .ok()
                .and_then(|raw| toml::from_str::<MachineProfile>(&raw).ok())
        })
        .collect()
}

/// Returns the machine name from sync.toml, falling back to hostname.
pub fn machine_name(cfg: &SyncConfig) -> Result<String> {
    if let Some(name) = cfg.machine.name.as_deref() {
        return Ok(name.to_string());
    }
    hostname()
}

/// Returns the machine tags from sync.toml.
pub fn machine_tags(cfg: &SyncConfig) -> Vec<String> {
    cfg.machine.tags.clone()
}

/// Returns true if `tag` is in the machine's tag list.
pub fn machine_has_tag(cfg: &SyncConfig, tag: &str) -> bool {
    cfg.machine.tags.iter().any(|t| t == tag)
}

fn hostname() -> Result<String> {
    // Try /etc/hostname first (no subprocess)
    if let Ok(raw) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    // Fall back to hostname(1)
    let out = std::process::Command::new("hostname")
        .output()
        .map_err(anyhow::Error::from)?;
    let s = String::from_utf8(out.stdout).map_err(anyhow::Error::from)?;
    Ok(s.trim().to_string())
}

#[allow(dead_code)]
fn format_last_sync(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M").to_string()
}
