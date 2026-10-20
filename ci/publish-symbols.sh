#!/usr/bin/env bash
# Publish debug symbols for release binaries to the breadbug symbol store.
#
# Delegates to bread-ecosystem's scripts/publish-symbols.sh at the commit
# pinned in ci/bread-ecosystem.rev (same pinning as ci/build.sh). If the pin
# predates that script this is a warning, not a failure, so a release is
# never blocked by the order the two repos' PRs merge in; bump the pin to
# make the check (a binary without a build-id fails the job) enforced.
#
# Usage: ci/publish-symbols.sh <unstripped-binary>...
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REV="$(cat "${ROOT}/ci/bread-ecosystem.rev")"
CACHE_DIR="/tmp/bread-ecosystem-ci-${REV}"

if [ ! -d "$CACHE_DIR" ]; then
    rm -rf /tmp/bread-ecosystem-ci-*
    git clone https://git.breadway.dev/Breadway/bread-ecosystem.git "$CACHE_DIR"
    git -C "$CACHE_DIR" checkout --quiet "$REV"
fi

if [ ! -x "${CACHE_DIR}/scripts/publish-symbols.sh" ]; then
    echo "::warning::ci/bread-ecosystem.rev (${REV}) predates scripts/publish-symbols.sh; debug symbols were NOT published. Bump the pin." >&2
    exit 0
fi

exec bash "${CACHE_DIR}/scripts/publish-symbols.sh" "$@"
