# bread-sync

Sync and module management library for the Bread reactive desktop automation daemon.

Provides:
- `SyncConfig` — load/save `~/.config/bread/sync.toml`
- Git backend (via git2) for push/pull of bread config to a remote repository
- Delegate file handling — copy arbitrary config files into the sync repo
- Package manifest generation for pacman/pip/npm/cargo
- Machine profile — name and tags read from sync.toml
