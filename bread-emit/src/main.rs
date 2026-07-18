//! Minimal fire-and-forget emitter for breadd's IPC socket.
//!
//! This exists because shell precmd/preexec hooks (and git hooks) fire on
//! every command / commit — spawning the full `bread` CLI (clap parsing +
//! a Tokio runtime + a round trip) on that path is perceptible latency in
//! an interactive shell. This binary skips all of that: no async runtime,
//! argv is parsed by hand, and it never waits for or reads a reply. A
//! down or slow daemon must never delay or hang the caller's shell.

use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn parse_args(args: &[String]) -> Option<(String, Option<String>, Option<String>, String)> {
    let mut event = None;
    let mut source = None;
    let mut kind = None;
    let mut data = "{}".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                source = args.get(i + 1).cloned();
                i += 2;
            }
            "--kind" => {
                kind = args.get(i + 1).cloned();
                i += 2;
            }
            "--data" => {
                data = args.get(i + 1).cloned().unwrap_or_else(|| "{}".to_string());
                i += 2;
            }
            other => {
                if event.is_none() {
                    event = Some(other.to_string());
                }
                i += 1;
            }
        }
    }

    event.map(|event| (event, source, kind, data))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((event, source, kind, data)) = parse_args(&args) else {
        eprintln!("usage: bread-emit <event> [--source <source> --kind <kind>] [--data <json>]");
        std::process::exit(1);
    };

    let parsed_data: Value = serde_json::from_str(&data).unwrap_or_else(|_| json!({}));
    let mut params = json!({ "event": event, "data": parsed_data });
    if let Some(source) = source {
        params["source"] = json!(source);
    }
    if let Some(kind) = kind {
        params["kind"] = json!(kind);
    }

    let request = json!({ "id": "0", "method": "emit", "params": params });
    let Ok(line) = serde_json::to_string(&request) else {
        return;
    };

    // Best-effort: connect, write, exit. Never read a reply, never retry,
    // never block longer than the write timeout below.
    if let Ok(mut stream) = UnixStream::connect(bread_shared::resolve_socket_path()) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let _ = writeln!(stream, "{line}");
    }
}
