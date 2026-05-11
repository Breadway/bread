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
            let rel = entry.path().strip_prefix(dst).unwrap_or(&entry.path()).to_path_buf();
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
