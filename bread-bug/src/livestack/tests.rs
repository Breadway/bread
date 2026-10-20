use std::cell::Cell;

use super::*;
use crate::procid::{identity_in, testutil::fake_proc};

// Real `eu-stack -b -m -r` output (elfutils 0.196), recorded from a C program
// run as a child and normalized to pid 4242 and /opt/breadpad/. One build has
// symbols, one is the same binary after `strip`, one was linked without a
// build-id.
const PIE: &str = include_str!("fixtures/eu-stack-pie-symbols.txt");
const STRIPPED: &str = include_str!("fixtures/eu-stack-pie-stripped.txt");
const NO_BUILD_ID: &str = include_str!("fixtures/eu-stack-no-build-id.txt");

const LIBC_ID: &str = "d320029ad1a23be2f4bce117fd321cbd23a2014b";
const APP_ID: &str = "465861337b535e0bb00b9893617849f61b998a42";

fn app_frames(frames: &[Frame]) -> Vec<&Frame> {
    frames
        .iter()
        .filter(|f| f.module.as_deref() == Some("/opt/breadpad/hang_pie"))
        .collect()
}

#[test]
fn frames_are_module_relative_with_build_id() {
    let frames = parse_eu_stack(PIE);
    assert_eq!(frames.len(), 8);

    let park = &frames[2];
    assert_eq!(park.symbol.as_deref(), Some("park"));
    assert_eq!(park.module.as_deref(), Some("/opt/breadpad/hang_pie"));
    assert_eq!(park.build_id.as_deref(), Some(APP_ID));
    // 0x55f9cb2901c1 (return address) - 0x55f9cb28f000 (load base) - 1.
    assert_eq!(park.offset, Some(0x11c0));

    let sleep = &frames[0];
    assert_eq!(sleep.module.as_deref(), Some("/usr/lib/libc.so.6"));
    assert_eq!(sleep.build_id.as_deref(), Some(LIBC_ID));
    // Frame 0 is the exact pc, not a return address: no -1 adjustment.
    assert_eq!(sleep.offset, Some(0xe4a6b));
}

#[test]
fn no_frame_carries_a_virtual_address() {
    for text in [PIE, STRIPPED, NO_BUILD_ID] {
        for f in parse_eu_stack(text) {
            if let Some(off) = f.offset {
                // Every absolute address in the fixtures is 0x4xxx_xxxx_xxxx
                // or higher; module offsets here are all below 1 MiB-ish.
                assert!(off < 0x10_0000, "offset {off:#x} looks like an address");
            }
        }
    }
}

#[test]
fn stripped_binary_keeps_module_and_offset_but_no_symbol() {
    let frames = parse_eu_stack(STRIPPED);
    let app: Vec<_> = frames
        .iter()
        .filter(|f| f.module.as_deref() == Some("/opt/breadpad/hang_strip"))
        .collect();
    assert_eq!(app.len(), 4, "{frames:#?}");
    for f in &app {
        assert_eq!(f.symbol, None, "stripped frames have no symbol");
        assert_eq!(f.build_id.as_deref(), Some(APP_ID));
    }
    assert_eq!(app[0].offset, Some(0x11c0));
    assert_eq!(app[1].offset, Some(0x11cb));
    assert_eq!(app[2].offset, Some(0x129c));
    // Symbols from the (unstripped) libc survive in the same trace.
    assert_eq!(
        frames[0].symbol.as_deref(),
        Some("clock_nanosleep@GLIBC_2.2.5")
    );
}

#[test]
fn module_without_build_id_gets_no_offset_and_no_invented_one() {
    let frames = parse_eu_stack(NO_BUILD_ID);
    assert!(frames.iter().all(|f| f.symbol.as_deref() != Some("park")));
    // The shared libc frames in the same trace are still fully resolved.
    assert_eq!(frames[0].build_id.as_deref(), Some(LIBC_ID));
    assert_eq!(frames[0].offset, Some(0xe4a6b));
}

#[test]
fn default_output_without_module_info_yields_no_offset() {
    // `eu-stack -p` with no flags: absolute pcs only, optional `+ disp`.
    let text = "PID 1234 - process\nTID 1234:\n\
                #0  0x00007f3a1c2d4a3d __poll + 0x1d\n\
                #1  0x00007f3a1c1b2d8e g_main_context_iterate.constprop.0\n\
                #2  0x000055b3c4a1b2c3 (unknown)\n\
                #3  0x000055b3c4a1b2c8\n";
    let f = parse_eu_stack(text);
    // No module metadata: absolute pcs and function displacements are never
    // substituted, and incomplete frames are not serialized.
    assert!(f.is_empty());
}

#[test]
fn a_detail_line_that_does_not_match_its_frame_is_ignored() {
    let text = "#0  0x00007f0000001000 foo - /usr/lib/libx.so\n\
                \x20   [aabbccddeeff00112233]@0x7f0000000000+0x5\n";
    // pc - base = 0x1000, not 0x5 and not 0x6: the lines do not belong
    // together, so nothing is invented.
    let f = parse_eu_stack(text);
    assert!(f.is_empty());
}

#[test]
fn malformed_build_ids_and_modules_are_rejected() {
    let text = "#0  0x00007f0000001001 foo - /usr/lib/libx.so\n\
                \x20   [nothex!!]@0x7f0000000000+0x1000\n\
                #1  0x00007f0000001001 bar - relative/path.so\n\
                \x20   [aabbccddeeff00112233]@0x7f0000000000+0x1000\n";
    let f = parse_eu_stack(text);
    assert!(f.is_empty());
}

#[test]
fn frame_count_is_capped() {
    let text: String = (0..500)
        .map(|i| {
            format!(
                "#{i}  0x00007f0000001001 f - /usr/lib/libx.so\n    [aabbccddeeff00112233]@0x7f0000000000+0x1000\n"
            )
        })
        .collect();
    assert_eq!(parse_eu_stack(&text).len(), MAX_FRAMES);
}

#[test]
fn memory_registers_and_environment_never_reach_the_payload() {
    // Hostile or verbose tool output: register dumps, memory words, an
    // environment-looking line, a core-file hint. Only the `#N` frame lines
    // and their bracket detail lines may contribute anything.
    let text = "PID 4242 - process\nTID 4242:\n\
        rax 0x00000000deadbeef rbx 0x4141414141414141\n\
        rip 0x00007f0000001001 rsp 0x00007ffc00000000\n\
        0x00007ffc00000010: 48454150 5f534543 52455453\n\
        ENV_SECRET_SENTINEL=ENV_SECRET_SENTINEL_VALUE_77\n\
        HEAP_SECRET_SENTINEL_9f3a\n\
        #0  0x00007f0000001001 work - /usr/lib/libx.so\n\
        \x20   [aabbccddeeff00112233]@0x7f0000000000+0x1000\n\
        locals: password=hunter2\n";
    let frames = parse_eu_stack(text);
    assert_eq!(frames.len(), 1);
    let json = serde_json::to_string(&frames).unwrap();
    for needle in [
        "deadbeef",
        "4141414141414141",
        "rax",
        "rsp",
        "SENTINEL",
        "HEAP_SECRET",
        "hunter2",
        "48454150",
    ] {
        assert!(!json.contains(needle), "{needle} leaked: {json}");
    }
}

#[test]
fn a_frame_has_exactly_four_fields_and_nothing_else_can_be_added_silently() {
    // Exhaustive destructuring: adding a field (memory, registers, ...) to
    // `Frame` stops this compiling and forces a privacy review.
    let Frame {
        module,
        offset,
        build_id,
        symbol,
    } = Frame::default();
    assert!((module, offset, build_id, symbol) == (None, None, None, None));

    let f = parse_eu_stack(PIE).remove(2);
    let v = serde_json::to_value(&f).unwrap();
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["build_id", "module", "offset", "symbol"]);
}

#[test]
fn eu_stack_is_invoked_with_a_fixed_frames_only_argument_list() {
    let args = eu_stack_args(4242);
    assert_eq!(args, ["-b", "-m", "-r", "-n", "128", "-p", "4242"]);
    // No activation records, no source, no debuginfo lookups, no core files.
    for banned in ["-a", "-v", "-s", "-d", "-i", "--core", "-e"] {
        assert!(
            !args.iter().any(|a| a == banned),
            "{banned} must not be used"
        );
    }
}

// ---------------------------------------------------------------- identity

const APP_EXE: &str = "/usr/bin/breadpad";

fn recorded(root: &Path) -> ProcessIdentity {
    identity_in(root, 10).unwrap()
}

#[test]
fn matching_instance_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    fake_proc(dir.path(), 10, "breadpad", 'S', 500, APP_EXE);
    let rec = recorded(dir.path());
    assert_eq!(check_target_in(dir.path(), "pad", 10, &rec), Ok(()));
}

#[test]
fn comm_prefix_is_not_an_identity() {
    // The reviewer's case: an unrelated program that set comm to
    // "bread-unrelated". Whether the marker recorded that program (a client
    // claiming to be pad) or recorded the real app and the pid was reused,
    // capture must be refused.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fake_proc(root, 10, "bread-unrelated", 'S', 500, "/usr/bin/python3.14");
    let claims_to_be_pad = recorded(root);
    assert_eq!(
        check_target_in(root, "pad", 10, &claims_to_be_pad),
        Err(Refusal::WrongApp {
            pid: 10,
            app: "pad".into()
        })
    );

    fake_proc(root, 10, "breadpad", 'S', 500, APP_EXE);
    let real = recorded(root);
    fake_proc(root, 10, "bread-unrelated", 'S', 900, "/usr/bin/python3.14");
    assert_eq!(
        check_target_in(root, "pad", 10, &real),
        Err(Refusal::Replaced { pid: 10 })
    );
}

#[test]
fn same_app_with_a_reused_pid_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fake_proc(root, 10, "breadpad", 'S', 500, APP_EXE);
    let rec = recorded(root);
    // pad was restarted and the new instance got the same pid.
    fake_proc(root, 10, "breadpad", 'S', 777, APP_EXE);
    assert_eq!(
        check_target_in(root, "pad", 10, &rec),
        Err(Refusal::Replaced { pid: 10 })
    );
}

#[test]
fn another_apps_executable_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fake_proc(root, 10, "breadclip", 'S', 500, "/usr/bin/breadclip");
    let rec = recorded(root);
    assert_eq!(
        check_target_in(root, "pad", 10, &rec),
        Err(Refusal::WrongApp {
            pid: 10,
            app: "pad".into()
        })
    );
}

#[test]
fn dead_and_zombie_processes_are_gone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fake_proc(root, 10, "breadpad", 'S', 500, APP_EXE);
    let rec = recorded(root);
    fake_proc(root, 10, "breadpad", 'Z', 500, APP_EXE);
    assert_eq!(
        check_target_in(root, "pad", 10, &rec),
        Err(Refusal::Gone { pid: 10 })
    );
    assert_eq!(
        check_target_in(root, "pad", 11, &rec),
        Err(Refusal::Gone { pid: 11 })
    );
}

#[test]
fn the_tool_is_not_run_when_the_target_does_not_match() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fake_proc(root, 10, "breadpad", 'S', 500, APP_EXE);
    let rec = recorded(root);
    fake_proc(root, 10, "bread-unrelated", 'S', 900, "/usr/bin/python3.14");
    let ran = Cell::new(false);
    let r = capture_with(root, "pad", 10, &rec, |_| {
        ran.set(true);
        Some(PIE.to_string())
    });
    assert_eq!(r, Err(Refusal::Replaced { pid: 10 }));
    assert!(
        !ran.get(),
        "eu-stack must not attach to a mismatched target"
    );
}

#[test]
fn a_swap_between_the_check_and_the_result_discards_the_frames() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fake_proc(root, 10, "breadpad", 'S', 500, APP_EXE);
    let rec = recorded(root);
    let r = capture_with(root, "pad", 10, &rec, |pid| {
        // The process exits and an unrelated one takes the pid while the
        // stack is being read.
        fake_proc(
            root,
            pid,
            "bread-unrelated",
            'S',
            900,
            "/usr/bin/python3.14",
        );
        Some(PIE.to_string())
    });
    assert_eq!(r, Err(Refusal::ChangedDuringCapture { pid: 10 }));
}

#[test]
fn a_verified_target_returns_the_parsed_frames() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fake_proc(root, 10, "breadpad", 'S', 500, APP_EXE);
    let rec = recorded(root);
    let frames = capture_with(root, "pad", 10, &rec, |pid| {
        assert_eq!(pid, 10);
        Some(PIE.to_string())
    })
    .unwrap();
    assert_eq!(app_frames(&frames).len(), 4);
}

#[test]
fn tool_failure_and_empty_output_are_reported_not_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fake_proc(root, 10, "breadpad", 'S', 500, APP_EXE);
    let rec = recorded(root);
    assert_eq!(
        capture_with(root, "pad", 10, &rec, |_| None),
        Err(Refusal::ToolFailed)
    );
    assert_eq!(
        capture_with(root, "pad", 10, &rec, |_| Some("PID 10 - process\n".into())),
        Err(Refusal::ToolFailed)
    );
}

#[test]
fn every_refusal_has_a_clear_message() {
    for (r, needle) in [
        (Refusal::NoActiveFreeze, "not a freeze"),
        (Refusal::NoIdentity, "cannot be verified"),
        (Refusal::Gone { pid: 7 }, "process 7 has exited"),
        (Refusal::Replaced { pid: 7 }, "different process"),
        (
            Refusal::WrongApp {
                pid: 7,
                app: "pad".into(),
            },
            "does not run the pad executable",
        ),
        (Refusal::ChangedDuringCapture { pid: 7 }, "discarding"),
        (Refusal::ToolFailed, "eu-stack"),
    ] {
        assert!(r.to_string().contains(needle), "{r}");
    }
}
