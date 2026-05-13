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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_config() -> SyncConfig {
        SyncConfig {
            remote: RemoteConfig {
                url: "git@github.com:user/repo.git".to_string(),
                branch: "main".to_string(),
            },
            machine: MachineConfig {
                name: "host".to_string(),
                tags: vec!["mobile".to_string()],
            },
            packages: PackagesConfig::default(),
            delegates: DelegatesConfig::default(),
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let cfg = sample_config();
        cfg.save(tmp.path()).unwrap();

        assert!(tmp.path().join("sync.toml").exists());

        let loaded = SyncConfig::load(tmp.path()).unwrap();
        assert_eq!(loaded.remote.url, cfg.remote.url);
        assert_eq!(loaded.remote.branch, cfg.remote.branch);
        assert_eq!(loaded.machine.name, cfg.machine.name);
        assert_eq!(loaded.machine.tags, cfg.machine.tags);
    }

    #[test]
    fn load_missing_config_returns_helpful_error() {
        let tmp = TempDir::new().unwrap();
        let err = SyncConfig::load(tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("sync not initialized") || msg.contains("bread sync init"),
            "expected init hint, got: {msg}",
        );
    }

    #[test]
    fn load_invalid_toml_returns_parse_error() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("sync.toml"), "this is not [valid toml").unwrap();
        let err = SyncConfig::load(tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.to_lowercase().contains("parse"), "got: {msg}");
    }

    #[test]
    fn packages_config_default_includes_all_managers() {
        let cfg = PackagesConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.managers.contains(&"pacman".to_string()));
        assert!(cfg.managers.contains(&"pip".to_string()));
        assert!(cfg.managers.contains(&"npm".to_string()));
        assert!(cfg.managers.contains(&"cargo".to_string()));
    }

    #[test]
    fn remote_branch_defaults_to_main_when_omitted() {
        let raw = r#"
[remote]
url = "git@example.com:r.git"

[machine]
name = "host"
"#;
        let cfg: SyncConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.remote.branch, "main");
    }

    #[test]
    fn delegates_default_is_empty() {
        let cfg = DelegatesConfig::default();
        assert!(cfg.include.is_empty());
        assert!(cfg.exclude.is_empty());
    }

    #[test]
    fn local_repo_path_resolves_to_data_dir() {
        let path = SyncConfig::local_repo_path();
        // Must include the bread sync-repo segment at the end.
        let suffix = path.iter().rev().take(2).collect::<Vec<_>>();
        assert_eq!(
            suffix,
            vec![
                std::ffi::OsStr::new("sync-repo"),
                std::ffi::OsStr::new("bread")
            ]
        );
    }

    #[test]
    fn expand_path_passes_through_absolute_paths() {
        assert_eq!(expand_path("/etc/bread"), PathBuf::from("/etc/bread"));
        assert_eq!(expand_path("relative/path"), PathBuf::from("relative/path"));
    }

    #[test]
    fn expand_path_expands_tilde_alone_to_home() {
        let home = dirs::home_dir().or_else(|| std::env::var("HOME").ok().map(PathBuf::from));
        if let Some(home) = home {
            assert_eq!(expand_path("~"), home);
        }
    }

    #[test]
    fn expand_path_expands_tilde_prefix() {
        let home = dirs::home_dir().or_else(|| std::env::var("HOME").ok().map(PathBuf::from));
        if let Some(home) = home {
            assert_eq!(expand_path("~/.config"), home.join(".config"));
        }
    }
}
