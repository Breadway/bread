//! Shared helpers for breadd's integration test binaries
//! (`ipc_integration.rs`, `module_host_sandbox.rs`). Lives under
//! `tests/common/mod.rs`, not `tests/common.rs`, specifically so cargo does
//! not also compile it as a standalone test binary of its own (a bare
//! `tests/common.rs` would run as an (empty) test target every `cargo test`
//! invocation).

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

/// The env var callers should set, to [`ensure_module_host_built`]'s
/// returned path, on every `breadd` `Command` they spawn:
/// `breadd::module_host::resolve_module_host_binary` (its own
/// `MODULE_HOST_BIN_ENV` constant) checks this first, ahead of its normal
/// sibling-of-`current_exe()` resolution, so a test always spawns the exact
/// binary this helper just built. Must match that constant's literal value;
/// there's no shared symbol for it since `breadd` is a binary-only crate
/// with no `[lib]` target these integration tests can import from. Exported
/// so both test binaries reference the same literal instead of each
/// hardcoding their own copy.
pub const BREAD_MODULE_HOST_BIN_ENV: &str = "BREAD_MODULE_HOST_BIN";

/// Build `bread-module-host` and return the exact path cargo produced for
/// it, memoized for the lifetime of this test binary process (each
/// integration test file is its own process, so this only runs once per
/// `cargo test` invocation per file, not once per `#[tokio::test]`).
///
/// `bread-module-host` is a bin-only crate (no `[lib]` target, deliberately:
/// see its `Cargo.toml`), so it can never be forced into existence the way
/// `breadd`'s own binary is, via an `env!("CARGO_BIN_EXE_breadd")`
/// reference — that macro requires the crate to be a `[dev-dependencies]`
/// path entry, and Cargo's artifact-dependency feature that would otherwise
/// cover a bin-only crate (`-Z bindeps`) is still nightly-only. So this
/// crate has to be built explicitly, from inside the test process, before
/// anything tries to spawn it.
///
/// This used to guess `target/{debug,release}/bread-module-host` next to
/// wherever the calling test binary itself lived, relying on `cargo test
/// --workspace` having already built every workspace member first (true in
/// this repo's own CI, but not guaranteed, and not remotely true under
/// `cargo test --release`, a custom `--target-dir`, or a cross-compiling
/// `--target`, where the guessed path is simply wrong). When the guess
/// missed, `resolve_module_host_binary`'s fallback did a bare `PATH`
/// lookup, which can silently succeed against a stale, separately
/// **installed** `~/.local/bin/bread-module-host` instead of the build this
/// test run actually means to exercise — a test passing against the wrong
/// binary is worse than one that fails loudly.
///
/// Reading the path back out of `cargo build --message-format=json`'s own
/// compiler-artifact message avoids all of that: it's the literal path
/// cargo itself just used, correct regardless of profile, a custom
/// `CARGO_TARGET_DIR`, or `CARGO_BUILD_TARGET` cross-compilation, since
/// those are ordinary environment variables cargo already honours on its
/// own and this nested `Command` inherits from this process unless cleared.
/// The caller is expected to pass the returned path to the `breadd` under
/// test via [`BREAD_MODULE_HOST_BIN_ENV`], so resolution never falls back
/// to guessing or `PATH` at all.
pub fn ensure_module_host_built() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(build_module_host).clone()
}

fn build_module_host() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args([
            "build",
            "-p",
            "bread-module-host",
            "--locked",
            "--message-format=json",
        ])
        .output()
        .expect("failed to run `cargo build -p bread-module-host`");
    assert!(
        output.status.success(),
        "cargo build -p bread-module-host failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut executable = None;
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let is_module_host = msg
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(Value::as_str)
            == Some("bread-module-host");
        if !is_module_host {
            continue;
        }
        if let Some(exe) = msg.get("executable").and_then(Value::as_str) {
            executable = Some(PathBuf::from(exe));
        }
    }

    executable.unwrap_or_else(|| {
        panic!(
            "cargo build -p bread-module-host --message-format=json produced no \
             compiler-artifact message with an `executable` field; stdout was:\n{stdout}"
        )
    })
}
