use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::DelegatesConfig;

/// Expand `~` in a path string to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path))
    } else if path == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
}

/// Returns `true` if `path` (relative to `base`) matches any of the `exclude` globs.
fn is_excluded(base: &Path, path: &Path, excludes: &[String]) -> bool {
    let rel = path.strip_prefix(base).unwrap_or(path);
    let rel_str = rel.to_string_lossy();
    for pattern in excludes {
        if glob_matches(pattern, &rel_str) {
            return true;
        }
    }
    false
}

/// Copy all files under `src` dir to `dest` dir, honouring `excludes`.
/// Creates `dest` if it doesn't exist. Deletes files in `dest` that are
/// absent in `src` (rsync `--delete` behaviour).
pub fn sync_dir(src: &Path, dest: &Path, excludes: &[String]) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    copy_recursive(src, src, dest, excludes)?;
    delete_extra(src, dest)?;
    Ok(())
}

fn copy_recursive(root: &Path, src: &Path, dest: &Path, excludes: &[String]) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();

        if is_excluded(root, &src_path, excludes) {
            continue;
        }

        let file_name = entry.file_name();
        let dest_path = dest.join(&file_name);

        if src_path.is_dir() {
            std::fs::create_dir_all(&dest_path)?;
            copy_recursive(root, &src_path, &dest_path, excludes)?;
        } else {
            std::fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

/// Remove files/dirs from `dest` that don't exist in `src`.
fn delete_extra(src: &Path, dest: &Path) -> Result<()> {
    if !dest.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dest)? {
        let entry = entry?;
        let dest_path = entry.path();
        let file_name = entry.file_name();
        let src_path = src.join(&file_name);
        if !src_path.exists() {
            if dest_path.is_dir() {
                std::fs::remove_dir_all(&dest_path)?;
            } else {
                std::fs::remove_file(&dest_path)?;
            }
        }
    }
    Ok(())
}

/// Copy each `include` path into `<repo_root>/configs/<basename>/`.
pub fn copy_delegates_to_repo(
    cfg: &DelegatesConfig,
    repo_root: &Path,
) -> Result<()> {
    let configs_dir = repo_root.join("configs");
    std::fs::create_dir_all(&configs_dir)?;

    for raw_path in &cfg.include {
        let src = expand_tilde(raw_path);
        if !src.exists() {
            tracing_warn(&format!(
                "delegate path does not exist, skipping: {}",
                src.display()
            ));
            continue;
        }
        let basename = src
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("delegate path has no filename: {}", src.display()))?;
        let dest = configs_dir.join(basename);
        if src.is_dir() {
            sync_dir(&src, &dest, &cfg.exclude)?;
        } else {
            std::fs::copy(&src, &dest)?;
        }
    }
    Ok(())
}

/// Restore each delegate path from `<repo_root>/configs/<basename>/` to its original location.
pub fn restore_delegates_from_repo(
    cfg: &DelegatesConfig,
    repo_root: &Path,
) -> Result<()> {
    let configs_dir = repo_root.join("configs");

    for raw_path in &cfg.include {
        let dest = expand_tilde(raw_path);
        let basename = match dest.file_name() {
            Some(n) => n.to_os_string(),
            None => continue,
        };
        let src = configs_dir.join(&basename);
        if !src.exists() {
            continue;
        }
        if src.is_dir() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            sync_dir(&src, &dest, &[])?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dest)?;
        }
    }
    Ok(())
}

/// Simple glob match for `**` and `*` patterns against a path string.
fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), path.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    // `**` matches any sequence including path separators
    if pattern.starts_with(b"**") {
        let rest = &pattern[2..];
        if rest.is_empty() {
            return true;
        }
        // skip leading separator in rest
        let rest = if rest.starts_with(b"/") { &rest[1..] } else { rest };
        for offset in 0..=text.len() {
            if glob_match_bytes(rest, &text[offset..]) {
                return true;
            }
        }
        return false;
    }

    match pattern[0] {
        b'*' => {
            let mut offset = 0;
            loop {
                if glob_match_bytes(&pattern[1..], &text[offset..]) {
                    return true;
                }
                if offset == text.len() {
                    break;
                }
                offset += 1;
            }
            false
        }
        b'?' => {
            if text.is_empty() {
                return false;
            }
            glob_match_bytes(&pattern[1..], &text[1..])
        }
        ch => {
            if text.first().copied() != Some(ch) {
                return false;
            }
            glob_match_bytes(&pattern[1..], &text[1..])
        }
    }
}

fn tracing_warn(msg: &str) {
    // Use eprintln since tracing may not be configured in library context
    eprintln!("warn: {msg}");
}
