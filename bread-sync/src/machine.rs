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
