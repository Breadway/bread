use super::*;
use crate::config::BugConfig;
use crate::store::Store;

const BID: &str = "3f2a9c1de4b5a6978081726354453423a1b2c3d4";

fn ctx() -> ScrubContext {
    ScrubContext {
        home: Some("/home/alicesmith".into()),
        username: Some("alicesmith".into()),
        hostname: Some("thinkbox-9".into()),
        extra_literals: vec!["Alice Smith".into()],
    }
}

fn seeded_store(dir: &Path) -> Store {
    let s = Store::open(dir, &BugConfig::default(), ctx()).unwrap();
    s.append_log(
        "clip",
        "info",
        "copied url https://x.example/p?token=abc from 203.0.113.42",
        10,
    )
    .unwrap();
    s.append_log("clip", "error", "failed for mail@example.org", 11)
        .unwrap();
    s.append_log(DAEMON_APP, "warn", "adapter flapped for alicesmith", 12)
        .unwrap();
    s.append_log(DAEMON_APP, "info", "boring info line", 13)
        .unwrap();
    s.append_event(
        "clip",
        "bread.window.opened",
        &serde_json::json!({"title": "Inbox - Gmail"}),
        14,
    )
    .unwrap();
    let mut m = Marker::new(MarkerKind::Panic, 20);
    m.app_version = Some("0.3.1".into());
    m.build_id = Some(BID.into());
    m.message = Some("boom at /home/alicesmith/p/src/lib.rs:4".into());
    m.frames = vec![Frame {
        module: Some("/home/alicesmith/.local/bin/breadclip".into()),
        offset: Some(4660),
        build_id: Some(BID.into()),
        symbol: None,
    }];
    s.record_marker("clip", m).unwrap();
    s
}

fn opts(dir: &Path) -> BuildOptions {
    let mut o = BuildOptions::new("clip", ReportType::Crash);
    o.store_root = dir.to_path_buf();
    o.socket = dir.join("no.sock");
    o.scrub = ctx();
    o.include_journal = false;
    o.now_unix = 1_000;
    o
}

#[test]
fn report_contains_expected_sections_and_no_seeded_pii() {
    let dir = tempfile::tempdir().unwrap();
    let _s = seeded_store(dir.path());
    let mut o = opts(dir.path());
    o.notes = Some("it broke when Alice Smith pasted mail@example.org".into());
    let p = build(&o).unwrap();
    let text = String::from_utf8(p.bytes.clone()).unwrap();
    for seed in [
        "alicesmith",
        "thinkbox-9",
        "Alice Smith",
        "mail@example.org",
        "203.0.113.42",
        "Gmail",
        "token=abc",
    ] {
        assert!(!text.contains(seed), "{seed:?} leaked:\n{text}");
    }
    assert_eq!(p.report.schema, SCHEMA_VERSION);
    assert_eq!(p.report.app.id, "clip");
    assert_eq!(p.report.logs.len(), 2);
    // only WARN/ERROR daemon lines
    assert_eq!(p.report.daemon_logs.len(), 1);
    assert_eq!(p.report.markers[0].frames[0].build_id.as_deref(), Some(BID));
    assert_eq!(p.report.app.build_id.as_deref(), Some(BID));
    assert!(!p.report.runtime.daemon_running);
    assert!(p.report.redactions.values().sum::<usize>() > 0);
    assert!(p.report.notes.as_deref().unwrap().contains("<REDACTED>"));
}

#[test]
fn bytes_hash_verify_and_save_match() {
    let dir = tempfile::tempdir().unwrap();
    let _s = seeded_store(dir.path());
    let p = build(&opts(dir.path())).unwrap();
    assert!(p.verify());
    let out = dir.path().join("report.json");
    p.save(&out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), p.bytes);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let parsed: Report = serde_json::from_slice(&p.bytes).unwrap();
    assert_eq!(parsed, p.report);
    let mut tampered = p.clone();
    tampered.bytes.push(b' ');
    assert!(!tampered.verify());
}

#[test]
fn two_builds_of_the_same_input_are_identical() {
    let dir = tempfile::tempdir().unwrap();
    let _s = seeded_store(dir.path());
    let a = build(&opts(dir.path())).unwrap();
    let b = build(&opts(dir.path())).unwrap();
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(a.sha256, b.sha256);
}

#[test]
fn long_notes_are_truncated_and_empty_notes_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let _s = seeded_store(dir.path());
    let mut o = opts(dir.path());
    o.notes = Some("é".repeat(MAX_NOTES_BYTES));
    let p = build(&o).unwrap();
    assert!(p.report.notes.unwrap().len() <= MAX_NOTES_BYTES);
    o.notes = Some("   ".into());
    assert!(build(&o).unwrap().report.notes.is_none());
}

#[test]
fn payload_is_trimmed_to_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BugConfig {
        store: crate::config::StoreLimits {
            max_bytes_per_file: 8 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
            ..Default::default()
        },
        ..BugConfig::default()
    };
    let s = Store::open(dir.path(), &cfg, ctx()).unwrap();
    for i in 0..3000u64 {
        s.append_log("clip", "info", &format!("{i} {}", "word ".repeat(150)), i)
            .unwrap();
    }
    let mut o = opts(dir.path());
    o.max_logs = 3000;
    let p = build(&o).unwrap();
    assert!(
        p.bytes.len() <= MAX_PAYLOAD_BYTES + 64 * 1024,
        "{}",
        p.bytes.len()
    );
    assert!(!p.report.logs.is_empty());
}

#[test]
fn invalid_app_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts(dir.path());
    o.app = "../x".into();
    assert!(build(&o).is_err());
}

#[test]
fn report_type_parsing() {
    assert_eq!("Crash".parse::<ReportType>().unwrap(), ReportType::Crash);
    assert_eq!("wrong".parse::<ReportType>().unwrap(), ReportType::Behavior);
    assert!("nope".parse::<ReportType>().is_err());
}

#[test]
fn picker_orders_problem_apps_first_and_limits_to_installed() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), &BugConfig::default(), ctx()).unwrap();
    s.record_marker("pad", Marker::new(MarkerKind::Freeze, 50))
        .unwrap();
    s.record_marker("clip", Marker::new(MarkerKind::Panic, 90))
        .unwrap();
    s.record_marker("bar", Marker::new(MarkerKind::Recovered, 99))
        .unwrap();
    s.append_log("search", "info", "x", 1).unwrap();
    let reader = Reader::open(dir.path());
    let installed = [
        InstalledApp {
            id: "bar".into(),
            version: "1".into(),
        },
        InstalledApp {
            id: "clip".into(),
            version: "2".into(),
        },
        InstalledApp {
            id: "pad".into(),
            version: "3".into(),
        },
        InstalledApp {
            id: "lock".into(),
            version: "4".into(),
        },
    ];
    let entries = app_entries(&reader, Some(&installed));
    let order: Vec<_> = entries.iter().map(|e| e.app.as_str()).collect();
    // clip (newest problem), pad, then the rest alphabetically; search is not installed.
    assert_eq!(order, ["clip", "pad", "bar", "lock"]);
    assert_eq!(entries[0].version.as_deref(), Some("2"));
    // Recovered markers are not problems.
    assert!(entries[2].last_problem.is_none());
    // Without bakery state, fall back to apps in the store.
    let fallback: Vec<_> = app_entries(&reader, None)
        .into_iter()
        .map(|e| e.app)
        .collect();
    assert!(fallback.contains(&"search".to_string()));
}

#[test]
fn machine_facts_are_not_eaten_by_identity_literals() {
    // On a default install the hostname equals the OS id; the OS id must
    // survive while a hostname in the free-text sections is still redacted.
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), &BugConfig::default(), ScrubContext::default()).unwrap();
    s.append_log("clip", "info", "running on mybox-7 now", 1)
        .unwrap();
    let mut o = opts(dir.path());
    o.scrub = ScrubContext {
        hostname: Some("mybox-7".into()),
        ..ScrubContext::default()
    };
    let p = build(&o).unwrap();
    assert!(
        p.report.logs[0].msg.contains("<HOST>"),
        "{:?}",
        p.report.logs
    );
    let mut report = p.report.clone();
    report.system.os_id = Some("mybox-7".into());
    super::scrub_report(&mut report, &o.scrub).unwrap();
    assert_eq!(report.system.os_id.as_deref(), Some("mybox-7"));
}
