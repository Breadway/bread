//! Process instance identity.
//!
//! A pid is not an identity: pids are reused, and `/proc/<pid>/comm` is a
//! name the process can set for itself. An instance is identified by the
//! kernel's start time for the process (`/proc/<pid>/stat` field 22, in clock
//! ticks since boot), the boot it belongs to, and the executable it runs.
//! breadd records this when it first hears from a process; `breadbug` and the
//! watchdog compare against it later.

use std::path::Path;

use crate::store::ProcessIdentity;

/// The parts of `/proc/<pid>/stat` that matter here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Process state letter (`R`, `S`, `Z`, ...).
    pub state: char,
    /// Start time in clock ticks since boot.
    pub start_ticks: u64,
}

impl Stat {
    /// Whether the process can still run (not a zombie or dead).
    pub fn is_running(&self) -> bool {
        !matches!(self.state, 'Z' | 'X')
    }
}

/// Why a recorded instance no longer matches the process behind its pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// No such process, or only a zombie.
    Gone,
    /// The pid now belongs to a different process (reused pid, new boot, or
    /// a different executable).
    Replaced,
}

/// Parse `/proc/<pid>/stat`. The command name is parenthesised and may
/// itself contain spaces and parentheses, so fields are counted after the
/// last `)`.
pub fn read_stat(proc_root: &Path, pid: u32) -> Option<Stat> {
    let text = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let rest = text.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    // `state` is field 3 and `starttime` is field 22: 18 fields in between.
    let start_ticks = fields.nth(18)?.parse().ok()?;
    Some(Stat { state, start_ticks })
}

/// `/proc/<pid>/exe` without the `" (deleted)"` suffix the kernel appends
/// when the file was replaced on disk, as bakery does while a bread app
/// keeps running.
pub fn normalize_exe(exe: &str) -> &str {
    exe.strip_suffix(" (deleted)").unwrap_or(exe)
}

/// The identity of the process currently behind `pid`, if it can be read.
pub fn identity_in(proc_root: &Path, pid: u32) -> Option<ProcessIdentity> {
    let stat = read_stat(proc_root, pid)?;
    if !stat.is_running() {
        return None;
    }
    let exe = std::fs::read_link(proc_root.join(pid.to_string()).join("exe")).ok()?;
    let boot_id = std::fs::read_to_string(proc_root.join("sys/kernel/random/boot_id"))
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty());
    Some(ProcessIdentity {
        start_ticks: stat.start_ticks,
        exe: exe.to_string_lossy().into_owned(),
        boot_id,
    })
}

/// [`identity_in`] for the real `/proc`.
pub fn identity_of(pid: u32) -> Option<ProcessIdentity> {
    identity_in(Path::new("/proc"), pid)
}

/// Whether two readings describe the same process instance.
pub fn same_instance(a: &ProcessIdentity, b: &ProcessIdentity) -> bool {
    a.start_ticks == b.start_ticks
        && normalize_exe(&a.exe) == normalize_exe(&b.exe)
        && match (&a.boot_id, &b.boot_id) {
            (Some(x), Some(y)) => x == y,
            _ => true,
        }
}

/// Whether `pid` is still the recorded instance.
pub fn verify_in(proc_root: &Path, pid: u32, recorded: &ProcessIdentity) -> Result<(), Change> {
    let Some(stat) = read_stat(proc_root, pid) else {
        return Err(Change::Gone);
    };
    if !stat.is_running() {
        return Err(Change::Gone);
    }
    let Some(now) = identity_in(proc_root, pid) else {
        // Alive a moment ago but unreadable now: it exited under us.
        return Err(Change::Gone);
    };
    if same_instance(&now, recorded) {
        Ok(())
    } else {
        Err(Change::Replaced)
    }
}

/// [`verify_in`] for the real `/proc`.
pub fn verify(pid: u32, recorded: &ProcessIdentity) -> Result<(), Change> {
    verify_in(Path::new("/proc"), pid, recorded)
}

/// Whether the executable at `exe` is the binary of bread app `app`.
///
/// Sibling apps ship as `bread<app>` (and `bread<app>d` for a daemon half,
/// such as `breadclipd`); `breadd` is the daemon and `bakery` is its own
/// name. This ties a process to the app a marker was filed under; the
/// process's self-chosen `comm` plays no part.
pub fn exe_matches_app(app: &str, exe: &str) -> bool {
    let name = Path::new(normalize_exe(exe))
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match app {
        "breadd" => name == "breadd",
        "bakery" => name == "bakery",
        _ => name == format!("bread{app}") || name == format!("bread{app}d"),
    }
}

/// Fake `/proc` entries for tests in this crate and in breadd. Not part of
/// the supported API.
#[doc(hidden)]
pub mod testutil {
    use std::path::Path;

    /// A `/proc/<pid>` directory with a stat line, an `exe` link and a boot id.
    pub fn fake_proc(root: &Path, pid: u32, comm: &str, state: char, start_ticks: u64, exe: &str) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // pid (comm) state, then fields 4..=21 (18 of them), then starttime.
        let filler = vec!["0"; 18].join(" ");
        std::fs::write(
            dir.join("stat"),
            format!("{pid} ({comm}) {state} {filler} {start_ticks} 0 0\n"),
        )
        .unwrap();
        let _ = std::fs::remove_file(dir.join("exe"));
        std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
        let boot = root.join("sys/kernel/random");
        std::fs::create_dir_all(&boot).unwrap();
        std::fs::write(boot.join("boot_id"), "boot-a\n").unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::fake_proc;
    use super::*;

    #[test]
    fn stat_is_parsed_after_the_last_paren() {
        let dir = tempfile::tempdir().unwrap();
        fake_proc(
            dir.path(),
            10,
            "my (weird) app",
            'S',
            4242,
            "/usr/bin/breadpad",
        );
        let s = read_stat(dir.path(), 10).unwrap();
        assert_eq!(s.state, 'S');
        assert_eq!(s.start_ticks, 4242);
        assert!(read_stat(dir.path(), 11).is_none());
    }

    #[test]
    fn identity_records_start_exe_and_boot() {
        let dir = tempfile::tempdir().unwrap();
        fake_proc(dir.path(), 10, "breadpad", 'S', 77, "/usr/bin/breadpad");
        let id = identity_in(dir.path(), 10).unwrap();
        assert_eq!(id.start_ticks, 77);
        assert_eq!(id.exe, "/usr/bin/breadpad");
        assert_eq!(id.boot_id.as_deref(), Some("boot-a"));
    }

    #[test]
    fn verify_distinguishes_gone_from_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fake_proc(root, 10, "breadpad", 'S', 77, "/usr/bin/breadpad");
        let rec = identity_in(root, 10).unwrap();
        assert_eq!(verify_in(root, 10, &rec), Ok(()));

        // Same pid, new process: different start time.
        fake_proc(root, 10, "breadpad", 'S', 99, "/usr/bin/breadpad");
        assert_eq!(verify_in(root, 10, &rec), Err(Change::Replaced));

        // Same start time, different executable.
        fake_proc(root, 10, "breadpad", 'S', 77, "/usr/bin/python3.14");
        assert_eq!(verify_in(root, 10, &rec), Err(Change::Replaced));

        // Zombie and missing are both gone.
        fake_proc(root, 10, "breadpad", 'Z', 77, "/usr/bin/breadpad");
        assert_eq!(verify_in(root, 10, &rec), Err(Change::Gone));
        assert_eq!(verify_in(root, 11, &rec), Err(Change::Gone));
    }

    #[test]
    fn boot_id_from_another_boot_never_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fake_proc(root, 10, "breadpad", 'S', 77, "/usr/bin/breadpad");
        let mut rec = identity_in(root, 10).unwrap();
        rec.boot_id = Some("boot-b".into());
        assert_eq!(verify_in(root, 10, &rec), Err(Change::Replaced));
    }

    #[test]
    fn an_updated_binary_is_still_the_same_instance() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fake_proc(root, 10, "breadpad", 'S', 77, "/usr/bin/breadpad");
        let rec = identity_in(root, 10).unwrap();
        fake_proc(root, 10, "breadpad", 'S', 77, "/usr/bin/breadpad (deleted)");
        assert_eq!(verify_in(root, 10, &rec), Ok(()));
    }

    #[test]
    fn exe_is_tied_to_the_app() {
        assert!(exe_matches_app("pad", "/usr/bin/breadpad"));
        assert!(exe_matches_app("pad", "/usr/bin/breadpad (deleted)"));
        assert!(exe_matches_app("clip", "/usr/bin/breadclipd"));
        assert!(exe_matches_app("breadd", "/usr/bin/breadd"));
        assert!(exe_matches_app("bakery", "/usr/bin/bakery"));
        assert!(!exe_matches_app("pad", "/usr/bin/breadclip"));
        assert!(!exe_matches_app("pad", "/usr/bin/python3.14"));
        assert!(!exe_matches_app("pad", "/home/x/breadpad/python3"));
        assert!(!exe_matches_app("pad", "/usr/bin/bread-unrelated"));
    }
}
