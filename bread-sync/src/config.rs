use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Top-level sync configuration stored in `~/.config/bread/sync.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub machine: MachineConfig,
    #[serde(default)]
    pub packages: PackagesConfig,
    #[serde(default)]
    pub delegates: DelegatesConfig,
}

/// Git remote configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteConfig {
    pub url: Option<String>,
    #[serde(default = "default_branch")]
    pub branch: String,
}

fn default_branch() -> String {
    "main".to_string()
}

/// Machine identity — name comes from here, falls back to hostname.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MachineConfig {
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Which package managers to snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagesConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_managers")]
    pub managers: Vec<String>,
}

impl Default for PackagesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            managers: default_managers(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_managers() -> Vec<String> {
    vec!["pacman".to_string(), "pip".to_string(), "npm".to_string()]
}

/// Config file delegation — which extra paths to include in the sync repo.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DelegatesConfig {
    /// Absolute or `~`-prefixed paths to copy into `configs/<basename>/`.
    #[serde(default)]
    pub include: Vec<String>,
    /// Glob patterns to exclude when copying.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl SyncConfig {
    /// Load from `~/.config/bread/sync.toml`, returning `Default` if not present.
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        let cfg: Self = toml::from_str(&raw)?;
        Ok(cfg)
    }

    /// Write to `~/.config/bread/sync.toml`, creating parent dirs as needed.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(&path, raw)?;
        Ok(())
    }

    /// Returns `true` if `~/.config/bread/sync.toml` exists on disk.
    pub fn is_initialized() -> Result<bool> {
        Ok(config_path()?.exists())
    }
}

/// Path to `~/.config/bread/sync.toml`.
pub fn config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine config directory"))?;
    Ok(config_dir.join("bread").join("sync.toml"))
}

/// Path to `~/.local/share/bread/sync-repo/`.
pub fn sync_repo_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine data directory"))?;
    Ok(data_dir.join("bread").join("sync-repo"))
}

/// Path to `~/.config/bread/`.
pub fn bread_config_dir() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine config directory"))?;
    Ok(config_dir.join("bread"))
}
