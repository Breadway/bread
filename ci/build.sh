#!/usr/bin/env bash
# Delegates to bread-ecosystem's shared CI build image/script, pinned to
# the commit in ci/bread-ecosystem.rev, not `main`. bread-ecosystem's CI
# files now affect every product's release pipeline, so bumping the pin
# is a deliberate act instead of silent drift (see the bread-theme test
# that broke here for exactly that reason, before it was pinned by rev).
#
# Usage: ci/build.sh cargo build --release --locked
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REV="$(cat "${ROOT}/ci/bread-ecosystem.rev")"

CACHE_ROOT="${BREAD_CI_CACHE_ROOT:-/tmp/bread-ci-cache}"
CACHE_DIR="${CACHE_ROOT}/${REV}"
mkdir -p "$CACHE_ROOT"

# A clone is invisible to other jobs until its pinned checkout is complete.
# Keep the cache outside the old bread-ecosystem-ci-* prefix so older release
# jobs cannot remove it while this job is building.
exec {CACHE_LOCK_FD}>"${CACHE_ROOT}/${REV}.lock"
flock "$CACHE_LOCK_FD"
if [ ! -e "$CACHE_DIR" ]; then
    CACHE_TMP="$(mktemp -d "${CACHE_ROOT}/checkout.XXXXXXXX")"
    trap 'rm -rf -- "$CACHE_TMP"' EXIT
    git clone https://git.breadway.dev/Breadway/bread-ecosystem.git "$CACHE_TMP"
    git -C "$CACHE_TMP" checkout --quiet "$REV"
    mv -- "$CACHE_TMP" "$CACHE_DIR"
    trap - EXIT
fi

if [ "$(git -C "$CACHE_DIR" rev-parse HEAD)" != "$REV" ]; then
    echo "CI cache at $CACHE_DIR does not match pinned revision $REV" >&2
    exit 1
fi
flock -u "$CACHE_LOCK_FD"
exec {CACHE_LOCK_FD}>&-

bash "${CACHE_DIR}/ci/build.sh" bread "$ROOT" "$@"
