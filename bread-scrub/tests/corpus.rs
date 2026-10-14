//! Golden PII corpus, fuzz and idempotence tests for bread-scrub.
//!
//! Every fixture is seeded with fake personal data. The test fails if any
//! seeded value survives scrubbing, and also checks that the debugging
//! value of the text (symbols, crate paths, build-ids) is preserved.

use bread_scrub::{EventAllowlist, ScrubContext, Scrubber};
use serde_json::json;

const HOME: &str = "/home/alicesmith";
const USER: &str = "alicesmith";
const HOST: &str = "thinkbox-9";
const BUILD_ID: &str = "3f2a9c1de4b5a6978081726354453423a1b2c3d4";

/// Values that must never appear in scrubbed output.
const SEEDS: &[&str] = &[
    "alicesmith",
    "thinkbox-9",
    "Alice Smith",
    "alice.smith@example.org",
    "203.0.113.42",
    "192.168.1.77",
    "2001:db8:85a3::8a2e:370:7334",
    "fe80::1c2d:3e4f:5a6b:7c8d",
    "aa:bb:cc:11:22:33",
    "AA:BB:CC:44:55:66",
    "123e4567-e89b-12d3-a456-426614174000",
    "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
    "hunter2hunter2",
    "sup3rS3cretValue",
    "Casa Alice 5G",
    "Inbox (3) - alice.smith@example.org - Gmail",
    "S3Z9NB0K1234",
    "q9Zr7LmX2vTb8NcW4yHd6KfJ3sPa1UeG",
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "BEGIN OPENSSH PRIVATE KEY",
    "b3BlbnNzaC1rZXktdjEAAAAABG5vbmU",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
    "AKIAIOSFODNN7EXAMPLE",
    "token=abc123def456",
    "access_token=ya29.a0AfH6SMBxExample",
];

fn corpus() -> Vec<String> {
    vec![
        // panic + backtrace
        format!(
            "thread 'main' panicked at {HOME}/Projects/bread/breadd/src/core/state_engine.rs:529:13:\n\
             called `Option::unwrap()` on a `None` value\n\
             stack backtrace:\n   0: _ZN4core9panicking5panic17h0123456789abcdefE\n\
             \x20            at {HOME}/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.40.0/src/runtime/mod.rs:42:5\n\
             build-id: {BUILD_ID}"
        ),
        // journal lines
        format!("Oct 09 12:01:02 {HOST} breadd[4242]: adapter hyprland connected for {USER}"),
        format!("{USER}@{HOST}:~$ breadbug --app clip"),
        "NetworkManager: device (wlan0): ssid=Casa Alice 5G bssid=AA:BB:CC:44:55:66 ip 192.168.1.77/24".into(),
        "wlan0: associated with aa:bb:cc:11:22:33 from 203.0.113.42".into(),
        "route via 2001:db8:85a3::8a2e:370:7334 dev wlan0 src fe80::1c2d:3e4f:5a6b:7c8d".into(),
        "mailto: Alice Smith <alice.smith@example.org> sent a note".into(),
        // events
        r#"{"event":"bread.window.opened","title":"Inbox (3) - alice.smith@example.org - Gmail","class":"firefox"}"#.into(),
        "udev: serial=S3Z9NB0K1234 vendor=0bda product=5411".into(),
        // credentials
        "login password=hunter2hunter2 api_key: sup3rS3cretValue".into(),
        "curl -H 'Authorization: Bearer q9Zr7LmX2vTb8NcW4yHd6KfJ3sPa1UeG' https://api.example.com/v1".into(),
        "git remote https://alicesmith:ghp_16C7e42F292c6912E7710c838347Ae178B4a@git.example.com/x.git".into(),
        "callback https://app.example.com/cb#access_token=ya29.a0AfH6SMBxExample&state=1".into(),
        "GET /reset?token=abc123def456&user=alice HTTP/1.1".into(),
        "AWS key AKIAIOSFODNN7EXAMPLE in env".into(),
        "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r end".into(),
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmU\n-----END OPENSSH PRIVATE KEY-----".into(),
        "random blob q9Zr7LmX2vTb8NcW4yHd6KfJ3sPa1UeG in a log".into(),
        "machine id 123e4567-e89b-12d3-a456-426614174000 boot".into(),
        "sha deadbeefdeadbeefdeadbeefdeadbeefdeadbeef pushed".into(),
        // user notes
        "it crashed when I pasted Alice Smith's address, my email is alice.smith@example.org".into(),
    ]
}

fn scrubber() -> Scrubber {
    Scrubber::new(ScrubContext {
        home: Some(HOME.into()),
        username: Some(USER.into()),
        hostname: Some(HOST.into()),
        extra_literals: vec!["Alice Smith".into()],
    })
}

#[test]
fn no_seeded_value_survives() {
    let mut failures = Vec::new();
    for line in corpus() {
        let mut s = scrubber();
        let out = s.scrub_text(&line);
        for seed in SEEDS {
            if out.contains(seed) {
                failures.push(format!(
                    "seed {seed:?} survived in:\n{out}\n(from:\n{line})"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}

#[test]
fn debugging_value_is_preserved() {
    let mut s = scrubber();
    let out = s.scrub_text(&corpus()[0]);
    for keep in [
        "thread 'main' panicked at",
        "Option::unwrap()",
        "<HOME>/Projects/bread/breadd/src/core/state_engine.rs:529:13",
        "_ZN4core9panicking5panic17h0123456789abcdefE",
        "tokio-1.40.0/src/runtime/mod.rs:42:5",
        BUILD_ID,
    ] {
        assert!(out.contains(keep), "lost {keep:?} in:\n{out}");
    }
}

#[test]
fn whole_corpus_in_one_scope_numbers_consistently() {
    let mut s = scrubber();
    let all = corpus().join("\n");
    let out = s.scrub_text(&all);
    for seed in SEEDS {
        assert!(
            !out.contains(seed),
            "seed {seed:?} survived the joined corpus"
        );
    }
    assert!(out.contains("<IP:1>") && out.contains("<IP:2>"));
}

#[test]
fn corpus_is_idempotent_with_fresh_and_same_scrubber() {
    for line in corpus() {
        let mut s = scrubber();
        let once = s.scrub_text(&line);
        assert_eq!(s.scrub_text(&once), once, "same scrubber, input:\n{line}");
        assert_eq!(
            scrubber().scrub_text(&once),
            once,
            "fresh scrubber, input:\n{line}"
        );
    }
}

#[test]
fn event_payload_allowlist_on_realistic_events() {
    let mut s = scrubber();
    let allow = EventAllowlist::new().allow("bread.power.**", &["ac_connected", "battery_percent"]);
    let window = json!({
        "address": "0x55d8c0ffee",
        "workspace": 2,
        "class": "firefox",
        "title": "Inbox (3) - alice.smith@example.org - Gmail"
    });
    let out = s.scrub_event_payload("bread.window.opened", &window, &allow);
    assert!(!out.to_string().contains("alice.smith"));
    assert_eq!(out["_dropped"], json!(true));
    let power = json!({"ac_connected": true, "battery_percent": 91, "owner": "alicesmith"});
    let out = s.scrub_event_payload("bread.power.ac.connected", &power, &allow);
    assert_eq!(out["ac_connected"], json!(true));
    assert!(out.get("owner").is_none(), "unlisted field leaked");
}

/// Tiny deterministic PRNG so the fuzz test needs no extra dependency.
struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const FRAGMENTS: &[&str] = &[
    "203.0.113.42",
    "192.168.1.77",
    "::1",
    "fe80::1",
    "2001:db8::1",
    "alice.smith@example.org",
    "aa:bb:cc:11:22:33",
    "123e4567-e89b-12d3-a456-426614174000",
    "password=",
    "token: ",
    "Authorization: Bearer ",
    "https://u:p@h.example/x?y=1#z",
    "/home/alicesmith/x",
    HOME,
    USER,
    HOST,
    "<IP:1>",
    "<HOME>",
    "<SECRET>",
    "<",
    ">",
    "<<",
    "title=",
    "\"title\":\"",
    "ssid=",
    "serial: ",
    "build-id: ",
    BUILD_ID,
    "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
    "q9Zr7LmX2vTb8NcW4yHd6KfJ3sPa1UeG",
    "-----BEGIN PRIVATE KEY-----",
    " ",
    "\n",
    "\t",
    "\"",
    "'",
    "=",
    ":",
    ";",
    ",",
    ".",
    "/",
    "\u{fffd}",
    "é",
    "日本",
    "0x7ffd1234",
    "_ZN4core3fmt17h0123456789abcdefE",
    "1.2.3.4.5",
    "v1.2.3.4",
    "a",
    "Z",
    "9",
    "-",
    "_",
    "+",
    "@",
    "?",
    "#",
    "&",
];

fn random_text(rng: &mut Xorshift) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..rng.below(24) {
        if rng.below(10) == 0 {
            for _ in 0..rng.below(6) {
                out.push(rng.next() as u8);
            }
        } else {
            out.extend_from_slice(FRAGMENTS[rng.below(FRAGMENTS.len())].as_bytes());
        }
    }
    out
}

#[test]
fn fuzz_never_panics_and_is_idempotent() {
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    for i in 0..20_000 {
        let input = random_text(&mut rng);
        let mut s = scrubber();
        let once = s.scrub_bytes(&input);
        let twice = s.scrub_text(&once);
        assert_eq!(
            once, twice,
            "not idempotent (iteration {i}) for input {input:?}"
        );
        let fresh = scrubber().scrub_text(&once);
        assert_eq!(
            once, fresh,
            "fresh scrubber disagrees (iteration {i}) for input {input:?}"
        );
    }
}

fn glued_to_number(input: &str, pii: &str) -> bool {
    input.match_indices(pii).any(|(i, _)| {
        let before = input[..i].chars().next_back();
        let after = input[i + pii.len()..].chars().next();
        let numeric = |c: char| c.is_ascii_digit() || c == '.';
        pii.starts_with(|c: char| c.is_ascii_digit())
            && (before.is_some_and(numeric) || after.is_some_and(numeric))
    })
}

#[test]
fn fuzz_fragment_pii_never_survives() {
    // Whatever surrounds them, these fragments must not survive intact.
    let must_not_survive = [
        "203.0.113.42",
        "192.168.1.77",
        "alice.smith@example.org",
        "aa:bb:cc:11:22:33",
        "123e4567-e89b-12d3-a456-426614174000",
        "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
        "q9Zr7LmX2vTb8NcW4yHd6KfJ3sPa1UeG",
    ];
    let mut rng = Xorshift(0xDEAD_BEEF_CAFE_F00D);
    for i in 0..20_000 {
        let input = String::from_utf8_lossy(&random_text(&mut rng)).into_owned();
        let out = scrubber().scrub_text(&input);
        for pii in must_not_survive {
            // A numeric fragment glued to more digits or dots ("...42" +
            // "1.2.3.4.5") is a different, ambiguous number, not a leak.
            if glued_to_number(&input, pii) {
                continue;
            }
            assert!(
                !(input.contains(pii) && out.contains(pii)),
                "{pii:?} survived (iteration {i})\ninput: {input:?}\noutput: {out:?}"
            );
        }
    }
}
