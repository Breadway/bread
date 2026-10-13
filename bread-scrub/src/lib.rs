//! PII scrubber for breadbug.
//!
//! The same [`Scrubber`] is used twice: when log lines and events are
//! ingested into the breadd store, and again when a report is assembled
//! (including the user's free-text notes). Two passes mean a pattern missed
//! at ingest still gets another chance before anything leaves the machine.
//!
//! Design rules:
//!
//! * **Structured data is allowlisted.** Event payloads are dropped unless
//!   an [`EventAllowlist`] rule names the fields to keep. Dropped payloads
//!   leave only the event name. Unlisted key names can contain personal data.
//! * **Free text is pattern-redacted.** Log lines, panic messages and notes
//!   go through [`Scrubber::scrub_text`], which replaces emails, IPs, MACs,
//!   UUIDs, tokens, credentials, home paths, the user and host names and
//!   similar values with markers such as `<IP:1>` or `<HOME>`.
//! * **Markers are stable within one scrubber.** The same value always maps
//!   to the same marker, so two distinct addresses stay distinguishable
//!   (`<IP:1>` vs `<IP:2>`). The table stores only hashes, never the values.
//!   Call [`Scrubber::reset`] to start a new numbering scope (a new report).
//! * **Scrubbing is idempotent.** Markers are preserved verbatim and never
//!   re-scrubbed, and `scrub_text` runs to a fixpoint, so
//!   `scrub(scrub(x)) == scrub(x)`.
//!
//! Limits worth knowing: this is best effort on free text. It cannot know
//! that an arbitrary word is a person's name. That is why structured fields
//! use an allowlist and why the user previews the final payload.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::BuildHasher;
use std::net::Ipv6Addr;
use std::str::FromStr;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

/// Cap on distinct numbered markers per category. Past this, values share
/// the overflow marker `<CAT:+>` so the table cannot grow without bound.
const MAX_NUMBERED: u32 = 4096;
/// Maximum JSON nesting depth scrubbed before a subtree is truncated.
const MAX_DEPTH: usize = 64;
/// Extra passes allowed when chasing the idempotence fixpoint.
const MAX_EXTRA_PASSES: usize = 3;

/// Names that are never treated as personal even if they are the user name
/// or hostname, because replacing them would only damage the log.
const COMMON_NAMES: &[&str] = &[
    "root",
    "user",
    "users",
    "admin",
    "test",
    "guest",
    "bread",
    "localhost",
    "nobody",
    "daemon",
    "home",
    "tmp",
    "bin",
    "usr",
    "var",
    "etc",
    "run",
];

/// Local identity values the scrubber must remove from free text.
#[derive(Clone, Debug, Default)]
pub struct ScrubContext {
    /// The user's home directory, e.g. `/home/alice`.
    pub home: Option<String>,
    /// The login name.
    pub username: Option<String>,
    /// The machine hostname.
    pub hostname: Option<String>,
    /// Anything else to redact verbatim (display name, SSIDs, ...).
    pub extra_literals: Vec<String>,
}

impl ScrubContext {
    /// Build a context from `$HOME`, `$USER`/`$LOGNAME` and the kernel
    /// hostname. Missing values are simply skipped.
    pub fn from_environment() -> Self {
        let nonempty = |v: String| {
            let v = v.trim().to_string();
            (!v.is_empty()).then_some(v)
        };
        let home = std::env::var("HOME").ok().and_then(nonempty);
        let username = std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("LOGNAME").ok())
            .and_then(nonempty);
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
            .and_then(nonempty);
        Self {
            home,
            username,
            hostname,
            extra_literals: Vec::new(),
        }
    }
}

/// Which event payload fields are safe to keep, keyed by event pattern.
///
/// Patterns use the bread event wildcards: `*` matches one dot-separated
/// segment, `**` matches any number of segments. Field names are dotted
/// paths into the payload object (`"workspace.id"`). Events with no
/// matching rule have their payload dropped entirely.
#[derive(Clone, Debug, Default)]
pub struct EventAllowlist {
    rules: Vec<(String, Vec<String>)>,
}

impl EventAllowlist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow `fields` for every event matching `pattern`.
    pub fn allow(mut self, pattern: &str, fields: &[&str]) -> Self {
        self.rules.push((
            pattern.to_string(),
            fields.iter().map(|f| f.to_string()).collect(),
        ));
        self
    }

    /// Union of the allowed fields of every rule matching `event`, or
    /// `None` when no rule matches.
    pub fn fields_for(&self, event: &str) -> Option<BTreeSet<&str>> {
        let mut found = false;
        let mut fields = BTreeSet::new();
        for (pattern, allowed) in &self.rules {
            if event_matches(pattern, event) {
                found = true;
                fields.extend(allowed.iter().map(String::as_str));
            }
        }
        found.then_some(fields)
    }
}

fn event_matches(pattern: &str, event: &str) -> bool {
    fn go(p: &[&str], e: &[&str]) -> bool {
        match p.split_first() {
            None => e.is_empty(),
            Some((&"**", rest)) => (0..=e.len()).any(|i| go(rest, &e[i..])),
            Some((&"*", rest)) => !e.is_empty() && go(rest, &e[1..]),
            Some((seg, rest)) => e.first() == Some(seg) && go(rest, &e[1..]),
        }
    }
    let p: Vec<&str> = pattern.split('.').collect();
    let e: Vec<&str> = event.split('.').collect();
    go(&p, &e)
}

enum Piece {
    Text(String),
    Marker(String),
}

/// Accumulates rule output as alternating text and marker pieces.
struct Out {
    pieces: Vec<Piece>,
    buf: String,
}

impl Out {
    fn new() -> Self {
        Self {
            pieces: Vec::new(),
            buf: String::new(),
        }
    }

    fn text(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    fn marker(&mut self, m: String) {
        if !self.buf.is_empty() {
            self.pieces.push(Piece::Text(std::mem::take(&mut self.buf)));
        }
        self.pieces.push(Piece::Marker(m));
    }

    fn finish(mut self) -> Vec<Piece> {
        if !self.buf.is_empty() {
            self.pieces.push(Piece::Text(self.buf));
        }
        self.pieces
    }
}

struct Rules {
    marker: Regex,
    secret_tokens: Regex,
    url: Regex,
    kv_secret: Regex,
    kv_free: Regex,
    kv_serial: Regex,
    email: Regex,
    mac: Regex,
    uuid: Regex,
    ipv4: Regex,
    ipv6_candidate: Regex,
    hex: Regex,
    home_path: Regex,
    entropy: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let re = |p: &str| Regex::new(p).expect("static scrub regex is valid");
        Rules {
            marker: re(r"<(?:HOME|USER|HOST|REDACTED|SECRET|CRED|QUERY|SSID|TITLE|SERIAL|EMAIL|MAC|UUID|IP|HEX|TOKEN|TRUNCATED)(?::(?:[0-9]+|\+))?>"),
            secret_tokens: re(concat!(
                r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
                r"|(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}",
                r"|github_pat_[A-Za-z0-9_]{20,}",
                r"|glpat-[A-Za-z0-9_-]{16,}",
                r"|xox[abprs]-[A-Za-z0-9-]{10,}",
                r"|AKIA[0-9A-Z]{16}",
                r"|AIza[0-9A-Za-z_-]{30,}",
                r"|\bsk-[A-Za-z0-9_-]{20,}",
                r"|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
                r"|(?i:\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{12,})",
            )),
            url: re(concat!(
                r#"(?i)\b[a-z][a-z0-9+.-]{1,15}://(?:[^\s"'<>`]|"#,
                r"<(?:HOME|USER|HOST|REDACTED|SECRET|CRED|QUERY|SSID|TITLE|SERIAL|EMAIL|MAC|UUID|IP|HEX|TOKEN|TRUNCATED)(?::(?:[0-9]+|\+))?>)+",
            )),
            kv_secret: re(concat!(
                r"(?i)\b(?P<key>[A-Za-z0-9_.-]*",
                r"(?:pass(?:word|wd|phrase)?|secret|token|api[_-]?key|apikey|auth(?:orization)?",
                r"|cookie|session[_-]?id|private[_-]?key|credential)",
                r"[A-Za-z0-9_.-]*)",
                r#"(?P<sep>["']?\s*[=:]\s*)"#,
                r#"(?P<val>"[^"]*"|'[^']*'|[^\s,;&"'<>]+)"#,
            )),
            kv_free: re(concat!(
                r"(?i)\b(?P<key>ssid|essid|window[_-]?title|title)",
                r#"(?P<sep>["']?\s*[=:]\s*)"#,
                r#"(?P<val>"[^"]*"|'[^']*'|[^\n<]*)"#,
            )),
            kv_serial: re(concat!(
                r"(?i)\b(?P<key>serial(?:[_ -]?(?:number|no))?)",
                r#"(?P<sep>["']?\s*[=:]\s*)"#,
                r#"(?P<val>"[^"]*"|'[^']*'|[^\s,;<>]+)"#,
            )),
            email: re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+"),
            mac: re(r"(?:[0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}"),
            uuid: re(
                r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}",
            ),
            ipv4: re(r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}"),
            ipv6_candidate: re(r"[0-9A-Fa-f:]{3,}"),
            hex: re(r"\b[0-9A-Fa-f]{24,}\b"),
            home_path: re(r"(?:/var/home|/home|/Users)/[A-Za-z0-9._-]+"),
            entropy: re(r"[A-Za-z0-9_+=-]{24,}"),
        }
    })
}

fn prev_char(t: &str, idx: usize) -> Option<char> {
    t[..idx].chars().next_back()
}

fn next_char(t: &str, idx: usize) -> Option<char> {
    t[idx..].chars().next()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn shannon_entropy(s: &str) -> f64 {
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_default() += 1;
    }
    let n = s.chars().count() as f64;
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn near_build_id(t: &str, start: usize) -> bool {
    let mut from = start.saturating_sub(24);
    while !t.is_char_boundary(from) {
        from -= 1;
    }
    let ctx = t[from..start].to_ascii_lowercase();
    ["buildid", "build-id", "build_id", "build id"]
        .iter()
        .any(|k| ctx.contains(k))
}

/// A literal to remove from free text, matched on word boundaries.
struct Literal {
    needle: String,
    cat: &'static str,
}

/// Redacts personal data from text and JSON. See the crate docs.
pub struct Scrubber {
    home: Option<String>,
    literals: Vec<Literal>,
    keep: BTreeSet<String>,
    hasher: RandomState,
    seen: HashMap<(&'static str, u64), u32>,
    counters: HashMap<&'static str, u32>,
    stats: BTreeMap<&'static str, usize>,
}

impl Scrubber {
    pub fn new(ctx: ScrubContext) -> Self {
        let mut literals = Vec::new();
        let mut add = |value: &str, cat: &'static str, min_len: usize, skip_common: bool| {
            let v = value.trim();
            if v.len() < min_len {
                return;
            }
            if skip_common && COMMON_NAMES.contains(&v.to_ascii_lowercase().as_str()) {
                return;
            }
            literals.push(Literal {
                needle: v.to_ascii_lowercase(),
                cat,
            });
        };
        if let Some(u) = &ctx.username {
            add(u, "USER", 1, true);
        }
        if let Some(h) = &ctx.hostname {
            add(h, "HOST", 1, true);
        }
        for extra in &ctx.extra_literals {
            add(extra, "REDACTED", 3, false);
        }
        // Longest first so a long literal wins over one it contains.
        literals.sort_by_key(|l| std::cmp::Reverse(l.needle.len()));
        let home = ctx
            .home
            .map(|h| h.trim_end_matches('/').to_string())
            .filter(|h| !h.is_empty());
        Self {
            home,
            literals,
            keep: BTreeSet::new(),
            hasher: RandomState::new(),
            seen: HashMap::new(),
            counters: HashMap::new(),
            stats: BTreeMap::new(),
        }
    }

    /// A scrubber for the current user and machine.
    pub fn from_environment() -> Self {
        Self::new(ScrubContext::from_environment())
    }

    /// Exempt a value (a known build-id) from the long-hex rule. Build-ids
    /// are needed server-side for symbolication and are not personal.
    pub fn keep_build_id(&mut self, id: &str) {
        self.keep.insert(id.to_ascii_lowercase());
    }

    /// Start a new numbering scope and clear counters, e.g. per report.
    pub fn reset(&mut self) {
        self.seen.clear();
        self.counters.clear();
        self.stats.clear();
    }

    /// How many values of each category were redacted since the last
    /// [`reset`](Self::reset). Shown in the preview.
    pub fn stats(&self) -> &BTreeMap<&'static str, usize> {
        &self.stats
    }

    /// Scrub free-form text. Idempotent.
    pub fn scrub_text(&mut self, input: &str) -> String {
        self.reserve_markers(input);
        let mut cur = self.pass(input);
        for _ in 0..MAX_EXTRA_PASSES {
            let next = self.pass(&cur);
            if next == cur {
                break;
            }
            cur = next;
        }
        cur
    }

    /// Scrub possibly non-UTF-8 bytes (raw log output). Invalid sequences
    /// become U+FFFD before scrubbing.
    pub fn scrub_bytes(&mut self, input: &[u8]) -> String {
        self.scrub_text(&String::from_utf8_lossy(input))
    }

    /// Scrub every string (and object key) in a JSON value.
    pub fn scrub_value(&mut self, v: &Value) -> Value {
        self.reserve_value_markers(v);
        self.scrub_value_at(v, 0)
    }

    /// Reserve existing ingest marker numbers before scrubbing new report
    /// fields (including notes). Call on the complete input before separate
    /// field-wise scrubbing, or use `scrub_value` for the complete report.
    pub fn reserve_value_markers(&mut self, v: &Value) {
        self.reserve_value_markers_at(v, 0);
    }

    fn reserve_value_markers_at(&mut self, v: &Value, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match v {
            Value::String(s) => self.reserve_markers(s),
            Value::Array(items) => items
                .iter()
                .for_each(|v| self.reserve_value_markers_at(v, depth + 1)),
            Value::Object(map) => {
                for (key, value) in map {
                    self.reserve_markers(key);
                    self.reserve_value_markers_at(value, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn reserve_markers(&mut self, input: &str) {
        for marker in rules().marker.find_iter(input) {
            let inner = &marker.as_str()[1..marker.as_str().len() - 1];
            let Some((cat, number)) = inner.split_once(':') else {
                continue;
            };
            let Some(cat) = [
                "USER", "HOST", "REDACTED", "EMAIL", "MAC", "UUID", "IP", "HEX",
            ]
            .into_iter()
            .find(|known| *known == cat) else {
                continue;
            };
            let number = number
                .parse::<u32>()
                .unwrap_or(MAX_NUMBERED)
                .min(MAX_NUMBERED);
            let count = self.counters.entry(cat).or_default();
            *count = (*count).max(number);
        }
    }

    /// Reduce an event payload to its allowlisted fields, scrubbed. With no
    /// matching allowlist rule the payload is dropped and only `_dropped`
    /// plus an empty `_keys` compatibility field remain.
    pub fn scrub_event_payload(
        &mut self,
        event: &str,
        payload: &Value,
        allow: &EventAllowlist,
    ) -> Value {
        match allow.fields_for(event) {
            Some(fields) => {
                let mut kept = Map::new();
                for path in fields {
                    if let Some(v) = pick(payload, path) {
                        let scrubbed = self.scrub_value(v);
                        insert_path(&mut kept, path, scrubbed);
                    }
                }
                Value::Object(kept)
            }
            None => {
                let mut out = Map::new();
                out.insert("_dropped".into(), Value::Bool(true));
                out.insert("_keys".into(), Value::Array(Vec::new()));
                Value::Object(out)
            }
        }
    }

    fn scrub_value_at(&mut self, v: &Value, depth: usize) -> Value {
        if depth > MAX_DEPTH {
            return Value::String("<TRUNCATED>".into());
        }
        match v {
            Value::String(s) => Value::String(self.scrub_text(s)),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|i| self.scrub_value_at(i, depth + 1))
                    .collect(),
            ),
            Value::Object(map) => {
                let mut out = Map::new();
                for (k, val) in map {
                    let key = self.scrub_text(k);
                    let val = self.scrub_value_at(val, depth + 1);
                    out.insert(key, val);
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }

    fn pass(&mut self, input: &str) -> String {
        // URLs are structural spans: existing markers inside credentials or
        // queries must not shield adjacent personal data from this rule.
        let urls: String = self
            .rule_urls(input)
            .into_iter()
            .map(|piece| match piece {
                Piece::Text(s) | Piece::Marker(s) => s,
            })
            .collect();
        let mut pieces = split_markers(&urls);
        pieces = self.apply(pieces, Self::rule_secret_tokens);
        pieces = self.apply(pieces, Self::rule_kv_secret);
        pieces = self.apply(pieces, Self::rule_kv_free);
        pieces = self.apply(pieces, Self::rule_kv_serial);
        pieces = self.apply(pieces, Self::rule_email);
        pieces = self.apply(pieces, Self::rule_mac);
        pieces = self.apply(pieces, Self::rule_uuid);
        pieces = self.apply(pieces, Self::rule_ipv4);
        pieces = self.apply(pieces, Self::rule_ipv6);
        pieces = self.apply(pieces, Self::rule_hex);
        pieces = self.apply(pieces, Self::rule_home_literal);
        pieces = self.apply(pieces, Self::rule_home_path);
        for i in 0..self.literals.len() {
            let needle = self.literals[i].needle.clone();
            let cat = self.literals[i].cat;
            pieces = self.apply(pieces, move |s, t| s.rule_literal(t, &needle, cat));
        }
        pieces = self.apply(pieces, Self::rule_entropy);
        let mut s = String::with_capacity(input.len());
        for p in pieces {
            match p {
                Piece::Text(t) | Piece::Marker(t) => s.push_str(&t),
            }
        }
        s
    }

    /// Run `rule` over every text piece, passing markers through untouched.
    fn apply<F>(&mut self, pieces: Vec<Piece>, mut rule: F) -> Vec<Piece>
    where
        F: FnMut(&mut Self, &str) -> Vec<Piece>,
    {
        let mut out = Vec::with_capacity(pieces.len());
        for p in pieces {
            match p {
                Piece::Text(t) => out.extend(rule(self, &t)),
                m @ Piece::Marker(_) => out.push(m),
            }
        }
        out
    }

    fn bump(&mut self, cat: &'static str) {
        *self.stats.entry(cat).or_default() += 1;
    }

    /// Marker with no number, e.g. `<SECRET>`.
    fn plain(&mut self, cat: &'static str) -> String {
        self.bump(cat);
        format!("<{cat}>")
    }

    /// Marker numbered by first appearance of `value`, e.g. `<IP:3>`.
    fn numbered(&mut self, cat: &'static str, value: &str) -> String {
        self.bump(cat);
        let key = (cat, self.hasher.hash_one(value.to_ascii_lowercase()));
        if let Some(n) = self.seen.get(&key) {
            return format!("<{cat}:{n}>");
        }
        let count = self.counters.entry(cat).or_insert(0);
        if *count >= MAX_NUMBERED {
            return format!("<{cat}:+>");
        }
        *count += 1;
        let n = *count;
        self.seen.insert(key, n);
        format!("<{cat}:{n}>")
    }

    fn rule_secret_tokens(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().secret_tokens.find_iter(t) {
            out.text(&t[last..m.start()]);
            out.marker(self.plain("SECRET"));
            last = m.end();
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_urls(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().url.find_iter(t) {
            out.text(&t[last..m.start()]);
            last = m.end();
            let url = m.as_str();
            let trimmed = url.trim_end_matches(['.', ',', ')', ']', '}', ';', ':', '!', '?']);
            let scheme_end = trimmed.find("://").map_or(0, |i| i + 3);
            let rest = &trimmed[scheme_end..];
            let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let authority = &rest[..auth_end];
            out.text(&trimmed[..scheme_end]);
            match authority.rfind('@') {
                Some(at) => {
                    out.marker(self.plain("CRED"));
                    out.text(&authority[at..]);
                }
                None => out.text(authority),
            }
            let tail = &rest[auth_end..];
            match tail.find('?') {
                Some(q) if !query_of(&tail[q + 1..]).is_empty() => {
                    let after = &tail[q + 1..];
                    let query_len = query_of(after).len();
                    out.text(&tail[..q + 1]);
                    out.marker(self.plain("QUERY"));
                    out.text(&after[query_len..]);
                }
                _ => out.text(tail),
            }
            out.text(&url[trimmed.len()..]);
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_kv_secret(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        let re = &rules().kv_secret;
        for caps in re.captures_iter(t) {
            let whole = caps.get(0).expect("group 0");
            let key = &caps["key"];
            if !key_is_secret(key) || caps["val"].len() <= 2 && caps["val"].starts_with(['"', '\''])
            {
                continue;
            }
            out.text(&t[last..whole.start()]);
            last = whole.end();
            out.text(key);
            out.text(&caps["sep"]);
            let val = &caps["val"];
            match val.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    out.text(&q.to_string());
                    out.marker(self.plain("SECRET"));
                    out.text(&q.to_string());
                }
                _ => out.marker(self.plain("SECRET")),
            }
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn kv_generic(&mut self, t: &str, re: &Regex, cat_for: fn(&str) -> &'static str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for caps in re.captures_iter(t) {
            let whole = caps.get(0).expect("group 0");
            let val = &caps["val"];
            // Empty values and a bare quote left next to a marker by an
            // earlier pass ("title":"<TITLE>") are already clean.
            if val.trim_matches(['"', '\'']).trim().is_empty() {
                continue;
            }
            out.text(&t[last..whole.start()]);
            last = whole.end();
            out.text(&caps["key"]);
            out.text(&caps["sep"]);
            let cat = cat_for(&caps["key"]);
            match val.chars().next() {
                Some(q @ ('"' | '\'')) if val.len() >= 2 && val.ends_with(q) => {
                    out.text(&q.to_string());
                    out.marker(self.plain(cat));
                    out.text(&q.to_string());
                }
                _ => out.marker(self.plain(cat)),
            }
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_kv_free(&mut self, t: &str) -> Vec<Piece> {
        self.kv_generic(t, &rules().kv_free, |key| {
            if key.to_ascii_lowercase().contains("ssid") {
                "SSID"
            } else {
                "TITLE"
            }
        })
    }

    fn rule_kv_serial(&mut self, t: &str) -> Vec<Piece> {
        self.kv_generic(t, &rules().kv_serial, |_| "SERIAL")
    }

    fn rule_email(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().email.find_iter(t) {
            out.text(&t[last..m.start()]);
            out.marker(self.numbered("EMAIL", m.as_str()));
            last = m.end();
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_mac(&mut self, t: &str) -> Vec<Piece> {
        self.simple_numbered(t, &rules().mac, "MAC")
    }

    fn rule_uuid(&mut self, t: &str) -> Vec<Piece> {
        self.simple_numbered(t, &rules().uuid, "UUID")
    }

    fn simple_numbered(&mut self, t: &str, re: &Regex, cat: &'static str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in re.find_iter(t) {
            out.text(&t[last..m.start()]);
            out.marker(self.numbered(cat, m.as_str()));
            last = m.end();
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_ipv4(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().ipv4.find_iter(t) {
            let octets: Vec<u32> = m
                .as_str()
                .split('.')
                .filter_map(|o| o.parse().ok())
                .collect();
            let valid = octets.len() == 4 && octets.iter().all(|&o| o <= 255);
            let loopback_or_unspecified =
                valid && (octets[0] == 127 || octets.iter().all(|&o| o == 0));
            // Glued to a letter still counts as an address (leaking is worse
            // than eating a four-part version), but a digit or dot before it
            // means a longer number.
            let part_of_dotted = prev_char(t, m.start())
                .is_some_and(|c| c.is_ascii_digit() || c == '.')
                || (next_char(t, m.end()) == Some('.')
                    && t[m.end() + 1..].starts_with(|c: char| c.is_ascii_digit()));
            if !valid || loopback_or_unspecified || part_of_dotted {
                continue;
            }
            out.text(&t[last..m.start()]);
            out.marker(self.numbered("IP", m.as_str()));
            last = m.end();
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_ipv6(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().ipv6_candidate.find_iter(t) {
            let mut s = m.as_str();
            // A sentence colon may follow an address. Prefer the complete
            // valid address, then peel punctuation only if parsing fails.
            while Ipv6Addr::from_str(s).is_err() && s.ends_with(':') {
                s = &s[..s.len() - 1];
            }
            let colons = s.matches(':').count();
            let boundary_ok = prev_char(t, m.start()).is_none_or(|c| !is_word(c))
                && next_char(t, m.end()).is_none_or(|c| !is_word(c));
            if colons < 2 || !boundary_ok {
                continue;
            }
            let Ok(addr) = Ipv6Addr::from_str(s) else {
                continue;
            };
            if addr.is_loopback() || addr.is_unspecified() {
                continue;
            }
            out.text(&t[last..m.start()]);
            out.marker(self.numbered("IP", s));
            last = m.start() + s.len();
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_hex(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().hex.find_iter(t) {
            let lower = m.as_str().to_ascii_lowercase();
            if self.keep.contains(&lower) || near_build_id(t, m.start()) {
                continue;
            }
            out.text(&t[last..m.start()]);
            out.marker(self.numbered("HEX", m.as_str()));
            last = m.end();
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_home_literal(&mut self, t: &str) -> Vec<Piece> {
        let Some(home) = self.home.clone() else {
            return vec![Piece::Text(t.to_string())];
        };
        let mut out = Out::new();
        let mut last = 0;
        for (i, _) in t.match_indices(&home) {
            if i < last {
                continue;
            }
            let end = i + home.len();
            let continues = next_char(t, end)
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if continues {
                continue;
            }
            out.text(&t[last..i]);
            out.marker(self.plain("HOME"));
            last = end;
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_home_path(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().home_path.find_iter(t) {
            out.text(&t[last..m.start()]);
            out.marker(self.plain("HOME"));
            last = m.end();
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_literal(&mut self, t: &str, needle: &str, cat: &'static str) -> Vec<Piece> {
        let lowered = t.to_ascii_lowercase();
        let mut out = Out::new();
        let mut last = 0;
        for (i, _) in lowered.match_indices(needle) {
            if i < last {
                continue;
            }
            let end = i + needle.len();
            let bounded = prev_char(t, i).is_none_or(|c| !is_word(c))
                && next_char(t, end).is_none_or(|c| !is_word(c));
            if !bounded {
                continue;
            }
            out.text(&t[last..i]);
            out.marker(self.plain(cat));
            last = end;
        }
        out.text(&t[last..]);
        out.finish()
    }

    fn rule_entropy(&mut self, t: &str) -> Vec<Piece> {
        let mut out = Out::new();
        let mut last = 0;
        for m in rules().entropy.find_iter(t) {
            // Skip a legitimately mangled Rust symbol, but still judge
            // whatever is glued after it, so `_ZN...E<secret>` cannot hide.
            let skip = mangled_symbol_len(m.as_str());
            let start = m.start() + skip;
            let s = &t[start..m.end()];
            if s.len() < 24 || self.keep.contains(&s.to_ascii_lowercase()) {
                continue;
            }
            let has_digit = s.chars().any(|c| c.is_ascii_digit());
            let has_alpha = s.chars().any(|c| c.is_ascii_alphabetic());
            let mixed_case = s.chars().any(|c| c.is_ascii_uppercase())
                && s.chars().any(|c| c.is_ascii_lowercase());
            if !(has_digit && has_alpha && (mixed_case || shannon_entropy(s) >= 4.0)) {
                continue;
            }
            out.text(&t[last..start]);
            out.marker(self.plain("TOKEN"));
            last = m.end();
        }
        out.text(&t[last..]);
        out.finish()
    }
}

/// Byte length of a leading Rust legacy-mangled symbol (`_ZN` followed by
/// length-prefixed identifiers, optionally closed by `E`), or 0 if `s` does
/// not start with one. Exact parsing keeps real symbols readable without
/// letting a `_ZN` prefix exempt arbitrary trailing text.
fn mangled_symbol_len(s: &str) -> usize {
    let Some(mut rest) = s.strip_prefix("_ZN") else {
        return 0;
    };
    let mut consumed = 3;
    loop {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            if rest.starts_with('E') {
                consumed += 1;
            }
            return consumed;
        }
        let Ok(n) = rest[..digits].parse::<usize>() else {
            return consumed;
        };
        let after = &rest[digits..];
        if n == 0 || after.len() < n || !after.is_char_boundary(n) {
            return consumed;
        }
        consumed += digits + n;
        rest = &after[n..];
    }
}

/// The query part of `tail` (what follows `?`), up to a fragment.
fn query_of(after_q: &str) -> &str {
    after_q.split('#').next().unwrap_or("")
}

/// Whether a key name that matched the broad secret regex really is one,
/// after removing benign words that contain secret-ish substrings.
fn key_is_secret(key: &str) -> bool {
    let mut k = key.to_ascii_lowercase();
    for benign in [
        "passed",
        "passing",
        "bypass",
        "compass",
        "tokenizer",
        "token_count",
        "tokens_per",
        "author",
        "authored",
    ] {
        k = k.replace(benign, "");
    }
    [
        "pass",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "auth",
        "cookie",
        "sessionid",
        "session_id",
        "session-id",
        "privatekey",
        "private_key",
        "private-key",
        "credential",
    ]
    .iter()
    .any(|w| k.contains(w))
}

fn split_markers(s: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut last = 0;
    for m in rules().marker.find_iter(s) {
        if m.start() > last {
            out.push(Piece::Text(s[last..m.start()].to_string()));
        }
        out.push(Piece::Marker(m.as_str().to_string()));
        last = m.end();
    }
    if last < s.len() {
        out.push(Piece::Text(s[last..].to_string()));
    }
    out
}

fn pick<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, seg| cur.get(seg))
}

fn insert_path(root: &mut Map<String, Value>, path: &str, value: Value) {
    let mut segs: Vec<&str> = path.split('.').collect();
    let last = segs.pop().unwrap_or(path);
    let mut cur = root;
    for seg in segs {
        let entry = cur
            .entry(seg.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cur = entry.as_object_mut().expect("just ensured object");
    }
    cur.insert(last.to_string(), value);
}

#[cfg(test)]
mod tests;
