use super::*;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::json;

fn prepared(note: &str) -> Prepared {
    let report = serde_json::from_value(json!({
        "schema": 1, "report_type": "crash", "created_unix": 1000,
        "app": {"id": "clip", "version": "0.3", "build_id": null},
        "system": {"os_id": null, "os_version": null, "os_build": null, "kernel": null,
                   "arch": "x86_64", "gpu_drivers": [], "compositor": null, "bakery_track": null,
                   "breadd_version": null, "api_version": null},
        "installed_apps": [],
        "runtime": {"daemon_running": false, "uptime_s": null, "adapters": {},
                    "modules_total": null, "modules_with_errors": null},
        "markers": [], "logs": [], "daemon_logs": [], "events": [], "journal": [],
        "live_stack": [], "notes": note, "redactions": {}
    }))
    .unwrap();
    Prepared::from_report(report).unwrap()
}

#[derive(Debug, Clone)]
struct Captured {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A one-thread HTTP server that answers each connection with the next
/// canned `(status, body)` and records what it was sent.
fn serve(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<Captured>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).unwrap();
            log.lock().unwrap().push(Captured {
                request_line: request_line.trim_end().to_string(),
                headers,
                body: buf,
            });
            let reason = if status < 300 { "OK" } else { "ERR" };
            let resp = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let mut s = stream;
            let _ = s.write_all(resp.as_bytes());
        }
    });
    (url, seen)
}

const RECEIPT: &str = "0123456789abcdef0123456789abcdef";
const OK_BODY: &str =
    r#"{"receipt":"0123456789abcdef0123456789abcdef","deletion_token":"tok","status":"received"}"#;

#[test]
fn install_id_is_stable_then_rotates_and_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let a = install_id(dir.path(), 1_000).unwrap();
    assert_eq!(a.len(), 32);
    assert_eq!(install_id(dir.path(), 2_000).unwrap(), a);
    let later = install_id(dir.path(), 1_000 + INSTALL_ID_LIFETIME_SECS + 1).unwrap();
    assert_ne!(later, a);
    let mode = fs::metadata(dir.path().join("install.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn corrupt_install_file_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("install.json"), "garbage").unwrap();
    assert_eq!(install_id(dir.path(), 5).unwrap().len(), 32);
}

#[test]
fn send_posts_exact_bytes_with_headers() {
    let (url, seen) = serve(vec![(201, OK_BODY)]);
    let p = prepared("hello");
    let acc = Client::new(&url)
        .unwrap()
        .send(&p, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    assert_eq!(
        acc,
        Accepted {
            receipt: RECEIPT.into(),
            deletion_token: Some("tok".into())
        }
    );
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req.request_line, "POST /v1/reports HTTP/1.1");
    assert_eq!(req.body, p.bytes);
    assert_eq!(req.header("x-breadbug-sha256"), Some(p.sha256.as_str()));
    assert_eq!(
        req.header("x-breadbug-install"),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(req.header("content-type"), Some("application/json"));
}

#[test]
fn duplicate_reply_has_no_token() {
    let (url, _) = serve(vec![(
        200,
        r#"{"receipt":"0123456789abcdef0123456789abcdef","duplicate":true}"#,
    )]);
    let acc = Client::new(&url)
        .unwrap()
        .send(&prepared("x"), "a".repeat(32).as_str())
        .unwrap();
    assert_eq!(acc.deletion_token, None);
}

#[test]
fn errors_are_classified() {
    for (status, retriable) in [
        (400, false),
        (413, false),
        (404, false),
        (429, true),
        (500, true),
        (503, true),
    ] {
        let (url, _) = serve(vec![(status, r#"{"error":"nope"}"#)]);
        let err = Client::new(&url)
            .unwrap()
            .send(&prepared("x"), &"a".repeat(32))
            .unwrap_err();
        assert_eq!(
            matches!(err, SendError::Retriable(_)),
            retriable,
            "{status}: {err}"
        );
    }
    // nothing listening
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    let err = Client::new(&url)
        .unwrap()
        .send(&prepared("x"), &"a".repeat(32))
        .unwrap_err();
    assert!(matches!(err, SendError::Retriable(_)));
}

#[test]
fn redirects_are_not_followed() {
    let (url, seen) = serve(vec![(302, "{}"), (200, "{}")]);
    let err = Client::new(&url)
        .unwrap()
        .send(&prepared("x"), &"a".repeat(32))
        .unwrap_err();
    assert!(matches!(err, SendError::Rejected(_)));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn reply_without_a_valid_receipt_is_not_success() {
    for body in [
        "{}",
        "not json",
        r#"{"receipt":"short"}"#,
        r#"{"receipt":"../../etc/passwd"}"#,
    ] {
        let (url, _) = serve(vec![(201, Box::leak(body.to_string().into_boxed_str()))]);
        assert!(
            Client::new(&url)
                .unwrap()
                .send(&prepared("x"), &"a".repeat(32))
                .is_err(),
            "{body}"
        );
    }
}

#[test]
fn tampered_bytes_are_never_sent() {
    let (url, seen) = serve(vec![(201, OK_BODY)]);
    let mut p = prepared("x");
    p.bytes.push(b' ');
    let err = Client::new(&url)
        .unwrap()
        .send(&p, &"a".repeat(32))
        .unwrap_err();
    assert!(matches!(err, SendError::Rejected(_)));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn endpoint_accepts_https_for_any_host() {
    for url in [
        "https://bugs.example.com",
        "https://bugs.example.com/",
        "https://bugs.breadway.dev",
        "https://localhost:8120",
        "https://127.0.0.1:8120",
        "https://[::1]:8120",
    ] {
        assert!(Client::new(url).is_ok(), "should accept {url}");
    }
}

#[test]
fn endpoint_accepts_plain_http_only_for_literal_loopback_host() {
    for url in [
        "http://127.0.0.1:8120",
        "http://127.0.0.1",
        "http://[::1]:8120",
        "http://[::1]",
        "http://localhost:8120",
        "http://localhost",
        "http://LOCALHOST:8120",
    ] {
        assert!(Client::new(url).is_ok(), "should accept {url}");
    }

    for url in [
        "http://bugs.example.com",
        "http://192.168.1.10:8120",
        "http://[2001:db8::1]:8120",
        // Hostnames that merely start with localhost or 127.0.0.1.
        "http://localhost.evil.example",
        "http://127.0.0.1.evil.example",
        "http://localhost.evil.example:8120",
        "http://127.0.0.1.nip.io:8120",
    ] {
        assert!(Client::new(url).is_err(), "should refuse {url}");
    }
}

#[test]
fn endpoint_refuses_userinfo_and_other_schemes() {
    for url in [
        "http://localhost@evil.example",
        "http://127.0.0.1@evil.example",
        "http://127.0.0.1:8120@evil.example",
        "http://user:pass@127.0.0.1",
        "http://user@localhost:8120",
        "https://user@bugs.example.com",
        "https://user:pass@bugs.example.com",
        "ftp://bugs.example.com",
        "file:///tmp/bugs",
        "javascript:alert(1)",
        "not a url",
        "",
        "https://",
    ] {
        assert!(Client::new(url).is_err(), "should refuse {url:?}");
    }
}

#[test]
fn delete_and_status() {
    let (url, seen) = serve(vec![
        (204, ""),
        (404, r#"{"error":"not found"}"#),
        (200, r#"{"status":"fixed","fixed_in":"0.4.0"}"#),
        (404, "{}"),
    ]);
    let c = Client::new(&url).unwrap();
    assert!(c.delete(RECEIPT, "tok").unwrap());
    assert!(!c.delete(RECEIPT, "tok").unwrap());
    assert_eq!(
        seen.lock().unwrap()[0].header("authorization"),
        Some("Bearer tok")
    );
    let st = c.status(RECEIPT).unwrap().unwrap();
    assert_eq!(
        (st.status.as_str(), st.fixed_in.as_deref()),
        ("fixed", Some("0.4.0"))
    );
    assert_eq!(c.status(RECEIPT).unwrap(), None);
}

#[test]
fn history_roundtrip_status_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::open(dir.path()).unwrap();
    h.append(&HistoryEntry {
        t: 1,
        kind: HistoryKind::Sent,
        app: "clip".into(),
        sha256: "s".into(),
        receipt: Some(RECEIPT.into()),
        deletion_token: Some("tok".into()),
        path: None,
        status: None,
        fixed_in: None,
        told_fixed: false,
    })
    .unwrap();
    h.append(&HistoryEntry {
        t: 2,
        kind: HistoryKind::Saved,
        app: "pad".into(),
        sha256: "s2".into(),
        receipt: None,
        deletion_token: None,
        path: Some("/tmp/x.json".into()),
        status: None,
        fixed_in: None,
        told_fixed: false,
    })
    .unwrap();
    assert_eq!(h.list().len(), 2);
    h.set_status(RECEIPT, "fixed", Some("0.4.0")).unwrap();
    h.mark_told_fixed(RECEIPT).unwrap();
    let e = h.find(RECEIPT).unwrap();
    assert_eq!(
        (e.status.as_deref(), e.fixed_in.as_deref(), e.told_fixed),
        (Some("fixed"), Some("0.4.0"), true)
    );
    assert!(h.remove(RECEIPT).unwrap());
    assert!(!h.remove(RECEIPT).unwrap());
    assert_eq!(h.list().len(), 1);
}

#[test]
fn history_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::open(dir.path()).unwrap();
    for i in 0..250u64 {
        h.append(&HistoryEntry {
            t: i,
            kind: HistoryKind::Saved,
            app: "clip".into(),
            sha256: i.to_string(),
            receipt: None,
            deletion_token: None,
            path: None,
            status: None,
            fixed_in: None,
            told_fixed: false,
        })
        .unwrap();
    }
    let all = h.list();
    assert_eq!(all.len(), 200);
    assert_eq!(all.last().unwrap().t, 249);
}

#[test]
fn queue_holds_retries_with_backoff_and_delivers_when_the_server_returns() {
    let dir = tempfile::tempdir().unwrap();
    let q = Queue::open(dir.path()).unwrap();
    let h = History::open(dir.path()).unwrap();
    let p = prepared("queued");
    assert!(q.enqueue(&p, 1_000).unwrap());
    assert_eq!(q.list().len(), 1);

    // Server down: backs off, stays queued.
    let down = TcpListener::bind("127.0.0.1:0").unwrap();
    let down_url = format!("http://{}", down.local_addr().unwrap());
    drop(down);
    let c = Client::new(&down_url).unwrap();
    let (sent, left, dropped) = q
        .flush(&c, &h, &"a".repeat(32), 1_000 + 10_000, false)
        .unwrap();
    assert_eq!((sent, left, dropped), (0, 1, 0));
    let meta = q.list()[0].clone();
    assert_eq!(meta.attempts, 1);
    assert!(meta.next_try > 1_000 + 10_000);
    // Not due yet: untouched.
    let (_, _, _) = q
        .flush(&c, &h, &"a".repeat(32), 1_000 + 10_001, false)
        .unwrap();
    assert_eq!(q.list()[0].attempts, 1);

    // Server back: delivered, recorded in history, queue empty.
    let (url, seen) = serve(vec![(201, OK_BODY)]);
    let c = Client::new(&url).unwrap();
    let (sent, left, dropped) = q.flush(&c, &h, &"a".repeat(32), 2_000_000, false).unwrap();
    assert_eq!((sent, left, dropped), (1, 0, 0));
    assert_eq!(seen.lock().unwrap()[0].body, p.bytes);
    let entry = h.find(RECEIPT).unwrap();
    assert_eq!(entry.deletion_token.as_deref(), Some("tok"));
}

#[test]
fn rejected_queue_items_are_dropped_and_cancel_works() {
    let dir = tempfile::tempdir().unwrap();
    let q = Queue::open(dir.path()).unwrap();
    let h = History::open(dir.path()).unwrap();
    let p = prepared("bad");
    q.enqueue(&p, 0).unwrap();
    let (url, _) = serve(vec![(400, r#"{"error":"invalid"}"#)]);
    let (sent, left, dropped) = q
        .flush(&Client::new(&url).unwrap(), &h, &"a".repeat(32), 0, true)
        .unwrap();
    assert_eq!((sent, left, dropped), (0, 0, 1));

    let p2 = prepared("cancel me");
    q.enqueue(&p2, 0).unwrap();
    assert!(q.cancel(&p2.sha256));
    assert!(!q.cancel("../../etc"));
    assert!(q.list().is_empty());
}

#[test]
fn queue_is_capped_and_clearable() {
    let dir = tempfile::tempdir().unwrap();
    let q = Queue::open(dir.path()).unwrap();
    for i in 0..MAX_QUEUED {
        assert!(q.enqueue(&prepared(&format!("n{i}")), i as u64).unwrap());
    }
    assert!(!q.enqueue(&prepared("one too many"), 99).unwrap());
    assert_eq!(q.clear(), MAX_QUEUED);
    assert!(q.list().is_empty());
}

#[test]
fn backoff_grows_and_caps() {
    assert_eq!(backoff(0), 60);
    assert_eq!(backoff(1), 120);
    assert_eq!(backoff(30), MAX_BACKOFF_SECS);
}
