//! Test-only stand-in for `bread-module-host`, used by
//! `tests/module_host_hyprland_bridge.rs`.
//!
//! `breadd` spawns it exactly like the real host (same sandbox, same
//! `BREAD_MODULE_*` environment), so it authenticates with a real one-time
//! token over the real `module_host.hello` handshake. It then replays the
//! requests listed in `script.json` next to the module's `init.lua` and
//! prints what the daemon answered, which lets a test speak the wire protocol
//! directly, including requests the real host would never send.
//!
//! Output, one line each on stdout (inherited from `breadd`):
//!   `FAKEHOST_PID <module> <pid>`       once the host process is running
//!   `FAKEHOST_READY <module> <token>`  after hello and the `loaded` status
//!   `FAKEHOST_DONE <module> <json>`    after the script finished
//!
//! Files next to the module's `init.lua`:
//!   `hello.json`   optional object merged into the hello params (never the
//!                  token), used to forge identity fields
//!   `script.json`  `{ "steps": [ { "label": "x", "request": <envelope> } ] }`,
//!                  read once a file named `go` appears

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn main() {
    let token = std::env::var("BREAD_MODULE_TOKEN").expect("BREAD_MODULE_TOKEN");
    let socket = std::env::var("BREAD_MODULE_SOCKET").expect("BREAD_MODULE_SOCKET");
    let entry = std::env::var("BREAD_MODULE_ENTRY").expect("BREAD_MODULE_ENTRY");
    let name = std::env::var("BREAD_MODULE_NAME").expect("BREAD_MODULE_NAME");
    let dir = PathBuf::from(&entry)
        .parent()
        .expect("module dir")
        .to_path_buf();
    let hello_extra: Value = std::fs::read_to_string(dir.join("hello.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);

    let stream = UnixStream::connect(&socket).expect("connect to breadd");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;
    let mut call = move |request: &Value| -> Value {
        let line = format!("{}\n", request);
        if writer.write_all(line.as_bytes()).is_err() {
            return json!({ "closed": true });
        }
        let mut reply = String::new();
        match reader.read_line(&mut reply) {
            Ok(0) | Err(_) => json!({ "closed": true }),
            Ok(_) => serde_json::from_str(&reply).unwrap_or_else(|_| json!({ "garbled": reply })),
        }
    };

    let mut hello_params = json!({ "token": token });
    if let Some(extra) = hello_extra.as_object() {
        for (key, value) in extra.iter().filter(|(key, _)| key.as_str() != "token") {
            hello_params[key] = value.clone();
        }
    }
    let hello =
        call(&json!({ "id": "hello", "method": "module_host.hello", "params": hello_params }));
    call(
        &json!({ "id": "status", "method": "module_host.status", "params": { "state": "loaded" } }),
    );

    println!("FAKEHOST_PID {name} {}", std::process::id());
    let _ = std::io::stdout().flush();
    println!("FAKEHOST_READY {name} {token}");
    let _ = std::io::stdout().flush();

    let gate = dir.join("go");
    let waited = Instant::now();
    while !gate.exists() {
        if waited.elapsed() > Duration::from_secs(120) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let script: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("script.json")).expect("script.json"),
    )
    .expect("script.json is JSON");
    let mut results = Vec::new();
    for step in script["steps"].as_array().into_iter().flatten() {
        let started = Instant::now();
        let response = call(&step["request"]);
        results.push(json!({
            "label": step["label"],
            "response": response,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }));
    }

    println!(
        "FAKEHOST_DONE {name} {}",
        json!({ "module": name, "token": token, "hello": hello, "results": results })
    );
    let _ = std::io::stdout().flush();
}
