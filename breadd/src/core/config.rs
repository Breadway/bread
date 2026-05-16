use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub lua: LuaConfig,
    #[serde(default)]
    pub modules: ModulesConfig,
    #[serde(default)]
    pub adapters: AdaptersConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
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
pub struct ModulesConfig {
    #[serde(default = "default_true")]
    pub builtin: bool,
    #[serde(default)]
    pub disable: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AdaptersConfig {
    #[serde(default)]
    pub hyprland: AdapterToggle,
    #[serde(default)]
    pub udev: UdevConfig,
    #[serde(default)]
    pub power: PowerConfig,
    #[serde(default)]
    pub network: AdapterToggle,
    #[serde(default)]
    pub bluetooth: AdapterToggle,
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

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationsConfig {
    #[serde(default = "default_notify_timeout")]
    pub default_timeout_ms: i64,
    #[serde(default = "default_notify_urgency")]
    pub default_urgency: String,
    #[serde(default = "default_notify_path")]
    pub notify_send_path: String,
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

impl Default for ModulesConfig {
    fn default() -> Self {
        Self {
            builtin: default_true(),
            disable: Vec::new(),
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

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            default_timeout_ms: default_notify_timeout(),
            default_urgency: default_notify_urgency(),
            notify_send_path: default_notify_path(),
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

fn default_notify_timeout() -> i64 {
    3000
}

fn default_notify_urgency() -> String {
    "normal".to_string()
}

fn default_notify_path() -> String {
    "notify-send".to_string()
}

fn default_udev_subsystems() -> Vec<String> {
    vec![
        "usb".to_string(),
        "input".to_string(),
        "drm".to_string(),
        "power_supply".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests that mutate process env vars must serialize against each other
    // — cargo runs tests in parallel by default and HOME/XDG_RUNTIME_DIR are
    // process-global. Tests that don't touch env are free to run unguarded.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(vars: &[&'static str]) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let saved = vars.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            Self {
                saved,
                _guard: guard,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn default_config_uses_documented_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.daemon.log_level, "info");
        assert!(cfg.daemon.socket_path.is_empty());
        assert_eq!(cfg.lua.entry_point, "~/.config/bread/init.lua");
        assert_eq!(cfg.lua.module_path, "~/.config/bread/modules");
        assert!(cfg.adapters.hyprland.enabled);
        assert!(cfg.adapters.udev.enabled);
        assert!(cfg.adapters.power.enabled);
        assert!(cfg.adapters.network.enabled);
        assert!(cfg.adapters.bluetooth.enabled);
        assert_eq!(cfg.adapters.power.poll_interval_secs, 30);
        assert_eq!(cfg.events.dedup_window_ms, 100);
        assert_eq!(cfg.notifications.default_timeout_ms, 3000);
        assert_eq!(cfg.notifications.default_urgency, "normal");
        assert_eq!(cfg.notifications.notify_send_path, "notify-send");
        assert!(cfg.modules.builtin);
        assert!(cfg.modules.disable.is_empty());
    }

    #[test]
    fn default_udev_subsystems_match_documented_list() {
        assert_eq!(
            default_udev_subsystems(),
            vec!["usb", "input", "drm", "power_supply"]
        );
    }

    #[test]
    fn parse_empty_toml_yields_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.daemon.log_level, "info");
        assert!(cfg.adapters.hyprland.enabled);
    }

    #[test]
    fn parse_full_toml_overrides_all_values() {
        let raw = r#"
[daemon]
log_level = "debug"
socket_path = "/tmp/custom.sock"

[lua]
entry_point = "/abs/init.lua"
module_path = "/abs/mods"

[modules]
builtin = false
disable = ["foo", "bar"]

[adapters.hyprland]
enabled = false

[adapters.udev]
enabled = true
subsystems = ["usb"]

[adapters.power]
enabled = false
poll_interval_secs = 5

[adapters.network]
enabled = false

[adapters.bluetooth]
enabled = false

[events]
dedup_window_ms = 250

[notifications]
default_timeout_ms = 1000
default_urgency = "critical"
notify_send_path = "/usr/local/bin/notify-send"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.daemon.log_level, "debug");
        assert_eq!(cfg.daemon.socket_path, "/tmp/custom.sock");
        assert_eq!(cfg.lua.entry_point, "/abs/init.lua");
        assert_eq!(cfg.lua.module_path, "/abs/mods");
        assert!(!cfg.modules.builtin);
        assert_eq!(cfg.modules.disable, vec!["foo", "bar"]);
        assert!(!cfg.adapters.hyprland.enabled);
        assert!(cfg.adapters.udev.enabled);
        assert_eq!(cfg.adapters.udev.subsystems, vec!["usb"]);
        assert!(!cfg.adapters.power.enabled);
        assert_eq!(cfg.adapters.power.poll_interval_secs, 5);
        assert!(!cfg.adapters.network.enabled);
        assert!(!cfg.adapters.bluetooth.enabled);
        assert_eq!(cfg.events.dedup_window_ms, 250);
        assert_eq!(cfg.notifications.default_timeout_ms, 1000);
        assert_eq!(cfg.notifications.default_urgency, "critical");
    }

    #[test]
    fn parse_partial_toml_fills_missing_with_defaults() {
        let raw = r#"
[daemon]
log_level = "trace"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.daemon.log_level, "trace");
        // Untouched sections still get their defaults.
        assert!(cfg.adapters.hyprland.enabled);
        assert_eq!(cfg.events.dedup_window_ms, 100);
    }

    #[test]
    fn invalid_toml_returns_error() {
        let result: Result<Config, _> = toml::from_str("[daemon\nbroken");
        assert!(result.is_err());
    }

    #[test]
    fn socket_path_uses_explicit_path_verbatim() {
        let mut cfg = Config::default();
        cfg.daemon.socket_path = "/run/bread.sock".to_string();
        assert_eq!(cfg.socket_path(), PathBuf::from("/run/bread.sock"));
    }

    #[test]
    fn socket_path_expands_tilde_when_explicit() {
        let _g = EnvGuard::new(&["HOME"]);
        std::env::set_var("HOME", "/synthetic/home");
        let mut cfg = Config::default();
        cfg.daemon.socket_path = "~/sockets/bread.sock".to_string();
        assert_eq!(
            cfg.socket_path(),
            PathBuf::from("/synthetic/home/sockets/bread.sock")
        );
    }

    #[test]
    fn socket_path_falls_back_to_xdg_runtime_dir() {
        let _g = EnvGuard::new(&["XDG_RUNTIME_DIR"]);
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp/xdg");
        let cfg = Config::default();
        assert_eq!(
            cfg.socket_path(),
            PathBuf::from("/tmp/xdg/bread/breadd.sock")
        );
    }

    #[test]
    fn socket_path_uses_tmp_when_no_xdg_runtime_dir() {
        let _g = EnvGuard::new(&["XDG_RUNTIME_DIR"]);
        std::env::remove_var("XDG_RUNTIME_DIR");
        let cfg = Config::default();
        assert_eq!(cfg.socket_path(), PathBuf::from("/tmp/bread/breadd.sock"));
    }

    #[test]
    fn lua_entry_point_and_module_path_expand_tilde() {
        let _g = EnvGuard::new(&["HOME"]);
        std::env::set_var("HOME", "/synthetic/home");
        let cfg = Config::default();
        assert_eq!(
            cfg.lua_entry_point(),
            PathBuf::from("/synthetic/home/.config/bread/init.lua")
        );
        assert_eq!(
            cfg.lua_module_path(),
            PathBuf::from("/synthetic/home/.config/bread/modules")
        );
    }

    #[test]
    fn lua_entry_point_returns_absolute_path_unchanged() {
        let mut cfg = Config::default();
        cfg.lua.entry_point = "/etc/bread/init.lua".to_string();
        assert_eq!(cfg.lua_entry_point(), PathBuf::from("/etc/bread/init.lua"));
    }

    #[test]
    fn expand_home_handles_missing_home_env() {
        let _g = EnvGuard::new(&["HOME"]);
        std::env::remove_var("HOME");
        // Without HOME, ~/-prefixed paths fall back to the literal string.
        assert_eq!(expand_home("~/foo"), PathBuf::from("~/foo"));
        // Non-tilde paths are unchanged regardless.
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn config_path_respects_xdg_config_home() {
        let _g = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        std::env::set_var("XDG_CONFIG_HOME", "/synthetic/xdg-config");
        assert_eq!(
            config_path(),
            PathBuf::from("/synthetic/xdg-config/bread/breadd.toml")
        );
    }

    #[test]
    fn config_path_falls_back_to_home_when_no_xdg() {
        let _g = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("HOME", "/synthetic/home");
        assert_eq!(
            config_path(),
            PathBuf::from("/synthetic/home/.config/bread/breadd.toml")
        );
    }
}
