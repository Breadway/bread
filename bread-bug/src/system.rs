//! Non-personal facts about the machine and the installed bread apps.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use bread_shared::apps::KNOWN_APPS;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runcmd;
use crate::store::DAEMON_APP;

/// What the machine looks like. Hardware ids and versions only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemInfo {
    pub os_id: Option<String>,
    pub os_version: Option<String>,
    pub os_build: Option<String>,
    pub kernel: Option<String>,
    pub arch: String,
    /// `driver (vendor:device)` per GPU, e.g. `amdgpu (1002:1114)`.
    pub gpu_drivers: Vec<String>,
    /// `hyprland 0.56.2` style, from the compositor itself.
    pub compositor: Option<String>,
    pub bakery_track: Option<String>,
    pub breadd_version: Option<String>,
    pub api_version: Option<String>,
}

/// One installed bread app, by app id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstalledApp {
    pub id: String,
    pub version: String,
}

/// The app id a bakery package provides, if it is a bread app.
///
/// `breadclip` is app `clip`, `bread` provides the `breadd` daemon, and
/// `bakery` is its own id. Anything else (themes, settings, ...) is not an
/// app that reports through breadd.
pub fn app_for_package(pkg: &str) -> Option<String> {
    if pkg == "bread" {
        return Some(DAEMON_APP.to_string());
    }
    let id = pkg.strip_prefix("bread").unwrap_or(pkg);
    KNOWN_APPS.contains(&id).then(|| id.to_string())
}

/// Parse bakery's `installed.json`: the track and the installed bread apps.
pub fn parse_installed(text: &str) -> Option<(String, Vec<InstalledApp>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let track = v
        .get("track")
        .and_then(Value::as_str)
        .unwrap_or("stable")
        .to_string();
    let mut apps = Vec::new();
    for (name, pkg) in v.get("packages")?.as_object()? {
        let (Some(id), Some(version)) = (
            app_for_package(name),
            pkg.get("version").and_then(Value::as_str),
        ) else {
            continue;
        };
        apps.push(InstalledApp {
            id,
            version: version.to_string(),
        });
    }
    apps.sort();
    Some((track, apps))
}

/// `~/.local/state/bakery/installed.json`.
pub fn bakery_state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("bakery")
        .join("installed.json")
}

/// Installed bread apps and the bakery track, or `None` when bakery has no
/// state (for example a pacman-only install).
pub fn installed_apps() -> Option<(String, Vec<InstalledApp>)> {
    parse_installed(&std::fs::read_to_string(bakery_state_path()).ok()?)
}

/// Parse `/etc/os-release` into `KEY -> value`.
pub fn parse_os_release(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect()
}

fn read_trimmed(path: &str) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// GPUs as `driver (vendor:device)`, from the DRM subsystem.
fn gpu_drivers() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/class/drm") else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_card = name
            .strip_prefix("card")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !is_card {
            continue;
        }
        let Ok(uevent) = std::fs::read_to_string(entry.path().join("device/uevent")) else {
            continue;
        };
        let kv = parse_os_release(&uevent);
        if let (Some(driver), Some(id)) = (kv.get("DRIVER"), kv.get("PCI_ID")) {
            let line = format!("{driver} ({})", id.to_ascii_lowercase());
            if !out.contains(&line) {
                out.push(line);
            }
        }
    }
    out.sort();
    out
}

fn compositor() -> Option<String> {
    let out = runcmd::run("hyprctl", &["version", "-j"], Duration::from_secs(2))?;
    let v: Value = serde_json::from_str(&out).ok()?;
    let version = v.get("version")?.as_str()?;
    Some(format!("hyprland {version}"))
}

impl SystemInfo {
    /// Gather everything available. `health` is breadd's `health` reply
    /// when the daemon is up.
    pub fn gather(health: Option<&Value>) -> Self {
        let os = std::fs::read_to_string("/etc/os-release")
            .map(|t| parse_os_release(&t))
            .unwrap_or_default();
        Self {
            os_id: os.get("ID").cloned(),
            os_version: os.get("VERSION_ID").cloned(),
            os_build: os.get("BUILD_ID").cloned(),
            kernel: read_trimmed("/proc/sys/kernel/osrelease"),
            arch: std::env::consts::ARCH.to_string(),
            gpu_drivers: gpu_drivers(),
            compositor: compositor(),
            bakery_track: installed_apps().map(|(t, _)| t),
            breadd_version: health
                .and_then(|h| h.get("version"))
                .and_then(Value::as_str)
                .map(ToString::to_string),
            api_version: health
                .and_then(|h| h.get("api_version"))
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_map_to_app_ids() {
        assert_eq!(app_for_package("breadclip").as_deref(), Some("clip"));
        assert_eq!(app_for_package("bread").as_deref(), Some(DAEMON_APP));
        assert_eq!(app_for_package("bakery").as_deref(), Some("bakery"));
        assert_eq!(app_for_package("bread-theme"), None);
        assert_eq!(app_for_package("bos-settings"), None);
        assert_eq!(app_for_package("breadnotanapp"), None);
    }

    #[test]
    fn installed_json_is_parsed() {
        let text = r#"{"track":"dev","packages":{
            "breadclip":{"version":"0.3.1-dev.1","name":"breadclip"},
            "bread":{"version":"0.9.1","name":"bread"},
            "bread-theme":{"version":"0.8.1","name":"bread-theme"}}}"#;
        let (track, apps) = parse_installed(text).unwrap();
        assert_eq!(track, "dev");
        let ids: Vec<_> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["breadd", "clip"]);
        assert!(parse_installed("not json").is_none());
    }

    #[test]
    fn os_release_parsing_strips_quotes() {
        let kv = parse_os_release("ID=bos\nPRETTY_NAME=\"Bread OS\"\n# c\nVERSION_ID=1\n");
        assert_eq!(kv["PRETTY_NAME"], "Bread OS");
        assert_eq!(kv["ID"], "bos");
    }

    #[test]
    fn gather_does_not_panic_and_reads_health() {
        let h = serde_json::json!({"version": "0.9.1", "api_version": "1.8.0"});
        let info = SystemInfo::gather(Some(&h));
        assert_eq!(info.breadd_version.as_deref(), Some("0.9.1"));
        assert!(!info.arch.is_empty());
    }
}
