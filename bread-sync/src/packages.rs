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
        "aur" => run_aur()?,
        "pip" => run_pip()?,
        "npm" => run_npm()?,
        "cargo" => run_cargo()?,
        other => {
            eprintln!("bread: unknown package manager '{}', skipping", other);
            return Ok(false);
        }
    };

    let Some(content) = content else {
        eprintln!("bread: package manager '{}' not found, skipping", manager);
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
        .map(|l| l.split_whitespace().next().unwrap_or(l).to_string())
        .collect()
}

fn run_aur() -> Result<Option<String>> {
    match Command::new("pacman").arg("-Qm").output() {
        Ok(out) if out.status.success() => {
            Ok(Some(String::from_utf8_lossy(&out.stdout).to_string()))
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn run_pacman() -> Result<Option<String>> {
    match Command::new("pacman").arg("-Qe").output() {
        Ok(out) if out.status.success() => {
            Ok(Some(String::from_utf8_lossy(&out.stdout).to_string()))
        }
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
        Ok(out) if out.status.success() => {
            Ok(Some(String::from_utf8_lossy(&out.stdout).to_string()))
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn run_cargo() -> Result<Option<String>> {
    match Command::new("cargo").args(["install", "--list"]).output() {
        Ok(out) if out.status.success() => {
            Ok(Some(String::from_utf8_lossy(&out.stdout).to_string()))
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── parse_pacman ─────────────────────────────────────────────────────

    #[test]
    fn pacman_parses_each_line_to_first_field() {
        let input = "firefox 128.0-1\ncurl 8.7.1-1\nrustup 1.27.1-1\n";
        assert_eq!(parse_pacman(input), vec!["firefox", "curl", "rustup"]);
    }

    #[test]
    fn pacman_skips_blank_lines() {
        let input = "firefox 1\n\n  \ncurl 2\n";
        assert_eq!(parse_pacman(input), vec!["firefox", "curl"]);
    }

    #[test]
    fn pacman_handles_empty_input() {
        assert!(parse_pacman("").is_empty());
        assert!(parse_pacman("\n\n\n").is_empty());
    }

    #[test]
    fn pacman_handles_single_token_lines() {
        // A line with no version still yields the package name.
        assert_eq!(parse_pacman("firefox\n"), vec!["firefox"]);
    }

    // ─── parse_pip ────────────────────────────────────────────────────────

    #[test]
    fn pip_strips_eq_and_ge_specifiers() {
        let input = "requests==2.32.3\nnumpy==2.0.1\nblack>=24.0\n";
        assert_eq!(parse_pip(input), vec!["requests", "numpy", "black"]);
    }

    #[test]
    fn pip_skips_comments_and_blank_lines() {
        let input = "# editable install\n\nflake8==1.0\n# trailing\n";
        assert_eq!(parse_pip(input), vec!["flake8"]);
    }

    #[test]
    fn pip_handles_package_without_specifier() {
        assert_eq!(parse_pip("requests\nblack\n"), vec!["requests", "black"]);
    }

    // ─── parse_npm ────────────────────────────────────────────────────────

    #[test]
    fn npm_extracts_basename_from_paths() {
        let input = "/usr/lib/node_modules/npm\n/usr/lib/node_modules/typescript\n/usr/lib/node_modules/yarn\n";
        let pkgs = parse_npm(input);
        assert!(pkgs.contains(&"npm".to_string()));
        assert!(pkgs.contains(&"typescript".to_string()));
        assert!(pkgs.contains(&"yarn".to_string()));
    }

    #[test]
    fn npm_skips_root_node_modules_entry() {
        let input = "/usr/lib/node_modules\n/usr/lib/node_modules/typescript\n";
        assert_eq!(parse_npm(input), vec!["typescript"]);
    }

    #[test]
    fn npm_handles_empty_input() {
        assert!(parse_npm("").is_empty());
    }

    // ─── parse_cargo ──────────────────────────────────────────────────────

    #[test]
    fn cargo_extracts_crate_names_from_install_list_output() {
        let input = "bottom v0.9.6:\n    btm\nripgrep v14.0.3:\n    rg\nbat v0.24.0:\n    bat\n";
        assert_eq!(parse_cargo(input), vec!["bottom", "ripgrep", "bat"]);
    }

    #[test]
    fn cargo_skips_binary_lines() {
        // Indented lines are binaries inside a crate.
        let input = "alpha v1.0.0:\n    bin1\n    bin2\nbeta v2.0.0:\n    bin3\n";
        assert_eq!(parse_cargo(input), vec!["alpha", "beta"]);
    }

    #[test]
    fn cargo_handles_empty_input() {
        assert!(parse_cargo("").is_empty());
    }

    // ─── snapshot dispatch ────────────────────────────────────────────────

    #[test]
    fn snapshot_unknown_manager_returns_false_without_writing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("out.txt");
        let wrote = snapshot("definitely-not-a-pkg-mgr", &dest).unwrap();
        assert!(!wrote);
        assert!(!dest.exists());
    }
}
