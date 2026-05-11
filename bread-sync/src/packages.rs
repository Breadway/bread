use anyhow::Result;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Snapshot a package manager's installed packages and write to `dest`.
/// Returns true if the snapshot was written, false if the package manager
/// is not installed (warns instead of failing).
pub fn snapshot(manager: &str, dest: &Path) -> Result<bool> {
    let content = match manager {
        "pacman" => run_pacman()?,
        "pip" => run_pip()?,
        "npm" => run_npm()?,
        "cargo" => run_cargo()?,
        other => {
            eprintln!("bread: unknown package manager '{}', skipping", other);
            return Ok(false);
        }
    };

    let Some(content) = content else {
        eprintln!(
            "bread: package manager '{}' not found, skipping",
            manager
        );
        return Ok(false);
    };

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, content)?;
    Ok(true)
}

/// Parse a pacman snapshot (one "name version" per line, space-separated) and
/// return a list of package names.
pub fn parse_pacman(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().next().unwrap_or(l).to_string())
        .collect()
}

/// Parse a pip freeze snapshot and return package names.
pub fn parse_pip(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.split("==")
                .next()
                .unwrap_or(l)
                .split(">=")
                .next()
                .unwrap_or(l)
                .trim()
                .to_string()
        })
        .collect()
}

/// Parse npm global packages list (parseable format, one path per line).
pub fn parse_npm(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            // `npm list -g --parseable` outputs paths like /usr/lib/node_modules/pkg
            let name = Path::new(l)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())?;
            // Skip npm itself and the root node_modules
            if name == "node_modules" {
                return None;
            }
            Some(name)
        })
        .collect()
}

/// Parse cargo install list.
/// Format: "crate v1.2.3 (some-path):\n  binary\n..."
pub fn parse_cargo(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.trim().is_empty())
        .map(|l| {
            l.split_whitespace()
                .next()
                .unwrap_or(l)
                .to_string()
        })
        .collect()
}

fn run_pacman() -> Result<Option<String>> {
    match Command::new("pacman").arg("-Qe").output() {
        Ok(out) if out.status.success() => Ok(Some(String::from_utf8_lossy(&out.stdout).to_string())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn run_pip() -> Result<Option<String>> {
    // Try pip3 first, then pip
    for cmd in ["pip3", "pip"] {
        match Command::new(cmd)
            .args(["list", "--user", "--format=freeze"])
            .output()
        {
            Ok(out) if out.status.success() => {
                return Ok(Some(String::from_utf8_lossy(&out.stdout).to_string()))
            }
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

fn run_npm() -> Result<Option<String>> {
    match Command::new("npm")
        .args(["list", "-g", "--depth=0", "--parseable"])
        .output()
    {
        Ok(out) if out.status.success() => Ok(Some(String::from_utf8_lossy(&out.stdout).to_string())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn run_cargo() -> Result<Option<String>> {
    match Command::new("cargo").args(["install", "--list"]).output() {
        Ok(out) if out.status.success() => Ok(Some(String::from_utf8_lossy(&out.stdout).to_string())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
