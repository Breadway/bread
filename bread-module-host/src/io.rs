//! The async half of `bread-module-host`: owns the Unix socket connection
//! back to `breadd` and speaks the newline-delimited-JSON IPC protocol
//! (`breadd/src/ipc/mod.rs`), extended with `module_host.*` methods (see
//! `breadd/src/module_host.rs` for the server side of this bridge).
//!
//! Runs on its own dedicated OS thread with its own single-threaded Tokio
//! runtime — mirroring `breadd`'s own `spawn_runtime` split between an async
//! IPC/adapters world and a synchronous, single-threaded Lua world (see
//! `breadd/src/lua/mod.rs`'s `spawn_runtime`). The Lua-driving thread in
//! `main.rs` talks to this thread over two plain `std::sync::mpsc` channels
//! (`IoCommand` out, `HostMessage` in) rather than sharing an async runtime,
//! since `mlua::Lua` values are not `Send` and Lua callbacks need to make
//! synchronous (blocking, from Lua's point of view) RPC calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use bread_shared::{ModuleHostHello, ModuleHostPush};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::warn;

/// A request the Lua-driving thread wants sent to `breadd`, with a reply
/// channel for the (blocking, from Lua's perspective) response.
pub enum IoCommand {
    Request {
        method: String,
        params: Value,
        reply: mpsc::Sender<Result<Value, String>>,
    },
}

/// Something the IO thread has for the Lua-driving thread: either an
/// unsolicited push (a subscribed event fired, a timer fired) or "the
/// connection is gone" (breadd exited, socket closed, etc).
pub enum HostMessage {
    Push(ModuleHostPush),
    Closed,
}

#[derive(serde::Deserialize)]
struct RpcResponse {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// Connect, perform the `module_host.hello` handshake, and — on success —
/// run the steady-state request/response + push-forwarding loop until the
/// connection closes. `hello_tx` is always sent to exactly once, before
/// anything else; the caller blocks on it to learn the module's granted
/// identity (or why the handshake failed) before doing anything else.
pub fn run(
    socket_path: PathBuf,
    token: String,
    cmd_rx: mpsc::Receiver<IoCommand>,
    host_tx: mpsc::Sender<HostMessage>,
    hello_tx: mpsc::Sender<Result<ModuleHostHello, String>>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = hello_tx.send(Err(format!("failed to start io runtime: {e}")));
            return;
        }
    };

    rt.block_on(async move {
        let stream = match UnixStream::connect(&socket_path).await {
            Ok(s) => s,
            Err(e) => {
                let _ = hello_tx.send(Err(format!(
                    "failed to connect to {}: {e}",
                    socket_path.display()
                )));
                return;
            }
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();

        let hello_req = json!({
            "id": "hello",
            "method": "module_host.hello",
            "params": { "token": token },
        });
        let Ok(hello_line) = serde_json::to_string(&hello_req) else {
            let _ = hello_tx.send(Err("failed to encode hello request".to_string()));
            return;
        };
        if write_half
            .write_all(format!("{hello_line}\n").as_bytes())
            .await
            .is_err()
        {
            let _ = hello_tx.send(Err("failed to write hello request".to_string()));
            return;
        }

        let response_line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                let _ = hello_tx.send(Err(
                    "connection closed before hello response".to_string(),
                ));
                return;
            }
            Err(e) => {
                let _ = hello_tx.send(Err(format!("read error awaiting hello: {e}")));
                return;
            }
        };
        let resp: RpcResponse = match serde_json::from_str(&response_line) {
            Ok(r) => r,
            Err(e) => {
                let _ = hello_tx.send(Err(format!("malformed hello response: {e}")));
                return;
            }
        };
        if let Some(err) = resp.error {
            let _ = hello_tx.send(Err(err));
            return;
        }
        let hello: ModuleHostHello = match resp
            .result
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(h) => h,
            None => {
                let _ = hello_tx.send(Err("hello response missing result".to_string()));
                return;
            }
        };
        if hello_tx.send(Ok(hello)).is_err() {
            return;
        }

        // Steady state. `pending` routes response lines back to whichever
        // Lua-side call is blocked waiting for them; a dedicated thread
        // bridges the synchronous `cmd_rx` (fed from the Lua thread) onto an
        // async channel this task can select on.
        let pending: Arc<Mutex<HashMap<String, mpsc::Sender<Result<Value, String>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (async_cmd_tx, mut async_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<IoCommand>();
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if async_cmd_tx.send(cmd).is_err() {
                    break;
                }
            }
        });

        let pending_for_writer = pending.clone();
        let write_task = tokio::spawn(async move {
            let mut next_id: u64 = 1;
            while let Some(IoCommand::Request {
                method,
                params,
                reply,
            }) = async_cmd_rx.recv().await
            {
                let id = format!("m{next_id}");
                next_id += 1;
                let req = json!({ "id": id, "method": method, "params": params });
                let line = match serde_json::to_string(&req) {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = reply.send(Err(e.to_string()));
                        continue;
                    }
                };
                pending_for_writer.lock().unwrap().insert(id.clone(), reply);
                if write_half
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    if let Some(tx) = pending_for_writer.lock().unwrap().remove(&id) {
                        let _ = tx.send(Err("write failed; connection lost".to_string()));
                    }
                    break;
                }
            }
        });

        loop {
            let line = match lines.next_line().await {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "module-host: connection read error");
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "module-host: malformed line from breadd");
                    continue;
                }
            };
            if value.get("push").is_some() {
                match serde_json::from_value::<ModuleHostPush>(value) {
                    Ok(push) => {
                        if host_tx.send(HostMessage::Push(push)).is_err() {
                            break;
                        }
                    }
                    Err(e) => warn!(error = %e, "module-host: malformed push message"),
                }
            } else if let Ok(resp) = serde_json::from_value::<RpcResponse>(value) {
                if let Some(tx) = pending.lock().unwrap().remove(&resp.id) {
                    let result = match resp.error {
                        Some(e) => Err(e),
                        None => Ok(resp.result.unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(result);
                }
            }
        }

        write_task.abort();
        // Any calls still blocked waiting for a reply need to be unblocked
        // rather than hanging forever now that the connection is gone.
        for (_, tx) in pending.lock().unwrap().drain() {
            let _ = tx.send(Err("connection closed".to_string()));
        }
        let _ = host_tx.send(HostMessage::Closed);
    });
}

/// Blocking helper used from Lua callback closures (which run on the
/// Lua-driving thread, not the async IO thread): send a request and wait —
/// with a timeout, so a wedged connection can't hang a Lua callback forever
/// — for its response.
pub fn call(
    cmd_tx: &mpsc::Sender<IoCommand>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    cmd_tx
        .send(IoCommand::Request {
            method: method.to_string(),
            params,
            reply: reply_tx,
        })
        .map_err(|_| "io thread gone".to_string())?;
    reply_rx
        .recv_timeout(timeout)
        .map_err(|_| format!("{method} timed out"))?
}
