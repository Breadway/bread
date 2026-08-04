//! Workstream G acceptance tests: the real, end-to-end version of the two
//! things this workstream exists to prove, going through a real spawned
//! `breadd` + a real spawned `bread-module-host` child + a real IPC
//! handshake — not the in-isolation Landlock-mechanism unit tests in
//! `breadd/src/module_host.rs` (`landlock_denies_reads_outside_granted_path`,
//! `no_exec_permission_means_binary_cannot_be_executed_at_all`), which only
//! exercise `apply_sandbox` directly against a plain `sh`/`cat`.
//!
//! 1. [`os_execute_and_io_open_are_denied_at_the_kernel_level_outside_granted_scope`] —
//!    a module granted `fs.read` for exactly one directory (and nothing
//!    else) runs real Lua that calls `io.open`/`os.execute` directly,
//!    bypassing the RPC bridge entirely and going straight for the
//!    `os`/`io` escape hatch Workstream D's in-process scoping admittedly
//!    leaves open (see `breadd/src/lua/mod.rs`'s `build_scoped_env` doc
//!    comment). This is the single most important test in the whole
//!    workstream: proving the denial is a *kernel* permission error, not a
//!    Lua-level check that a well-behaved module merely chooses to respect.
//! 2. [`killing_a_module_host_child_does_not_take_down_breadd_or_other_modules`] —
//!    `kill -9` on a running module-host child's PID, confirming `breadd`
//!    itself and a second, unrelated module both keep responding, and that
//!    `breadd` detects the death and reports it via `bread.module.crashed`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

// NOTE: these tests need `target/{debug,release}/bread-module-host` to
// already exist — `breadd::module_host::resolve_module_host_binary` looks
// for it as a sibling of `breadd`'s own executable. `bread-module-host` is
// a bin-only crate (no `[lib]` target — deliberately, see its Cargo.toml),
// so it can't be pulled in as a `[dev-dependencies]` entry to force cargo
// to build it via `env!("CARGO_BIN_EXE_...")`, the usual trick for this.
// Running via `cargo test --workspace` (this repo's documented/required
// verification command — see Documentation.md) builds every workspace
// member, including `bread-module-host`, before any test runs, so this
// isn't a problem in practice; running `cargo test -p breadd` in isolation
// without a prior `cargo build --workspace` would need one first.

struct TestHarness {
    _temp: TempDir,
    child: Child,
    socket_path: PathBuf,
    #[allow(dead_code)]
    home: PathBuf,
}

impl TestHarness {
    /// Spawns a real `breadd` with `[modules] builtin = false` and one
    /// directory-based module per `(name, manifest_toml, init_lua)` entry —
    /// the same on-disk shape `bread modules install` produces
    /// (`<modules_dir>/<name>/{bread.module.toml,init.lua}`).
    fn spawn_with_modules(modules: &[(&str, &str, &str)]) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let runtime_dir = temp.path().join("runtime");
        let config_home = temp.path().join("config");
        let home = temp.path().join("home");
        fs::create_dir_all(&runtime_dir)?;
        fs::create_dir_all(&config_home)?;
        fs::create_dir_all(&home)?;

        let bread_cfg = config_home.join("bread");
        fs::create_dir_all(bread_cfg.join("modules"))?;
        fs::write(
            bread_cfg.join("init.lua"),
            "bread.on('bread.system.startup', function() end)\n",
        )?;

        for (name, manifest_toml, init_lua) in modules {
            let module_dir = bread_cfg.join("modules").join(name);
            fs::create_dir_all(&module_dir)?;
            if !manifest_toml.is_empty() {
                fs::write(module_dir.join("bread.module.toml"), manifest_toml)?;
            }
            fs::write(module_dir.join("init.lua"), init_lua)?;
        }

        fs::write(
            bread_cfg.join("breadd.toml"),
            r#"
[daemon]
log_level = "error"

[lua]
entry_point = "~/.config/bread/init.lua"
module_path = "~/.config/bread/modules"

[modules]
builtin = false

[adapters.hyprland]
enabled = false

[adapters.udev]
enabled = false

[adapters.power]
enabled = false

[adapters.network]
enabled = false

[adapters.podman]
enabled = false
"#,
        )?;

        let socket_path = runtime_dir.join("bread").join("breadd.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_breadd"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        Ok(Self {
            _temp: temp,
            child,
            socket_path,
            home,
        })
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    async fn wait_until_ready(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if self.socket_path.exists() {
                if self.send_request("ping", json!({})).await.is_ok() {
                    return Ok(());
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!("daemon did not become ready in time"))
    }

    /// Poll `modules.list` until `name` shows up `Loaded` — out-of-process
    /// modules report their load outcome asynchronously (see
    /// `breadd/src/lua/mod.rs`'s `load_out_of_process_module`), so a plain
    /// `wait_until_ready` (which only proves the daemon's IPC socket is up)
    /// isn't enough to know a specific module has finished spawning,
    /// connecting, authenticating, and running its `init.lua`.
    async fn wait_for_module_loaded(&self, name: &str) -> Result<()> {
        // Comfortably exceeds module_host::READY_TIMEOUT (breadd's own
        // spawn-side wait) so this test-side poll doesn't give up before
        // breadd itself would.
        let deadline = Instant::now() + Duration::from_secs(55);
        while Instant::now() < deadline {
            let modules = self.send_request("modules.list", json!({})).await?;
            if let Some(arr) = modules.as_array() {
                for m in arr {
                    if m.get("name").and_then(Value::as_str) == Some(name) {
                        if m.get("status").and_then(Value::as_str) == Some("loaded") {
                            return Ok(());
                        }
                        if m.get("status").and_then(Value::as_str) == Some("load_error") {
                            return Err(anyhow!(
                                "module '{name}' failed to load: {:?}",
                                m.get("last_error")
                            ));
                        }
                    }
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!("module '{name}' did not reach Loaded within timeout"))
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let stream = UnixStream::connect(self.socket_path()).await?;
        let (read_half, mut write_half) = stream.into_split();

        let req = json!({ "id": "1", "method": method, "params": params });
        write_half
            .write_all(format!("{}\n", serde_json::to_string(&req)?).as_bytes())
            .await?;

        let mut lines = BufReader::new(read_half).lines();
        let line = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("missing ipc response"))?;
        let parsed: Value = serde_json::from_str(&line)?;

        if let Some(err) = parsed.get("error").and_then(Value::as_str) {
            return Err(anyhow!(err.to_string()));
        }
        Ok(parsed.get("result").cloned().unwrap_or_else(|| json!({})))
    }

    /// Find the PID of a `bread-module-host` child spawned for this
    /// harness's `breadd` by scanning `/proc/*/environ` for
    /// `BREAD_MODULE_NAME=<module_name>` — the module-host binary never
    /// puts its identity in argv (see its own doc comment on why:
    /// `/proc/*/cmdline` is visible to any process), so this is the same
    /// kind of environment-based lookup, just from the test side instead
    /// of breadd's.
    fn find_module_host_pid(&self, module_name: &str) -> Result<u32> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            for entry in fs::read_dir("/proc")?.flatten() {
                let file_name = entry.file_name();
                let Some(pid_str) = file_name.to_str() else {
                    continue;
                };
                let Ok(pid) = pid_str.parse::<u32>() else {
                    continue;
                };
                let environ_path = entry.path().join("environ");
                let Ok(environ) = fs::read(&environ_path) else {
                    continue;
                };
                let wanted = format!("BREAD_MODULE_NAME={module_name}");
                if environ
                    .split(|b| *b == 0)
                    .any(|var| var == wanted.as_bytes())
                {
                    return Ok(pid);
                }
            }
            if Instant::now() > deadline {
                return Err(anyhow!(
                    "no bread-module-host process found for module '{module_name}'"
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn shutdown(self) {
        // Drop does the actual killing (see below) — this method exists so
        // call sites can be explicit about "done with this harness" without
        // caring exactly how cleanup happens.
        drop(self);
    }
}

impl Drop for TestHarness {
    /// Any `?`-propagated failure partway through a test (a timed-out
    /// event, a failed assertion via `anyhow!` — though assertion panics
    /// unwind rather than `?`-return, they still run `Drop`) must not leak
    /// a live `breadd` (and, transitively, any `bread-module-host`
    /// children it spawned) — `kill` here, not just on the happy path via
    /// `shutdown()`, is what keeps a failed test run from leaving orphaned
    /// sandboxed processes behind for the next run to trip over.
    fn drop(&mut self) {
        // SIGTERM first, not straight to SIGKILL — see the matching comment
        // in `ipc_integration.rs`'s `TestHarness::drop`. SIGKILL prevents
        // `breadd` from ever running its own graceful shutdown path, which
        // is what actually fires `kill_on_drop` on adapter-spawned child
        // processes (e.g. `PodmanAdapter`'s `podman events` watcher) —
        // exactly how a day of repeated `cargo test --workspace` runs left
        // 1,559 orphaned `podman events` processes system-wide. Bounded
        // wait, then SIGKILL as a fallback so a genuinely wedged `breadd`
        // doesn't hang the test suite.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The P0 acceptance test: verified at the OS level, not asserted. See this
/// file's module doc comment.
#[tokio::test]
async fn os_execute_and_io_open_are_denied_at_the_kernel_level_outside_granted_scope() -> Result<()>
{
    let allowed_dir = tempfile::tempdir()?;
    let allowed_file = allowed_dir.path().join("allowed.txt");
    fs::write(&allowed_file, "allowed-content")?;

    // Deliberately NOT under $HOME/anything the manifest grants, and
    // deliberately world-readable-by-this-user (normal DAC permissions
    // alone would NOT deny this) so a pass here can only be explained by
    // Landlock, not by an unrelated ordinary permission error — the same
    // reasoning as the `deny_dir`/`secret.txt` split in
    // `breadd/src/module_host.rs`'s unit tests, just end-to-end this time.
    let deny_dir = tempfile::tempdir()?;
    let deny_file = deny_dir.path().join("secret.txt");
    fs::write(&deny_file, "top-secret-content")?;

    let manifest = format!(
        r#"
name = "escape-hatch-test"

[[permissions]]
type = "fs.read"
path = "{}"
"#,
        allowed_dir.path().display()
    );

    // No `exec` permission granted at all, so `os.execute` should fail
    // outright (can't even launch `/bin/sh` under Landlock) — and
    // `io.open`, which doesn't need a subprocess at all, directly tests
    // the FsRead scoping. Results are reported back over `bread.emit`
    // (baseline, always available) since this module runs in a separate
    // process we can't otherwise introspect from the test.
    //
    // The checks run on a `bread.on("test.trigger", ...)` handler, NOT in
    // `on_load` — a module loads (and, if it ran in `on_load`, would emit
    // its result) as part of daemon startup, which races the test's own
    // `events.subscribe` connection. `tokio::sync::broadcast` (what
    // `events.subscribe` reads from) does not replay history to a
    // subscriber that joins after a send already happened — a late
    // subscription just misses it, no error, no buffering — so without
    // this explicit trigger the test would be racing the daemon's own
    // startup sequence rather than reliably observing anything.
    let init_lua = format!(
        r#"
local M = bread.module({{ name = "escape-hatch-test", version = "1.0.0" }})

bread.on("test.trigger", function(trigger_event)
    local allowed_result = "ALLOWED_READ_FAILED"
    local fh = io.open("{allowed}", "r")
    if fh then
        local content = fh:read("*a")
        fh:close()
        allowed_result = "ALLOWED_READ_OK:" .. content
    end

    local denied_result = "DENIED_READ_UNEXPECTEDLY_SUCCEEDED"
    local deny_fh = io.open("{denied}", "r")
    if deny_fh then
        local content = deny_fh:read("*a")
        deny_fh:close()
        denied_result = "DENIED_READ_UNEXPECTEDLY_SUCCEEDED:" .. content
    else
        denied_result = "io.open denied"
    end

    local exec_ok = os.execute("cat {denied} > /dev/null 2>&1")
    local exec_result
    if exec_ok == true then
        exec_result = "EXEC_UNEXPECTEDLY_SUCCEEDED"
    else
        exec_result = "exec denied or failed"
    end

    bread.emit("test.escape_hatch_result", {{
        allowed_result = allowed_result,
        denied_result = denied_result,
        exec_result = exec_result,
    }})
end)

return M
"#,
        allowed = allowed_file.display(),
        denied = deny_file.display(),
    );

    let harness = TestHarness::spawn_with_modules(&[("escape-hatch-test", &manifest, &init_lua)])?;
    harness.wait_until_ready().await?;
    // Guarantees the module's `bread.on("test.trigger", ...)` subscription
    // is already registered server-side before the trigger below is sent —
    // "loaded" status is only reported (via `module_host.status`) after the
    // module's whole init.lua chunk, including that top-level `bread.on`
    // call, has finished executing. See this test's other race-avoidance
    // comment above for why this matters.
    harness.wait_for_module_loaded("escape-hatch-test").await?;

    let stream = UnixStream::connect(harness.socket_path()).await?;
    let (read_half, mut write_half) = stream.into_split();
    let subscribe = json!({
        "id": "sub-1",
        "method": "events.subscribe",
        "params": { "filter": "test.escape_hatch_result" },
    });
    write_half
        .write_all(format!("{}\n", serde_json::to_string(&subscribe)?).as_bytes())
        .await?;
    let mut reader = BufReader::new(read_half).lines();
    let _ack = reader.next_line().await?;

    harness
        .send_request("emit", json!({ "event": "test.trigger", "data": {} }))
        .await?;

    let line = timeout(Duration::from_secs(15), reader.next_line())
        .await
        .map_err(|_| anyhow!("timed out waiting for test.escape_hatch_result event"))??
        .ok_or_else(|| anyhow!("connection closed before event arrived"))?;
    let event: Value = serde_json::from_str(&line)?;
    let data = event
        .get("data")
        .ok_or_else(|| anyhow!("event missing data"))?;

    let allowed_result = data.get("allowed_result").and_then(Value::as_str).unwrap_or("");
    let denied_result = data.get("denied_result").and_then(Value::as_str).unwrap_or("");
    let exec_result = data.get("exec_result").and_then(Value::as_str).unwrap_or("");

    assert!(
        allowed_result.starts_with("ALLOWED_READ_OK"),
        "the granted fs.read directory should remain readable via direct io.open; got {allowed_result:?}"
    );
    assert!(
        !denied_result.contains("UNEXPECTEDLY_SUCCEEDED"),
        "io.open on a path OUTSIDE the granted fs.read scope must be denied at the kernel level (Landlock), not merely un-offered by an RPC binding — got {denied_result:?}"
    );
    assert!(
        !exec_result.contains("UNEXPECTEDLY_SUCCEEDED"),
        "os.execute with no `exec` permission granted must not be able to run anything at all — got {exec_result:?}"
    );

    harness.shutdown();
    Ok(())
}

/// P0 item 5: killing a module-host child must not take `breadd` (or any
/// other module) down with it, and `breadd` must notice and report it.
#[tokio::test]
async fn killing_a_module_host_child_does_not_take_down_breadd_or_other_modules() -> Result<()> {
    // An explicit, empty `permissions = []` — not "no manifest at all" — is
    // what opts a module into the out-of-process sandboxed path with zero
    // grants (see `ModuleDecl::permissions`'s doc comment in
    // `breadd/src/lua/mod.rs`: `None` means "no manifest", which keeps
    // today's in-process, ungated legacy behavior; `Some(vec![])` means
    // "deliberately baseline-only" and IS routed out-of-process).
    let victim_manifest = "name = \"victim\"\npermissions = []\n";

    let victim_init = r#"
local M = bread.module({ name = "victim", version = "1.0.0" })
function M.on_load() end
return M
"#;

    // The "control" module stays in-process (no manifest at all — the
    // legacy/backward-compat path) specifically so this test also proves
    // an out-of-process module's crash doesn't disturb an *in-process*
    // module either, not just breadd's own IPC responsiveness.
    let control_init = r#"
local M = bread.module({ name = "control", version = "1.0.0" })
bread.on("bread.custom.ping_control", function(event)
    bread.emit("bread.custom.pong_control", {})
end)
return M
"#;

    let harness = TestHarness::spawn_with_modules(&[
        ("victim", victim_manifest, victim_init),
        ("control", "", control_init),
    ])?;
    harness.wait_until_ready().await?;
    harness.wait_for_module_loaded("victim").await?;

    // Subscribe to bread.module.crashed BEFORE killing, so we can't miss it.
    let crash_stream = UnixStream::connect(harness.socket_path()).await?;
    let (crash_read, mut crash_write) = crash_stream.into_split();
    crash_write
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "id": "crash-sub",
                    "method": "events.subscribe",
                    "params": { "filter": "bread.module.crashed" },
                }))?
            )
            .as_bytes(),
        )
        .await?;
    let mut crash_reader = BufReader::new(crash_read).lines();
    let _ack = crash_reader.next_line().await?;

    let victim_pid = harness.find_module_host_pid("victim")?;
    let kill_status = Command::new("kill").args(["-9", &victim_pid.to_string()]).status()?;
    assert!(kill_status.success(), "failed to send SIGKILL to victim module-host");

    // breadd itself must keep responding.
    let ping = harness.send_request("ping", json!({})).await?;
    assert_eq!(ping.get("ok").and_then(Value::as_bool), Some(true));

    // The unrelated in-process "control" module must keep dispatching
    // events normally.
    let control_stream = UnixStream::connect(harness.socket_path()).await?;
    let (control_read, mut control_write) = control_stream.into_split();
    control_write
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "id": "pong-sub",
                    "method": "events.subscribe",
                    "params": { "filter": "bread.custom.pong_control" },
                }))?
            )
            .as_bytes(),
        )
        .await?;
    let mut control_reader = BufReader::new(control_read).lines();
    let _ack = control_reader.next_line().await?;

    harness
        .send_request(
            "emit",
            json!({ "event": "bread.custom.ping_control", "data": {} }),
        )
        .await?;

    let pong_line = timeout(Duration::from_secs(10), control_reader.next_line())
        .await
        .map_err(|_| anyhow!("control module did not respond after victim was killed"))??
        .ok_or_else(|| anyhow!("control connection closed unexpectedly"))?;
    let pong: Value = serde_json::from_str(&pong_line)?;
    assert_eq!(
        pong.get("event").and_then(Value::as_str),
        Some("bread.custom.pong_control"),
        "control module should still be alive and responsive after the victim module-host was killed"
    );

    // breadd must have detected the death and reported it.
    let crash_line = timeout(Duration::from_secs(10), crash_reader.next_line())
        .await
        .map_err(|_| anyhow!("bread.module.crashed was not emitted after kill -9"))??
        .ok_or_else(|| anyhow!("crash subscription connection closed unexpectedly"))?;
    let crash_event: Value = serde_json::from_str(&crash_line)?;
    assert_eq!(
        crash_event
            .get("data")
            .and_then(|d| d.get("module"))
            .and_then(Value::as_str),
        Some("victim"),
        "bread.module.crashed should identify the module whose host process died"
    );
    assert_eq!(
        crash_event
            .get("data")
            .and_then(|d| d.get("signal"))
            .and_then(Value::as_i64),
        Some(9),
        "the crash report should reflect that the process was killed by SIGKILL"
    );

    harness.shutdown();
    Ok(())
}
