# Bread — Sync & Module System Implementation Spec
### Instructions for Claude Code

This document defines exactly what to build, how it must behave, and what conditions must be met before iteration stops. Read it fully before writing any code. Do not stop iterating until every condition in the **Completion Checklist** at the bottom is met.

---

## Context

Bread is a reactive desktop automation daemon for Linux. The existing codebase is a Rust workspace with three crates:

- `breadd/` — the runtime daemon (Rust + Lua via mlua)
- `bread-cli/` — the CLI binary (Rust, talks to daemon over Unix socket IPC)
- `bread-shared/` — shared types (`BreadEvent`, `RawEvent`, `AdapterSource`)

The daemon exposes a Unix socket at `$XDG_RUNTIME_DIR/bread/breadd.sock`. The IPC protocol is newline-delimited JSON request/response. The Lua runtime runs on a dedicated OS thread. All existing code compiles and tests pass — do not break anything that currently works.

The two things being added in this iteration:

1. **Module system** — install, list, remove, and update Lua modules from GitHub URLs
2. **Sync** — snapshot and restore system state (Bread config + arbitrary config files + package manifests) via a Git remote

---

## Part 1: Module System

### What a module is

A Bread module is a directory (or single `.lua` file) that gets installed into `~/.config/bread/modules/`. Modules are already loaded by the daemon — what's missing is the install/manage layer.

A module directory looks like:

```
~/.config/bread/modules/
└── wifi/
    ├── bread.module.toml    ← module manifest (required)
    ├── init.lua             ← entry point (required)
    └── lib/                 ← optional support files
```

### Module manifest (`bread.module.toml`)

Every installed module must have a manifest:

```toml
name = "wifi"
version = "1.0.0"
description = "WiFi management for Bread"
author = "someuser"
source = "github:someuser/bread-wifi"      # where it was installed from
installed_at = "2026-05-11T09:00:00Z"     # RFC 3339 timestamp, set on install
```

All fields are required. `source` is the original install source string. `installed_at` is written by Bread at install time, not by the module author.

### Install sources

The module installer must support these source formats:

```
github:user/repo                  # installs default branch
github:user/repo@v1.2.0           # installs specific tag
github:user/repo@abc1234          # installs specific commit
/path/to/local/dir                # installs from local directory (copies it)
```

Anything else is an error with a clear message.

### New Cargo dependencies allowed

Add to `bread-cli/Cargo.toml` as needed:
- `git2 = "0.18"` for Git operations
- `reqwest = { version = "0.11", features = ["blocking", "json"] }` for GitHub API
- `flate2`, `tar` for archive extraction

Add to `breadd/Cargo.toml` as needed:
- `git2 = "0.18"`
- `toml = "0.8"` (already present)

### CLI commands to implement

All module commands live under `bread modules`:

```
bread modules install <source>     Install a module
bread modules remove <name>        Remove an installed module
bread modules list                 List installed modules with name, version, status
bread modules update               Update all installed modules to latest
bread modules update <name>        Update a specific module
bread modules info <name>          Show full manifest details for a module
```

**`bread modules install <source>`**

1. Parse the source string.
2. For `github:user/repo[@ref]`:
   - Use the GitHub API to resolve the ref (or default branch if none specified).
   - Download the repository archive as a `.tar.gz`.
   - Extract to a temp directory.
   - Verify a `bread.module.toml` exists at the root. If not, error cleanly.
   - Copy the module directory to `~/.config/bread/modules/<name>/`.
   - Write `installed_at` into the manifest.
3. For local paths:
   - Verify the path exists and contains `bread.module.toml`.
   - Copy to `~/.config/bread/modules/<name>/`.
   - Write `installed_at`.
4. Print `installed <name> v<version>` on success.
5. Tell the daemon to reload via IPC (`modules.reload`) after install.

**`bread modules remove <name>`**

1. Find `~/.config/bread/modules/<name>/`.
2. Ask for confirmation: `remove <name>? (y/n)`. Skip if `--yes` flag is passed.
3. Delete the directory.
4. Tell the daemon to reload via IPC.
5. Print `removed <name>`.

**`bread modules list`**

Scan `~/.config/bread/modules/` for directories containing `bread.module.toml`. For each, print:

```
  wifi          1.0.0    loaded    github:someuser/bread-wifi
  redox         0.3.1    loaded    github:breadway/bread-redox
  broken-mod    0.1.0    error     /home/user/local-module
```

Status (`loaded`, `error`, `not_found`, `degraded`) comes from the daemon's IPC `modules.list` response, matched by module name. If the daemon is unreachable, show `unknown` for status.

**`bread modules update [name]`**

1. Read `bread.module.toml` for each module to update.
2. If `source` starts with `github:`, re-run the install for that source.
3. If `source` is a local path, error with `cannot update local module — reinstall manually`.
4. Print `updated <name> v<old> → v<new>` or `<name> already up to date`.

**`bread modules info <name>`**

Print full manifest contents plus daemon-reported status. Example:

```
name:         wifi
version:      1.0.0
description:  WiFi management for Bread
author:       someuser
source:       github:someuser/bread-wifi
installed_at: 2026-05-11T09:00:00Z
status:       loaded
```

### Daemon-side: expose `ID_VENDOR_ID` and `ID_MODEL_ID` in udev events

In `breadd/src/adapters/udev.rs`, the `run_udev_monitor` function builds the payload for each udev event. Add `vendor_id` and `product_id` to the payload:

```rust
"vendor_id": prop_str(&event, "ID_VENDOR_ID"),
"product_id": prop_str(&event, "ID_MODEL_ID"),
```

These are the raw hex USB IDs (e.g. `"4d44"` and `"5244"`). Do the same in `raw_change_event` for the fallback poller — read them from sysfs at `<syspath>/idVendor` and `<syspath>/idProduct` if available. Also add `vendor_id` and `product_id` to the `Device` struct in `breadd/src/core/types.rs` as `Option<String>`.

---

## Part 2: Sync System

### Overview

Sync saves and restores a complete description of the user's environment. It is not a disk image. It saves:

1. **Bread config** — everything in `~/.config/bread/` (always included)
2. **Delegated configs** — other config directories the user explicitly opts in (e.g. `~/.config/nvim/`)
3. **Package manifest** — lists of explicitly-installed packages per package manager
4. **Machine profile** — machine name and tags for machine-aware config

Everything is stored in a Git repository. `bread sync push` commits and pushes. `bread sync pull` pulls and applies.

### New crate: `bread-sync`

Create a new crate `bread-sync/` in the workspace. Add it to `[workspace.members]` in the root `Cargo.toml`.

```
bread-sync/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── config.rs      ← SyncConfig type, load/save
    ├── git.rs         ← Git operations via git2
    ├── packages.rs    ← Package manifest generation
    ├── delegates.rs   ← Config file delegation
    └── machine.rs     ← Machine profile
```

`bread-cli` depends on `bread-sync`. `breadd` does not — sync is a CLI-only feature.

### Sync configuration (`~/.config/bread/sync.toml`)

This file is created by `bread sync init` and edited by the user. It is committed to the sync repo.

```toml
[remote]
url = "git@github.com:user/bread-sync.git"    # required, set by bread sync init
branch = "main"                                # default: "main"

[machine]
name = "laptop"                                # required, set by bread sync init
tags = ["mobile", "battery", "single-monitor"] # user-defined, optional

[packages]
enabled = true
managers = ["pacman", "pip", "npm"]           # which package managers to snapshot

[delegates]
# Additional config directories to include in sync.
# ~/.config/bread/ is always included and does not need to be listed here.
include = [
    "~/.config/nvim",
    "~/.config/fish",
    "~/.config/kitty",
]
exclude = [
    "**/.git",
    "**/node_modules",
    "**/__pycache__",
    "**/*.log",
    "**/*.cache",
    "~/.config/nvim/.repro",
]
```

All paths support `~` expansion. Globs in `exclude` use standard glob syntax.

### Sync repo layout

The Git repository managed by Bread has this structure:

```
<sync-repo>/
├── bread/                      ← copy of ~/.config/bread/ (minus sync.toml secrets if any)
├── configs/
│   ├── nvim/                   ← copy of ~/.config/nvim/
│   ├── fish/                   ← copy of ~/.config/fish/
│   └── kitty/                  ← copy of ~/.config/kitty/
├── packages/
│   ├── pacman.txt              ← output of `pacman -Qe`
│   ├── pip.txt                 ← output of `pip list --user --format=freeze`
│   └── npm.txt                 ← output of `npm list -g --depth=0`
├── machines/
│   └── laptop.toml             ← machine profile for this machine
└── .bread-sync                 ← sync metadata (not committed to Git)
```

`machines/<name>.toml` contains:

```toml
name = "laptop"
hostname = "breadway-laptop"    # auto-detected via gethostname
tags = ["mobile", "battery", "single-monitor"]
last_sync = "2026-05-11T09:15:00Z"
```

### CLI commands to implement

All sync commands live under `bread sync`:

```
bread sync init [--remote <url>]   Initialize sync for this machine
bread sync push [--message <msg>]  Snapshot and push current state
bread sync pull                    Pull and apply latest state
bread sync status                  Show what has changed since last push
bread sync diff                    Show file-level diff vs remote
bread sync machines                List known machines from sync repo
```

**`bread sync init [--remote <url>]`**

1. Check if `~/.config/bread/sync.toml` already exists. If so, error: `sync already initialized. Edit ~/.config/bread/sync.toml to reconfigure.`
2. If `--remote` is not provided, prompt: `Sync remote URL (git remote or path): `.
3. Prompt: `Machine name [laptop]: ` (default: hostname).
4. Prompt: `Machine tags (comma-separated, e.g. mobile,battery): `.
5. Create `~/.config/bread/sync.toml` with the provided values.
6. If the remote is a URL (not a local path), check if the repo exists:
   - If it exists, clone it to a temp location and verify it looks like a Bread sync repo (has a `bread/` directory or is empty).
   - If it doesn't exist, print: `remote does not exist yet — it will be created on first push`.
7. Print setup summary.

**`bread sync push [--message <msg>]`**

1. Load `~/.config/bread/sync.toml`. Error if not initialized.
2. Resolve the local sync repo path (`~/.local/share/bread/sync-repo/`). Clone from remote if it doesn't exist locally.
3. Snapshot each section:
   - Copy `~/.config/bread/` → `<repo>/bread/` (rsync-style: delete files in dest that don't exist in source)
   - For each path in `delegates.include`: copy to `<repo>/configs/<basename>/`
   - If `packages.enabled`: run package manager queries and write to `<repo>/packages/`
   - Write `<repo>/machines/<name>.toml`
4. Stage all changes (`git add -A`).
5. If there are no changes, print `nothing to push — already up to date` and exit.
6. Commit with message: `sync: <machine-name> <timestamp>` or the user-provided `--message`.
7. Push to remote.
8. Print a summary of what was snapshotted.

**`bread sync pull`**

1. Load `~/.config/bread/sync.toml`. Error if not initialized.
2. Pull from remote (fetch + merge or rebase — use merge, simpler).
3. Apply each section in order:
   - Copy `<repo>/bread/` → `~/.config/bread/` (same rsync-style)
   - For each path in `delegates.include` that exists in `<repo>/configs/`: copy back
   - If `packages.enabled` and `--install-packages` flag is passed: run package installs (see below)
4. Tell the daemon to reload via IPC (`modules.reload`) after applying.
5. Print a summary of what was applied.

**Package install on pull** (only when `--install-packages` is explicitly passed):

- `pacman.txt` → `sudo pacman -S --needed $(cat pacman.txt | awk '{print $1}')`
- `pip.txt` → `pip install --user -r pip.txt`
- `npm.txt` → parse package names and run `npm install -g`

Never run package installs automatically without the flag. Print a note at the end of `pull` if packages differ: `run 'bread sync pull --install-packages' to install missing packages`.

**`bread sync status`**

1. Load sync config and local repo.
2. Pull remote refs without merging (fetch only).
3. Compare working tree to last commit and compare last commit to remote HEAD.
4. Print:

```
bread sync status
  machine      laptop
  remote       git@github.com:user/bread-sync.git
  last push    2026-05-11 09:15:00

local changes (not yet pushed):
  M  bread/init.lua
  A  bread/modules/wifi/init.lua

remote changes (not yet pulled):
  none
```

**`bread sync diff`**

Run `git diff HEAD` in the sync repo and print it. If `--remote` flag is passed, run `git diff HEAD..origin/<branch>`.

**`bread sync machines`**

List all `machines/*.toml` files from the sync repo:

```
  laptop     last sync: 2026-05-11 09:15   tags: mobile, battery, single-monitor
  desktop    last sync: 2026-05-10 22:00   tags: stationary, multi-monitor, docked
```

### Package manager support

Implement these four. Each must handle the case where the package manager is not installed (skip with a warning, don't error).

| Manager | Snapshot command | Install command |
|---------|-----------------|-----------------|
| `pacman` | `pacman -Qe` | `sudo pacman -S --needed <pkg>` |
| `pip` | `pip list --user --format=freeze` | `pip install --user -r <file>` |
| `npm` | `npm list -g --depth=0 --parseable` | `npm install -g <pkg>` |
| `cargo` | `cargo install --list` | `cargo install <pkg>` |

For `cargo`, the snapshot format is one package per line: `<name> <version>`. Parse `cargo install --list` output accordingly.

### Git operations

Use the `git2` crate for all Git operations. Do not shell out to `git`. Required operations:

- Clone a remote repo
- Open an existing repo
- Stage all changes (`add -A` equivalent: index all tracked and untracked files)
- Create a commit with a message and the current timestamp as author date
- Push to remote (support SSH and HTTPS — `git2` handles this via callbacks)
- Pull (fetch + merge fast-forward; if non-fast-forward, error with clear message)
- Fetch (without merging)
- Get diff between working tree and HEAD
- Get diff between HEAD and remote branch HEAD

For SSH auth, use the user's default SSH agent (`git2::transport::smart::SmartSubtransport` with `SshKey` credential). For HTTPS, use the system credential store or prompt for credentials.

---

## Part 3: Daemon additions (IPC)

Add these IPC methods to `breadd/src/ipc/mod.rs`:

**`sync.status`** — returns current sync state from `sync.toml` if it exists:
```json
{ "initialized": true, "machine": "laptop", "remote": "git@github.com:..." }
```
or `{ "initialized": false }` if no sync.toml.

**`modules.install`** — triggers a reload after external install (already covered by `modules.reload`, no new method needed — `bread modules install` calls `modules.reload` via IPC after installing).

No other daemon changes are needed for sync — it is entirely CLI-side.

---

## Part 4: Lua API additions

Add to `breadd/src/lua/mod.rs` in `install_api`:

**`bread.machine`** table:

```lua
bread.machine.name()          -- returns machine name from sync.toml, or hostname if no sync.toml
bread.machine.tags()          -- returns array of tags, or empty array
bread.machine.has_tag("mobile")  -- returns bool
```

Read `~/.config/bread/sync.toml` directly from Lua (parse it in Rust, expose via the API). If `sync.toml` doesn't exist, `name()` returns `os.getenv("HOSTNAME")` and `tags()` returns `{}`.

**`bread.fs`** table:

```lua
bread.fs.write(path, content)   -- write string to file, create dirs as needed
bread.fs.read(path)             -- read file to string, returns nil if not found
bread.fs.exists(path)           -- returns bool
bread.fs.expand(path)           -- expand ~ to home directory
```

All paths support `~` expansion. `bread.fs.write` creates parent directories automatically. Errors in `write` propagate as Lua errors.

---

## Error handling requirements

Every command must handle these cases cleanly:

- Daemon not running: print `bread: daemon is not running. Start it with: systemctl --user start breadd` and exit 1.
- No sync.toml: print `bread: sync not initialized. Run: bread sync init` and exit 1.
- Network unreachable during push/pull: print the error clearly and exit 1. Do not leave the repo in a partial state.
- Module not found during remove/info: print `bread: module '<name>' is not installed` and exit 1.
- Git conflicts on pull: print `bread: sync conflict — resolve manually in ~/.local/share/bread/sync-repo/` and exit 1. Do not auto-merge or discard changes.
- Package manager not installed: warn and skip, do not fail the whole operation.

---

## File locations

| Purpose | Path |
|---------|------|
| Sync config | `~/.config/bread/sync.toml` |
| Local sync repo | `~/.local/share/bread/sync-repo/` |
| Module manifests | `~/.config/bread/modules/<name>/bread.module.toml` |
| Bread config | `~/.config/bread/` |
| Daemon socket | `$XDG_RUNTIME_DIR/bread/breadd.sock` |

All paths must use `dirs` crate or manual `$HOME`/`$XDG_*` expansion — never hardcode `/home/breadway` or any username.

Add to `bread-cli/Cargo.toml`: `dirs = "5.0"`.

---

## Tests

### Module system tests (`bread-cli/tests/modules.rs`)

```rust
// 1. Install from local path succeeds when bread.module.toml exists
// 2. Install from local path fails when bread.module.toml is missing
// 3. Remove deletes the module directory
// 4. List reads manifests correctly from disk
// 5. Manifest is written correctly on install (all fields present, installed_at is valid RFC 3339)
```

### Sync tests (`bread-sync/tests/sync.rs`)

```rust
// 1. bread sync init creates sync.toml with correct fields
// 2. bread sync push with a local bare Git repo as remote: creates correct directory structure
// 3. bread sync push snapshots bread/ directory correctly
// 4. bread sync pull copies files from repo to correct locations
// 5. Package manifest for pacman: parses output correctly
// 6. Package manifest for pip: parses output correctly
// 7. Delegates: exclude globs filter correctly
// 8. Machine profile is written to machines/<name>.toml with correct fields
// 9. Status shows no changes when working tree matches last commit
// 10. Push with no changes prints "nothing to push" and does not create a commit
```

All tests must pass with `cargo test --workspace`. Tests that require network access must be feature-gated with `#[cfg(feature = "network-tests")]` and not run by default.

---

## Completion Checklist

Do not stop iterating until every item on this list is true.

### Compilation
- [ ] `cargo build --workspace` succeeds with zero errors
- [ ] `cargo build --workspace --release` succeeds with zero errors
- [ ] Zero compiler warnings in new code (existing warnings are acceptable)
- [ ] `cargo clippy --workspace` produces no errors in new code

### Tests
- [ ] `cargo test --workspace` passes with zero failures
- [ ] All tests listed in the Tests section above exist and pass
- [ ] Integration tests in `breadd/tests/ipc_integration.rs` still pass

### Module system — functional
- [ ] `bread modules install github:user/repo` downloads and installs a module
- [ ] `bread modules install /local/path` copies and installs a local module
- [ ] `bread modules install` with an invalid source prints a clear error and exits 1
- [ ] `bread modules install` writes a valid `bread.module.toml` with all required fields including `installed_at`
- [ ] `bread modules install` calls `modules.reload` IPC after successful install
- [ ] `bread modules remove <name>` removes the module directory
- [ ] `bread modules remove <name>` with `--yes` skips confirmation
- [ ] `bread modules remove <nonexistent>` prints a clear error and exits 1
- [ ] `bread modules list` reads all installed module manifests
- [ ] `bread modules list` shows daemon-reported status when daemon is running
- [ ] `bread modules list` shows `unknown` status when daemon is not running (no crash)
- [ ] `bread modules update` re-installs all github-sourced modules
- [ ] `bread modules update` skips local-path modules with a warning
- [ ] `bread modules info <name>` shows all manifest fields and daemon status

### Sync — functional
- [ ] `bread sync init` creates `~/.config/bread/sync.toml` with all required fields
- [ ] `bread sync init` errors if already initialized
- [ ] `bread sync push` creates the correct repo directory structure
- [ ] `bread sync push` copies `~/.config/bread/` to `bread/` in the repo
- [ ] `bread sync push` copies each delegate path to `configs/<basename>/`
- [ ] `bread sync push` writes package manifests to `packages/`
- [ ] `bread sync push` writes `machines/<name>.toml`
- [ ] `bread sync push` creates a Git commit with a sensible message
- [ ] `bread sync push` pushes to the configured remote
- [ ] `bread sync push` with no changes prints `nothing to push` and exits 0
- [ ] `bread sync pull` copies `bread/` from repo to `~/.config/bread/`
- [ ] `bread sync pull` copies `configs/` entries back to their original locations
- [ ] `bread sync pull` calls `modules.reload` IPC after applying
- [ ] `bread sync pull --install-packages` runs package installs
- [ ] `bread sync pull` without `--install-packages` does not run package installs
- [ ] `bread sync status` shows local uncommitted changes
- [ ] `bread sync status` shows remote changes not yet pulled
- [ ] `bread sync status` prints `nothing to push — already up to date` when clean
- [ ] `bread sync machines` lists all `machines/*.toml` entries
- [ ] `bread sync init` without `--remote` prompts for URL interactively

### Sync — error handling
- [ ] `bread sync push` without init prints clear error and exits 1
- [ ] `bread sync pull` without init prints clear error and exits 1
- [ ] Git conflict on pull prints clear message pointing to sync repo path and exits 1
- [ ] Package manager not installed is warned and skipped, not a fatal error

### Lua API
- [ ] `bread.machine.name()` returns machine name from sync.toml
- [ ] `bread.machine.name()` returns hostname when sync.toml does not exist
- [ ] `bread.machine.tags()` returns array of tags
- [ ] `bread.machine.has_tag("x")` returns true/false correctly
- [ ] `bread.fs.write(path, content)` writes the file and creates parent dirs
- [ ] `bread.fs.read(path)` returns file content as string
- [ ] `bread.fs.read(nonexistent)` returns nil, does not error
- [ ] `bread.fs.exists(path)` returns correct bool
- [ ] `bread.fs.expand("~/foo")` returns the correct absolute path
- [ ] All `bread.fs` paths handle `~` expansion

### Udev vendor/product ID
- [ ] `vendor_id` and `product_id` fields are present in udev device events
- [ ] `Device` struct in `types.rs` has `vendor_id: Option<String>` and `product_id: Option<String>`
- [ ] `bread events` output shows `vendor_id` and `product_id` when available

### No regressions
- [ ] `bread reload` still works
- [ ] `bread state` still works
- [ ] `bread events` still works
- [ ] `bread doctor` still works
- [ ] `bread ping` still works
- [ ] `bread emit` still works
- [ ] Daemon starts cleanly with no existing `sync.toml`
- [ ] Daemon starts cleanly with a valid `sync.toml`
- [ ] All existing IPC methods still respond correctly

### Code quality
- [ ] No hardcoded paths containing usernames or `/home/<anything>`
- [ ] No `unwrap()` calls in new code that can fail at runtime — use `?` or explicit error handling
- [ ] No `expect("...")` calls in new async code — only in tests and truly-impossible cases
- [ ] All new public functions have doc comments
- [ ] `bread-sync` crate has a `README.md` explaining its purpose and public API

---

## Implementation order

Work in this order. Do not move to the next step until the current one compiles and its tests pass.

1. Add `bread-sync` crate skeleton to workspace (compiles, no logic yet)
2. Implement `SyncConfig` (load/save `sync.toml`)
3. Implement `bread sync init`
4. Implement Git backend in `bread-sync/src/git.rs`
5. Implement `bread sync push` (bread config only, no delegates or packages yet)
6. Implement delegate file handling
7. Implement package manifest generation
8. Implement `bread sync pull`
9. Implement `bread sync status`, `diff`, `machines`
10. Implement `bread modules install` (local path first, then GitHub)
11. Implement `bread modules remove`, `list`, `update`, `info`
12. Add `vendor_id`/`product_id` to udev adapter and `Device` type
13. Add `bread.machine` Lua API
14. Add `bread.fs` Lua API
15. Write all tests
16. Run full checklist — fix anything not passing
17. Run `cargo clippy --workspace` — fix any new warnings
