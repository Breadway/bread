use std::path::Path;
use std::process::Command;

use anyhow::Result;

/// Write package manifests to `<repo>/packages/`.
/// Skips package managers that are not installed (warns instead of erroring).
pub fn snapshot_packages(managers: &[String], repo_root: &Path) -> Result<()> {
    let pkg_dir = repo_root.join("packages");
    std::fs::create_dir_all(&pkg_dir)?;

    for mgr in managers {
        match mgr.as_str() {
            "pacman" => {
                if let Some(content) = run_pacman() {
                    std::fs::write(pkg_dir.join("pacman.txt"), content)?;
                } else {
                    eprintln!("warn: pacman not found, skipping package snapshot");
                }
            }
            "pip" => {
                if let Some(content) = run_pip() {
                    std::fs::write(pkg_dir.join("pip.txt"), content)?;
                } else {
                    eprintln!("warn: pip not found, skipping package snapshot");
                }
            }
            "npm" => {
                if let Some(content) = run_npm() {
                    std::fs::write(pkg_dir.join("npm.txt"), content)?;
                } else {
                    eprintln!("warn: npm not found, skipping package snapshot");
                }
            }
            "cargo" => {
                if let Some(content) = run_cargo() {
                    std::fs::write(pkg_dir.join("cargo.txt"), content)?;
                } else {
                    eprintln!("warn: cargo not found, skipping package snapshot");
                }
            }
            other => {
                eprintln!("warn: unknown package manager '{other}', skipping");
            }
        }
    }
    Ok(())
}

/// Parse a `pacman.txt` snapshot into a list of package names.
pub fn parse_pacman(content: &str) -> Vec<String> {
    content.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Parse a `pip.txt` (freeze format) snapshot into package names.
pub fn parse_pip(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split("==").next().map(|s| s.trim().to_string()))
        .collect()
}

/// Parse an `npm.txt` (parseable) snapshot into package names.
pub fn parse_npm(content: &str) -> Vec<String> {
    content
        .lines()
        .skip(1) // first line is the npm global prefix path
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            Path::new(l.trim())
                .file_name()
                .and_then(|n| n.to_str())
                .map(ToString::to_string)
        })
        .collect()
}

/// Parse `cargo install --list` output into `name version` lines.
pub fn parse_cargo(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.trim().is_empty())
        .filter_map(|l| {
            // Format: `name v1.2.3 (...):` or `name v1.2.3:`
            let parts: Vec<&str> = l.splitn(2, ' ').collect();
            if parts.len() == 2 {
                let name = parts[0];
                let version = parts[1].trim_start_matches('v').split_whitespace().next().unwrap_or("?").trim_end_matches(':');
                Some(format!("{name} {version}"))
            } else {
                None
            }
        })
        .collect()
}

fn run_pacman() -> Option<String> {
    let output = Command::new("pacman").args(["-Qe"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn run_pip() -> Option<String> {
    let output = Command::new("pip")
        .args(["list", "--user", "--format=freeze"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn run_npm() -> Option<String> {
    let output = Command::new("npm")
        .args(["list", "-g", "--depth=0", "--parseable"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn run_cargo() -> Option<String> {
    let output = Command::new("cargo")
        .args(["install", "--list"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}
