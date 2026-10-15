//! BREAD-47: the operation lifetime of a `module_host.hyprland_*` call, with a
//! real `breadd`, a real authenticated (fake) module host and a fake
//! compositor socket that misbehaves.
//!
//! The bridge's deadline is 8s, so the stall scenarios take roughly 8-15s
//! each; they run in parallel as separate daemons.

mod hypr_support;

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hypr_support::*;
use serde_json::{json, Value};

const DEADLINE_SECS: f64 = 8.0;

fn run_one(
    h: &Harness,
    name: &str,
    method: &str,
    params: Value,
    within: Duration,
) -> (Value, Duration) {
    h.wait_ready(&[name]);
    h.set_script(name, vec![("op", request("1", method, params))]);
    h.go(&[name]);
    let started = Instant::now();
    let done = h.wait_done(&[name], within);
    let result = done[name]["results"][0].clone();
    (result["response"].clone(), started.elapsed())
}

/// (a) The compositor stalls past the deadline and only then rejects the
/// legacy dispatch, exactly like the BREAD-47 probe. The module is told it
/// timed out, and the daemon must NEVER send the eval fallback afterwards
/// (that is a command executing after the caller was told it failed).
#[test]
fn stalled_compositor_times_out_and_no_later_request_ever_arrives() {
    let (h, comp) = Harness::spawn(&[ModuleSpec::granted("hypr-a")], |runtime| {
        Compositor::start(
            runtime,
            Arc::new(|request, conn| {
                if request.starts_with("dispatch ") {
                    std::thread::sleep(Duration::from_millis(9500));
                    let _ = conn.write_all(b"Invalid dispatcher");
                } else {
                    let _ = conn.write_all(b"ok");
                }
            }),
        )
    });
    let (response, elapsed) = run_one(
        &h,
        "hypr-a",
        "module_host.hyprland_dispatch",
        json!({ "cmd": "workspace", "args": "9" }),
        Duration::from_secs(30),
    );
    assert!(error_of(&response).contains("timed out"), "{response}");
    assert!(
        elapsed.as_secs_f64() < DEADLINE_SECS + 2.0,
        "the caller must hear back around the deadline, took {elapsed:?}"
    );

    // Well past the 9.5s the compositor held the legacy request, and past
    // the 11.1s at which the unfixed daemon sent its fallback.
    std::thread::sleep(Duration::from_secs(7));
    assert_eq!(
        comp.requests(),
        vec!["dispatch workspace 9"],
        "nothing may be sent after the caller was told the call failed"
    );
}

/// (b) The compositor replies late, after the deadline, to a keyword that
/// needs a read-back. The late "ok" must not trigger the read-back request,
/// and the module must have seen a timeout rather than a success.
#[test]
fn late_reply_is_a_timeout_and_triggers_no_read_back() {
    let (h, comp) = Harness::spawn(&[ModuleSpec::granted("hypr-a")], |runtime| {
        Compositor::start(
            runtime,
            Arc::new(|request, conn| {
                if request.starts_with("keyword bind") {
                    std::thread::sleep(Duration::from_millis(9500));
                    let _ = conn.write_all(b"ok");
                } else {
                    let _ = conn.write_all(br#"[{"key":"Q","modmask":64}]"#);
                }
            }),
        )
    });
    let (response, _) = run_one(
        &h,
        "hypr-a",
        "module_host.hyprland_keyword",
        json!({ "key": "bind", "value": "SUPER, Q, exec, kitty" }),
        Duration::from_secs(30),
    );
    assert!(error_of(&response).contains("timed out"), "{response}");
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(comp.requests(), vec!["keyword bind SUPER, Q, exec, kitty"]);
}

/// A host killed while its dispatch is stalled must not cause the daemon to
/// send the eval fallback afterwards. The independent connection reader sees
/// the disconnect while the first request is in flight and cancels the whole
/// operation before the compositor rejects it.
#[test]
fn host_death_during_stalled_dispatch_sends_no_fallback() {
    let (h, comp) = Harness::spawn(&[ModuleSpec::granted("hypr-a")], |runtime| {
        Compositor::start(
            runtime,
            Arc::new(|request, conn| {
                if request.starts_with("dispatch ") {
                    std::thread::sleep(Duration::from_secs(2));
                    let _ = conn.write_all(b"Invalid dispatcher");
                } else {
                    let _ = conn.write_all(b"ok");
                }
            }),
        )
    });
    h.wait_ready(&["hypr-a"]);
    let pids = h.wait_host_pids(&["hypr-a"]);
    h.set_script(
        "hypr-a",
        vec![(
            "op",
            request(
                "1",
                "module_host.hyprland_dispatch",
                json!({ "cmd": "workspace", "args": "9" }),
            ),
        )],
    );
    h.go(&["hypr-a"]);

    let observed = Instant::now() + Duration::from_secs(10);
    loop {
        if comp
            .requests()
            .iter()
            .any(|seen| seen == "dispatch workspace 9")
        {
            break;
        }
        assert!(
            Instant::now() < observed,
            "the stalled dispatch never reached the compositor"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    Harness::kill_host(pids["hypr-a"], Duration::from_secs(5));
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        comp.requests(),
        vec!["dispatch workspace 9"],
        "no fallback after the owning host disconnects"
    );
}

/// (c) Oversized reply: refused, not buffered, not forwarded to the module.
/// And an oversized request is rejected before the compositor is contacted.
#[test]
fn oversized_reply_and_request_are_refused() {
    let (h, comp) = Harness::spawn(&[ModuleSpec::granted("hypr-a")], |runtime| {
        Compositor::start(
            runtime,
            Arc::new(|_, conn| {
                let chunk = vec![b'x'; 64 * 1024];
                for _ in 0..64 {
                    if conn.write_all(&chunk).is_err() {
                        return;
                    }
                }
            }),
        )
    });
    h.wait_ready(&["hypr-a"]);
    h.set_script(
        "hypr-a",
        vec![
            (
                "big-reply",
                request(
                    "1",
                    "module_host.hyprland_eval",
                    json!({ "expr": "hl.get_windows()" }),
                ),
            ),
            (
                "big-request",
                request(
                    "2",
                    "module_host.hyprland_eval",
                    json!({ "expr": "x".repeat(64 * 1024) }),
                ),
            ),
        ],
    );
    h.go(&["hypr-a"]);
    let done = h.wait_done(&["hypr-a"], Duration::from_secs(30));
    let results = &done["hypr-a"]["results"];

    let reply = &results[0]["response"];
    assert!(error_of(reply).contains("byte limit"), "{}", {
        let text = reply.to_string();
        text.chars().take(300).collect::<String>()
    });
    assert!(reply.get("result").is_none());
    let request_resp = &results[1]["response"];
    assert!(
        error_of(request_resp).contains("byte limit"),
        "{request_resp}"
    );

    let seen = comp.requests();
    assert_eq!(
        seen,
        vec!["eval hl.get_windows()"],
        "oversized request must not be sent"
    );
}

/// (d) The compositor accepts a query and then stalls. The test stops the
/// daemon only after that actual request is observed, so shutdown is tested
/// with a genuinely in-flight call. A blocked compositor worker must not
/// delay daemon shutdown, and no fallback or read-back may follow.
#[test]
fn accepted_but_stalled_call_does_not_hold_up_shutdown() {
    let (mut h, comp) = Harness::spawn(&[ModuleSpec::granted("hypr-a")], |runtime| {
        Compositor::start(
            runtime,
            Arc::new(|_, conn| {
                std::thread::sleep(Duration::from_secs(12));
                let _ = conn.write_all(b"[]");
            }),
        )
    });
    h.wait_ready(&["hypr-a"]);
    h.set_script(
        "hypr-a",
        vec![(
            "op",
            request(
                "1",
                "module_host.hyprland_query",
                json!({ "kind": "monitors" }),
            ),
        )],
    );
    h.go(&["hypr-a"]);

    let observed = Instant::now() + Duration::from_secs(10);
    loop {
        if comp.requests().iter().any(|seen| seen == "j/monitors") {
            break;
        }
        assert!(
            Instant::now() < observed,
            "the stalled query never reached the compositor"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let started = Instant::now();
    let status = h.terminate(Duration::from_secs(5));
    assert!(
        status.is_some(),
        "daemon did not exit within 5s of SIGTERM with a stalled compositor call in flight"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        comp.requests(),
        vec!["j/monitors"],
        "shutdown must leave the stalled call alone"
    );
}

/// Stalled calls from many hosts at once cannot all pin workers: calls beyond
/// the global bound are refused immediately and never reach the compositor.
#[test]
fn stalled_compositor_cannot_accumulate_unbounded_workers() {
    const HOSTS: usize = 12;
    const NAMES: [&str; HOSTS] = [
        "h00", "h01", "h02", "h03", "h04", "h05", "h06", "h07", "h08", "h09", "h10", "h11",
    ];
    let specs: Vec<ModuleSpec> = NAMES.iter().map(|n| ModuleSpec::granted(n)).collect();
    let (h, comp) = Harness::spawn(&specs, |runtime| {
        Compositor::start(
            runtime,
            Arc::new(|_, conn| {
                std::thread::sleep(Duration::from_secs(12));
                let _ = conn.write_all(b"[]");
            }),
        )
    });
    h.wait_ready(&NAMES);
    for name in NAMES {
        h.set_script(
            name,
            vec![(
                "op",
                request(
                    "1",
                    "module_host.hyprland_query",
                    json!({ "kind": "monitors" }),
                ),
            )],
        );
    }
    h.go(&NAMES);
    let done = h.wait_done(&NAMES, Duration::from_secs(40));

    let mut refused = 0;
    let mut timed_out = 0;
    for name in NAMES {
        let response = &done[name]["results"][0]["response"];
        let err = error_of(response);
        if err.contains("too many requests in flight") {
            refused += 1;
        } else if err.contains("timed out") {
            timed_out += 1;
        } else {
            panic!("{name}: unexpected response {response}");
        }
    }
    assert_eq!(timed_out + refused, HOSTS);
    assert!(
        refused >= 1,
        "no call was refused; the bound is not enforced"
    );
    assert!(
        timed_out <= 8,
        "more than 8 calls were in flight at once: {timed_out}"
    );
    assert!(comp.requests().len() <= 8, "{}", comp.requests().len());
}
