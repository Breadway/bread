# bread-sync

Sync engine for [Bread](../README.md) — snapshot and restore desktop state via a Git remote.

## Purpose

`bread-sync` provides the library backing `bread sync` commands. It handles:

- **Git operations** — clone, commit, push, pull, fetch, diff via `git2`
- **Config serialization** — read/write `sync.toml` (machine name, remote URL, delegates, packages)
- **Delegate file sync** — rsync-style directory copy with glob excludes
- **Package snapshots** — capture installed packages from pacman, pip, npm, cargo
- **Machine profiles** — per-machine TOML records with hostname, tags, and last-sync timestamp

## Public API

### `config`

```rust
SyncConfig::load(config_dir: &Path) -> Result<SyncConfig>
SyncConfig::save(&self, config_dir: &Path) -> Result<()>
SyncConfig::local_repo_path() -> PathBuf   // ~/.local/share/bread/sync-repo/
bread_config_dir() -> PathBuf              // ~/.config/bread/
expand_path(path: &str) -> PathBuf         // expands ~/
```

### `git`

```rust
SyncRepo::init(path: &Path) -> Result<SyncRepo>
SyncRepo::open(path: &Path) -> Result<SyncRepo>
SyncRepo::clone_from(url: &str, path: &Path) -> Result<SyncRepo>
SyncRepo::open_or_clone(url: &str, path: &Path) -> Result<SyncRepo>
SyncRepo::commit(&self, message: &str) -> Result<Option<git2::Oid>>  // None = nothing to commit
SyncRepo::push(&self, remote: &str, branch: &str) -> Result<()>
SyncRepo::pull(&self, remote: &str, branch: &str) -> Result<()>      // fast-forward only
SyncRepo::fetch(&self, remote: &str, branch: &str) -> Result<()>
SyncRepo::is_clean(&self) -> Result<bool>
SyncRepo::local_changes(&self) -> Result<Vec<(char, String)>>
SyncRepo::remote_changes(&self, remote: &str, branch: &str) -> Result<Vec<(char, String)>>
SyncRepo::working_diff(&self) -> Result<String>
SyncRepo::remote_diff(&self, remote: &str, branch: &str) -> Result<String>
SyncRepo::set_remote(&self, name: &str, url: &str) -> Result<()>
SyncRepo::last_commit_time(&self) -> Option<DateTime<Local>>
```

### `delegates`

```rust
sync_dir(src: &Path, dst: &Path, exclude: &[String]) -> Result<()>
resolve_include_paths(includes: &[String]) -> Vec<(String, PathBuf)>
```

### `machine`

```rust
MachineProfile::new(name: String, tags: Vec<String>) -> MachineProfile
MachineProfile::write(&self, machines_dir: &Path) -> Result<()>
MachineProfile::read(machines_dir: &Path, name: &str) -> Result<MachineProfile>
MachineProfile::list(machines_dir: &Path) -> Result<Vec<MachineProfile>>
hostname() -> String
```

### `packages`

```rust
snapshot(manager: &str, dest: &Path) -> Result<bool>  // false = manager not found (non-fatal)
parse_pacman(content: &str) -> Vec<String>
parse_pip(content: &str) -> Vec<String>
parse_npm(content: &str) -> Vec<String>
parse_cargo(content: &str) -> Vec<String>
```

## Sync repo layout

```
~/.local/share/bread/sync-repo/
├── bread/          ← snapshot of ~/.config/bread/
├── configs/
│   └── <basename>/ ← delegate paths
├── machines/
│   └── <name>.toml ← per-machine profiles
└── packages/
    ├── pacman.txt
    ├── pip.txt
    ├── npm.txt
    └── cargo.txt
```
