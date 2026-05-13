use anyhow::Result;
use glob::Pattern;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::expand_path;

/// Copy all files from `src` into `dst`, mirroring the directory tree.
/// Files present in `dst` but not in `src` are deleted (rsync-style).
/// Files matching any `exclude` glob are skipped.
pub fn sync_dir(src: &Path, dst: &Path, exclude: &[String]) -> Result<()> {
    let patterns: Vec<Pattern> = exclude
        .iter()
        .filter_map(|g| Pattern::new(g).ok())
        .collect();

    fs::create_dir_all(dst)?;
    sync_dir_inner(src, dst, src, &patterns)
}

fn sync_dir_inner(src: &Path, dst: &Path, root: &Path, patterns: &[Pattern]) -> Result<()> {
    // Remove files in dst that don't exist in src.
    if dst.exists() {
        for entry in fs::read_dir(dst)? {
            let entry = entry?;
            let rel = entry
                .path()
                .strip_prefix(dst)
                .unwrap_or(&entry.path())
                .to_path_buf();
            let src_counterpart = src.join(&rel);
            if !src_counterpart.exists() {
                let p = entry.path();
                if p.is_dir() {
                    let _ = fs::remove_dir_all(&p);
                } else {
                    let _ = fs::remove_file(&p);
                }
            }
        }
    }

    if !src.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let rel = src_path.strip_prefix(root).unwrap_or(&src_path);

        if is_excluded(rel, root, patterns) {
            continue;
        }

        let dst_path = dst.join(src_path.strip_prefix(src).unwrap_or(&src_path));

        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)?;
            sync_dir_inner(&src_path, &dst_path, root, patterns)?;
        } else {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn is_excluded(rel: &Path, _root: &Path, patterns: &[Pattern]) -> bool {
    let rel_str = rel.to_string_lossy();
    let file_name = rel
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();

    for pat in patterns {
        // Match against full relative path or just filename
        if pat.matches(&rel_str) || pat.matches(&file_name) {
            return true;
        }
        // For directory-name patterns (e.g. "**/.git"), also check component names
        if let Some(pat_str) = pat.as_str().strip_prefix("**/") {
            for component in rel.components() {
                if let std::path::Component::Normal(name) = component {
                    if Pattern::new(pat_str)
                        .map(|p| p.matches(&name.to_string_lossy()))
                        .unwrap_or(false)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Resolve delegate paths from the config (expanding `~`).
pub fn resolve_include_paths(includes: &[String]) -> Vec<(String, PathBuf)> {
    includes
        .iter()
        .map(|s| {
            let expanded = expand_path(s);
            let basename = expanded
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| s.clone());
            (basename, expanded)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn sync_dir_copies_nested_tree() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();

        fs::create_dir_all(src.path().join("a/b/c")).unwrap();
        fs::write(src.path().join("a/b/c/leaf.txt"), "hello").unwrap();
        fs::write(src.path().join("root.txt"), "root").unwrap();

        sync_dir(src.path(), dst.path(), &[]).unwrap();

        assert_eq!(
            fs::read_to_string(dst.path().join("a/b/c/leaf.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::read_to_string(dst.path().join("root.txt")).unwrap(),
            "root"
        );
    }

    #[test]
    fn sync_dir_overwrites_existing_files() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::write(src.path().join("f"), "new").unwrap();
        fs::write(dst.path().join("f"), "old").unwrap();

        sync_dir(src.path(), dst.path(), &[]).unwrap();
        assert_eq!(fs::read_to_string(dst.path().join("f")).unwrap(), "new");
    }

    #[test]
    fn sync_dir_removes_files_no_longer_in_src() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::write(dst.path().join("orphan.txt"), "to remove").unwrap();
        fs::write(src.path().join("keeper.txt"), "stay").unwrap();

        sync_dir(src.path(), dst.path(), &[]).unwrap();

        assert!(!dst.path().join("orphan.txt").exists());
        assert!(dst.path().join("keeper.txt").exists());
    }

    #[test]
    fn sync_dir_removes_directories_no_longer_in_src() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::create_dir_all(dst.path().join("ghost-dir")).unwrap();
        fs::write(dst.path().join("ghost-dir/x"), "").unwrap();

        sync_dir(src.path(), dst.path(), &[]).unwrap();
        assert!(!dst.path().join("ghost-dir").exists());
    }

    #[test]
    fn sync_dir_exclude_filters_by_basename_pattern() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::write(src.path().join("keep.lua"), "lua").unwrap();
        fs::write(src.path().join("trash.cache"), "").unwrap();

        sync_dir(src.path(), dst.path(), &["**/*.cache".to_string()]).unwrap();
        assert!(dst.path().join("keep.lua").exists());
        assert!(!dst.path().join("trash.cache").exists());
    }

    #[test]
    fn sync_dir_exclude_filters_nested_directory_by_name() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::create_dir_all(src.path().join(".git/objects")).unwrap();
        fs::write(src.path().join(".git/objects/abc"), "").unwrap();
        fs::write(src.path().join("init.lua"), "lua").unwrap();

        sync_dir(src.path(), dst.path(), &["**/.git".to_string()]).unwrap();
        assert!(dst.path().join("init.lua").exists());
        assert!(!dst.path().join(".git").exists());
    }

    #[test]
    fn sync_dir_creates_destination_if_missing() {
        let src = TempDir::new().unwrap();
        let dst_parent = TempDir::new().unwrap();
        let dst = dst_parent.path().join("brand-new");
        fs::write(src.path().join("hi"), "hi").unwrap();

        sync_dir(src.path(), &dst, &[]).unwrap();
        assert!(dst.join("hi").exists());
    }

    #[test]
    fn sync_dir_empty_src_clears_dst() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::write(dst.path().join("a"), "").unwrap();
        fs::write(dst.path().join("b"), "").unwrap();

        sync_dir(src.path(), dst.path(), &[]).unwrap();
        let remaining: Vec<_> = fs::read_dir(dst.path()).unwrap().collect();
        assert!(remaining.is_empty());
    }

    // ─── resolve_include_paths ────────────────────────────────────────────

    #[test]
    fn resolve_include_paths_uses_basename_as_key() {
        let includes = vec!["/etc/foo/bar".to_string(), "/var/lib/quux".to_string()];
        let resolved = resolve_include_paths(&includes);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].0, "bar");
        assert_eq!(resolved[0].1, PathBuf::from("/etc/foo/bar"));
        assert_eq!(resolved[1].0, "quux");
    }

    #[test]
    fn resolve_include_paths_expands_tilde_in_source() {
        let home = dirs::home_dir().or_else(|| std::env::var("HOME").ok().map(PathBuf::from));
        if let Some(home) = home {
            let resolved = resolve_include_paths(&["~/Documents".to_string()]);
            assert_eq!(resolved.len(), 1);
            assert_eq!(resolved[0].1, home.join("Documents"));
            assert_eq!(resolved[0].0, "Documents");
        }
    }
}
