use super::*;
use bread_bug::store::{Marker, Store};

fn env(dir: &std::path::Path) -> Env {
    let ctx = ScrubContext {
        home: Some("/home/alicesmith".into()),
        username: Some("alicesmith".into()),
        hostname: None,
        extra_literals: vec![],
    };
    let store = Store::open(dir.join("store"), &BugConfig::default(), ctx.clone()).unwrap();
    store
        .append_log("clip", "warn", "failed for alicesmith@example.org", 5)
        .unwrap();
    store
        .record_marker("clip", Marker::new(MarkerKind::Panic, 9))
        .unwrap();
    store.append_log("pad", "info", "ok", 6).unwrap();
    Env {
        store_root: dir.join("store"),
        socket: dir.join("none.sock"),
        config_path: dir.join("breadbug.toml"),
        scrub: ctx,
        installed: Some(Some(vec![
            InstalledApp {
                id: "clip".into(),
                version: "0.3".into(),
            },
            InstalledApp {
                id: "pad".into(),
                version: "0.6".into(),
            },
        ])),
        include_journal: false,
        live_capture: |_| {
            Some(vec![Frame {
                symbol: Some("__poll".into()),
                ..Frame::default()
            }])
        },
        pid_ok: |pid| pid == 4242,
    }
}

fn cli(args: &[&str]) -> Cli {
    Cli::parse_from(std::iter::once("breadbug").chain(args.iter().copied()))
}

fn go(args: &[&str], env: &Env, input: &str, interactive: bool) -> (Result<()>, String) {
    let mut out = Vec::new();
    let r = run(
        &cli(args),
        env,
        &mut input.as_bytes(),
        &mut out,
        interactive,
    );
    (r, String::from_utf8(out).unwrap())
}

#[test]
fn list_shows_installed_apps_problem_first() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let (r, out) = go(&["list"], &e, "", false);
    r.unwrap();
    let lines: Vec<_> = out.lines().collect();
    assert!(lines[0].starts_with("clip 0.3"), "{out}");
    assert!(lines[0].contains("panic"));
    assert!(lines[1].starts_with("pad"));
}

#[test]
fn dry_run_prints_exact_scrubbed_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let (r, out) = go(
        &[
            "--app",
            "clip",
            "--dry-run",
            "--notes",
            "mail alicesmith@example.org",
        ],
        &e,
        "",
        false,
    );
    r.unwrap();
    assert!(!out.contains("alicesmith"), "{out}");
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["report_type"], "crash");
    assert_eq!(parsed["app"]["id"], "clip");
}

#[test]
fn save_writes_the_same_bytes_as_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let path = dir.path().join("r.json");
    let (r, _) = go(
        &["--app", "clip", "--save", path.to_str().unwrap()],
        &e,
        "",
        false,
    );
    r.unwrap();
    let (_, dry) = go(&["--app", "clip", "--dry-run"], &e, "", false);
    // created_unix differs only if the clock ticked a second between runs.
    let saved = std::fs::read_to_string(&path).unwrap();
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.contains("created_unix"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&saved), strip(&dry));
}

#[test]
fn noninteractive_without_action_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let (r, out) = go(&["--app", "clip"], &e, "", false);
    r.unwrap();
    assert!(out.contains("Nothing was sent"), "{out}");
}

#[test]
fn send_is_refused_while_upload_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let (r, out) = go(&["--app", "clip", "--send"], &e, "", false);
    r.unwrap();
    assert!(out.contains("Sending is turned off"), "{out}");
}

#[test]
fn config_upload_toggles_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    go(&["config", "upload", "on"], &e, "", false).0.unwrap();
    assert!(BugConfig::load_from(&e.config_path).unwrap().upload_enabled);
    go(&["config", "upload", "off"], &e, "", false).0.unwrap();
    assert!(!BugConfig::load_from(&e.config_path).unwrap().upload_enabled);
}

#[test]
fn interactive_flow_picks_app_defaults_type_and_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    // pick #1 (clip), default type, a note, then cancel
    let (r, out) = go(&[], &e, "1\n\nit froze when I copied\nc\n", true);
    r.unwrap();
    assert!(out.contains("Report for clip (crash)"), "{out}");
    assert!(out.contains("your notes: it froze when I copied"));
    assert!(out.contains("Cancelled. Nothing was sent."));
}

#[test]
fn interactive_view_then_save_to_file() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let path = dir.path().join("out.json");
    let input = format!("2\nbehavior\n\nv\nf\n{}\n", path.display());
    let (r, out) = go(&[], &e, &input, true);
    r.unwrap();
    assert!(out.contains("\"schema\": 1"), "{out}");
    assert!(path.exists());
    assert!(out.contains("Nothing was sent"));
}

#[test]
fn interactive_rejects_bad_numbers_then_quits() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let (r, out) = go(&[], &e, "9\nabc\nq\n", true);
    r.unwrap();
    assert_eq!(out.matches("Please enter a number").count(), 2);
}

#[test]
fn eof_at_any_prompt_is_a_clean_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    for input in ["", "1\n", "1\n\n", "1\n\nnote\n"] {
        let (r, _) = go(&[], &e, input, true);
        r.unwrap();
    }
}

#[test]
fn missing_app_without_tty_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    assert!(go(&[], &e, "", false).0.is_err());
}

fn freeze_env(dir: &std::path::Path, pid: u32) -> Env {
    let e = env(dir);
    let store = Store::open(&e.store_root, &BugConfig::default(), e.scrub.clone()).unwrap();
    let mut m = Marker::new(MarkerKind::Freeze, 30);
    m.pid = Some(pid);
    store.record_marker("pad", m).unwrap();
    e
}

#[test]
fn live_stack_is_included_only_with_consent() {
    let dir = tempfile::tempdir().unwrap();
    let e = freeze_env(dir.path(), 4242);
    // --live-stack is explicit consent
    let (r, out) = go(&["--app", "pad", "--type", "freeze", "--live-stack", "--dry-run"], &e, "", false);
    r.unwrap();
    assert!(out.contains("__poll"), "{out}");
    // no flag, non-interactive: no capture
    let (_, out) = go(&["--app", "pad", "--type", "freeze", "--dry-run"], &e, "", false);
    assert!(!out.contains("__poll"));
    // interactive: asked, and "n" means no
    let (_, out) = go(&[], &e, "1\n\n\nn\nc\n", true);
    assert!(out.contains("Capture one now"), "{out}");
    assert!(out.contains("live stack frames      0"), "{out}");
    // interactive yes
    let (_, out) = go(&[], &e, "1\n\n\ny\nc\n", true);
    assert!(out.contains("live stack frames      1"), "{out}");
}

#[test]
fn live_stack_skips_dead_or_reused_pids() {
    let dir = tempfile::tempdir().unwrap();
    let e = freeze_env(dir.path(), 9999);
    let (r, out) = go(&["--app", "pad", "--type", "freeze", "--live-stack", "--dry-run"], &e, "", false);
    r.unwrap();
    assert!(out.contains("No running frozen pad process"), "{out}");
    assert!(!out.contains("__poll"));
}
