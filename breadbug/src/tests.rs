use super::*;
use bread_bug::store::{Marker, ProcessIdentity, Store};
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many times the (fake) capture function ran.
static CAPTURES: AtomicUsize = AtomicUsize::new(0);
/// Serializes the tests that read `CAPTURES`.
static CAPTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn identity(start_ticks: u64) -> ProcessIdentity {
    ProcessIdentity {
        start_ticks,
        exe: "/usr/bin/breadpad".into(),
        boot_id: None,
    }
}

/// pid 4242 started at tick 500 is the live pad instance; anything else is a
/// different process (pid 9999 is gone entirely).
fn fake_check(_app: &str, pid: u32, id: &ProcessIdentity) -> Result<(), Refusal> {
    match (pid, id.start_ticks) {
        (4242, 500) => Ok(()),
        (4242, _) => Err(Refusal::Replaced { pid }),
        _ => Err(Refusal::Gone { pid }),
    }
}

fn fake_capture(app: &str, pid: u32, id: &ProcessIdentity) -> Result<Vec<Frame>, Refusal> {
    // Like the real one, verify again right before "attaching".
    fake_check(app, pid, id)?;
    CAPTURES.fetch_add(1, Ordering::SeqCst);
    Ok(vec![Frame {
        symbol: Some("__poll".into()),
        ..Frame::default()
    }])
}

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
        state_dir: dir.join("state"),
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
        live_capture: fake_capture,
        live_check: fake_check,
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

fn freeze_marker(pid: u32, start_ticks: u64, t: u64) -> Marker {
    let mut m = Marker::new(MarkerKind::Freeze, t);
    m.pid = Some(pid);
    m.process = Some(identity(start_ticks));
    m
}

fn freeze_env(dir: &std::path::Path, pid: u32) -> Env {
    let e = env(dir);
    let store = Store::open(&e.store_root, &BugConfig::default(), e.scrub.clone()).unwrap();
    store
        .record_marker("pad", freeze_marker(pid, 500, 30))
        .unwrap();
    e
}

fn seed(e: &Env, marker: Marker) {
    let store = Store::open(&e.store_root, &BugConfig::default(), e.scrub.clone()).unwrap();
    store.record_marker("pad", marker).unwrap();
}

const LIVE: &[&str] = &[
    "--app",
    "pad",
    "--type",
    "freeze",
    "--live-stack",
    "--dry-run",
];

#[test]
fn live_stack_is_included_only_with_consent() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let e = freeze_env(dir.path(), 4242);
    // --live-stack is explicit consent
    let (r, out) = go(LIVE, &e, "", false);
    r.unwrap();
    assert!(out.contains("__poll"), "{out}");
    // no flag, non-interactive: no capture
    let before = CAPTURES.load(Ordering::SeqCst);
    let (_, out) = go(
        &["--app", "pad", "--type", "freeze", "--dry-run"],
        &e,
        "",
        false,
    );
    assert!(!out.contains("__poll"));
    // interactive: asked, and "n" means no
    let (_, out) = go(&[], &e, "1\n\n\nn\nc\n", true);
    assert!(out.contains("Capture one now"), "{out}");
    assert!(out.contains("live stack frames      0"), "{out}");
    assert_eq!(
        CAPTURES.load(Ordering::SeqCst),
        before,
        "declined, yet captured"
    );
    // interactive yes
    let (_, out) = go(&[], &e, "1\n\n\ny\nc\n", true);
    assert!(out.contains("live stack frames      1"), "{out}");
}

#[test]
fn consent_prompt_says_what_a_capture_contains() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let e = freeze_env(dir.path(), 4242);
    let (_, out) = go(&[], &e, "1\n\n\nn\nc\n", true);
    assert!(out.contains("no memory, registers or environment"), "{out}");
}

#[test]
fn live_stack_skips_dead_pids() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let e = freeze_env(dir.path(), 9999);
    let before = CAPTURES.load(Ordering::SeqCst);
    let (r, out) = go(LIVE, &e, "", false);
    r.unwrap();
    assert!(out.contains("process 9999 has exited"), "{out}");
    assert!(!out.contains("__poll"));
    assert_eq!(CAPTURES.load(Ordering::SeqCst), before);
}

#[test]
fn live_stack_refuses_a_reused_pid_with_a_clear_message() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    // pid 4242 is alive, but it is not the instance that froze (tick 123).
    seed(&e, freeze_marker(4242, 123, 30));
    let before = CAPTURES.load(Ordering::SeqCst);
    let (r, out) = go(LIVE, &e, "", false);
    r.unwrap();
    assert!(
        out.contains("different process than the one that froze"),
        "{out}"
    );
    assert!(!out.contains("__poll"));
    assert_eq!(CAPTURES.load(Ordering::SeqCst), before);
}

#[test]
fn live_stack_refuses_a_marker_without_process_identity() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let mut m = Marker::new(MarkerKind::Freeze, 30);
    m.pid = Some(4242);
    seed(&e, m);
    let before = CAPTURES.load(Ordering::SeqCst);
    let (_, out) = go(LIVE, &e, "", false);
    assert!(out.contains("does not record which process froze"), "{out}");
    assert_eq!(CAPTURES.load(Ordering::SeqCst), before);
}

#[test]
fn a_recovered_stopped_or_later_marker_invalidates_the_freeze() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    for later in [
        MarkerKind::Recovered,
        MarkerKind::Stopped,
        MarkerKind::Panic,
        MarkerKind::Crash,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let e = freeze_env(dir.path(), 4242);
        let mut m = Marker::new(later, 40);
        m.pid = Some(4242);
        m.process = Some(identity(500));
        seed(&e, m);
        let before = CAPTURES.load(Ordering::SeqCst);
        let (r, out) = go(LIVE, &e, "", false);
        r.unwrap();
        assert!(out.contains("not a freeze"), "{later:?}: {out}");
        assert!(!out.contains("__poll"), "{later:?}: {out}");
        assert_eq!(
            CAPTURES.load(Ordering::SeqCst),
            before,
            "{later:?}: capture was invoked"
        );
    }
}

#[test]
fn a_new_freeze_after_recovery_is_capturable_again() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let e = freeze_env(dir.path(), 4242);
    seed(&e, Marker::new(MarkerKind::Recovered, 40));
    seed(&e, freeze_marker(4242, 500, 50));
    let (_, out) = go(LIVE, &e, "", false);
    assert!(out.contains("__poll"), "{out}");
}

#[test]
fn interactive_prompt_is_not_offered_after_recovery() {
    let _g = CAPTURE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let e = freeze_env(dir.path(), 4242);
    seed(&e, Marker::new(MarkerKind::Recovered, 40));
    let (_, out) = go(&[], &e, "1\n\n\ny\nc\n", true);
    assert!(!out.contains("Capture one now"), "{out}");
}

#[test]
fn identity_is_checked_again_after_consent() {
    use std::sync::atomic::AtomicBool;
    static SWAP: AtomicBool = AtomicBool::new(false);
    // The pre-prompt check passes; by the time the capture function runs
    // (after the user said yes) the process was replaced.
    fn check(_: &str, _: u32, _: &ProcessIdentity) -> Result<(), Refusal> {
        Ok(())
    }
    fn capture(_: &str, pid: u32, _: &ProcessIdentity) -> Result<Vec<Frame>, Refusal> {
        SWAP.store(true, Ordering::SeqCst);
        Err(Refusal::Replaced { pid })
    }
    let dir = tempfile::tempdir().unwrap();
    let mut e = freeze_env(dir.path(), 4242);
    e.live_check = check;
    e.live_capture = capture;
    let (r, out) = go(&[], &e, "1\n\n\ny\nc\n", true);
    r.unwrap();
    assert!(SWAP.load(Ordering::SeqCst), "capture never ran");
    assert!(
        out.contains("different process than the one that froze"),
        "{out}"
    );
    assert!(out.contains("live stack frames      0"), "{out}");
}

// ---- sending, history, queue -------------------------------------------

use std::io::Read as _;
use std::net::TcpListener;

/// Answers each connection with the next canned `(status, body)`.
fn serve(responses: Vec<(u16, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end().to_ascii_lowercase();
                if line.is_empty() {
                    break;
                }
                if let Some(v) = line.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).unwrap();
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let mut s = stream;
            let _ = std::io::Write::write_all(&mut s, resp.as_bytes());
        }
    });
    url
}

const RECEIPT: &str = "0123456789abcdef0123456789abcdef";

fn enable_upload(e: &Env, endpoint: &str) {
    let cfg = BugConfig {
        upload_enabled: true,
        endpoint: endpoint.to_string(),
        ..BugConfig::default()
    };
    cfg.save_to(&e.config_path).unwrap();
}

#[test]
fn send_records_receipt_and_history_then_delete_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let url = serve(vec![
        (
            201,
            r#"{"receipt":"0123456789abcdef0123456789abcdef","deletion_token":"tok","status":"received"}"#,
        ),
        (204, ""),
    ]);
    enable_upload(&e, &url);
    let (r, out) = go(&["--app", "clip", "--send"], &e, "", false);
    r.unwrap();
    assert!(out.contains(&format!("Your receipt: {RECEIPT}")), "{out}");
    let (_, out) = go(&["history"], &e, "", false);
    assert!(out.contains(RECEIPT), "{out}");
    let (r, out) = go(&["delete", RECEIPT], &e, "", false);
    r.unwrap();
    assert!(out.contains("Deleted from the server"), "{out}");
    let (_, out) = go(&["history"], &e, "", false);
    assert!(!out.contains(RECEIPT));
    // deleting something unknown is an error
    assert!(go(&["delete", RECEIPT], &e, "", false).0.is_err());
}

#[test]
fn unreachable_server_queues_and_flush_delivers_later() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    // closed port
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    enable_upload(&e, &dead);
    let (r, out) = go(&["--app", "clip", "--send"], &e, "", false);
    r.unwrap();
    assert!(out.contains("queued"), "{out}");
    let (_, out) = go(&["queue"], &e, "", false);
    assert!(out.contains("clip"), "{out}");

    let url = serve(vec![(
        201,
        r#"{"receipt":"0123456789abcdef0123456789abcdef","deletion_token":"tok"}"#,
    )]);
    enable_upload(&e, &url);
    let (r, out) = go(&["queue", "flush"], &e, "", false);
    r.unwrap();
    assert!(out.contains("Sent 1"), "{out}");
    assert!(go(&["queue"], &e, "", false)
        .1
        .contains("Nothing is waiting"));
    assert!(go(&["history"], &e, "", false).1.contains(RECEIPT));
}

#[test]
fn rejected_report_is_not_queued() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    enable_upload(&e, &serve(vec![(400, r#"{"error":"invalid report"}"#)]));
    let (r, out) = go(&["--app", "clip", "--send"], &e, "", false);
    r.unwrap();
    assert!(out.contains("rejected"), "{out}");
    assert!(go(&["queue"], &e, "", false)
        .1
        .contains("Nothing is waiting"));
}

#[test]
fn queue_clear_and_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    enable_upload(&e, &format!("http://{}", closed.local_addr().unwrap()));
    drop(closed);
    go(&["--app", "clip", "--send"], &e, "", false).0.unwrap();
    let (_, listing) = go(&["queue"], &e, "", false);
    let id = listing.split_whitespace().next().unwrap().to_string();
    assert!(go(&["queue", "cancel", "zz"], &e, "", false).0.is_err());
    assert!(go(&["queue", "cancel", &id], &e, "", false)
        .1
        .contains("Dropped"));
    go(
        &["--app", "clip", "--notes", "again", "--send"],
        &e,
        "",
        false,
    )
    .0
    .unwrap();
    assert!(go(&["queue", "clear"], &e, "", false)
        .1
        .contains("Dropped 1"));
}

#[test]
fn nothing_touches_the_network_while_upload_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    // A server that would fail the test by being contacted: it answers 500 and
    // we assert nothing was queued or recorded.
    let url = serve(vec![(500, "{}")]);
    let cfg = BugConfig {
        upload_enabled: false,
        endpoint: url,
        ..BugConfig::default()
    };
    cfg.save_to(&e.config_path).unwrap();
    for args in [
        vec!["--app", "clip", "--send"],
        vec!["queue", "flush"],
        vec!["history", "--refresh"],
    ] {
        let (r, out) = go(&args, &e, "", false);
        r.unwrap();
        assert!(
            !out.contains("queued") && !out.contains("Sent"),
            "{args:?}: {out}"
        );
    }
    assert!(go(&["queue"], &e, "", false)
        .1
        .contains("Nothing is waiting"));
}

#[test]
fn saved_files_appear_in_history() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(dir.path());
    let path = dir.path().join("r.json");
    go(
        &["--app", "clip", "--save", path.to_str().unwrap()],
        &e,
        "",
        false,
    )
    .0
    .unwrap();
    let (_, out) = go(&["history"], &e, "", false);
    assert!(out.contains("saved") && out.contains("r.json"), "{out}");
}
