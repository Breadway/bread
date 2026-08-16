# CLAUDE.md — Repo hygiene

This repo follows the branch/release workflow documented in `CONTRIBUTING.md`
— read and follow it for any git, branch, or release work here (the
dev/beta/main lifecycle, `feature/x`/`fix/x` branch naming, when to cut or
reset `beta`, etc). Don't improvise a different workflow.
When starting work on a new feature, create branch "feature/<feature-name>" \
When working on a bug or issue, create branch "fix/<issue you are fixing>"

## Remotes
- `origin` — Forgejo (`git.breadway.dev` via Hestia, SSH) — authoritative.
- `github` — GitHub mirror. Push both when publishing.

## CI
- `dev-release.yml` triggers on `push: branches: ['dev']`; `beta-release.yml`
  on `push: branches: ['beta']`; `release.yml` on a `v*` tag push. `package.yml` triggers the same way for the pacman-channel package.
  None of these run on plain commits or PRs beyond what's listed.

## Don't
- Don't embed credentials in remote URLs — SSH or a credential helper only.
