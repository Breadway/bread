use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Machine profile stored in `machines/<name>.toml` in the sync repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineProfile {
    pub name: String,
    pub hostname: String,
    pub tags: Vec<String>,
    pub last_sync: String, // RFC 3339
}

impl MachineProfile {
    /// Create a new profile for this machine.
    pub fn new(name: String, tags: Vec<String>) -> Self {
        Self {
            hostname: hostname(),
            name,
            tags,
            last_sync: Utc::now().to_rfc3339(),
        }
    }

    /// Write this profile to `<machines_dir>/<name>.toml`.
    pub fn write(&self, machines_dir: &Path) -> Result<()> {
        fs::create_dir_all(machines_dir)?;
        let path = machines_dir.join(format!("{}.toml", self.name));
        let raw = toml::to_string_pretty(self).context("failed to serialize machine profile")?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))
    }

    /// Read a machine profile from `<machines_dir>/<name>.toml`.
    pub fn read(machines_dir: &Path, name: &str) -> Result<Self> {
        let path = machines_dir.join(format!("{name}.toml"));
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&raw).context("failed to parse machine profile")
    }

    /// List all machine profiles in `machines_dir`.
    pub fn list(machines_dir: &Path) -> Result<Vec<Self>> {
        if !machines_dir.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(machines_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Ok(raw) = fs::read_to_string(&path) {
                    if let Ok(profile) = toml::from_str::<Self>(&raw) {
                        out.push(profile);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

/// Return the system hostname.
pub fn hostname() -> String {
    // Try gethostname via libc, fall back to environment variable.
    let mut buf = [0u8; 256];
    unsafe {
        if libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) == 0 {
            if let Ok(s) = std::ffi::CStr::from_ptr(buf.as_ptr() as *const libc::c_char).to_str() {
                return s.to_string();
            }
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_creates_machines_dir_if_missing() {
        let tmp = TempDir::new().unwrap();
        let machines = tmp.path().join("does/not/exist/yet");
        let profile = MachineProfile::new("host".to_string(), vec![]);
        profile.write(&machines).unwrap();
        assert!(machines.join("host.toml").exists());
    }

    #[test]
    fn write_overwrites_existing_profile() {
        let tmp = TempDir::new().unwrap();
        let p1 = MachineProfile::new("host".to_string(), vec!["a".to_string()]);
        p1.write(tmp.path()).unwrap();

        let p2 = MachineProfile::new("host".to_string(), vec!["b".to_string(), "c".to_string()]);
        p2.write(tmp.path()).unwrap();

        let loaded = MachineProfile::read(tmp.path(), "host").unwrap();
        assert_eq!(loaded.tags, vec!["b", "c"]);
    }

    #[test]
    fn list_returns_empty_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        assert!(MachineProfile::list(&missing).unwrap().is_empty());
    }

    #[test]
    fn list_returns_sorted_profiles_only_for_toml_files() {
        let tmp = TempDir::new().unwrap();
        MachineProfile::new("zebra".to_string(), vec![])
            .write(tmp.path())
            .unwrap();
        MachineProfile::new("alpha".to_string(), vec![])
            .write(tmp.path())
            .unwrap();
        MachineProfile::new("middle".to_string(), vec![])
            .write(tmp.path())
            .unwrap();
        // Non-toml file should be ignored.
        std::fs::write(tmp.path().join("notes.txt"), "ignored").unwrap();

        let list = MachineProfile::list(tmp.path()).unwrap();
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn list_skips_invalid_toml_files_without_failing() {
        let tmp = TempDir::new().unwrap();
        MachineProfile::new("valid".to_string(), vec![])
            .write(tmp.path())
            .unwrap();
        std::fs::write(tmp.path().join("garbage.toml"), "not valid [toml").unwrap();

        let list = MachineProfile::list(tmp.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "valid");
    }

    #[test]
    fn read_returns_helpful_error_when_missing() {
        let tmp = TempDir::new().unwrap();
        let err = MachineProfile::read(tmp.path(), "ghost").unwrap_err();
        assert!(err.to_string().contains("failed to read"));
    }

    #[test]
    fn new_assigns_current_hostname_and_timestamp() {
        let p = MachineProfile::new("h".to_string(), vec![]);
        assert!(!p.hostname.is_empty());
        assert!(chrono::DateTime::parse_from_rfc3339(&p.last_sync).is_ok());
    }

    #[test]
    fn hostname_returns_non_empty_string() {
        // Whether libc or env fallback fires, the result must be non-empty.
        assert!(!hostname().is_empty());
    }
}
