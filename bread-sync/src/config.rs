use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration stored in `~/.config/bread/sync.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub remote: RemoteConfig,
    pub machine: MachineConfig,
    #[serde(default)]
    pub packages: PackagesConfig,
    #[serde(default)]
    pub delegates: DelegatesConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub url: String,
    #[serde(default = "default_branch")]
    pub branch: String,
}

fn default_branch() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineConfig {
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagesConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub managers: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for PackagesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            managers: vec![
                "pacman".to_string(),
                "pip".to_string(),
                "npm".to_string(),
                "cargo".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DelegatesConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl SyncConfig {
    /// Load sync config from the given bread config directory.
    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = config_dir.join("sync.toml");
        let raw = fs::read_to_string(&path)
            .with_context(|| "bread: sync not initialized. Run: bread sync init".to_string())?;
        toml::from_str(&raw).context("failed to parse sync.toml")
    }

    /// Save sync config to the given bread config directory.
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        let path = config_dir.join("sync.toml");
        fs::create_dir_all(config_dir)?;
        let raw = toml::to_string_pretty(self).context("failed to serialize sync config")?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))
    }

    /// Returns the local sync repo path (`~/.local/share/bread/sync-repo/`).
    pub fn local_repo_path() -> PathBuf {
        if let Some(data_dir) = dirs::data_dir() {
            return data_dir.join("bread").join("sync-repo");
        }
        // Fallback using $HOME
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("bread")
                .join("sync-repo");
        }
        PathBuf::from(".local/share/bread/sync-repo")
    }
}

/// Returns the bread config directory (`~/.config/bread/`).
pub fn bread_config_dir() -> PathBuf {
    if let Some(cfg) = dirs::config_dir() {
        return cfg.join("bread");
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("bread");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("bread");
    }
    PathBuf::from(".config/bread")
}

/// Expand `~` to the home directory in a path string.
pub fn expand_path(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}
