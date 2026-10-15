//! Run a short helper command with a hard timeout and bounded output.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Most bytes of stdout kept from a helper command.
const MAX_OUTPUT: u64 = 512 * 1024;

/// Run `program args...`, returning stdout if it exits successfully within
/// `timeout`. A missing program, a failure, or a timeout all yield `None`;
/// breadbug treats every helper as optional.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    run_accepting(program, args, timeout, &[0])
}

/// Like [`run`] but also accepts the listed exit codes. `eu-stack` exits 1
/// when it printed some frames but hit a non-fatal unwinding error, which is
/// common for real processes.
pub fn run_accepting(
    program: &str,
    args: &[&str],
    timeout: Duration,
    ok_codes: &[i32],
) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().ok()?;
    status
        .filter(|s| s.code().is_some_and(|c| ok_codes.contains(&c)))
        .map(|_| String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout() {
        assert_eq!(
            run("echo", &["hi"], Duration::from_secs(2)).as_deref(),
            Some("hi\n")
        );
    }

    #[test]
    fn missing_program_is_none() {
        assert!(run("definitely-not-a-program-xyz", &[], Duration::from_secs(1)).is_none());
    }

    #[test]
    fn failure_is_none() {
        assert!(run("false", &[], Duration::from_secs(1)).is_none());
    }

    #[test]
    fn timeout_kills_and_returns_none() {
        let start = Instant::now();
        assert!(run("sleep", &["10"], Duration::from_millis(100)).is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
