//! `bread-module-host` — the out-of-process runtime for a single third-party
//! Bread module (Workstream G).
//!
//! `breadd` spawns one of these per out-of-process module (see
//! `breadd/src/module_host.rs`), sandboxed at the OS level via a Landlock
//! ruleset applied by the parent *before* this binary's own `main()` ever
//! runs (through `Command::pre_exec` — see that module's doc comment for
//! why this binary itself has no Landlock dependency at all). This process
//! then:
//!
//! 1. Connects to `breadd`'s existing IPC socket
//!    (`$XDG_RUNTIME_DIR/bread/breadd.sock` by default).
//! 2. Presents the one-time token `breadd` gave it (via `$BREAD_MODULE_TOKEN`,
//!    an env var rather than argv, which is visible to any process via
//!    `/proc/*/cmdline`) via `module_host.hello` and learns its own identity
//!    (module name + granted permissions) from `breadd`'s answer — it never
//!    asserts its own name and have that trusted.
//! 3. Loads exactly one module's `init.lua` (`$BREAD_MODULE_ENTRY`) into a
//!    fresh Lua VM whose `bread` table is built entirely from RPC-backed
//!    proxies (see `lua_env`) instead of direct in-process bindings.
//! 4. Reports load success/failure back to `breadd` (`module_host.status`),
//!    then dispatches subscribed events/timers pushed down the same
//!    connection until it closes.
//!
//! Env vars, all required except `BREAD_MODULE_SOCKET` and
//! `BREAD_MODULE_NAME`:
//! - `BREAD_MODULE_TOKEN` — one-time handshake token.
//! - `BREAD_MODULE_ENTRY` — absolute path to the module's entry `.lua` file.
//! - `BREAD_MODULE_SOCKET` — override for breadd's socket path (defaults to
//!   `bread_shared::resolve_socket_path()`, the same resolution breadd's own
//!   `Config::socket_path` uses).
//! - `BREAD_MODULE_NAME` — informational only (early log lines before the
//!   hello response arrives); never trusted for permission lookup.

mod io;
mod lua_env;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use bread_shared::{ModuleHostHello, ModuleHostPush};
use tracing::{error, info, warn};

use io::{HostMessage, IoCommand};
use lua_env::ModuleHostLua;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let module_name_hint = std::env::var("BREAD_MODULE_NAME").unwrap_or_else(|_| "?".to_string());

    let token = match std::env::var("BREAD_MODULE_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            eprintln!("bread-module-host: missing BREAD_MODULE_TOKEN env var");
            std::process::exit(1);
        }
    };
    let entry = match std::env::var("BREAD_MODULE_ENTRY") {
        Ok(e) => PathBuf::from(e),
        Err(_) => {
            eprintln!("bread-module-host: missing BREAD_MODULE_ENTRY env var");
            std::process::exit(1);
        }
    };
    let socket_path = match std::env::var("BREAD_MODULE_SOCKET") {
        Ok(s) => PathBuf::from(s),
        Err(_) => bread_shared::resolve_socket_path(),
    };

    info!(
        module_hint = %module_name_hint,
        entry = %entry.display(),
        socket = %socket_path.display(),
        "bread-module-host starting"
    );

    let (cmd_tx, cmd_rx) = mpsc::channel::<IoCommand>();
    let (host_tx, host_rx) = mpsc::channel::<HostMessage>();
    let (hello_tx, hello_rx) = mpsc::channel::<Result<ModuleHostHello, String>>();

    if std::thread::Builder::new()
        .name("module-host-io".to_string())
        .spawn(move || io::run(socket_path, token, cmd_rx, host_tx, hello_tx))
        .is_err()
    {
        eprintln!("bread-module-host: failed to spawn io thread");
        std::process::exit(1);
    }

    let hello = match hello_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            error!(error = %e, "bread-module-host: hello handshake failed");
            std::process::exit(1);
        }
        Err(_) => {
            error!("bread-module-host: timed out waiting for hello handshake");
            std::process::exit(1);
        }
    };

    info!(
        module = %hello.module,
        permissions = ?hello.permissions,
        api_version = %hello.api_version,
        "bread-module-host: identity established by breadd"
    );

    let engine = match ModuleHostLua::new(cmd_tx.clone(), hello.module.clone(), hello.permissions.clone()) {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, "bread-module-host: failed to build lua environment");
            report_status(&cmd_tx, false, Some(e.to_string()));
            std::process::exit(1);
        }
    };

    match engine.load_entry(&entry) {
        Ok(()) => {
            info!(module = %hello.module, "bread-module-host: module loaded successfully");
            report_status(&cmd_tx, true, None);
        }
        Err(e) => {
            error!(module = %hello.module, error = %e, "bread-module-host: module load failed");
            report_status(&cmd_tx, false, Some(e.to_string()));
            std::process::exit(1);
        }
    }

    // Steady state: dispatch pushed events/timers until the connection to
    // breadd drops (breadd exited, socket closed, or we were killed and
    // this line never runs at all — see breadd/src/module_host.rs's
    // child-reap thread for the other half of that crash-isolation story).
    loop {
        match host_rx.recv() {
            Ok(HostMessage::Push(ModuleHostPush::Event {
                subscription_id,
                event,
            })) => {
                engine.dispatch_event(&subscription_id, &event);
            }
            Ok(HostMessage::Push(ModuleHostPush::Timer { timer_id })) => {
                engine.dispatch_timer(&timer_id);
            }
            Ok(HostMessage::Closed) | Err(_) => {
                warn!("bread-module-host: connection to breadd closed, exiting");
                break;
            }
        }
    }
}

fn report_status(cmd_tx: &mpsc::Sender<IoCommand>, ok: bool, error: Option<String>) {
    let params = if ok {
        serde_json::json!({ "state": "loaded" })
    } else {
        serde_json::json!({ "state": "load_error", "error": error })
    };
    let _ = io::call(cmd_tx, "module_host.status", params, Duration::from_secs(5));
}
