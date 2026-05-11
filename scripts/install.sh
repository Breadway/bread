#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_PREFIX="${INSTALL_PREFIX:-/usr/bin}"
SERVICE_DIR="${HOME}/.config/systemd/user"

# ── build ──────────────────────────────────────────────────────────────────────
echo "building bread (release)..."
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"

# ── install binaries ───────────────────────────────────────────────────────────
echo "installing binaries to $INSTALL_PREFIX (requires sudo)..."
sudo install -Dm755 "$REPO_ROOT/target/release/breadd" "$INSTALL_PREFIX/breadd"
sudo install -Dm755 "$REPO_ROOT/target/release/bread"  "$INSTALL_PREFIX/bread"
echo "  installed $INSTALL_PREFIX/breadd"
echo "  installed $INSTALL_PREFIX/bread"

# ── systemd user service ───────────────────────────────────────────────────────
echo "installing systemd user service..."
mkdir -p "$SERVICE_DIR"
install -Dm644 "$REPO_ROOT/packaging/systemd/breadd.service" "$SERVICE_DIR/breadd.service"
echo "  installed $SERVICE_DIR/breadd.service"

systemctl --user daemon-reload
systemctl --user enable --now breadd
echo "  breadd enabled and started"

# ── verify ─────────────────────────────────────────────────────────────────────
sleep 0.5
if bread ping &>/dev/null; then
    echo ""
    bread doctor
else
    echo "warning: daemon did not respond to ping — check: journalctl --user -u breadd -n 20"
fi
