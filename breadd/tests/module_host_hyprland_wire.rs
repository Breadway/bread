//! BREAD-42 R2: the `module_host.hyprland_*` boundary, exercised over the real
//! authenticated wire protocol rather than through Lua nil-ness.
//!
//! Every host here is a real sandboxed child that authenticated with a real
//! one-time token (`examples/fake_module_host.rs`) and then sends the exact
//! request lines a test scripts. The compositor is a fake socket that records
//! everything it receives, so "denied" means the daemon answered with a denial
//! AND nothing reached the compositor.

mod hypr_support;

use std::collections::HashSet;
use std::time::Duration;

use hypr_support::*;
use serde_json::{json, Value};

const MONITORS: &str = r#"[{"name":"DP-1"}]"#;
const WORKSPACES: &str = r#"[{"id":1,"name":"1"}]"#;
const CLIENTS: &str = r#"[{"class":"kitty"}]"#;
const WINDOW: &str = r#"{"class":"kitty"}"#;

/// Answers like a `hyprland.lua` Hyprland. `eval echo <tag>` replies with the
/// request it received, so a test can tell whose request produced a reply.
fn compositor_behaviour() -> Behaviour {
    std::sync::Arc::new(|request, conn| {
        use std::io::Write;
        // Spread replies out so concurrent hosts really interleave.
        let jitter = request.bytes().map(u64::from).sum::<u64>() % 7;
        std::thread::sleep(Duration::from_millis(5 + jitter * 3));
        let reply = match request {
            "dispatch workspace 3" => "ok".to_string(),
            "dispatch workspace 9" => "Invalid dispatcher".to_string(),
            "eval hl.dispatch(hl.dsp.focus({ workspace = '9' }))" => {
                "error: no such workspace 9".to_string()
            }
            "j/monitors" => MONITORS.to_string(),
            "j/workspaces" => WORKSPACES.to_string(),
            "j/clients" => CLIENTS.to_string(),
            "j/activewindow" => WINDOW.to_string(),
            "keyword general:gaps_in 5" => "keyword can't work with non-legacy parsers".to_string(),
            "eval hl.config({ general = { gaps_in = 5 } })" => "ok".to_string(),
            other => format!("echo:{other}"),
        };
        let _ = conn.write_all(reply.as_bytes());
    })
}

type Params = fn() -> Value;

const FOUR_METHODS: [(&str, Params); 4] = [
    (
        "module_host.hyprland_dispatch",
        || json!({ "cmd": "workspace", "args": "3" }),
    ),
    (
        "module_host.hyprland_keyword",
        || json!({ "key": "general:gaps_in", "value": "5" }),
    ),
    (
        "module_host.hyprland_eval",
        || json!({ "expr": "echo denied-probe" }),
    ),
    (
        "module_host.hyprland_query",
        || json!({ "kind": "monitors" }),
    ),
];

fn results_by_label(done: &Value) -> std::collections::HashMap<String, Value> {
    done["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["label"].as_str().unwrap().to_string(),
                r["response"].clone(),
            )
        })
        .collect()
}

#[test]
fn authenticated_host_without_the_grant_is_denied_every_hyprland_method() {
    let (h, comp) = Harness::spawn(&[ModuleSpec::denied("hypr-denied")], |runtime| {
        Compositor::start(runtime, compositor_behaviour())
    });
    h.wait_ready(&["hypr-denied"]);

    let mut steps = Vec::new();
    for (i, (method, params)) in FOUR_METHODS.iter().enumerate() {
        steps.push((
            format!("plain-{i}"),
            request(&format!("p{i}"), method, params()),
        ));
    }
    for (i, (method, params)) in FOUR_METHODS.iter().enumerate() {
        // Forged claims inside the request must not count for anything.
        let mut forged = params();
        forged["module"] = json!("hypr-granted");
        forged["permissions"] = json!([{ "type": "hyprland" }]);
        forged["permission"] = json!("hyprland");
        forged["host"] = json!("hypr-granted");
        steps.push((
            format!("forged-{i}"),
            request(&format!("f{i}"), method, forged),
        ));
    }
    for (i, method) in [
        "module_host.hyprland_workspaces",
        "module_host.hyprland_clients",
        "module_host.hyprland_made_up",
        "module_host.hyprland_",
    ]
    .iter()
    .enumerate()
    {
        steps.push((
            format!("invented-{i}"),
            request(&format!("i{i}"), method, json!({})),
        ));
    }
    // The query kinds workspaces/clients go through hyprland_query too.
    for kind in ["workspaces", "clients", "active_window"] {
        steps.push((
            format!("query-{kind}"),
            request(
                &format!("q-{kind}"),
                "module_host.hyprland_query",
                json!({ "kind": kind }),
            ),
        ));
    }
    let labels: Vec<String> = steps.iter().map(|(l, _)| l.clone()).collect();
    h.set_script(
        "hypr-denied",
        steps.iter().map(|(l, r)| (l.as_str(), r.clone())).collect(),
    );
    h.go(&["hypr-denied"]);
    let done = h.wait_done(&["hypr-denied"], Duration::from_secs(30));
    let results = results_by_label(&done["hypr-denied"]);

    for label in &labels {
        let response = &results[label];
        assert_eq!(
            error_of(response),
            "hyprland not granted to this module",
            "{label}: {response}"
        );
        assert!(response.get("result").is_none(), "{label}: {response}");
    }
    assert!(
        comp.requests().is_empty(),
        "a denied host must never reach the compositor: {:?}",
        comp.requests()
    );
}

#[test]
fn granted_host_reaches_workspaces_and_clients_through_the_bridge() {
    let (h, comp) = Harness::spawn(&[ModuleSpec::granted("hypr-a")], |runtime| {
        Compositor::start(runtime, compositor_behaviour())
    });
    h.wait_ready(&["hypr-a"]);
    h.set_script(
        "hypr-a",
        vec![
            (
                "workspaces",
                request(
                    "1",
                    "module_host.hyprland_query",
                    json!({ "kind": "workspaces" }),
                ),
            ),
            (
                "clients",
                request(
                    "2",
                    "module_host.hyprland_query",
                    json!({ "kind": "clients" }),
                ),
            ),
            (
                "bad-type",
                request(
                    "3",
                    "module_host.hyprland_dispatch",
                    json!({ "cmd": "workspace", "args": 3 }),
                ),
            ),
        ],
    );
    h.go(&["hypr-a"]);
    let done = h.wait_done(&["hypr-a"], Duration::from_secs(30));
    let results = results_by_label(&done["hypr-a"]);

    assert_eq!(
        results["workspaces"]["result"]["value"],
        json!([{ "id": 1, "name": "1" }])
    );
    assert_eq!(
        results["clients"]["result"]["value"],
        json!([{ "class": "kitty" }])
    );
    assert_eq!(error_of(&results["bad-type"]), "args must be a string");
    assert_eq!(comp.requests(), vec!["j/workspaces", "j/clients"]);
}

#[test]
fn granted_host_cannot_act_as_another_host() {
    // The granted host holds hyprland; the denied host must not become it by
    // claiming its name, its permissions or its request ids.
    let forged_hello = json!({
        "module": "hypr-granted",
        "permissions": [{ "type": "hyprland" }],
        "permission": "hyprland",
    });
    let (h, comp) = Harness::spawn(
        &[
            ModuleSpec::granted("hypr-granted"),
            ModuleSpec::denied("hypr-denied").with_hello(forged_hello),
        ],
        |runtime| Compositor::start(runtime, compositor_behaviour()),
    );
    let tokens = h.wait_ready(&["hypr-granted", "hypr-denied"]);

    // 1. A denied host that forged its hello (module name, permissions) and
    //    reuses the granted host's request ids is still denied.
    h.set_script(
        "hypr-denied",
        vec![(
            "spoofed-hello",
            request(
                "same-id",
                "module_host.hyprland_dispatch",
                json!({ "cmd": "workspace", "args": "3" }),
            ),
        )],
    );
    // 2. The granted host sends the same request id twice (a replay): each
    //    request is answered on its own, once, and is not served from a cache.
    h.set_script(
        "hypr-granted",
        vec![
            (
                "first",
                request(
                    "same-id",
                    "module_host.hyprland_dispatch",
                    json!({ "cmd": "workspace", "args": "3" }),
                ),
            ),
            (
                "replayed-id",
                request(
                    "same-id",
                    "module_host.hyprland_eval",
                    json!({ "expr": "echo replay" }),
                ),
            ),
            (
                "as-denied-host",
                request(
                    "x",
                    "module_host.hyprland_query",
                    json!({ "kind": "monitors", "module": "hypr-denied", "permissions": [] }),
                ),
            ),
        ],
    );
    h.go(&["hypr-granted", "hypr-denied"]);
    let done = h.wait_done(&["hypr-granted", "hypr-denied"], Duration::from_secs(30));

    let denied = results_by_label(&done["hypr-denied"]);
    assert_eq!(
        error_of(&denied["spoofed-hello"]),
        "hyprland not granted to this module",
        "hello: {}",
        done["hypr-denied"]["hello"]
    );
    // The daemon told the denied host who it is, whatever it claimed.
    assert_eq!(
        done["hypr-denied"]["hello"]["result"]["module"],
        json!("hypr-denied")
    );
    let perms = done["hypr-denied"]["hello"]["result"]["permissions"].to_string();
    assert!(!perms.contains("hyprland"), "{perms}");

    let granted = results_by_label(&done["hypr-granted"]);
    assert_eq!(granted["first"]["id"], json!("same-id"));
    assert_eq!(granted["first"]["result"]["ok"], json!(true));
    assert_eq!(granted["replayed-id"]["id"], json!("same-id"));
    assert_eq!(
        granted["replayed-id"]["result"]["reply"],
        json!("echo:eval echo replay")
    );
    // Claiming to be another module in the params changes nothing.
    assert_eq!(
        granted["as-denied-host"]["result"]["value"],
        json!([{ "name": "DP-1" }])
    );

    // 3. Replaying or inventing a token on a fresh connection grants nothing,
    //    even for a token that was real and is held by a live host.
    let probe = json!({ "cmd": "workspace", "args": "3" });
    let invented = uuid_like();
    for (label, token) in [
        ("replayed granted token", tokens["hypr-granted"].clone()),
        ("replayed denied token", tokens["hypr-denied"].clone()),
        ("invented token", invented),
        ("empty token", String::new()),
    ] {
        let replies = raw_exchange(
            &h.socket_path,
            &[
                request("h", "module_host.hello", json!({ "token": token })),
                request("d", "module_host.hyprland_dispatch", probe.clone()),
            ],
            Duration::from_secs(3),
        );
        let hello = replies[0]
            .as_ref()
            .unwrap_or_else(|| panic!("{label}: no hello reply"));
        assert_eq!(
            error_of(hello),
            "invalid or expired module-host token",
            "{label}"
        );
        assert!(hello.get("result").is_none(), "{label}: {hello}");
        assert!(
            replies[1].is_none(),
            "{label}: connection must be closed after a failed hello, got {:?}",
            replies[1]
        );
    }
    let no_token = raw_exchange(
        &h.socket_path,
        &[request("h", "module_host.hello", json!({}))],
        Duration::from_secs(3),
    );
    assert_eq!(
        error_of(no_token[0].as_ref().unwrap()),
        "module_host.hello: missing token"
    );

    // 4. Without any handshake the methods do not exist.
    for (method, params) in FOUR_METHODS {
        let replies = raw_exchange(
            &h.socket_path,
            &[request("u", method, params())],
            Duration::from_secs(3),
        );
        let reply = replies[0].as_ref().unwrap();
        assert!(
            error_of(reply).contains("unknown method"),
            "{method}: {reply}"
        );
    }

    // Only the granted host's three requests ever reached the compositor.
    assert_eq!(
        comp.requests().len(),
        3,
        "unexpected compositor traffic: {:?}",
        comp.requests()
    );
    assert!(comp
        .requests()
        .iter()
        .all(|r| r == "dispatch workspace 3" || r == "eval echo replay" || r == "j/monitors"));
}

#[test]
fn two_granted_hosts_calling_concurrently_never_receive_each_others_responses() {
    const CALLS: usize = 25;
    let (h, comp) = Harness::spawn(
        &[
            ModuleSpec::granted("hypr-a"),
            ModuleSpec::granted("hypr-b"),
            ModuleSpec::denied("hypr-denied"),
        ],
        |runtime| Compositor::start(runtime, compositor_behaviour()),
    );
    h.wait_ready(&["hypr-a", "hypr-b", "hypr-denied"]);

    // Both granted hosts reuse the SAME request ids (c0..cN). Host A's
    // requests succeed and echo A-tagged text; host B's fail and its errors
    // come from a different compositor path, so any cross-delivery shows.
    let mut a = Vec::new();
    let mut b = Vec::new();
    let mut denied = Vec::new();
    for i in 0..CALLS {
        let id = format!("c{i}");
        a.push((
            format!("a{i}"),
            request(
                &id,
                "module_host.hyprland_eval",
                json!({ "expr": format!("echo host-a-{i}") }),
            ),
        ));
        b.push((
            format!("b{i}"),
            if i % 2 == 0 {
                request(
                    &id,
                    "module_host.hyprland_dispatch",
                    json!({ "cmd": "workspace", "args": "9" }),
                )
            } else {
                request(
                    &id,
                    "module_host.hyprland_eval",
                    json!({ "expr": format!("echo host-b-{i}") }),
                )
            },
        ));
        denied.push((
            format!("d{i}"),
            request(
                &id,
                "module_host.hyprland_eval",
                json!({ "expr": format!("echo host-d-{i}") }),
            ),
        ));
    }
    for (name, steps) in [("hypr-a", &a), ("hypr-b", &b), ("hypr-denied", &denied)] {
        h.set_script(
            name,
            steps.iter().map(|(l, r)| (l.as_str(), r.clone())).collect(),
        );
    }
    h.go(&["hypr-a", "hypr-b", "hypr-denied"]);
    let done = h.wait_done(
        &["hypr-a", "hypr-b", "hypr-denied"],
        Duration::from_secs(60),
    );

    let (ra, rb, rd) = (
        results_by_label(&done["hypr-a"]),
        results_by_label(&done["hypr-b"]),
        results_by_label(&done["hypr-denied"]),
    );
    for i in 0..CALLS {
        let id = format!("c{i}");
        let ours = &ra[&format!("a{i}")];
        assert_eq!(ours["id"], json!(id));
        assert_eq!(
            ours["result"]["reply"],
            json!(format!("echo:eval echo host-a-{i}")),
            "{ours}"
        );

        let theirs = &rb[&format!("b{i}")];
        assert_eq!(theirs["id"], json!(id));
        if i % 2 == 0 {
            let err = error_of(theirs);
            assert!(
                err.contains("hyprland rejected `dispatch workspace 9`"),
                "{theirs}"
            );
            assert!(err.contains("no such workspace 9"), "{theirs}");
        } else {
            assert_eq!(
                theirs["result"]["reply"],
                json!(format!("echo:eval echo host-b-{i}")),
                "{theirs}"
            );
        }

        let none = &rd[&format!("d{i}")];
        assert_eq!(none["id"], json!(id));
        assert_eq!(
            error_of(none),
            "hyprland not granted to this module",
            "{none}"
        );
    }

    let seen = comp.requests();
    assert!(
        !seen.iter().any(|r| r.contains("host-d-")),
        "a denied host's request reached the compositor"
    );
    let expected: HashSet<String> = (0..CALLS)
        .flat_map(|i| {
            let mut v = vec![format!("eval echo host-a-{i}")];
            if i % 2 == 0 {
                v.push("dispatch workspace 9".to_string());
                v.push("eval hl.dispatch(hl.dsp.focus({ workspace = '9' }))".to_string());
            } else {
                v.push(format!("eval echo host-b-{i}"));
            }
            v
        })
        .collect();
    for request in &seen {
        assert!(
            expected.contains(request),
            "unexpected compositor request {request:?}"
        );
    }
}

fn uuid_like() -> String {
    "00000000-0000-4000-8000-000000000000".to_string()
}
