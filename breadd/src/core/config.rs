use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub lua: LuaConfig,
    #[serde(default)]
    pub adapters: AdaptersConfig,
    #[serde(default)]
    pub events: EventsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub socket_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LuaConfig {
    #[serde(default = "default_lua_entry")]
    pub entry_point: String,
    #[serde(default = "default_lua_modules")]
    pub module_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdaptersConfig {
    #[serde(default)]
    pub hyprland: AdapterToggle,
    #[serde(default)]
    pub udev: UdevConfig,
    #[serde(default)]
    pub power: PowerConfig,
    #[serde(default)]
    pub network: AdapterToggle,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdapterToggle {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UdevConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_udev_subsystems")]
    pub subsystems: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PowerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventsConfig {
    #[serde(default = "default_dedup_window")]
    pub dedup_window_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            lua: LuaConfig::default(),
            adapters: AdaptersConfig::default(),
            events: EventsConfig::default(),
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
            socket_path: String::new(),
        }
    }
}

impl Default for LuaConfig {
    fn default() -> Self {
        Self {
            entry_point: default_lua_entry(),
            module_path: default_lua_modules(),
        }
    }
}

impl Default for AdaptersConfig {
    fn default() -> Self {
        Self {
            hyprland: AdapterToggle::default(),
            udev: UdevConfig::default(),
            power: PowerConfig::default(),
            network: AdapterToggle::default(),
        }
    }
}

impl Default for AdapterToggle {
    fn default() -> Self {
        Self {
            enabled: default_true(),
        }
    }
}

impl Default for UdevConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            subsystems: default_udev_subsystems(),
        }
    }
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            poll_interval_secs: default_poll_interval(),
        }
    }
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            dedup_window_ms: default_dedup_window(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(&path)?;
        let cfg: Config = toml::from_str(&raw)?;
        Ok(cfg)
    }

    pub fn socket_path(&self) -> PathBuf {
        if !self.daemon.socket_path.is_empty() {
            return expand_home(&self.daemon.socket_path);
        }

        let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        Path::new(&runtime_dir).join("bread").join("breadd.sock")
    }

    pub fn lua_entry_point(&self) -> PathBuf {
        expand_home(&self.lua.entry_point)
    }

    pub fn lua_module_path(&self) -> PathBuf {
        expand_home(&self.lua.module_path)
    }
}

fn config_path() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return Path::new(&xdg).join("bread").join("breadd.toml");
    }

    expand_home("~/.config/bread/breadd.toml")
}

fn expand_home(input: &str) -> PathBuf {
    if let Some(stripped) = input.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return Path::new(&home).join(stripped);
        }
    }
    PathBuf::from(input)
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_lua_entry() -> String {
    "~/.config/bread/init.lua".to_string()
}

fn default_lua_modules() -> String {
    "~/.config/bread/modules".to_string()
}

fn default_true() -> bool {
    true
}

fn default_poll_interval() -> u64 {
    30
}

fn default_dedup_window() -> u64 {
    100
}

fn default_udev_subsystems() -> Vec<String> {
    vec![
        "usb".to_string(),
        "input".to_string(),
        "drm".to_string(),
        "power_supply".to_string(),
    ]
}
