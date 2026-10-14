//! The `module_host.*` side of the IPC protocol (Workstream G): once a
//! connection presents a valid one-time token via `module_host.hello`, this
//! module takes over its remaining lifetime as a bidirectional RPC bridge
//! (ordinary request/response lines interleaved with unsolicited
//! event/timer pushes) for exactly one `bread-module-host` child.
//!
//! # Wire shape
//!
//! Requests/responses reuse the existing `IpcRequest`/`IpcResponse`
//! envelope unchanged. Pushes are a separate, `"push"`-tagged envelope
//! (`bread_shared::ModuleHostPush`) that never collides with a response:
//! see that type's doc comment. A single `mpsc` channel (`out_tx`/`out_rx`)
//! feeds one writer task so both kinds of outgoing line interleave safely
//! on the one underlying socket without any extra locking.
//!
//! # Where the "belt" is, relative to the "suspenders"
//!
//! Every method here re-checks the module's granted `PermissionKind`s
//! before doing anything: `fs_read`/`fs_write`/`exec`/`exec_capture`
//! additionally check the manifest's `path`/`bin` scoping hint. This is
//! the belt; `module_host::apply_sandbox`'s Landlock ruleset (enforced by
//! the kernel on the child process directly, independent of whether the
//! child even uses this RPC bridge at all) is the suspenders. A module
//! that skips this bridge entirely and calls `os.execute`/`io.open`
//! directly from Lua bypasses every check in this file: that's the
//! scenario the sandbox exists for, not this file.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use bread_shared::{glob, ModuleHostHello, ModuleHostPush, ModulePermission, PermissionKind};
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::core::types::ModuleLoadState;
use crate::lua::hyprland_compat;
use crate::module_host::ModuleHostOutcome;

use super::{IpcRequest, IpcResponse, Server, API_VERSION};

impl Server {
    /// Authenticate a `module_host.hello` request against the pending-token
    /// registry and, on success, run this connection's dedicated
    /// request/response + push loop until it closes. Mirrors
    /// `handle_connection`'s `events.subscribe` special-case in spirit
    /// (taking over the rest of the connection's lifetime) but is
    /// bidirectional rather than one-directional.
    pub(super) async fn handle_module_host_connection(
        &self,
        hello_req: IpcRequest,
        mut lines: Lines<BufReader<OwnedReadHalf>>,
        write_half: OwnedWriteHalf,
    ) -> anyhow::Result<()> {
        let hello_id = hello_req.id.clone();
        let token = hello_req
            .params
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_string);

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let writer_task = tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(line) = out_rx.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        let send = |resp: IpcResponse| -> anyhow::Result<()> {
            let line = format!("{}\n", serde_json::to_string(&resp)?);
            let _ = out_tx.send(line);
            Ok(())
        };

        let Some(token) = token else {
            send(IpcResponse {
                id: hello_id,
                result: None,
                error: Some("module_host.hello: missing token".to_string()),
            })?;
            drop(out_tx);
            let _ = writer_task.await;
            return Ok(());
        };

        let Some(pending) = self.module_host_registry.take_pending(&token) else {
            send(IpcResponse {
                id: hello_id,
                result: None,
                error: Some("invalid or expired module-host token".to_string()),
            })?;
            drop(out_tx);
            let _ = writer_task.await;
            return Ok(());
        };

        let module_name = pending.module_name.clone();
        let permissions = pending.permissions.clone();
        let mut outcome_tx = Some(pending.outcome_tx);

        let hello_result = ModuleHostHello {
            module: module_name.clone(),
            permissions: permissions.clone(),
            api_version: API_VERSION.to_string(),
        };
        send(IpcResponse {
            id: hello_id,
            result: Some(serde_json::to_value(&hello_result)?),
            error: None,
        })?;

        info!(module = %module_name, permissions = ?permissions, "module-host authenticated");

        let mut subs: HashMap<String, JoinHandle<()>> = HashMap::new();
        let mut timers: HashMap<String, JoinHandle<()>> = HashMap::new();

        loop {
            let line = match lines.next_line().await {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(e) => {
                    warn!(module = %module_name, error = %e, "module-host connection read error");
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let req: IpcRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    send(IpcResponse {
                        id: "?".to_string(),
                        result: None,
                        error: Some(format!("parse error: {e}")),
                    })?;
                    continue;
                }
            };
            let req_id = req.id.clone();
            let result = self
                .dispatch_module_host_method(
                    &req,
                    &module_name,
                    &permissions,
                    &out_tx,
                    &mut subs,
                    &mut timers,
                    &mut outcome_tx,
                )
                .await;
            let resp = match result {
                Ok(v) => IpcResponse {
                    id: req_id,
                    result: Some(v),
                    error: None,
                },
                Err(e) => IpcResponse {
                    id: req_id,
                    result: None,
                    error: Some(e),
                },
            };
            send(resp)?;
        }

        for (_, h) in subs.drain() {
            h.abort();
        }
        for (_, h) in timers.drain() {
            h.abort();
        }
        drop(out_tx);
        let _ = writer_task.await;

        // The connection dropped before the module ever reported
        // load-success/load-failure (e.g. it crashed mid-`init.lua`, or
        // never got that far): unblock whatever's still waiting in
        // `spawn_module_host` rather than leaving it to time out.
        if let Some(tx) = outcome_tx.take() {
            let _ = tx.send(ModuleHostOutcome::LoadError(format!(
                "module-host connection for '{module_name}' closed before reporting ready"
            )));
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_module_host_method(
        &self,
        req: &IpcRequest,
        module_name: &str,
        permissions: &[ModulePermission],
        out_tx: &mpsc::UnboundedSender<String>,
        subs: &mut HashMap<String, JoinHandle<()>>,
        timers: &mut HashMap<String, JoinHandle<()>>,
        outcome_tx: &mut Option<std::sync::mpsc::Sender<ModuleHostOutcome>>,
    ) -> std::result::Result<Value, String> {
        match req.method.as_str() {
            "module_host.on" | "module_host.once" => {
                let once = req.method == "module_host.once";
                let pattern = req
                    .params
                    .get("pattern")
                    .and_then(Value::as_str)
                    .ok_or("missing pattern")?
                    .to_string();
                let sub_id = Uuid::new_v4().to_string();
                let mut rx = self.event_tx.subscribe();
                let out_tx2 = out_tx.clone();
                let sid = sub_id.clone();
                let handle = tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(evt) => {
                                if glob::matches_pattern(&pattern, &evt.event) {
                                    let push = ModuleHostPush::Event {
                                        subscription_id: sid.clone(),
                                        event: evt,
                                    };
                                    let Ok(line) = serde_json::to_string(&push) else {
                                        continue;
                                    };
                                    if out_tx2.send(format!("{line}\n")).is_err() {
                                        break;
                                    }
                                    if once {
                                        break;
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
                subs.insert(sub_id.clone(), handle);
                Ok(json!({ "subscription_id": sub_id }))
            }
            "module_host.off" => {
                let id = req
                    .params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("missing id")?
                    .to_string();
                if let Some(h) = subs.remove(&id) {
                    h.abort();
                }
                Ok(json!({ "ok": true }))
            }
            "module_host.after" | "module_host.every" => {
                let every = req.method == "module_host.every";
                let key = if every { "interval_ms" } else { "delay_ms" };
                let ms = req
                    .params
                    .get(key)
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .max(1);
                let timer_id = Uuid::new_v4().to_string();
                let out_tx2 = out_tx.clone();
                let tid = timer_id.clone();
                let handle = tokio::spawn(async move {
                    if every {
                        let mut iv = tokio::time::interval(Duration::from_millis(ms));
                        iv.tick().await; // first tick fires immediately; consume it so the module's first callback fires after one full interval
                        loop {
                            iv.tick().await;
                            let push = ModuleHostPush::Timer {
                                timer_id: tid.clone(),
                            };
                            let Ok(line) = serde_json::to_string(&push) else {
                                continue;
                            };
                            if out_tx2.send(format!("{line}\n")).is_err() {
                                break;
                            }
                        }
                    } else {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        let push = ModuleHostPush::Timer { timer_id: tid };
                        if let Ok(line) = serde_json::to_string(&push) {
                            let _ = out_tx2.send(format!("{line}\n"));
                        }
                    }
                });
                timers.insert(timer_id.clone(), handle);
                Ok(json!({ "timer_id": timer_id }))
            }
            "module_host.cancel" => {
                let id = req
                    .params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("missing id")?
                    .to_string();
                if let Some(h) = timers.remove(&id) {
                    h.abort();
                }
                Ok(json!({ "ok": true }))
            }
            "module_host.state_get" => {
                if !permissions
                    .iter()
                    .any(|p| p.kind == PermissionKind::StateRead)
                {
                    return Err("state.read not granted to this module".to_string());
                }
                let key = req
                    .params
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match self.state_handle.state_get(&key).await {
                    Some(v) => Ok(json!({ "value": v })),
                    None => Err("state path not found".to_string()),
                }
            }
            "module_host.emit" => {
                let event = req
                    .params
                    .get("event")
                    .and_then(Value::as_str)
                    .ok_or("missing event")?
                    .to_string();
                let data = req.params.get("data").cloned().unwrap_or_else(|| json!({}));
                self.manual_emit(&event, data)
            }
            "module_host.log" | "module_host.warn" | "module_host.error" => {
                let message = req
                    .params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match req.method.as_str() {
                    "module_host.log" => info!(module = %module_name, "{message}"),
                    "module_host.warn" => warn!(module = %module_name, "{message}"),
                    _ => error!(module = %module_name, "{message}"),
                }
                Ok(json!({ "ok": true }))
            }
            "module_host.fs_read" => {
                if !permissions.iter().any(|p| p.kind == PermissionKind::FsRead) {
                    return Err("fs.read not granted to this module".to_string());
                }
                let path = req
                    .params
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("missing path")?
                    .to_string();
                if !path_allowed(permissions, PermissionKind::FsRead, &path) {
                    return Err(format!(
                        "path '{path}' is outside this module's granted fs.read scope"
                    ));
                }
                let expanded = bread_shared::expand_path(&path);
                let content = std::fs::read_to_string(&expanded).ok();
                Ok(json!({ "content": content }))
            }
            "module_host.fs_write" => {
                if !permissions
                    .iter()
                    .any(|p| p.kind == PermissionKind::FsWrite)
                {
                    return Err("fs.write not granted to this module".to_string());
                }
                let path = req
                    .params
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("missing path")?
                    .to_string();
                if !path_allowed(permissions, PermissionKind::FsWrite, &path) {
                    return Err(format!(
                        "path '{path}' is outside this module's granted fs.write scope"
                    ));
                }
                let content = req
                    .params
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let expanded = bread_shared::expand_path(&path);
                if let Some(parent) = expanded.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&expanded, content).map_err(|e| e.to_string())?;
                Ok(json!({ "ok": true }))
            }
            "module_host.exec" => {
                if !permissions.iter().any(|p| p.kind == PermissionKind::Exec) {
                    return Err("exec not granted to this module".to_string());
                }
                let cmd = req
                    .params
                    .get("cmd")
                    .and_then(Value::as_str)
                    .ok_or("missing cmd")?
                    .to_string();
                let argv = exec_argv(permissions, &cmd)?;
                tokio::task::spawn_blocking(move || {
                    match std::process::Command::new(&argv[0])
                        .args(&argv[1..])
                        .status()
                    {
                        Ok(status) if !status.success() => {
                            warn!(cmd = %argv[0], code = ?status.code(), "module_host.exec exited non-zero");
                        }
                        Err(e) => {
                            error!(cmd = %argv[0], error = %e, "module_host.exec failed to spawn");
                        }
                        _ => {}
                    }
                });
                Ok(json!({ "ok": true }))
            }
            "module_host.exec_capture" => {
                if !permissions.iter().any(|p| p.kind == PermissionKind::Exec) {
                    return Err("exec not granted to this module".to_string());
                }
                let cmd = req
                    .params
                    .get("cmd")
                    .and_then(Value::as_str)
                    .ok_or("missing cmd")?
                    .to_string();
                let argv = exec_argv(permissions, &cmd)?;
                let timeout_ms = req
                    .params
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(2000);
                let handle = tokio::task::spawn_blocking(move || {
                    std::process::Command::new(&argv[0])
                        .args(&argv[1..])
                        .output()
                });
                match tokio::time::timeout(Duration::from_millis(timeout_ms + 500), handle).await {
                    Ok(Ok(Ok(out))) => Ok(json!({
                        "ok": out.status.success(),
                        "stdout": String::from_utf8_lossy(&out.stdout),
                    })),
                    _ => Ok(json!({ "ok": false, "stdout": "" })),
                }
            }
            method if method.starts_with(HYPRLAND_METHOD_PREFIX) => {
                hyprland_bridge(method, &req.params, permissions).await
            }
            "module_host.status" => {
                let state = req
                    .params
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("load_error");
                let error = req
                    .params
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let (load_state, outcome) = if state == "loaded" {
                    (ModuleLoadState::Loaded, ModuleHostOutcome::Ready)
                } else {
                    (
                        ModuleLoadState::LoadError,
                        ModuleHostOutcome::LoadError(
                            error
                                .clone()
                                .unwrap_or_else(|| "module load failed".to_string()),
                        ),
                    )
                };
                // Out-of-process modules are never "ungated": they only
                // exist in this branch because they declared a manifest
                // (see lua/mod.rs's load_module), so `ungated=false`
                // unconditionally here is correct, not a placeholder.
                self.state_handle.set_module_status_ex(
                    module_name.to_string(),
                    load_state,
                    error,
                    false,
                    false,
                );
                if let Some(tx) = outcome_tx.take() {
                    let _ = tx.send(outcome);
                }
                Ok(json!({ "ok": true }))
            }
            other => Err(format!("unknown module_host method: {other}")),
        }
    }
}

const HYPRLAND_METHOD_PREFIX: &str = "module_host.hyprland_";

/// Below the host's own 10s RPC timeout, so a wedged compositor surfaces as
/// this error rather than as an anonymous "timed out" in the module.
const HYPRLAND_TIMEOUT: Duration = Duration::from_secs(8);

/// Every `module_host.hyprland_*` method. The `hyprland` grant is checked
/// first and unconditionally, so a host that was not granted it gets the same
/// refusal for any method name under the prefix, real or invented. The
/// requests are made here, by the daemon, through the same checked helpers as
/// the in-process `bread.hyprland`: the child never gets a compositor socket.
async fn hyprland_bridge(
    method: &str,
    params: &Value,
    permissions: &[ModulePermission],
) -> std::result::Result<Value, String> {
    if !permissions
        .iter()
        .any(|p| p.kind == PermissionKind::Hyprland)
    {
        return Err("hyprland not granted to this module".to_string());
    }

    let text = |key: &str| -> std::result::Result<String, String> {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("missing {key}"))
    };

    let work: Box<dyn FnOnce() -> std::result::Result<Value, String> + Send> =
        match &method[HYPRLAND_METHOD_PREFIX.len()..] {
            "dispatch" => {
                let cmd = text("cmd")?;
                let args = params
                    .get("args")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Box::new(move || {
                    hyprland_compat::dispatch(hyprland_compat::request, &cmd, &args)
                        .map(|()| json!({ "ok": true }))
                        .map_err(|e| e.to_string())
                })
            }
            "keyword" => {
                let key = text("key")?;
                let value = text("value")?;
                Box::new(move || {
                    hyprland_compat::keyword(hyprland_compat::request, &key, &value)
                        .map(|()| json!({ "ok": true }))
                        .map_err(|e| e.to_string())
                })
            }
            "eval" => {
                let expr = text("expr")?;
                Box::new(move || {
                    hyprland_compat::request(&format!("eval {expr}"))
                        .map(|reply| json!({ "reply": reply }))
                        .map_err(|e| e.to_string())
                })
            }
            "query" => {
                let request = match text("kind")?.as_str() {
                    "active_window" => "j/activewindow",
                    "monitors" => "j/monitors",
                    "workspaces" => "j/workspaces",
                    "clients" => "j/clients",
                    other => return Err(format!("unknown hyprland query: {other}")),
                };
                Box::new(move || {
                    let reply = hyprland_compat::request(request).map_err(|e| e.to_string())?;
                    serde_json::from_str::<Value>(&reply)
                        .map(|value| json!({ "value": value }))
                        .map_err(|e| e.to_string())
                })
            }
            _ => return Err(format!("unknown module_host method: {method}")),
        };

    match tokio::time::timeout(HYPRLAND_TIMEOUT, tokio::task::spawn_blocking(work)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(format!("hyprland request failed: {e}")),
        Err(_) => Err("hyprland request timed out".to_string()),
    }
}

/// Belt-and-suspenders path scoping for `fs_read`/`fs_write`: if the
/// manifest declared a `path` hint for this permission kind, the requested
/// path (after `~`-expansion and canonicalize) must be a real descendant
/// of at least one granted prefix. No hint at all means this RPC-level
/// check stays permissive (matching Workstream D's existing "un-hinted
/// grant = ungated within that namespace" behavior): Landlock's own
/// ruleset (built independently in `module_host::apply_sandbox`) does NOT
/// grant a filesystem rule for an un-hinted permission, so the direct
/// `os`/`io` escape hatch remains kernel-denied for that case regardless
/// of what this function returns.
fn path_allowed(permissions: &[ModulePermission], kind: PermissionKind, path: &str) -> bool {
    let hints: Vec<&String> = permissions
        .iter()
        .filter(|p| p.kind == kind)
        .filter_map(|p| p.path.as_ref())
        .collect();
    if hints.is_empty() {
        return true;
    }
    let Some(requested) = resolve_scoped_path(path) else {
        return false;
    };
    hints.iter().any(|hint| {
        let Some(granted) = resolve_scoped_path(hint) else {
            return false;
        };
        // Path::starts_with is component-wise; str::starts_with would let
        // `/Wallpapers-evil` match a `/Wallpapers` grant.
        requested.starts_with(&granted)
    })
}

/// Canonicalize for containment: existing paths resolve symlinks and `..`;
/// new files canonicalize the parent then re-join the filename. Anything
/// that still contains `..` after lexical normalization is rejected.
fn resolve_scoped_path(path: &str) -> Option<PathBuf> {
    let expanded = bread_shared::expand_path(path);
    if let Ok(canon) = expanded.canonicalize() {
        return Some(canon);
    }
    let parent = expanded.parent().filter(|p| !p.as_os_str().is_empty());
    if let (Some(parent), Some(name)) = (parent, expanded.file_name()) {
        if let Ok(parent_canon) = parent.canonicalize() {
            return Some(parent_canon.join(name));
        }
    }
    lexical_abs(&expanded)
}

fn lexical_abs(path: &Path) -> Option<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::absolute(path).ok()?
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            rest => out.push(rest),
        }
    }
    if out.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(out)
}

/// Split `cmd` into argv without a shell. Metacharacters are rejected so a
/// hinted binary cannot smuggle extra commands (`hyprpaper; curl evil`).
fn parse_exec_argv(cmd: &str) -> Option<Vec<String>> {
    if cmd.chars().any(|c| {
        matches!(
            c,
            '|' | ';' | '&' | '$' | '`' | '\n' | '\r' | '<' | '>' | '(' | ')'
        )
    }) {
        return None;
    }
    let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        None
    } else {
        Some(argv)
    }
}

/// Parse `cmd` and enforce the exec `bin` hint against argv[0].
fn exec_argv(permissions: &[ModulePermission], cmd: &str) -> Result<Vec<String>, String> {
    let argv = parse_exec_argv(cmd)
        .ok_or_else(|| "command contains shell metacharacters or is empty".to_string())?;
    if !bin_allowed(permissions, &argv[0]) {
        return Err("command is outside this module's granted exec bin scope".to_string());
    }
    Ok(argv)
}

/// Same idea as [`path_allowed`] for `exec`'s `bin` hint: compares argv[0]
/// by file name (so `bin = "hyprpaper"` matches `/usr/bin/hyprpaper` as
/// well as a bare `hyprpaper`) or an exact path match.
fn bin_allowed(permissions: &[ModulePermission], program: &str) -> bool {
    let hints: Vec<&String> = permissions
        .iter()
        .filter(|p| p.kind == PermissionKind::Exec)
        .filter_map(|p| p.bin.as_ref())
        .collect();
    if hints.is_empty() {
        return true;
    }
    let cmd_leaf = Path::new(program)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(program);
    hints.iter().any(|hint| {
        let hint_leaf = Path::new(hint.as_str())
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(hint.as_str());
        cmd_leaf == hint_leaf || program == hint.as_str()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs_grant(kind: PermissionKind, path: &str) -> Vec<ModulePermission> {
        vec![ModulePermission {
            kind,
            path: Some(path.to_string()),
            bin: None,
        }]
    }

    fn exec_grant(bin: &str) -> Vec<ModulePermission> {
        vec![ModulePermission {
            kind: PermissionKind::Exec,
            path: None,
            bin: Some(bin.to_string()),
        }]
    }

    #[tokio::test]
    async fn hyprland_methods_are_refused_without_the_grant_whatever_the_name() {
        let perms = vec![
            ModulePermission {
                kind: PermissionKind::StateRead,
                path: None,
                bin: None,
            },
            ModulePermission {
                kind: PermissionKind::Exec,
                path: None,
                bin: None,
            },
        ];
        for method in [
            "module_host.hyprland_dispatch",
            "module_host.hyprland_keyword",
            "module_host.hyprland_eval",
            "module_host.hyprland_query",
            "module_host.hyprland_made_up",
            "module_host.hyprland_",
        ] {
            let err = hyprland_bridge(method, &json!({}), &perms)
                .await
                .unwrap_err();
            assert_eq!(err, "hyprland not granted to this module", "{method}");
        }
    }

    #[tokio::test]
    async fn hyprland_granted_still_rejects_unknown_methods_and_missing_params() {
        let perms = vec![ModulePermission {
            kind: PermissionKind::Hyprland,
            path: None,
            bin: None,
        }];
        let err = hyprland_bridge("module_host.hyprland_made_up", &json!({}), &perms)
            .await
            .unwrap_err();
        assert!(err.contains("unknown module_host method"), "{err}");
        let err = hyprland_bridge("module_host.hyprland_dispatch", &json!({}), &perms)
            .await
            .unwrap_err();
        assert_eq!(err, "missing cmd");
        let err = hyprland_bridge(
            "module_host.hyprland_query",
            &json!({ "kind": "keybinds" }),
            &perms,
        )
        .await
        .unwrap_err();
        assert!(err.contains("unknown hyprland query"), "{err}");
    }

    #[test]
    fn path_allowed_denies_dotdot_escape_from_granted_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wallpapers = tmp.path().join("Wallpapers");
        std::fs::create_dir(&wallpapers).unwrap();
        let granted = wallpapers.to_str().unwrap();
        let perms = fs_grant(PermissionKind::FsRead, granted);

        let escape = wallpapers.join("../../.ssh/id_rsa");
        assert!(
            !path_allowed(&perms, PermissionKind::FsRead, escape.to_str().unwrap()),
            "Wallpapers/../../.ssh/id_rsa must not match a Wallpapers grant"
        );
    }

    #[test]
    fn path_allowed_denies_string_prefix_sibling() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wallpapers = tmp.path().join("Wallpapers");
        let evil = tmp.path().join("Wallpapers-evil");
        std::fs::create_dir(&wallpapers).unwrap();
        std::fs::create_dir(&evil).unwrap();
        let secret = evil.join("secret");
        std::fs::write(&secret, "x").unwrap();
        let perms = fs_grant(PermissionKind::FsRead, wallpapers.to_str().unwrap());
        assert!(!path_allowed(
            &perms,
            PermissionKind::FsRead,
            secret.to_str().unwrap()
        ));
    }

    #[test]
    fn path_allowed_accepts_real_descendant_and_new_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wallpapers = tmp.path().join("Wallpapers");
        std::fs::create_dir(&wallpapers).unwrap();
        let existing = wallpapers.join("bg.png");
        std::fs::write(&existing, "x").unwrap();
        let perms = fs_grant(PermissionKind::FsWrite, wallpapers.to_str().unwrap());
        assert!(path_allowed(
            &perms,
            PermissionKind::FsWrite,
            existing.to_str().unwrap()
        ));
        let new_file = wallpapers.join("new.png");
        assert!(path_allowed(
            &perms,
            PermissionKind::FsWrite,
            new_file.to_str().unwrap()
        ));
    }

    #[test]
    fn exec_argv_denies_shell_chaining_when_bin_hinted() {
        let perms = exec_grant("hyprpaper");
        assert!(exec_argv(&perms, "hyprpaper; curl evil").is_err());
        assert!(exec_argv(&perms, "hyprpaper").is_ok());
        assert!(exec_argv(&perms, "/usr/bin/hyprpaper --config x").is_ok());
        assert!(exec_argv(&perms, "curl evil").is_err());
    }

    #[test]
    fn parse_exec_argv_rejects_metacharacters() {
        assert!(parse_exec_argv("hyprpaper; curl evil").is_none());
        assert!(parse_exec_argv("hyprpaper | curl evil").is_none());
        assert!(parse_exec_argv("hyprpaper && curl evil").is_none());
        assert_eq!(
            parse_exec_argv("hyprpaper --config x"),
            Some(vec!["hyprpaper".into(), "--config".into(), "x".into()])
        );
    }
}
