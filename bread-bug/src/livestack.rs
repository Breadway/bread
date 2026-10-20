//! Optional stack capture of a frozen process, with the user's consent.
//!
//! Never automatic: `breadbug` asks (or the user passes `--live-stack`).
//! Only frames are kept: module path, module-relative offset, the module's
//! GNU build-id and an optional symbol name. Nothing from memory, registers
//! or the environment is read, parsed or serialized.
//!
//! The target is a recorded process *instance* (pid, kernel start time and
//! executable), not a pid or a `comm` prefix. It is re-validated right before
//! `eu-stack` attaches and once more when it returns, so a pid that was reused
//! by an unrelated program is refused with a reason.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::procid::{self, Change};
use crate::runcmd;
use crate::store::{Frame, ProcessIdentity};

const MAX_FRAMES: usize = 128;
const MAX_SYMBOL: usize = 512;

/// Why a live capture was not done. Each variant has a user-facing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The app's latest marker is not a freeze (it recovered, stopped, died,
    /// crashed, or never froze).
    NoActiveFreeze,
    /// The freeze marker has no recorded process identity, so the target
    /// cannot be proven.
    NoIdentity,
    /// The frozen process has exited.
    Gone { pid: u32 },
    /// The pid now belongs to a different process than the one that froze.
    Replaced { pid: u32 },
    /// The process runs an executable that is not this app's.
    WrongApp { pid: u32, app: String },
    /// The process was replaced while the stack was being read.
    ChangedDuringCapture { pid: u32 },
    /// `eu-stack` is missing, was denied, timed out or produced nothing.
    ToolFailed,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveFreeze => f.write_str(
                "the app's latest marker is not a freeze (it recovered, stopped or never froze)",
            ),
            Self::NoIdentity => f.write_str(
                "the freeze marker does not record which process froze, so it cannot be verified",
            ),
            Self::Gone { pid } => write!(f, "process {pid} has exited"),
            Self::Replaced { pid } => write!(
                f,
                "pid {pid} now belongs to a different process than the one that froze"
            ),
            Self::WrongApp { pid, app } => {
                write!(f, "process {pid} does not run the {app} executable")
            }
            Self::ChangedDuringCapture { pid } => write!(
                f,
                "process {pid} changed while its stack was being read; discarding the result"
            ),
            Self::ToolFailed => f.write_str(
                "eu-stack could not read the stack (is elfutils installed, and is ptrace allowed?)",
            ),
        }
    }
}

/// Check that `pid` is still the recorded instance of `app`.
pub fn check_target(app: &str, pid: u32, recorded: &ProcessIdentity) -> Result<(), Refusal> {
    check_target_in(Path::new("/proc"), app, pid, recorded)
}

fn check_target_in(
    proc_root: &Path,
    app: &str,
    pid: u32,
    recorded: &ProcessIdentity,
) -> Result<(), Refusal> {
    match procid::verify_in(proc_root, pid, recorded) {
        Ok(()) => {}
        Err(Change::Gone) => return Err(Refusal::Gone { pid }),
        Err(Change::Replaced) => return Err(Refusal::Replaced { pid }),
    }
    // The recorded exe is what must belong to the app; the live check above
    // already proved it is still the same.
    if !procid::exe_matches_app(app, &recorded.exe) {
        return Err(Refusal::WrongApp {
            pid,
            app: app.to_string(),
        });
    }
    Ok(())
}

/// The exact `eu-stack` arguments. Fixed on purpose: `-b -m` add module
/// path, build-id and module-relative offset; `-r` keeps raw symbol names.
/// Flags that would print more (`-a` activation, `-v`, `-s`, `-d`, `-i`) and
/// core-file input are never used.
fn eu_stack_args(pid: u32) -> Vec<String> {
    ["-b", "-m", "-r", "-n", "128", "-p"]
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(pid.to_string()))
        .collect()
}

fn run_eu_stack(pid: u32) -> Option<String> {
    let args = eu_stack_args(pid);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Exit 1 means some frames were shown with a non-fatal unwind error.
    runcmd::run_accepting("eu-stack", &args, Duration::from_secs(10), &[0, 1])
}

/// Capture the live stack of the recorded instance `pid` of `app`.
///
/// The instance is verified immediately before `eu-stack` runs and again
/// after it returns; a stack read from a process that changed in between is
/// discarded. (The kernel offers no way to bind `eu-stack -p` to a pidfd, so
/// a window of a few milliseconds remains between the check and the attach;
/// the second check detects, rather than prevents, a swap inside it.)
pub fn capture(app: &str, pid: u32, recorded: &ProcessIdentity) -> Result<Vec<Frame>, Refusal> {
    capture_with(Path::new("/proc"), app, pid, recorded, run_eu_stack)
}

fn capture_with(
    proc_root: &Path,
    app: &str,
    pid: u32,
    recorded: &ProcessIdentity,
    run: impl FnOnce(u32) -> Option<String>,
) -> Result<Vec<Frame>, Refusal> {
    check_target_in(proc_root, app, pid, recorded)?;
    let out = run(pid).ok_or(Refusal::ToolFailed)?;
    check_target_in(proc_root, app, pid, recorded)
        .map_err(|_| Refusal::ChangedDuringCapture { pid })?;
    let frames = parse_eu_stack(&out);
    if frames.is_empty() {
        return Err(Refusal::ToolFailed);
    }
    Ok(frames)
}

/// A frame whose first line has been read and whose detail line may follow.
struct Pending {
    pc: u64,
    symbol: Option<String>,
    module: Option<String>,
}

/// Parse `eu-stack -b -m -r` output into frames.
///
/// A frame is two lines:
///
/// ```text
/// #2  0x000055d9fa3e41c1 park - /usr/bin/breadpad
///     [465861337b535e0bb00b9893617849f61b998a42]@0x55d9fa3e3000+0x11c0
/// ```
///
/// The first gives the symbol (if any) and module path; the second gives the
/// module's build-id, its load address and the module-relative offset. The
/// absolute address on the first line is used only to cross-check the second
/// and is never stored. A frame without all three verified values (module,
/// build-id, module-relative offset) is omitted; nothing is invented for a
/// module without a GNU build-id or for output lacking module metadata.
/// Unrecognized lines are ignored.
pub fn parse_eu_stack(text: &str) -> Vec<Frame> {
    fn finish(frames: &mut Vec<Frame>, p: Pending, detail: Option<(String, u64, u64)>) {
        let mut frame = Frame {
            module: p.module,
            offset: None,
            build_id: None,
            symbol: p.symbol,
        };
        if let Some((build_id, base, offset)) = detail {
            // eu-stack subtracts one from return addresses before looking
            // them up, so the offset is the pc offset or one less. Anything
            // else means the two lines do not belong together.
            let delta = p.pc.wrapping_sub(base);
            if frame.module.is_some() && (delta == offset || delta == offset.wrapping_add(1)) {
                frame.offset = Some(offset);
                frame.build_id = Some(build_id);
            }
        }
        if frame.module.is_some() && frame.offset.is_some() && frame.build_id.is_some() {
            frames.push(frame);
        }
    }

    let mut frames = Vec::new();
    let mut pending: Option<Pending> = None;
    for line in text.lines() {
        if frames.len() >= MAX_FRAMES {
            pending = None;
            break;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('#') {
            if let Some(p) = pending.take() {
                finish(&mut frames, p, None);
                if frames.len() >= MAX_FRAMES {
                    break;
                }
            }
            pending = parse_frame_line(rest);
        } else if trimmed.starts_with('[') {
            if let Some(p) = pending.take() {
                finish(&mut frames, p, parse_module_line(trimmed));
            }
        } else if let Some(p) = pending.take() {
            // Any other line ends the frame without module details.
            finish(&mut frames, p, None);
        }
    }
    if let Some(p) = pending {
        if frames.len() < MAX_FRAMES {
            finish(&mut frames, p, None);
        }
    }
    frames
}

/// `N  0xPC [symbol[ + 0xdisp]] [- /module/path]`
fn parse_frame_line(rest: &str) -> Option<Pending> {
    let mut parts = rest.trim_start().splitn(2, char::is_whitespace);
    parts.next()?.parse::<u32>().ok()?;
    let tail = parts.next()?.trim();
    let (pc_text, tail) = match tail.split_once(char::is_whitespace) {
        Some((pc, tail)) => (pc, tail.trim()),
        None => (tail, ""),
    };
    let pc = u64::from_str_radix(pc_text.strip_prefix("0x")?, 16).ok()?;

    let (symbol_part, module_part) = if let Some(m) = tail.strip_prefix("- ") {
        ("", Some(m))
    } else if tail == "-" {
        ("", None)
    } else {
        match tail.split_once(" - ") {
            Some((s, m)) => (s, Some(m)),
            None => (tail, None),
        }
    };
    // Older eu-stack prints `symbol + 0xdisp`. A displacement from the start
    // of a function is not a module offset; it is dropped, never reused.
    let symbol = symbol_part
        .split(" + ")
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('('))
        .map(|s| truncate(s, MAX_SYMBOL));
    let module = module_part
        .map(str::trim)
        .filter(|m| m.starts_with('/') && !m.chars().any(char::is_control))
        .map(ToString::to_string);
    Some(Pending { pc, symbol, module })
}

/// `[build-id]@0xBASE+0xOFFSET`
fn parse_module_line(line: &str) -> Option<(String, u64, u64)> {
    let inner = line.strip_prefix('[')?;
    let (build_id, rest) = inner.split_once("]@")?;
    if !(8..=128).contains(&build_id.len())
        || build_id.len() % 2 != 0
        || !build_id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let (base, offset) = rest.trim().split_once('+')?;
    let base = u64::from_str_radix(base.strip_prefix("0x")?, 16).ok()?;
    let offset = u64::from_str_radix(offset.trim().strip_prefix("0x")?, 16).ok()?;
    Some((build_id.to_ascii_lowercase(), base, offset))
}

fn truncate(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests;
