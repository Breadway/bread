//! Optional stack capture of a frozen process, with the user's consent.
//!
//! Never automatic: `breadbug` asks (or the user passes `--live-stack`).
//! Only frames are kept: symbol names and offsets, nothing from memory.
//! The pid comes from the freeze marker and is re-checked, because pids are
//! reused: the process must still exist and be a `bread*` binary before
//! anything attaches to it.

use std::path::Path;
use std::time::Duration;

use crate::runcmd;
use crate::store::Frame;

const MAX_FRAMES: usize = 128;

/// Whether `pid` is alive and its command name starts with `bread`.
pub fn is_bread_process(pid: u32) -> bool {
    is_bread_process_in(Path::new("/proc"), pid)
}

fn is_bread_process_in(proc_root: &Path, pid: u32) -> bool {
    std::fs::read_to_string(proc_root.join(pid.to_string()).join("comm"))
        .is_ok_and(|c| c.trim().starts_with("bread"))
}

/// Parse `eu-stack` output into frames (first thread's `#N` lines, all
/// threads in order). Lines look like `#0  0x00007f3a1c2d4a3d __poll + 0x1d`.
pub fn parse_eu_stack(text: &str) -> Vec<Frame> {
    let mut frames = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix('#') else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        if parts.next().and_then(|n| n.parse::<u32>().ok()).is_none() {
            continue;
        }
        let Some(addr) = parts.next().filter(|a| a.starts_with("0x")) else {
            continue;
        };
        let rest: Vec<&str> = parts.collect();
        let (symbol, offset) = match rest.iter().position(|t| *t == "+") {
            Some(i) => (
                rest[..i].join(" "),
                rest.get(i + 1)
                    .and_then(|o| o.strip_prefix("0x"))
                    .and_then(|o| u64::from_str_radix(o, 16).ok()),
            ),
            None => (rest.join(" "), None),
        };
        let symbol = (!symbol.is_empty() && !symbol.starts_with('(')).then_some(symbol);
        frames.push(Frame {
            module: None,
            offset: offset.or_else(|| u64::from_str_radix(&addr[2..], 16).ok()),
            build_id: None,
            symbol,
        });
        if frames.len() >= MAX_FRAMES {
            break;
        }
    }
    frames
}

/// Capture the live stack of `pid` with `eu-stack`. `None` if the process
/// is not a bread process, `eu-stack` is missing, or it times out.
pub fn capture(pid: u32) -> Option<Vec<Frame>> {
    if !is_bread_process(pid) {
        return None;
    }
    let out = runcmd::run(
        "eu-stack",
        &["-p", &pid.to_string()],
        Duration::from_secs(10),
    )?;
    let frames = parse_eu_stack(&out);
    (!frames.is_empty()).then_some(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_symbols_and_offsets() {
        let text = "PID 1234 - process\nTID 1234:\n\
                    #0  0x00007f3a1c2d4a3d __poll + 0x1d\n\
                    #1  0x00007f3a1c1b2d8e g_main_context_iterate.constprop.0\n\
                    #2  0x000055b3c4a1b2c3 (unknown)\n\
                    not a frame\n";
        let f = parse_eu_stack(text);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].symbol.as_deref(), Some("__poll"));
        assert_eq!(f[0].offset, Some(0x1d));
        assert_eq!(
            f[1].symbol.as_deref(),
            Some("g_main_context_iterate.constprop.0")
        );
        assert_eq!(f[1].offset, Some(0x7f3a1c1b2d8e));
        assert_eq!(f[2].symbol, None);
    }

    #[test]
    fn caps_frames() {
        let text: String = (0..500).map(|i| format!("#{i}  0x10 f\n")).collect();
        assert_eq!(parse_eu_stack(&text).len(), MAX_FRAMES);
    }

    #[test]
    fn only_bread_processes_qualify() {
        let dir = tempfile::tempdir().unwrap();
        for (pid, comm) in [(1, "breadclip\n"), (2, "firefox\n")] {
            std::fs::create_dir_all(dir.path().join(pid.to_string())).unwrap();
            std::fs::write(dir.path().join(format!("{pid}/comm")), comm).unwrap();
        }
        assert!(is_bread_process_in(dir.path(), 1));
        assert!(!is_bread_process_in(dir.path(), 2));
        assert!(!is_bread_process_in(dir.path(), 3));
        assert!(capture(std::process::id()).is_none());
    }
}
