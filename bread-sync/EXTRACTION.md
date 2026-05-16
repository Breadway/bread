# bread-sync — slated for extraction

This crate is **no longer part of the `bread` workspace**. It is parked here
pending extraction into its own standalone project.

## Why

`bread`'s architecture deliberately scopes itself to a reactive automation
fabric — see the Non-Goals in `Overview.md`. State/dotfile synchronization
across machines is explicitly *out* of that scope. `bread-sync` grew into a
git-backed snapshot/restore + package + delegate-path manager, which is a
genuinely useful tool but a different product with a different lifecycle. It
was the one component pulling `bread`'s scope discipline out of shape, so it
is being spun out rather than removed (the code is good; it just doesn't
belong in this repo).

## Status

- Removed from the root `Cargo.toml` workspace (`members` → `exclude`).
- The `bread sync …` CLI subcommands have been removed from `bread-cli`.
- The `sync.status` IPC method and its integration tests have been removed
  from `breadd`.
- No code in `bread`/`breadd`/`bread-cli` depends on this crate anymore.

## For whoever extracts it (name polls are open)

1. Move this directory into the new repository.
2. It inherited workspace dependencies (`serde`, `git2`, `dirs`, `chrono`,
   `tempfile`, `glob`, …). Pin concrete versions in its own `Cargo.toml`;
   `*.workspace = true` will not resolve outside this workspace.
3. The only helper that had to leave this crate is `config::expand_path`,
   which moved to `bread-shared::expand_path` because non-sync code (the
   module installer) needed it. Reintroduce a local copy in the new project
   so it no longer depends on `bread-shared`.
4. Re-add the `bread sync` UX as a standalone binary, or as a `breadd` IPC
   client, in the new project — not here.
