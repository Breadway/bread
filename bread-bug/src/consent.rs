//! The consent notice shown before a user's first breadbug session.
//!
//! One text, used by the CLI, the GUI and bos-settings and mirrored in
//! `docs/breadbug.md` and the breadhelp page, so they cannot drift apart.

use std::path::{Path, PathBuf};

pub const CONSENT_TEXT: &str = "\
Reporting a problem

A report contains your app's recent log lines, crash frames, event names and
basic system facts such as OS, kernel, GPU driver and bread versions. Personal
values such as your name, home folder, hostname, IP addresses, emails and
tokens are replaced before anything is stored or shown. You can read the whole
report before deciding.

Nothing is sent unless you choose Send, and sending is off by default. You can
save the report to a file instead.";

fn marker(state_dir: &Path) -> PathBuf {
    state_dir.join("consent-shown")
}

/// Whether the notice has been acknowledged on this machine.
pub fn seen(state_dir: &Path) -> bool {
    marker(state_dir).exists()
}

/// Remember that the notice was acknowledged. Best effort: failing to write
/// the marker only means the notice is shown again next time.
pub fn mark_seen(state_dir: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir);
    let _ = std::fs::write(marker(state_dir), b"1\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        assert!(!seen(&state));
        mark_seen(&state);
        assert!(seen(&state));
    }

    #[test]
    fn the_notice_states_the_three_promises() {
        for needle in [
            "Nothing is sent unless you choose Send",
            "off by default",
            "read the whole",
        ] {
            assert!(CONSENT_TEXT.contains(needle), "{needle}");
        }
    }
}
