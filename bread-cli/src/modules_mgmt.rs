use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Contents of `bread.module.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub source: String,
    pub installed_at: String,
}

/// Resolve a module source string to a local directory path.
///
/// Only local paths are accepted. Remote fetching (`github:user/repo`) was
/// removed: it pulled arbitrary, unsandboxed Lua that the daemon then runs with
/// full `bread.exec()` privileges as the user. Installing a remote module now
/// requires cloning it yourself, so the review step stays in the user's hands.
pub fn parse_source(source: &str) -> Result<PathBuf> {
    if source.starts_with("github:") || source.starts_with("git:") {
        bail!(
            "bread: remote module installation has been removed for security \
             (it ran unreviewed third-party Lua with full exec privileges). \
             Clone the repository yourself, review it, then run \
             'bread modules install /path/to/checkout'"
        );
    }
    if source.starts_with('/')
        || source.starts_with("./")
        || source.starts_with("../")
        || source.starts_with('~')
    {
        Ok(bread_shared::expand_path(source))
    } else {
        bail!(
            "bread: invalid module source '{}'. Provide an absolute or relative \
             path to a local module directory",
            source
        )
    }
}

/// Install a module from a local directory into `modules_dir`.
/// `source_str` is the original source string recorded in the manifest.
pub fn install_from_local(
    src: &Path,
    source_str: &str,
    modules_dir: &Path,
) -> Result<ModuleManifest> {
    let manifest_path = src.join("bread.module.toml");
    if !manifest_path.exists() {
        bail!("bread: no bread.module.toml found in {}", src.display());
    }

    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let mut manifest: ModuleManifest =
        toml::from_str(&raw).context("failed to parse bread.module.toml")?;

    manifest.source = source_str.to_string();
    manifest.installed_at = Utc::now().to_rfc3339();

    let dest = modules_dir.join(&manifest.name);
    if dest.exists() {
        fs::remove_dir_all(&dest)
            .with_context(|| format!("failed to remove existing module at {}", dest.display()))?;
    }
    copy_dir(src, &dest)?;

    // Rewrite the manifest with the updated fields.
    let manifest_dest = dest.join("bread.module.toml");
    let out = toml::to_string_pretty(&manifest).context("failed to serialize module manifest")?;
    fs::write(&manifest_dest, out)
        .with_context(|| format!("failed to write manifest to {}", manifest_dest.display()))?;

    Ok(manifest)
}

/// Remove a module directory from `modules_dir`.
pub fn remove_module(name: &str, modules_dir: &Path) -> Result<()> {
    let module_dir = modules_dir.join(name);
    if !module_dir.exists() {
        bail!("bread: module '{}' is not installed", name);
    }
    fs::remove_dir_all(&module_dir)
        .with_context(|| format!("failed to remove {}", module_dir.display()))
}

/// List all installed modules in `modules_dir`.
pub fn list_modules(modules_dir: &Path) -> Result<Vec<ModuleManifest>> {
    if !modules_dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(modules_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let manifest_path = path.join("bread.module.toml");
            if manifest_path.exists() {
                if let Ok(m) = read_manifest_file(&manifest_path) {
                    out.push(m);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Read a module manifest by name.
pub fn read_module_manifest(name: &str, modules_dir: &Path) -> Result<ModuleManifest> {
    let manifest_path = modules_dir.join(name).join("bread.module.toml");
    if !manifest_path.exists() {
        bail!("bread: module '{}' is not installed", name);
    }
    read_manifest_file(&manifest_path)
}

/// Read and parse a `bread.module.toml` file.
pub fn read_manifest_file(path: &Path) -> Result<ModuleManifest> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&raw).context("failed to parse module manifest")
}

/// Returns the default modules directory.
pub fn modules_dir() -> PathBuf {
    if let Some(cfg) = dirs::config_dir() {
        return cfg.join("bread").join("modules");
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("bread").join("modules");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("bread")
            .join("modules");
    }
    PathBuf::from(".config/bread/modules")
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}
