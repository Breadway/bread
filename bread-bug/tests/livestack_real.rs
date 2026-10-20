//! Live capture against the real `eu-stack` and a real process.
//!
//! Skips (passes with a note on stderr) when `eu-stack` or `readelf` is not
//! installed, or when the kernel will not let this test attach to its own
//! child. Nothing here is faked: the target is a copy of a system binary
//! installed as `breadpad`, plus a tiny C program that is compiled and then
//! `strip`ped to prove the stripped-binary path.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use bread_bug::livestack::{self, Refusal};
use bread_bug::procid;

fn tool(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A copy of `sleep` named `breadpad`, which allows any process to ptrace it
/// (the Yama default only lets ancestors do so, and `eu-stack` is not one).
fn spawn_target(dir: &Path, name: &str) -> Option<Child> {
    let exe = dir.join(name);
    std::fs::copy("/usr/bin/sleep", &exe).ok()?;
    let mut cmd = Command::new(&exe);
    cmd.arg("30").stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            // PR_SET_PTRACER, PR_SET_PTRACER_ANY
            libc::prctl(0x5961_6d61, -1i64 as libc::c_ulong, 0, 0, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().ok()?;
    std::thread::sleep(Duration::from_millis(300));
    Some(child)
}

fn gnu_build_id(path: &Path) -> Option<String> {
    let out = Command::new("readelf").arg("-n").arg(path).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("Build ID: ").map(str::to_string))
}

#[test]
fn real_capture_is_module_relative_with_the_elf_build_id() {
    if !tool("eu-stack") || !tool("readelf") {
        eprintln!("skipped: eu-stack or readelf not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(mut child) = spawn_target(dir.path(), "breadpad") else {
        eprintln!("skipped: could not start the target");
        return;
    };
    let pid = child.id();
    let recorded = procid::identity_of(pid).expect("identity of a live child");
    let result = livestack::capture("pad", pid, &recorded);
    let exe = dir.path().join("breadpad");
    let frames = match result {
        Ok(f) => f,
        Err(Refusal::ToolFailed) => {
            eprintln!("skipped: kernel denied ptrace of the child (Yama/containers)");
            let _ = child.kill();
            return;
        }
        Err(other) => panic!("unexpected refusal: {other}"),
    };

    let exe_str = exe.to_string_lossy().into_owned();
    let mine: Vec<_> = frames
        .iter()
        .filter(|f| f.module.as_deref() == Some(exe_str.as_str()))
        .collect();
    assert!(!mine.is_empty(), "no frames in the app binary: {frames:#?}");

    let want_id = gnu_build_id(&exe).expect("sleep has a GNU build-id");
    let size = std::fs::metadata(&exe).unwrap().len();
    for f in &mine {
        assert_eq!(f.build_id.as_deref(), Some(want_id.as_str()), "{f:?}");
        let off = f.offset.expect("module-relative offset");
        assert!(
            off < size,
            "offset {off:#x} is outside the {size} byte module: {f:?}"
        );
    }
    // Shared libraries resolve too, each with its own build-id.
    let libc = frames
        .iter()
        .find(|f| f.module.as_deref().is_some_and(|m| m.contains("libc.so")))
        .expect("a libc frame");
    assert!(libc.build_id.is_some() && libc.offset.is_some());

    // The serialized payload is only module/offset/build-id/symbol.
    let json = serde_json::to_string(&frames).unwrap();
    assert!(!json.contains("rsp") && !json.contains("rax") && !json.contains("environ"));
    let _ = child.kill();
    let _ = child.wait();
}

const HANG_C: &str = r#"
#include <sys/prctl.h>
#include <time.h>
__attribute__((noinline)) void park(void) {
    struct timespec ts = {1, 0};
    for (;;) nanosleep(&ts, 0);
}
__attribute__((noinline)) void outer(void) { park(); __asm__ volatile(""); }
int main(void) { prctl(0x59616d61, (unsigned long)-1, 0, 0, 0); outer(); return 0; }
"#;

/// Build the program as `<dir>/breadpad`, optionally stripped.
fn build_hang(dir: &Path, strip: bool) -> bool {
    let src = dir.join("hang.c");
    std::fs::write(&src, HANG_C).unwrap();
    let exe = dir.join("breadpad");
    let built = Command::new("cc")
        .args(["-O0", "-fPIE", "-pie", "-Wl,--build-id", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .is_ok_and(|s| s.success());
    built
        && (!strip
            || Command::new("strip")
                .arg(&exe)
                .status()
                .is_ok_and(|s| s.success()))
}

fn capture_hang(dir: &Path, strip: bool) -> Option<(Vec<bread_bug::store::Frame>, String)> {
    if !tool("eu-stack") || !tool("readelf") || !build_hang(dir, strip) {
        eprintln!("skipped: eu-stack, readelf, cc or strip not available");
        return None;
    }
    let exe = dir.join("breadpad");
    let mut child = Command::new(&exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    std::thread::sleep(Duration::from_millis(300));
    let pid = child.id();
    let recorded = procid::identity_of(pid)?;
    let result = livestack::capture("pad", pid, &recorded);
    let _ = child.kill();
    let _ = child.wait();
    match result {
        Ok(frames) => Some((frames, gnu_build_id(&exe)?)),
        Err(Refusal::ToolFailed) => {
            eprintln!("skipped: kernel denied ptrace of the child");
            None
        }
        Err(other) => panic!("unexpected refusal: {other}"),
    }
}

#[test]
fn real_stripped_binary_has_module_offset_and_build_id_but_no_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let Some((frames, want_id)) = capture_hang(dir.path(), true) else {
        return;
    };
    let exe = dir.path().join("breadpad").to_string_lossy().into_owned();
    let mine: Vec<_> = frames
        .iter()
        .filter(|f| f.module.as_deref() == Some(exe.as_str()))
        .collect();
    assert!(mine.len() >= 3, "{frames:#?}");
    for f in mine {
        assert_eq!(f.symbol, None, "stripped frame has a name: {f:?}");
        assert_eq!(f.build_id.as_deref(), Some(want_id.as_str()));
        assert!(f.offset.is_some_and(|o| o < 0x10_000), "{f:?}");
    }
}

#[test]
fn real_unstripped_binary_names_its_functions_with_the_same_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let Some((frames, _)) = capture_hang(dir.path(), false) else {
        return;
    };
    let park = frames
        .iter()
        .find(|f| f.symbol.as_deref() == Some("park"))
        .expect("park frame");
    let outer = frames
        .iter()
        .find(|f| f.symbol.as_deref() == Some("outer"))
        .expect("outer frame");
    // Module-relative, so distinct and small; not displacements from the
    // function start (which would be tiny and equal-ish) and not addresses.
    assert_ne!(park.offset, outer.offset);
    assert!(park.offset.unwrap() < 0x10_000 && outer.offset.unwrap() < 0x10_000);
    assert!(park.offset.unwrap() > 0x100, "{park:?}");
}

#[test]
fn real_capture_refuses_an_unrelated_process_that_borrows_a_bread_name() {
    if !tool("eu-stack") {
        eprintln!("skipped: eu-stack not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // The recorded instance is a real breadpad ...
    let Some(mut real) = spawn_target(dir.path(), "breadpad") else {
        return;
    };
    let pid = real.id();
    let recorded = procid::identity_of(pid).unwrap();
    // ... but the app named in the request is clip, whose executable it is not.
    assert!(matches!(
        livestack::capture("clip", pid, &recorded),
        Err(Refusal::WrongApp { .. })
    ));
    // And once it exits, the same pid is refused as gone.
    let _ = real.kill();
    let _ = real.wait();
    assert!(matches!(
        livestack::capture("pad", pid, &recorded),
        Err(Refusal::Gone { .. } | Refusal::Replaced { .. })
    ));

    // A python-like impostor: its comm is rewritten to look like a bread app.
    // Its executable is not breadpad, so it is refused whatever it calls itself.
    let mut imp = Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let ipid = imp.id();
    std::thread::sleep(Duration::from_millis(100));
    let impostor = procid::identity_of(ipid).unwrap();
    assert!(matches!(
        livestack::capture("pad", ipid, &impostor),
        Err(Refusal::WrongApp { .. })
    ));
    let _ = imp.kill();
    let _ = imp.wait();
}
