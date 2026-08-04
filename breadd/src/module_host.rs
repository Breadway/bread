//! Spawning, token-based identity, and OS-level sandboxing for out-of-process
//! module hosts (Workstream G).
//!
//! # Why a process, not just the existing Lua-level scoping
//!
//! Workstream D's `build_scoped_env` (see `lua/mod.rs`) gates the
//! *documented* `bread.*` surface by controlling which keys exist on the
//! `bread` table a module's chunk sees. Its own doc comment says plainly
//! that `os.execute`/`io.open`/`debug.*` remain fully reachable from a
//! scoped module — Lua's stdlib isn't sandboxed at all, only the `bread`
//! table is. A module that never calls `bread.fs`/`bread.exec` and instead
//! calls `io.open`/`os.execute` directly bypasses the whole mechanism,
//! because everything still runs as Lua code inside `breadd`'s own OS
//! process, sharing its real filesystem/exec access at the kernel level.
//!
//! This module closes that gap for any module that opted into the
//! capability-manifest system (`decl.permissions.is_some()` — see
//! `lua/mod.rs`'s `load_module`): instead of loading its chunk in-process,
//! `breadd` spawns a separate `bread-module-host` OS process for it,
//! restricted by a Landlock ruleset built from that module's granted
//! `ModulePermission`s *before* the child ever executes a byte of the
//! module's Lua.
//!
//! # Why Landlock over bubblewrap/firejail
//!
//! - Pure Rust, no external sandboxing binary dependency — this workspace's
//!   existing style already favors native Rust crates over shelling out
//!   (see e.g. `udev`, `zbus`, `rtnetlink` instead of wrapping CLI tools).
//! - Unprivileged: no setuid helper, no CAP_SYS_ADMIN, works from an
//!   ordinary user session exactly like the rest of `breadd`.
//! - Available since Linux 5.13; this repo's dev kernel is 6.18 and the
//!   mechanism was verified against it directly before adoption (see the
//!   `landlock` entry in the workspace `Cargo.toml` and
//!   `module_host::tests::landlock_denies_reads_outside_granted_path`
//!   below) — a `pre_exec`-restricted child process attempting to read a
//!   file outside its granted rule set gets `EACCES` from the kernel, not a
//!   Lua-level error.
//! - `bubblewrap`-wrapping remains a documented fallback if a target
//!   platform's kernel lacks Landlock support (pre-5.13, or a hardened
//!   kernel config with it compiled out) — not implemented here since
//!   Landlock covers this repo's actual target (a modern desktop Linux
//!   kernel) and keeps the dependency footprint native-Rust-only.
//!
//! # What Landlock does *not* cover here (P2, explicitly deferred)
//!
//! Network access. Landlock gained TCP bind/connect mediation in ABI v4+
//! (kernel 6.7+), but wiring a `network` permission kind through the
//! manifest schema, `PermissionKind`, and this sandbox builder is scoped
//! out of this workstream's P0 — see `Documentation.md`.
//!
//! # The token handshake
//!
//! Workstream A deliberately did not build a generic IPC connection-identity
//! system (it closed a narrower spoofing gap instead), so there's no
//! existing `module:<name>` identity concept to hook into. This module adds
//! the minimal mechanism Workstream G actually needs: `breadd` generates a
//! random one-time token when spawning a module-host child, hands it to the
//! child via `$BREAD_MODULE_TOKEN` (an env var, not argv — argv is visible
//! to any process on the system via `/proc/<pid>/cmdline`, env vars are not
//! without `/proc/<pid>/environ` + matching privileges), and the child's
//! first message on the IPC socket (`module_host.hello`) presents that
//! token. `breadd` looks up which module name/manifest/permission set the
//! token was issued for — see [`ModuleHostRegistry::take_pending`] — rather
//! than trusting any name the child process might assert about itself.

use std::collections::HashMap;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use bread_shared::{AdapterSource, BreadEvent, ModulePermission, PermissionKind};
use landlock::{
    make_bitflags, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, ABI,
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info, warn};

/// What `load_module` ultimately learns about a spawn attempt, reported back
/// over IPC (`module_host.hello` consumes the pending entry;
/// `module_host.status` supplies the final verdict) via
/// [`PendingModuleHost::outcome_tx`].
pub enum ModuleHostOutcome {
    Ready,
    LoadError(String),
}

/// What `breadd` knows about a spawned-but-not-yet-authenticated module-host
/// child, keyed by the one-time token it was handed. Consumed exactly once,
/// by whichever connection presents the matching token first (see
/// `ipc::Server`'s `module_host.hello` handling).
pub struct PendingModuleHost {
    pub module_name: String,
    pub permissions: Vec<ModulePermission>,
    pub outcome_tx: std::sync::mpsc::Sender<ModuleHostOutcome>,
}

struct ActiveModuleHost {
    pid: u32,
}

/// Shared handle to the pending-token / active-child bookkeeping, cloned
/// into both the Lua engine thread (which spawns children) and the IPC
/// server (which authenticates them and serves their RPC calls).
#[derive(Clone)]
pub struct ModuleHostRegistry {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    pending: HashMap<String, PendingModuleHost>,
    active: HashMap<String, ActiveModuleHost>,
}

impl Default for ModuleHostRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleHostRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    fn insert_pending(&self, token: String, pending: PendingModuleHost) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .insert(token, pending);
    }

    /// One-time consumption of a pending token, called from the IPC side
    /// when a connection presents it via `module_host.hello`. Returns
    /// `None` for an unknown/already-consumed/expired token — the caller
    /// must not extend any trust to that connection in that case.
    pub fn take_pending(&self, token: &str) -> Option<PendingModuleHost> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(token)
    }

    fn insert_active(&self, name: String, pid: u32) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .insert(name, ActiveModuleHost { pid });
    }

    fn remove_active(&self, name: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(name);
    }

    /// Best-effort SIGTERM of a previously spawned module-host for `name`,
    /// if still tracked as active. Called at the top of
    /// [`spawn_module_host`] so `bread reload`/`modules.reload` respawning
    /// the same module doesn't leak an orphaned duplicate process still
    /// holding an open IPC connection and reacting to events alongside its
    /// replacement. The old process's own reap thread (started when it was
    /// first spawned) will still notice it exit and emit
    /// `bread.module.crashed` for it — a known rough edge documented in
    /// `Documentation.md`: an intentional reload-triggered replacement
    /// currently looks identical, on the wire, to an unexpected crash.
    fn terminate_existing(&self, name: &str) {
        let pid = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.active.get(name).map(|a| a.pid)
        };
        if let Some(pid) = pid {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }

    /// Best-effort SIGTERM of every still-tracked module-host child. Called
    /// from `breadd`'s shutdown path so stopping the daemon doesn't leave
    /// orphaned sandboxed processes holding a now-dead socket connection.
    pub fn shutdown_all(&self) {
        let pids: Vec<u32> = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.active.values().map(|a| a.pid).collect()
        };
        for pid in pids {
            // SAFETY: kill(2) with a pid we just read from our own
            // bookkeeping and a plain termination signal; no memory safety
            // concerns, just an FFI call.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
}

/// How long `load_module` blocks waiting for a freshly spawned module-host
/// to either report ready (`module_host.status{state:"loaded"}`) or fail —
/// mirrors the synchronous "a module either loaded or it didn't" contract
/// `load_scoped_lua_file` already has for in-process modules. 25s rather
/// than something tighter: a real spawn (process fork/exec + Landlock
/// ruleset setup + Lua init) takes well under a second in isolation, but
/// this repo's integration test suite spawns many real `breadd` +
/// `bread-module-host` process pairs concurrently (`cargo test`'s default
/// parallelism), and under that load a spawn occasionally takes several
/// seconds of wall-clock time waiting for CPU/scheduler time rather than
/// being slow on its own merits.
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// Spawn a sandboxed `bread-module-host` child for one third-party module
/// and block (on a `std::sync::mpsc` channel, not an async await — this is
/// called from the Lua engine's own dedicated OS thread, which is not
/// async) until it reports ready or fails to within [`READY_TIMEOUT`].
///
/// `emit_tx` is used once, later, not by this function directly: the
/// crash-detection thread this function spawns uses it to emit
/// `bread.module.crashed` if the child dies after having successfully
/// loaded.
pub fn spawn_module_host(
    registry: &ModuleHostRegistry,
    module_name: &str,
    entry_path: &Path,
    permissions: &[ModulePermission],
    socket_path: &Path,
    emit_tx: &UnboundedSender<BreadEvent>,
) -> Result<ModuleHostOutcome> {
    registry.terminate_existing(module_name);

    let token = uuid::Uuid::new_v4().to_string();
    let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
    registry.insert_pending(
        token.clone(),
        PendingModuleHost {
            module_name: module_name.to_string(),
            permissions: permissions.to_vec(),
            outcome_tx,
        },
    );

    let bin_path = resolve_module_host_binary();

    let mut cmd = Command::new(&bin_path);
    cmd.env("BREAD_MODULE_TOKEN", &token)
        .env("BREAD_MODULE_ENTRY", entry_path)
        .env("BREAD_MODULE_SOCKET", socket_path)
        .env("BREAD_MODULE_NAME", module_name)
        .stdin(std::process::Stdio::null());

    let sandbox_permissions = permissions.to_vec();
    let sandbox_bin_path = bin_path.clone();
    let sandbox_module_name = module_name.to_string();
    let sandbox_entry_path = entry_path.to_path_buf();
    // SAFETY: the closure runs in the forked child between fork() and
    // execve() (that's what pre_exec is for). It only touches its own
    // captured, already-allocated data plus filesystem/landlock syscalls —
    // no allocation-unsafe signal-handler tricks, matching the same
    // pattern the `landlock` crate's own sandboxing examples use for
    // restricting a spawned child.
    unsafe {
        cmd.pre_exec(move || {
            apply_sandbox(&sandbox_bin_path, &sandbox_entry_path, &sandbox_permissions).map_err(|e| {
                std::io::Error::other(format!(
                    "landlock sandbox setup failed for module '{sandbox_module_name}': {e}"
                ))
            })
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            registry.take_pending(&token);
            return Err(anyhow!(
                "failed to spawn bread-module-host at {}: {e}",
                bin_path.display()
            ));
        }
    };

    let pid = child.id();
    registry.insert_active(module_name.to_string(), pid);
    info!(module = %module_name, pid, bin = %bin_path.display(), "spawned bread-module-host");

    // Reap thread: detects the child exiting for ANY reason (clean exit,
    // panic, `kill -9`) without blocking breadd's IPC server or the Lua
    // engine thread — this is the mechanism behind the "crash isolation"
    // acceptance test (P0 item 5): killing this child must not take breadd
    // or any other module down with it, and breadd must notice and report
    // it via `bread.module.crashed`.
    {
        let registry = registry.clone();
        let emit_tx = emit_tx.clone();
        let module_name = module_name.to_string();
        let thread_name = format!("mh-reap-{}", short(&module_name));
        if let Err(e) = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let status = child.wait();
                registry.remove_active(&module_name);
                let (reason, exit_code, signal) = describe_exit(&status);
                warn!(module = %module_name, pid, reason = %reason, "bread-module-host exited");
                let _ = emit_tx.send(BreadEvent::new(
                    "bread.module.crashed",
                    AdapterSource::System,
                    serde_json::json!({
                        "module": module_name,
                        "pid": pid,
                        "reason": reason,
                        "exit_code": exit_code,
                        "signal": signal,
                    }),
                ));
            })
        {
            error!(error = %e, "failed to spawn module-host reap thread");
        }
    }

    match outcome_rx.recv_timeout(READY_TIMEOUT) {
        Ok(outcome) => Ok(outcome),
        Err(_) => {
            registry.take_pending(&token);
            Ok(ModuleHostOutcome::LoadError(format!(
                "module-host for '{module_name}' did not report ready within {:?}",
                READY_TIMEOUT
            )))
        }
    }
}

fn short(name: &str) -> String {
    name.chars().take(12).collect()
}

fn describe_exit(
    status: &std::io::Result<std::process::ExitStatus>,
) -> (String, Option<i32>, Option<i32>) {
    match status {
        Ok(s) => {
            if let Some(code) = s.code() {
                (format!("exited with code {code}"), Some(code), None)
            } else if let Some(sig) = s.signal() {
                (format!("killed by signal {sig}"), None, Some(sig))
            } else {
                ("exited (unknown reason)".to_string(), None, None)
            }
        }
        Err(e) => (format!("wait() failed: {e}"), None, None),
    }
}

/// Resolve the `bread-module-host` binary's path: prefer the sibling of
/// `breadd`'s own executable (the layout `cargo build --workspace` and this
/// repo's packaging both produce — all workspace binaries land in the same
/// `target/{debug,release}` or install bindir), falling back to a bare
/// `PATH` lookup for layouts where `current_exe()` resolution is
/// unreliable.
pub fn resolve_module_host_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("bread-module-host");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("bread-module-host")
}

/// Build and apply the Landlock ruleset for a module-host child, from
/// inside `Command::pre_exec` (i.e. after `fork()`, before `execve()` of
/// `bread-module-host` itself — so the restriction covers that very
/// `execve()` too, which is why the baseline rules below exist at all).
///
/// # The baseline (always granted, not manifest-driven)
///
/// A dynamically linked binary needs to read its own file (to `execve` it)
/// and load the shared libraries `ld.so` maps into it. The initial version
/// of this function assumed Landlock's `Execute` access right gates
/// `execve()`/`execveat()` only, and that granting plain `ReadFile` on the
/// library directories would be enough for the dynamic linker's
/// `mmap(..., PROT_EXEC, ...)` calls on `.so` files. That assumption was
/// **wrong** — verified empirically (not just reasoned about from the
/// kernel docs) by spawning a real sandboxed child: with library
/// directories restricted to `ReadFile`-only, even `/bin/sh -c "true"`
/// fails `execve()` with `EACCES` before running a single line of script;
/// granting `Execute` on those directories too makes it work. So the
/// running kernel's Landlock implementation *does* mediate the executable
/// `mmap` the dynamic linker performs via the same `Execute` right,
/// contrary to what a first reading of "Execute a file" (the kernel doc's
/// one-line description) suggests. The baseline therefore grants:
/// - `ReadFile | ReadDir | Execute` on the system library directories and
///   `ReadFile` on `/etc/ld.so.cache`/`/etc/ld.so.preload` — what the
///   dynamic linker actually needs to start this binary at all.
/// - `ReadFile | Execute` on the `bread-module-host` binary's own resolved
///   path specifically (not a whole directory).
///
/// **Known trade-off, not swept under the rug**: this means a module-host
/// child's direct `os.execute`/`io.open` escape hatch, if it names a path
/// under `/usr/lib`/`/lib` (etc.) directly, is not denied by Landlock the
/// way an arbitrary path elsewhere on the filesystem is — the baseline
/// necessarily grants real `Execute` there, not just enough for the linker.
/// This is a materially smaller exposure than "no sandbox at all" (it's
/// bounded to files already shipped in the system's own library
/// directories, not the whole filesystem, and not anything a manifest
/// didn't otherwise ask for), but it is a real gap worth being honest
/// about — see `Documentation.md`'s "Workstream G" section. A fully static
/// build of `bread-module-host` (e.g. targeting `x86_64-unknown-linux-musl`
/// — confirmed available via `rustup target list --installed` in this
/// repo's dev environment) would remove the need for this baseline
/// entirely, since there'd be no dynamic linker involved at all; that's
/// flagged as follow-up work rather than attempted here, since it's a
/// build/packaging change (cross-compiling mlua's vendored Lua and every
/// transitive dependency against musl, plus a CI/xtask change) bigger than
/// this workstream's remaining time budget affords.
///
/// # The manifest-driven grants
///
/// - `fs.read` with a `path` hint -> `ReadFile | ReadDir` scoped to that
///   (`~`-expanded) path prefix.
/// - `fs.write` with a `path` hint -> the read bits above plus
///   `WriteFile | MakeReg | MakeDir` (matches `bread.fs.write`'s own
///   `create_dir_all` + `write` behavior).
/// - `exec` with a `bin` hint -> `ReadFile | Execute` scoped to that
///   binary's resolved path (absolute paths used as-is; bare names are
///   resolved via a `$PATH` search, `which`-style).
/// - `fs.read`/`fs.write` with **no** `path` hint: the RPC bridge's
///   belt-and-suspenders permission check still applies (see
///   `ipc/mod.rs`), but no Landlock rule is added, since Landlock scoping
///   needs a concrete path. A module author who wants the direct
///   `os`/`io` escape hatch mediated at the kernel level too needs to
///   declare a `path` — documented as a known sharp edge in
///   `Documentation.md` rather than silently "fixed" by granting
///   filesystem-wide access.
/// - Every other `PermissionKind` (`state.*`, `notify`, `machine`,
///   `hyprland`, `widget`, `bluetooth`, `profile.activate`) is RPC-gated
///   only (see `ipc/mod.rs`) — they have no filesystem shape to hand
///   Landlock in the first place.
fn apply_sandbox(
    module_host_bin: &Path,
    entry_path: &Path,
    permissions: &[ModulePermission],
) -> Result<()> {
    let abi = ABI::V1;
    let lib_dir_access = make_bitflags!(AccessFs::{ReadFile | ReadDir | Execute});
    let read_file_only = make_bitflags!(AccessFs::{ReadFile});
    let read_only = make_bitflags!(AccessFs::{ReadFile | ReadDir});
    let read_and_exec = make_bitflags!(AccessFs::{ReadFile | Execute});
    let read_and_write =
        make_bitflags!(AccessFs::{ReadFile | ReadDir | WriteFile | MakeReg | MakeDir});

    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| anyhow!("landlock handle_access: {e}"))?
        .create()
        .map_err(|e| anyhow!("landlock ruleset create: {e}"))?;

    for dir in ["/usr/lib", "/usr/lib64", "/lib", "/lib64"] {
        let p = Path::new(dir);
        if p.exists() {
            if let Ok(fd) = PathFd::new(p) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, lib_dir_access))
                    .map_err(|e| anyhow!("landlock rule for {dir}: {e}"))?;
            }
        }
    }
    for f in ["/etc/ld.so.cache", "/etc/ld.so.preload"] {
        let p = Path::new(f);
        if p.exists() {
            if let Ok(fd) = PathFd::new(p) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, read_file_only))
                    .map_err(|e| anyhow!("landlock rule for {f}: {e}"))?;
            }
        }
    }
    if let Ok(fd) = PathFd::new(module_host_bin) {
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, read_and_exec))
            .map_err(|e| anyhow!("landlock rule for module-host binary: {e}"))?;
    }
    // The module-host bootstrap process needs to read its OWN module's
    // directory (init.lua, bread.module.toml, an optional lib/ subtree —
    // the same directory shape load_scoped_lua_file's in-process
    // counterpart reads from) to load any Lua at all, entirely separate
    // from whatever `fs.read` the manifest grants for the module's own
    // runtime file I/O. Without this rule, EVERY out-of-process module
    // fails to load — including ones with no `fs.read` permission at
    // all — since it can't even read its own entry file.
    if let Some(module_dir) = entry_path.parent() {
        if let Ok(fd) = PathFd::new(module_dir) {
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, read_only))
                .map_err(|e| {
                    anyhow!("landlock rule for module directory {}: {e}", module_dir.display())
                })?;
        }
    }

    for perm in permissions {
        match perm.kind {
            PermissionKind::FsRead => {
                if let Some(path) = &perm.path {
                    let expanded = bread_shared::expand_path(path);
                    if let Ok(fd) = PathFd::new(&expanded) {
                        ruleset = ruleset
                            .add_rule(PathBeneath::new(fd, read_only))
                            .map_err(|e| {
                                anyhow!("landlock fs.read rule for {}: {e}", expanded.display())
                            })?;
                    }
                }
            }
            PermissionKind::FsWrite => {
                if let Some(path) = &perm.path {
                    let expanded = bread_shared::expand_path(path);
                    if let Ok(fd) = PathFd::new(&expanded) {
                        ruleset = ruleset
                            .add_rule(PathBeneath::new(fd, read_and_write))
                            .map_err(|e| {
                                anyhow!("landlock fs.write rule for {}: {e}", expanded.display())
                            })?;
                    }
                }
            }
            PermissionKind::Exec => {
                if let Some(bin) = &perm.bin {
                    if let Some(resolved) = resolve_bin_path(bin) {
                        if let Ok(fd) = PathFd::new(&resolved) {
                            ruleset = ruleset
                                .add_rule(PathBeneath::new(fd, read_and_exec))
                                .map_err(|e| {
                                    anyhow!("landlock exec rule for {}: {e}", resolved.display())
                                })?;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let status = ruleset
        .restrict_self()
        .map_err(|e| anyhow!("landlock restrict_self: {e}"))?;
    if !matches!(status.ruleset, RulesetStatus::FullyEnforced) {
        // Not fatal: PartiallyEnforced still means real kernel enforcement
        // for whatever subset the running kernel/LSM stack supports (see
        // this module's doc comment — verified directly against this
        // repo's dev kernel, which reports PartiallyEnforced yet still
        // denies out-of-scope reads). NotEnforced (pre-5.13 kernel, or
        // Landlock compiled out) would mean this module is running fully
        // unsandboxed — loud enough to want in the log, not loud enough to
        // refuse to start the module entirely and regress availability.
        eprintln!(
            "bread-module-host: landlock ruleset status = {:?} (not fully enforced on this kernel)",
            status.ruleset
        );
    }
    Ok(())
}

/// `which`-style resolution for an `exec` permission's `bin` hint: absolute
/// paths are used as-is, bare names are searched on `$PATH`.
fn resolve_bin_path(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The single most important test in this whole workstream (see the
    /// task's P0 item 4 and `Documentation.md`'s "Workstream G" section):
    /// a real spawned child, restricted only by `apply_sandbox` for a
    /// module granted `fs.read` on exactly one directory, must be denied
    /// by the *kernel* — not a Lua-level check — when it tries to read a
    /// file outside that directory. This talks to `apply_sandbox` and
    /// `Command::pre_exec` exactly the way `spawn_module_host` does; the
    /// full end-to-end version (going through the real IPC handshake and
    /// an actual `os.execute`/`io.open` call from inside Lua) lives in
    /// `breadd/tests/module_host_sandbox.rs`.
    #[test]
    fn landlock_denies_reads_outside_granted_path() {
        let allowed_dir = tempfile::tempdir().unwrap();
        let allowed_file = allowed_dir.path().join("allowed.txt");
        std::fs::write(&allowed_file, b"ok").unwrap();

        let denied_dir = tempfile::tempdir().unwrap();
        let denied_file = denied_dir.path().join("secret.txt");
        std::fs::write(&denied_file, b"nope").unwrap();

        // Mirrors the task's own acceptance scenario verbatim:
        // `os.execute("cat /etc/shadow")` from inside a module granted
        // `fs.read` for exactly one other directory. `cat` does a plain
        // `open()`+`read()` — no shell builtin involved — which is both
        // the most faithful stand-in for the direct `os`/`io` escape hatch
        // and (empirically, see the note on `no_exec_permission_...` below)
        // avoids a bash `read`-builtin quirk that turned out to need more
        // than a `ReadFile` grant for reasons unrelated to what this test
        // is actually checking.
        let cat_bin = resolve_bin_path("cat").expect("cat not found on $PATH");
        let permissions = vec![
            ModulePermission {
                kind: PermissionKind::FsRead,
                path: Some(allowed_dir.path().to_string_lossy().to_string()),
                bin: None,
            },
            ModulePermission {
                kind: PermissionKind::Exec,
                path: None,
                bin: Some(cat_bin.to_string_lossy().to_string()),
            },
        ];

        let sh_bin = which_sh();
        let mut cmd = Command::new(&sh_bin);
        cmd.arg("-c").arg(format!(
            "{cat} {allowed} && echo ALLOWED_OK; {cat} {denied} && echo DENIED_UNEXPECTEDLY_OK",
            cat = cat_bin.display(),
            allowed = allowed_file.display(),
            denied = denied_file.display(),
        ));
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let sandbox_bin = sh_bin.clone();
        unsafe {
            cmd.pre_exec(move || {
                apply_sandbox(&sandbox_bin, Path::new("/nonexistent/dummy/entry.lua"), &permissions)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            });
        }

        let output = cmd.output().expect("failed to run sandboxed sh");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            stdout.contains("ALLOWED_OK"),
            "expected the granted directory to remain readable; stdout={stdout} stderr={stderr}"
        );
        assert!(
            !stdout.contains("DENIED_UNEXPECTEDLY_OK"),
            "sandboxed process read a file OUTSIDE its granted fs.read path — Landlock did not enforce; stdout={stdout} stderr={stderr}"
        );
        // The kernel denial surfaces as `cat`'s own "Permission denied"
        // (EACCES from open()), on stderr — confirming this was an OS-level
        // denial, not e.g. the file simply not existing.
        assert!(
            stderr.to_lowercase().contains("permission denied"),
            "expected a kernel permission-denied error for the out-of-scope read; stderr={stderr}"
        );
    }

    #[test]
    fn no_exec_permission_means_binary_cannot_be_executed_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("run.sh");
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            writeln!(f, "#!/bin/sh\necho SHOULD_NOT_RUN").unwrap();
        }
        std::fs::set_permissions(
            &script_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        // No permissions granted at all: the sandboxed process should not
        // be able to execute ANYTHING, including a script sitting right
        // next to files it might otherwise be able to read.
        let permissions: Vec<ModulePermission> = vec![];

        let sh_bin = which_sh();
        let mut cmd = Command::new(&sh_bin);
        cmd.arg("-c")
            .arg(format!("{} && echo RAN", script_path.display()));
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let sandbox_bin = sh_bin.clone();
        unsafe {
            cmd.pre_exec(move || {
                apply_sandbox(&sandbox_bin, Path::new("/nonexistent/dummy/entry.lua"), &permissions)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            });
        }

        let output = cmd.output().expect("failed to run sandboxed sh");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("RAN"),
            "sandboxed process executed a script with no `exec` permission granted; stdout={stdout}"
        );
    }

    fn which_sh() -> PathBuf {
        for candidate in ["/bin/sh", "/usr/bin/sh"] {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return p;
            }
        }
        panic!("no /bin/sh or /usr/bin/sh found — cannot run sandbox tests");
    }
}
