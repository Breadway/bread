# Contributing

`bread` — Reactive automation daemon (breadd) and CLI for Linux desktops.

Part of the bread ecosystem; this repo follows the same branch/release
workflow as every other ecosystem product.

## Branches

There is one long-lived branch: **`main`**. All day-to-day work lands here.
Every push to `main` automatically builds and publishes a **dev-track**
build (see Tracks below) — a real install you can test before cutting
anything more formal.

New work — features and bug fixes alike — goes on a short-lived branch:

```
feature/<short-name>
fix/<issue-number-or-short-name>
```

Branch off `main`, open a PR/push back into `main` when ready. Short-lived
branches get deleted on merge — they never accumulate the kind of drift a
second long-lived branch does.

## The release cycle

There's no separate `beta` or release branch — "stable" and "beta" are both
just **tags** on `main`, not branches that need to be kept in sync:

1. Work accumulates on `main` via `feature/x` / `fix/x` branches. Each push
   auto-publishes a dev build — install it with `bakery track set dev` and
   `bakery update --all`, then fix anything broken with another push.
2. When you want to stabilize before a real release, tag a release
   candidate: `git tag vX.Y.Z-rc.1 && git push origin vX.Y.Z-rc.1` (push to
   both remotes). That tag alone triggers a beta-track build —
   "freezing" is just pausing pushes to `main` while you test it, not a
   branch operation. Cut `-rc.2`, `-rc.3`, etc. for further fixes.
3. Once an RC has gone without issues, tag the real release:
   `git tag vX.Y.Z && git push origin vX.Y.Z` — that's what triggers the
   signed stable release build.

## Tracks, from a user's perspective

```
bakery track show              # what you're currently on (defaults to stable)
bakery track set dev           # or beta, or stable
bakery update --all            # pull the latest build on your current track
```

| Track  | What it is | Published from |
|--------|-----------|-----------------|
| `stable` | The last tagged release | a `vX.Y.Z` tag |
| `beta` | Latest release candidate | a `vX.Y.Z-rc.N` tag |
| `dev` | Bleeding edge | `main`, on every push |

Dev versions are auto-computed (`X.Y.Z-dev.<timestamp>+<sha>`) from the
latest published stable tag, so they always sort as newer than what you
have installed — no manual version bumping needed. Beta versions are just
the RC tag itself (already valid semver, already sorts below the real
release it's a candidate for).

## Local development

```sh
cargo build --release --workspace
cargo test --release --workspace
```

### Keeping the API docs honest

`Documentation.md`'s Lua API and IPC protocol sections are hand-written and
have drifted from the actual code before — there's a checked-in registry,
`api-schema.toml`, plus an `xtask` checker that catches it happening again.

Whenever you add, rename, or remove a `bread.*` Lua binding
(`breadd/src/lua/mod.rs`) or an IPC method (`breadd/src/ipc/mod.rs`):

1. Add/update/remove its entry in `api-schema.toml` to match.
2. Add/update the corresponding section in `Documentation.md` (a
   `#### bread.<name>` heading for a Lua binding, or a row in the IPC
   Methods table for an IPC method).
3. Run `cargo run -p xtask -- check-docs` before committing. It fails with
   a non-zero exit and a list of exactly what's out of sync — added but
   undocumented, stale in the schema, or missing a doc heading/row — if
   `api-schema.toml`, the code, and `Documentation.md` don't all agree.

`check-docs` is **not yet wired into CI** — it's a local, manually-run
check today, not an enforced gate. That's a deliberate, still-open gap
(not an oversight): CI pipeline changes get a separate review pass before
landing, same as any other workflow-file edit. Wiring `cargo run -p xtask
-- check-docs` into `dev-release.yml` (fail the build on drift) is the
natural next step whenever that review happens — until then, discipline
running it before committing is what keeps `api-schema.toml`, the code,
and `Documentation.md` in sync, not anything automatic.

Also note `check-docs`'s scope: it covers `bread.*` Lua bindings and IPC
methods against `Documentation.md` only. It does not cover the `bread`
CLI's subcommands against `README.md`'s hand-written CLI reference —
that's a separate, currently-unguarded copy of information (see
`README.md`'s "CLI reference" section) and has drifted before for exactly
the same reason `Documentation.md` used to.

## CI

- `dev-release.yml` — triggered on push to `main`.
- `rc-release.yml` — triggered on any `vX.Y.Z-rc.N` tag push.
- `release.yml` — triggered on any other `v*` tag push, cuts the actual
  stable release.

All CI runs on a self-hosted runner; nothing runs automatically on plain
commits or PRs beyond the track builds above. See
[bread-ecosystem's docs/release-channels.md](https://git.breadway.dev/Breadway/bread-ecosystem/src/branch/main/docs/release-channels.md)
for the full policy, including how a new product gets wired onto these tracks.

## Questions

Open an issue on this repo's Forgejo tracker.
