use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[test]
fn reopened_store_reserves_markers_across_apps_and_rotations() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(dir.path(), StoreLimits::default(), &[]);
        store
            .append_log("clip", "info", "remote 203.0.113.42", 10)
            .unwrap();
        store
            .append_log(DAEMON_APP, "warn", "remote 198.51.100.8", 11)
            .unwrap();
    }
    let clip = dir.path().join("apps/clip");
    fs::rename(clip.join(LOG_FILE), clip.join(format!("{LOG_FILE}.1"))).unwrap();
    let store = open(dir.path(), StoreLimits::default(), &[]);
    store
        .append_log("clip", "info", "remote 192.0.2.9", 12)
        .unwrap();
    let snapshot = Reader::open(dir.path()).snapshot("clip", 10, 0).unwrap();
    assert_eq!(snapshot.logs[0].msg, "remote <IP:1>");
    assert_eq!(snapshot.logs[1].msg, "remote <IP:3>");
}

fn ctx() -> ScrubContext {
    ScrubContext {
        home: Some("/home/alicesmith".into()),
        username: Some("alicesmith".into()),
        hostname: Some("thinkbox-9".into()),
        extra_literals: vec![],
    }
}

fn open(dir: &Path, limits: StoreLimits, excluded: &[&str]) -> Store {
    let cfg = BugConfig {
        store: limits,
        excluded_apps: excluded.iter().map(|s| s.to_string()).collect(),
        ..BugConfig::default()
    };
    Store::open(dir, &cfg, ctx()).unwrap()
}

fn all_bytes(root: &Path) -> String {
    let mut out = String::new();
    for app in fs::read_dir(root.join("apps")).unwrap().flatten() {
        for f in fs::read_dir(app.path()).unwrap().flatten() {
            out.push_str(&fs::read_to_string(f.path()).unwrap());
        }
    }
    out
}

#[test]
fn log_roundtrip_and_scrubbed_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    s.append_log(
        "clip",
        "warn",
        "failed for alicesmith at /home/alicesmith/x from 203.0.113.42",
        10,
    )
    .unwrap();
    let on_disk = all_bytes(dir.path());
    assert!(!on_disk.contains("alicesmith"), "{on_disk}");
    assert!(!on_disk.contains("203.0.113.42"), "{on_disk}");
    let snap = Reader::open(dir.path()).snapshot("clip", 10, 10).unwrap();
    assert_eq!(snap.logs.len(), 1);
    assert_eq!(snap.logs[0].level, "WARN");
    assert_eq!(
        snap.logs[0].msg,
        "failed for <USER> at <HOME>/x from <IP:1>"
    );
}

#[test]
fn files_and_dirs_are_private() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    s.append_log("clip", "info", "hello", 1).unwrap();
    let app = dir.path().join("apps/clip");
    assert_eq!(
        fs::metadata(&app).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(app.join(LOG_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn bad_app_ids_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    for bad in ["", "../etc", "a/b", "Clip", "1abc", "a b", &"x".repeat(33)] {
        assert!(s.append_log(bad, "info", "x", 1).is_err(), "{bad:?}");
        assert!(s.wipe_app(bad).is_err(), "{bad:?}");
    }
    assert!(Reader::open(dir.path()).snapshot("../x", 1, 1).is_err());
}

#[test]
fn rotation_keeps_two_generations_and_latest_lines() {
    let dir = tempfile::tempdir().unwrap();
    let limits = StoreLimits {
        max_bytes_per_file: 600,
        ..StoreLimits::default()
    };
    let s = open(dir.path(), limits, &[]);
    for i in 0..200u64 {
        s.append_log("clip", "info", &format!("line number {i}"), i)
            .unwrap();
    }
    let files: Vec<_> = fs::read_dir(dir.path().join("apps/clip"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    let snap = Reader::open(dir.path()).snapshot("clip", 1000, 0).unwrap();
    assert_eq!(snap.logs.last().unwrap().msg, "line number 199");
    assert!(snap.logs.len() < 200);
    let usage = s.usage().unwrap();
    assert!(usage.total_bytes <= 2 * 700, "{usage:?}");
}

#[test]
fn snapshot_tail_limits() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    for i in 0..50u64 {
        s.append_log("clip", "info", &format!("m{i}"), i).unwrap();
    }
    let snap = Reader::open(dir.path()).snapshot("clip", 5, 5).unwrap();
    let msgs: Vec<_> = snap.logs.iter().map(|l| l.msg.as_str()).collect();
    assert_eq!(msgs, ["m45", "m46", "m47", "m48", "m49"]);
}

#[test]
fn unreadable_app_fails_closed_and_usage_reports_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let s = open(
        dir.path(),
        StoreLimits {
            max_bytes_per_file: 100_000,
            max_total_bytes: 1000,
            ..StoreLimits::default()
        },
        &[],
    );
    s.append_log("pad", "info", &"boring message ".repeat(30), 1)
        .unwrap();
    s.append_log("clip", "info", "seed", 2).unwrap();
    let app = dir.path().join("apps/pad");
    fs::set_permissions(&app, fs::Permissions::from_mode(0o000)).unwrap();
    // Permission probes must run unprivileged, never silently pass as root.
    let unreadable = fs::read_dir(&app).is_err();
    let usage = s.usage();
    let first = s.append_log("clip", "info", &"boring message ".repeat(30), 3);
    let second = s.append_log("clip", "info", &"boring message ".repeat(30), 4);
    fs::set_permissions(&app, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        unreadable,
        "permission regression requires an unprivileged test runner"
    );
    assert!(usage.is_err());
    assert!(first.is_err());
    assert!(second.is_err());
    assert!(s.usage().unwrap().total_bytes <= 1000);
    assert_eq!(
        Reader::open(dir.path())
            .snapshot("clip", 10, 0)
            .unwrap()
            .logs
            .len(),
        1
    );
}

#[test]
fn failed_eviction_does_not_allow_append() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let s = open(
        dir.path(),
        StoreLimits {
            max_bytes_per_file: 100_000,
            max_total_bytes: 600,
            ..StoreLimits::default()
        },
        &[],
    );
    s.append_log("pad", "info", &"boring message ".repeat(30), 1)
        .unwrap();
    s.append_log("clip", "info", "seed", 2).unwrap();
    let app = dir.path().join("apps/pad");
    // Filesystem timestamp resolution can tie consecutive writes. Establish
    // the unwritable pad file as the oldest victim, without relying on sleep.
    let pad_file = app.join("log.jsonl");
    fs::File::open(&pad_file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    assert!(
        fs::metadata(&pad_file).unwrap().modified().unwrap()
            < fs::metadata(dir.path().join("apps/clip/log.jsonl"))
                .unwrap()
                .modified()
                .unwrap()
    );
    fs::set_permissions(&app, fs::Permissions::from_mode(0o500)).unwrap();
    let result = s.append_log("clip", "info", &"boring message ".repeat(30), 3);
    fs::set_permissions(&app, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        result.is_err(),
        "eviction failure must propagate (requires unprivileged runner)"
    );
    assert!(s.usage().unwrap().total_bytes <= 600);
    assert_eq!(
        Reader::open(dir.path())
            .snapshot("clip", 10, 0)
            .unwrap()
            .logs
            .len(),
        1
    );
}

#[test]
fn total_cap_drops_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let limits = StoreLimits {
        max_bytes_per_file: 400,
        max_total_bytes: 1500,
        ..StoreLimits::default()
    };
    let s = open(dir.path(), limits, &[]);
    for i in 0..300u64 {
        let app = ["clip", "pad", "bar"][(i % 3) as usize];
        s.append_log(app, "info", &format!("payload line {i} xxxxxxxxxxxx"), i)
            .unwrap();
        assert!(s.usage().unwrap().total_bytes <= 1500);
    }
    let total = s.usage().unwrap().total_bytes;
    assert!(total <= 1500, "total {total}");
}

#[test]
fn total_cap_is_enforced_without_rotation_and_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let limits = StoreLimits {
        max_bytes_per_file: 100_000,
        max_total_bytes: 1000,
        ..StoreLimits::default()
    };
    {
        let s = open(dir.path(), limits.clone(), &[]);
        for i in 0..50 {
            assert!(s
                .append_log("clip", "info", &"boring message ".repeat(30), i)
                .unwrap());
            assert!(s.usage().unwrap().total_bytes <= 1000);
        }
        assert!(!Reader::open(dir.path())
            .snapshot("clip", 100, 0)
            .unwrap()
            .logs
            .is_empty());
    }
    let smaller = StoreLimits {
        max_total_bytes: 100,
        ..limits
    };
    let s = open(dir.path(), smaller, &[]);
    assert!(s.usage().unwrap().total_bytes <= 100);
}

#[test]
fn oversized_serialized_records_are_dropped_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let limits = StoreLimits {
        max_bytes_per_file: 100,
        max_total_bytes: 100,
        ..StoreLimits::default()
    };
    let s = open(dir.path(), limits, &[]);
    // Escaping, not just the input length, counts towards the limit.
    assert!(!s.append_log("clip", "info", &"\"".repeat(90), 1).unwrap());
    assert!(!s
        .append_event("clip", &"x".repeat(128), &json!({}), 2)
        .unwrap());
    let mut marker = Marker::new(MarkerKind::Crash, 2);
    marker.message = Some("x".repeat(100));
    assert!(!s.record_marker("clip", marker).unwrap());
    assert_eq!(s.usage().unwrap().total_bytes, 0);
    assert!(s.append_log("clip", "info", "small", 3).unwrap());
    assert!(s.usage().unwrap().total_bytes <= 100);
}

#[test]
fn mixed_records_and_lowered_file_caps_stay_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let limits = StoreLimits {
        max_bytes_per_file: 500,
        max_total_bytes: 900,
        ..StoreLimits::default()
    };
    {
        let s = open(dir.path(), limits.clone(), &[]);
        for i in 0..100 {
            let app = ["clip", "pad", "bar"][(i % 3) as usize];
            s.append_log(app, "info", "message", i).unwrap();
            assert!(s.usage().unwrap().total_bytes <= 900);
            s.append_event(
                app,
                "bread.power.ac.connected",
                &json!({"ac_connected": true}),
                i,
            )
            .unwrap();
            assert!(s.usage().unwrap().total_bytes <= 900);
            s.record_marker(app, Marker::new(MarkerKind::Crash, i))
                .unwrap();
            assert!(s.usage().unwrap().total_bytes <= 900);
            for app in fs::read_dir(dir.path().join("apps")).unwrap() {
                for file in fs::read_dir(app.unwrap().path()).unwrap() {
                    assert!(file.unwrap().metadata().unwrap().len() <= 500);
                }
            }
        }
    }
    let s = open(
        dir.path(),
        StoreLimits {
            max_bytes_per_file: 20,
            ..limits
        },
        &[],
    );
    for app in fs::read_dir(dir.path().join("apps")).unwrap() {
        for file in fs::read_dir(app.unwrap().path()).unwrap() {
            assert!(file.unwrap().metadata().unwrap().len() <= 20);
        }
    }
    assert!(s.usage().unwrap().total_bytes <= 900);
}

#[test]
fn excluded_apps_store_nothing_and_are_wiped_when_added() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &["search"]);
    assert!(!s.append_log("search", "info", "q", 1).unwrap());
    assert!(!dir.path().join("apps/search").exists());
    assert!(s.append_log("clip", "info", "x", 1).unwrap());
    s.set_excluded(&["clip".to_string()]).unwrap();
    assert!(!dir.path().join("apps/clip").exists());
    assert!(!s.append_log("clip", "info", "x", 2).unwrap());
}

#[test]
fn exclusion_in_config_wipes_old_data_on_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = open(dir.path(), StoreLimits::default(), &[]);
        s.append_log("clip", "info", "secret stuff", 1).unwrap();
    }
    let _s = open(dir.path(), StoreLimits::default(), &["clip"]);
    assert!(!dir.path().join("apps/clip").exists());
}

#[test]
fn events_drop_unlisted_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    s.append_event(
        DAEMON_APP,
        "bread.window.opened",
        &json!({"title": "Inbox - Gmail", "class": "firefox"}),
        1,
    )
    .unwrap();
    s.append_event(
        DAEMON_APP,
        "bread.power.ac.connected",
        &json!({"ac_connected": true, "battery_percent": 80, "owner": "alicesmith"}),
        2,
    )
    .unwrap();
    let on_disk = all_bytes(dir.path());
    assert!(!on_disk.contains("Gmail"));
    assert!(!on_disk.contains("firefox"));
    assert!(!on_disk.contains("alicesmith"));
    let snap = Reader::open(dir.path())
        .snapshot(DAEMON_APP, 0, 10)
        .unwrap();
    assert_eq!(snap.events[0].data["_dropped"], json!(true));
    assert_eq!(
        snap.events[1].data,
        json!({"ac_connected": true, "battery_percent": 80})
    );
}

#[test]
fn markers_are_scrubbed_but_build_ids_survive() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    let bid = "3f2a9c1de4b5a6978081726354453423a1b2c3d4";
    let mut m = Marker::new(MarkerKind::Panic, 5);
    m.message = Some("boom for alicesmith".into());
    m.location = Some("/home/alicesmith/p/src/lib.rs:4:2".into());
    m.build_id = Some(bid.into());
    m.frames = vec![
        Frame {
            module: Some("/home/alicesmith/.local/bin/breadclip".into()),
            offset: Some(0x1a2b),
            build_id: Some(bid.into()),
            symbol: None,
        },
        Frame {
            module: None,
            offset: None,
            build_id: Some("not a build id".into()),
            symbol: Some("core::panicking::panic".into()),
        },
    ];
    s.record_marker("clip", m).unwrap();
    let on_disk = all_bytes(dir.path());
    assert!(!on_disk.contains("alicesmith"), "{on_disk}");
    let got = Reader::open(dir.path()).last_marker("clip").unwrap();
    assert_eq!(got.kind, MarkerKind::Panic);
    assert_eq!(got.build_id.as_deref(), Some(bid));
    assert_eq!(got.frames[0].build_id.as_deref(), Some(bid));
    assert_eq!(
        got.frames[0].module.as_deref(),
        Some("<HOME>/.local/bin/breadclip")
    );
    assert_eq!(got.frames[1].build_id, None);
    assert_eq!(got.location.as_deref(), Some("<HOME>/p/src/lib.rs:4:2"));
}

#[test]
fn frames_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    let mut m = Marker::new(MarkerKind::Crash, 1);
    m.frames = vec![Frame::default(); 1000];
    s.record_marker("clip", m).unwrap();
    let got = Reader::open(dir.path()).last_marker("clip").unwrap();
    assert_eq!(got.frames.len(), MAX_FRAMES);
}

#[test]
fn process_identity_is_preserved_locally_but_removed_from_reports() {
    use crate::report::{self, BuildOptions, ReportType};
    use serde_json::Value;

    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    let mut marker = Marker::new(MarkerKind::Freeze, 10);
    marker.pid = Some(4242);
    marker.process = Some(ProcessIdentity {
        start_ticks: 123456,
        exe: "/usr/bin/breadpad".into(),
        boot_id: Some("boot-id-for-local-matching".into()),
    });
    s.record_marker("pad", marker).unwrap();

    let local = Reader::open(dir.path()).last_marker("pad").unwrap();
    assert_eq!(local.pid, Some(4242));
    assert_eq!(local.process.as_ref().unwrap().start_ticks, 123456);
    assert_eq!(local.process.as_ref().unwrap().exe, "/usr/bin/breadpad");

    let mut opts = BuildOptions::new("pad", ReportType::Freeze);
    opts.store_root = dir.path().to_path_buf();
    opts.include_journal = false;
    opts.now_unix = 20;
    let prepared = report::build(&opts).unwrap();
    let json: Value = serde_json::from_slice(&prepared.bytes).unwrap();
    let marker = &json["markers"][0];
    assert!(marker.get("pid").is_none());
    assert!(marker.get("process").is_none());
    assert!(!prepared.bytes.windows(12).any(|w| w == b"start_ticks"));
    assert!(!prepared.bytes.windows(7).any(|w| w == b"boot-id"));
}

#[test]
fn long_lines_are_truncated_on_a_char_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let limits = StoreLimits {
        max_line_bytes: 10,
        ..StoreLimits::default()
    };
    let s = open(dir.path(), limits, &[]);
    s.append_log("clip", "info", "ééééééééééé", 1).unwrap();
    let snap = Reader::open(dir.path()).snapshot("clip", 5, 0).unwrap();
    assert_eq!(snap.logs[0].msg.chars().count(), 5);
}

#[test]
fn reader_skips_a_torn_final_line_and_works_without_a_store() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = open(dir.path(), StoreLimits::default(), &[]);
        s.append_log("clip", "info", "good", 1).unwrap();
    }
    let path = dir.path().join("apps/clip").join(LOG_FILE);
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(br#"{"t":2,"level":"INFO","msg":"tor"#).unwrap();
    let snap = Reader::open(dir.path()).snapshot("clip", 10, 0).unwrap();
    assert_eq!(snap.logs.len(), 1);
    assert_eq!(snap.logs[0].msg, "good");
}

#[test]
fn data_survives_reopen_and_appends_continue() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = open(dir.path(), StoreLimits::default(), &[]);
        s.append_log("clip", "info", "before", 1).unwrap();
    }
    let s = open(dir.path(), StoreLimits::default(), &[]);
    s.append_log("clip", "info", "after", 2).unwrap();
    let snap = Reader::open(dir.path()).snapshot("clip", 10, 0).unwrap();
    let msgs: Vec<_> = snap.logs.iter().map(|l| l.msg.as_str()).collect();
    assert_eq!(msgs, ["before", "after"]);
}

#[test]
fn app_cap_evicts_the_least_recent_app() {
    let dir = tempfile::tempdir().unwrap();
    let limits = StoreLimits {
        max_apps: 2,
        ..StoreLimits::default()
    };
    let s = open(dir.path(), limits, &[]);
    s.append_log("clip", "info", "a", 1).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    s.append_log("pad", "info", "b", 2).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    s.append_log("bar", "info", "c", 3).unwrap();
    let apps = Reader::open(dir.path()).apps();
    assert_eq!(apps, ["bar", "pad"]);
}

#[test]
fn wipe_all_clears_everything_and_store_still_works() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreLimits::default(), &[]);
    s.append_log("clip", "info", "x", 1).unwrap();
    s.append_log("pad", "info", "y", 1).unwrap();
    s.wipe_all().unwrap();
    assert!(Reader::open(dir.path()).apps().is_empty());
    s.append_log("clip", "info", "z", 2).unwrap();
    assert_eq!(Reader::open(dir.path()).apps(), ["clip"]);
}

#[test]
fn event_routing() {
    let known = ["clip", "pad"];
    assert_eq!(app_for_event("bread.clip.copied", &known), "clip");
    assert_eq!(
        app_for_event("bread.power.ac.connected", &known),
        DAEMON_APP
    );
    assert_eq!(app_for_event("other.clip.x", &known), DAEMON_APP);
    assert_eq!(app_for_event("bread", &known), DAEMON_APP);
}

#[test]
fn level_normalization() {
    assert_eq!(normalize_level("warning"), "WARN");
    assert_eq!(normalize_level("fatal"), "ERROR");
    assert_eq!(normalize_level("whatever"), "INFO");
}
