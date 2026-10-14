//! A tiny blocking client for breadd's IPC socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

/// `$XDG_RUNTIME_DIR/bread/breadd.sock`, falling back to `/tmp` like breadd.
pub fn default_socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    Path::new(&runtime).join("bread").join("breadd.sock")
}

/// Send one request and return its `result`. Errors on an `error` reply.
pub fn request(socket: &Path, method: &str, params: Value, timeout: Duration) -> Result<Value> {
    let stream =
        UnixStream::connect(socket).with_context(|| format!("connect {}", socket.display()))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut writer = stream.try_clone()?;
    let req = json!({ "id": "breadbug", "method": method, "params": params });
    writer.write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line).context("parse breadd reply")?;
    if let Some(err) = reply.get("error").and_then(Value::as_str) {
        return Err(anyhow!("{err}"));
    }
    Ok(reply.get("result").cloned().unwrap_or_else(|| json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn roundtrip_and_error_reply() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            for reply in [
                r#"{"id":"breadbug","result":{"ok":true}}"#,
                r#"{"id":"breadbug","error":"nope"}"#,
            ] {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                (&stream)
                    .write_all(format!("{reply}\n").as_bytes())
                    .unwrap();
            }
        });
        let t = Duration::from_secs(2);
        assert_eq!(
            request(&sock, "ping", json!({}), t).unwrap()["ok"],
            json!(true)
        );
        assert_eq!(
            request(&sock, "x", json!({}), t).unwrap_err().to_string(),
            "nope"
        );
        handle.join().unwrap();
    }

    #[test]
    fn missing_socket_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(request(
            &dir.path().join("none"),
            "ping",
            json!({}),
            Duration::from_secs(1)
        )
        .is_err());
    }
}
