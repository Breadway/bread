use super::*;
use serde_json::json;

fn scrubber() -> Scrubber {
    Scrubber::new(ScrubContext {
        home: Some("/home/alicesmith".into()),
        username: Some("alicesmith".into()),
        hostname: Some("thinkbox-9".into()),
        extra_literals: vec!["Alice Smith".into()],
    })
}

#[test]
fn numbering_is_stable_and_distinct() {
    let mut s = scrubber();
    let out = s.scrub_text("a 203.0.113.42 b 198.51.100.7 c 203.0.113.42");
    assert_eq!(out, "a <IP:1> b <IP:2> c <IP:1>");
}

#[test]
fn reset_restarts_numbering() {
    let mut s = scrubber();
    assert_eq!(s.scrub_text("203.0.113.42"), "<IP:1>");
    s.reset();
    assert_eq!(s.scrub_text("198.51.100.7"), "<IP:1>");
}

#[test]
fn markers_pass_through_untouched() {
    let mut s = scrubber();
    assert_eq!(
        s.scrub_text("x <IP:7> <HOME>/a <SECRET>"),
        "x <IP:7> <HOME>/a <SECRET>"
    );
}

#[test]
fn username_equal_to_marker_word_does_not_corrupt_markers() {
    let mut s = Scrubber::new(ScrubContext {
        username: Some("secret".into()),
        ..Default::default()
    });
    let once = s.scrub_text("hello secret and 203.0.113.42");
    assert_eq!(s.scrub_text(&once), once);
    assert!(once.contains("<IP:1>"));
}

#[test]
fn loopback_and_unspecified_are_kept() {
    let mut s = scrubber();
    assert_eq!(
        s.scrub_text("bind 127.0.0.1:7000 and 0.0.0.0 and ::1"),
        "bind 127.0.0.1:7000 and 0.0.0.0 and ::1"
    );
}

#[test]
fn longer_dotted_numbers_are_not_ips() {
    let mut s = scrubber();
    assert_eq!(
        s.scrub_text("lib 1.2.3.4.5 and 9.1.2.3.4"),
        "lib 1.2.3.4.5 and 9.1.2.3.4"
    );
}

#[test]
fn rust_paths_are_not_ipv6() {
    let mut s = scrubber();
    let line = "at core::fmt::Write::write_fmt cafe::bad 12:34:56";
    assert_eq!(s.scrub_text(line), line);
}

#[test]
fn home_paths_collapse() {
    let mut s = scrubber();
    assert_eq!(
        s.scrub_text("/home/alicesmith/.config/bread/init.lua and /home/bob/x"),
        "<HOME>/.config/bread/init.lua and <HOME>/x"
    );
}

#[test]
fn home_prefix_of_longer_name_is_not_replaced_by_literal() {
    let mut s = Scrubber::new(ScrubContext {
        home: Some("/srv/al".into()),
        ..Default::default()
    });
    assert_eq!(s.scrub_text("/srv/alpha /srv/al/x"), "/srv/alpha <HOME>/x");
}

#[test]
fn url_credentials_and_query_are_removed_but_path_kept() {
    let mut s = scrubber();
    assert_eq!(
        s.scrub_text("GET https://bob:hunter2@example.com/a/b?token=abc&x=1#frag now"),
        "GET https://<CRED>@example.com/a/b?<QUERY>#frag now"
    );
    assert_eq!(
        s.scrub_text("see https://example.com/docs."),
        "see https://example.com/docs."
    );
}

#[test]
fn kv_secrets_keep_the_key_and_quotes() {
    let mut s = scrubber();
    assert_eq!(s.scrub_text("password=hunter2 ok"), "password=<SECRET> ok");
    assert_eq!(
        s.scrub_text(r#"{"api_key": "abc123", "n": 1}"#),
        r#"{"api_key": "<SECRET>", "n": 1}"#
    );
    assert_eq!(
        s.scrub_text("Authorization: Bearer abcdefghijklmnop1234"),
        "Authorization: <SECRET>"
    );
}

#[test]
fn benign_keys_with_secret_substrings_survive() {
    let mut s = scrubber();
    assert_eq!(s.scrub_text("tests passed: 12"), "tests passed: 12");
    assert_eq!(s.scrub_text("tokenizer=bpe"), "tokenizer=bpe");
}

#[test]
fn ssid_title_serial_values_are_removed() {
    let mut s = scrubber();
    assert_eq!(s.scrub_text("ssid=Casa Alice 5G"), "ssid=<SSID>");
    assert_eq!(
        s.scrub_text(r#"{"title":"Inbox - Gmail","class":"firefox"}"#),
        r#"{"title":"<TITLE>","class":"firefox"}"#
    );
    assert_eq!(
        s.scrub_text("Serial Number: S3Z9NB0K1234"),
        "Serial Number: <SERIAL>"
    );
}

#[test]
fn identity_literals_respect_word_boundaries() {
    let mut s = scrubber();
    assert_eq!(
        s.scrub_text("alicesmith@thinkbox-9 ran; alicesmithson ok; Alice Smith wrote"),
        "<USER>@<HOST> ran; alicesmithson ok; <REDACTED> wrote"
    );
}

#[test]
fn common_names_are_not_redacted_as_identity() {
    let mut s = Scrubber::new(ScrubContext {
        username: Some("root".into()),
        hostname: Some("localhost".into()),
        ..Default::default()
    });
    assert_eq!(s.scrub_text("root@localhost"), "root@localhost");
}

#[test]
fn rust_symbols_hashes_and_build_ids_survive() {
    let mut s = scrubber();
    let sym = "_ZN4core3fmt5Write9write_fmt17h0123456789abcdefE";
    assert_eq!(s.scrub_text(sym), sym);
    let bid = "build-id: 3f2a9c1de4b5a6978081726354453423a1b2c3d4";
    assert_eq!(s.scrub_text(bid), bid);
    let loose = "3f2a9c1de4b5a6978081726354453423a1b2c3d4";
    assert_eq!(s.scrub_text(loose), "<HEX:1>");
    s.keep_build_id(loose);
    s.reset();
    assert_eq!(s.scrub_text(loose), loose);
}

#[test]
fn private_key_blocks_are_removed_even_when_truncated() {
    let mut s = scrubber();
    let out = s.scrub_text("k:\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3Nza\n");
    assert_eq!(out, "k:\n<SECRET>");
}

#[test]
fn non_utf8_bytes_do_not_panic_and_are_stable() {
    let mut s = scrubber();
    let bytes = b"bad \xff\xfe 203.0.113.42 \xc3\x28";
    let out = s.scrub_bytes(bytes);
    assert!(out.contains("<IP:1>"));
    assert_eq!(s.scrub_text(&out), out);
}

#[test]
fn json_values_and_keys_are_scrubbed() {
    let mut s = scrubber();
    let v = json!({"mail@example.org": ["203.0.113.42", {"n": 3}], "ok": true});
    let out = s.scrub_value(&v);
    assert_eq!(out, json!({"<EMAIL:1>": ["<IP:1>", {"n": 3}], "ok": true}));
}

#[test]
fn deeply_nested_json_is_truncated_not_overflowed() {
    let mut s = scrubber();
    let mut v = json!("leaf");
    for _ in 0..200 {
        v = json!([v]);
    }
    let out = s.scrub_value(&v);
    assert!(out.to_string().contains("<TRUNCATED>"));
}

#[test]
fn event_patterns() {
    assert!(event_matches("bread.window.opened", "bread.window.opened"));
    assert!(event_matches("bread.*.opened", "bread.window.opened"));
    assert!(!event_matches("bread.*", "bread.window.opened"));
    assert!(event_matches("bread.**", "bread.window.opened"));
    assert!(event_matches("bread.**", "bread"));
    assert!(!event_matches("bread.window", "bread.window.opened"));
}

#[test]
fn unlisted_event_payload_is_dropped_keeping_only_safe_key_names() {
    let mut s = scrubber();
    let allow = EventAllowlist::new().allow("bread.power.**", &["battery_percent"]);
    let payload = json!({"title": "Inbox", "class": "firefox", "Weird Key": 1, "address": "0xabc"});
    let out = s.scrub_event_payload("bread.window.opened", &payload, &allow);
    assert_eq!(
        out,
        json!({"_dropped": true, "_keys": ["address", "class", "title"]})
    );
}

#[test]
fn allowlisted_fields_are_kept_nested_and_scrubbed() {
    let mut s = scrubber();
    let allow = EventAllowlist::new()
        .allow("bread.network.*", &["online", "iface.name", "iface.addr"])
        .allow("bread.**", &["online"]);
    let payload = json!({
        "online": true,
        "iface": {"name": "wlan0", "addr": "203.0.113.42", "ssid": "Casa"},
        "extra": "x"
    });
    let out = s.scrub_event_payload("bread.network.connected", &payload, &allow);
    assert_eq!(
        out,
        json!({"online": true, "iface": {"name": "wlan0", "addr": "<IP:1>"}})
    );
}

#[test]
fn stats_count_redactions_by_category() {
    let mut s = scrubber();
    s.scrub_text("203.0.113.42 198.51.100.7 mail@example.org");
    assert_eq!(s.stats().get("IP"), Some(&2));
    assert_eq!(s.stats().get("EMAIL"), Some(&1));
}

#[test]
fn overflow_marker_after_cap() {
    let mut s = scrubber();
    for i in 0..(MAX_NUMBERED + 5) {
        s.numbered("IP", &format!("10.{}.{}.1", i / 250, i % 250));
    }
    let m = s.numbered("IP", "99.99.99.99");
    assert_eq!(m, "<IP:+>");
}

#[test]
fn default_distro_names_are_not_treated_as_identity() {
    let mut s = Scrubber::new(ScrubContext {
        hostname: Some("bos".into()),
        username: Some("arch".into()),
        ..Default::default()
    });
    assert_eq!(s.scrub_text("bos-settings on arch"), "bos-settings on arch");
}
