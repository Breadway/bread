# AGENTS.md — Repo hygiene

This repo follows the branch/release workflow in `CONTRIBUTING.md`.
Read that before any git, branch, or release work. Do not invent a
different workflow.

Day-to-day: one long-lived branch (`main`). New work goes on
`feature/<name>` or `fix/<name>`, then back into `main`. There is no
`dev` or `beta` branch — those names are **bakery tracks**, published
from `main` (dev) and from tags (`vX.Y.Z-rc.N` = beta, `vX.Y.Z` = stable).

API surface, event names, and IPC contracts live in `Documentation.md`
(kept honest by `api-schema.toml` + `cargo run -p xtask -- check-docs`).
Treat that file, not this one, as the source of truth.

## Remotes

- `origin` — Forgejo (`git.breadway.dev`) — authoritative.
- `github` — mirror. Push tags/releases per `CONTRIBUTING.md`; day-to-day
  work pushes to `origin` only.

## CI

- `dev-release.yml` — push to `main` (includes `check-docs`).
- `rc-release.yml` — `vX.Y.Z-rc.N` tag.
- `release.yml` — other `v*` tags.

All three run on the self-hosted hestia runner. Nothing else runs on
plain commits or PRs. Distribution is `bakery`, not a PKGBUILD.

## Local architecture (still true)

- `breadd` — daemon (adapters → normalizer → state engine → Lua / IPC).
- `bread` — CLI over `$XDG_RUNTIME_DIR/bread/breadd.sock`.
- `bread-emit` — fire-and-forget IPC emit (hooks + command bus).
- `bread-module-host` — sandboxed out-of-process module runtime.
- Known-app registry and reserved domains: `bread-shared/src/apps.rs`.

## Don't

- Don't embed credentials in remote URLs — SSH or a credential helper only.
- Don't commit straight to `main`.
