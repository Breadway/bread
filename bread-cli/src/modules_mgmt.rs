use anyhow::{bail, Context, Result};
use bread_shared::{ModulePermission, PermissionKind};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Contents of `bread.module.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub source: String,
    pub installed_at: String,
    /// Declared `[[permissions]]` entries. `None` means the manifest has no
    /// `permissions` key at all — either because it predates this field (an
    /// already-installed module) or because the author simply didn't add
    /// one. `breadd` treats that the same way: full, ungated `bread.*`
    /// access, same as today, but `bread doctor` flags it so the gap is
    /// visible instead of silently permanent. An explicit `permissions = []`
    /// is different: it's a deliberate "baseline only" declaration and does
    /// *not* get flagged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<ModulePermission>>,
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

/// Validate that a module name is safe to join onto `modules_dir`.
///
/// Module names ultimately come from untrusted input: a manifest file
/// (`bread.module.toml`, which could be crafted by anyone who hands the user
/// a "module" to install) or a raw CLI argument (`bread modules remove
/// <name>`). Without this check, a name like `../../../../etc` or an
/// absolute path would let install/remove escape `modules_dir` entirely —
/// classic path traversal. Reject any name containing a path separator,
/// a `..` component, or that is otherwise not a single plain path segment.
fn validate_module_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("bread: module name must not be empty");
    }
    let path = Path::new(name);
    // A valid module name must be exactly one normal path component, e.g.
    // it must not contain `/`, must not be `.`/`..`, and must not be an
    // absolute path or reference a prefix/root.
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(seg)), None) if seg == name => {}
        _ => {
            bail!(
                "bread: invalid module name '{}' (must be a single path segment, \
                 no '/', '..', or absolute paths)",
                name
            );
        }
    }
    Ok(())
}

/// Join `name` onto `modules_dir`, validating the name and verifying the
/// resulting path is still contained within `modules_dir`.
///
/// This is defense in depth on top of [`validate_module_name`]: even if the
/// name passes the component check, we canonicalize the parent directory
/// and confirm the joined path's parent resolves to it before allowing any
/// filesystem operation on the result.
fn resolve_module_dir(name: &str, modules_dir: &Path) -> Result<PathBuf> {
    validate_module_name(name)?;
    let dest = modules_dir.join(name);

    // Canonicalize modules_dir itself (it should exist by the time we're
    // installing/removing/reading from it in practice; callers that need it
    // pre-creation call fs::create_dir_all first).
    if let Ok(canonical_root) = modules_dir.canonicalize() {
        if let Some(parent) = dest.parent() {
            if let Ok(canonical_parent) = parent.canonicalize() {
                if canonical_parent != canonical_root {
                    bail!(
                        "bread: resolved module path '{}' escapes modules directory",
                        dest.display()
                    );
                }
            }
        }
    }

    Ok(dest)
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

    fs::create_dir_all(modules_dir)
        .with_context(|| format!("failed to create {}", modules_dir.display()))?;
    let dest = resolve_module_dir(&manifest.name, modules_dir)?;
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
    let module_dir = resolve_module_dir(name, modules_dir)?;
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
    let module_dir = resolve_module_dir(name, modules_dir)?;
    let manifest_path = module_dir.join("bread.module.toml");
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

// ---------------------------------------------------------------------------
// `bread modules audit` — best-effort static permission suggestion
// ---------------------------------------------------------------------------
//
// This is deliberately a text scan, not a Lua parser. `breadd`'s own
// scoping mechanism only cares whether a permission was declared at all
// (see `Documentation.md`'s "Capability-scoped modules" section), so the
// bar here is the same one the report set: false positives (suggesting a
// permission a module doesn't strictly need) are fine, false negatives on
// a plain `bread.exec("...")`-style call site should be rare. It is not
// expected to follow dynamic dispatch, string-built calls, or anything a
// real parser would be needed for.

/// Statically scan every `.lua` file in `module_dir` (recursively — a
/// module may `require()` sibling files from its own directory) for
/// `bread.*` call-site patterns and return a suggested, deduplicated
/// permission list for the user to review.
pub fn audit_module(module_dir: &Path) -> Result<Vec<ModulePermission>> {
    let mut found: std::collections::BTreeMap<PermissionKind, ModulePermission> =
        std::collections::BTreeMap::new();
    let mut files = Vec::new();
    collect_lua_files(module_dir, &mut files)?;
    for file in &files {
        if let Ok(src) = fs::read_to_string(file) {
            scan_lua_source(&src, &mut found);
        }
    }
    Ok(found.into_values().collect())
}

/// Render a suggested permission list as a pastable `[[permissions]]` TOML
/// block, matching exactly what `bread.module.toml` expects.
pub fn render_permissions_toml(perms: &[ModulePermission]) -> Result<String> {
    #[derive(Serialize)]
    struct PermissionsBlock<'a> {
        permissions: &'a [ModulePermission],
    }
    toml::to_string_pretty(&PermissionsBlock { permissions: perms })
        .context("failed to render suggested permissions as TOML")
}

fn collect_lua_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_lua_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("lua") {
            out.push(path);
        }
    }
    Ok(())
}

/// Scan one file's source for `bread.<ident>[.<ident>]` occurrences and
/// classify each into a permission, accumulating into `found` (keyed by
/// kind, so repeated call sites for the same permission collapse to one
/// suggestion — first-seen scoping hint wins).
fn scan_lua_source(src: &str, found: &mut std::collections::BTreeMap<PermissionKind, ModulePermission>) {
    const NEEDLE: &str = "bread.";
    let mut cursor = 0usize;
    while let Some(rel) = src[cursor..].find(NEEDLE) {
        let ident_start = cursor + rel + NEEDLE.len();
        cursor = ident_start;
        let rest = &src[ident_start..];
        let ident_len = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(rest.len());
        let ident = rest[..ident_len].trim_end_matches('.');
        if ident.is_empty() {
            continue;
        }
        classify_call_site(ident, &rest[ident_len..], found);
    }
}

fn classify_call_site(
    ident: &str,
    tail: &str,
    found: &mut std::collections::BTreeMap<PermissionKind, ModulePermission>,
) {
    let (kind, bin, path): (PermissionKind, Option<String>, Option<String>) = match ident {
        "fs.write" => (PermissionKind::FsWrite, None, extract_first_string_arg(tail)),
        "fs.read" | "fs.exists" | "fs.readlink" | "fs.expand" => {
            (PermissionKind::FsRead, None, extract_first_string_arg(tail))
        }
        "exec" | "exec_capture" => {
            let hint = extract_first_string_arg(tail)
                .and_then(|s| s.split_whitespace().next().map(str::to_string));
            (PermissionKind::Exec, hint, None)
        }
        "notify" => (PermissionKind::Notify, None, None),
        "profile.activate" => (PermissionKind::ProfileActivate, None, None),
        "state.watch" => (PermissionKind::StateWatch, None, extract_first_string_arg(tail)),
        other if other == "state" || other.starts_with("state.") => {
            (PermissionKind::StateRead, None, extract_first_string_arg(tail))
        }
        other if other == "machine" || other.starts_with("machine.") => {
            (PermissionKind::Machine, None, None)
        }
        other if other == "hyprland" || other.starts_with("hyprland.") => {
            (PermissionKind::Hyprland, None, None)
        }
        other if other == "widget" || other.starts_with("widget.") => {
            (PermissionKind::Widget, None, None)
        }
        other if other == "bluetooth" || other.starts_with("bluetooth.") => {
            (PermissionKind::Bluetooth, None, None)
        }
        // Everything else (on/once/filter/off/emit/after/every/cancel/json/
        // module/log/warn/error/debounce/spawn/wait/wait_any/wait_all/
        // workflow/__private) is baseline — always available, nothing to
        // suggest.
        _ => return,
    };
    found
        .entry(kind)
        .or_insert(ModulePermission { kind, path, bin });
}

/// Best-effort extraction of the first quoted string literal appearing on
/// the same line right after a call-site's opening paren, e.g.
/// `bread.exec("hyprpaper --config foo")` -> `Some("hyprpaper --config foo")`.
/// Returns `None` for dynamic/variable arguments (`bread.fs.read(path)`) —
/// the permission is still suggested, just without a scoping hint.
fn extract_first_string_arg(tail: &str) -> Option<String> {
    let line_end = tail.find('\n').unwrap_or(tail.len());
    let window = &tail[..line_end];
    let quote_pos = window.find(['"', '\''])?;
    let quote_char = window.as_bytes()[quote_pos] as char;
    let after = &window[quote_pos + 1..];
    let quote_end = after.find(quote_char)?;
    Some(after[..quote_end].to_string())
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn audit_detects_fs_read_and_widget_from_cpu_temp_widget_style_module() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("init.lua"),
            r#"
            local M = bread.module({ name = "cpu-temp-widget", version = "1.0.0" })
            local function read_temp_c()
                local raw = bread.fs.read("/sys/class/hwmon/hwmon6/temp1_input")
                return raw
            end
            function M.on_load()
                bread.widget.register({ id = "cpu-temp" })
                bread.every(5000, function()
                    bread.widget.update("cpu-temp", {})
                end)
            end
            return M
            "#,
        )
        .unwrap();

        let perms = audit_module(dir.path()).unwrap();
        let kinds: Vec<PermissionKind> = perms.iter().map(|p| p.kind).collect();
        assert!(kinds.contains(&PermissionKind::FsRead));
        assert!(kinds.contains(&PermissionKind::Widget));
        assert!(!kinds.contains(&PermissionKind::Exec));
        assert!(!kinds.contains(&PermissionKind::Bluetooth));

        let fs_perm = perms.iter().find(|p| p.kind == PermissionKind::FsRead).unwrap();
        assert_eq!(fs_perm.path.as_deref(), Some("/sys/class/hwmon/hwmon6/temp1_input"));
    }

    #[test]
    fn audit_extracts_exec_bin_hint_and_ignores_baseline_calls() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("init.lua"),
            r#"
            local M = bread.module({ name = "wallpaper", version = "1.0.0" })
            function M.on_load()
                bread.on("bread.monitor.connected", function()
                    bread.exec("hyprpaper --config /tmp/foo")
                end)
                bread.log("loaded")
            end
            return M
            "#,
        )
        .unwrap();

        let perms = audit_module(dir.path()).unwrap();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].kind, PermissionKind::Exec);
        assert_eq!(perms[0].bin.as_deref(), Some("hyprpaper"));
    }

    #[test]
    fn audit_scans_required_sibling_files_in_module_directory() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("init.lua"),
            r#"local lib = require("./lib"); return bread.module({ name = "m", version = "1.0.0" })"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("lib.lua"),
            r#"return { go = function() bread.bluetooth.power(true) end }"#,
        )
        .unwrap();

        let perms = audit_module(dir.path()).unwrap();
        assert!(perms.iter().any(|p| p.kind == PermissionKind::Bluetooth));
    }

    #[test]
    fn render_permissions_toml_produces_pastable_block() {
        let perms = vec![ModulePermission {
            kind: PermissionKind::Exec,
            path: None,
            bin: Some("hyprpaper".to_string()),
        }];
        let rendered = render_permissions_toml(&perms).unwrap();
        assert!(rendered.contains("[[permissions]]"));
        assert!(rendered.contains("type = \"exec\""));
        assert!(rendered.contains("bin = \"hyprpaper\""));
    }
}
