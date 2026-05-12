#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
SERVICE_DIR="${HOME}/.config/systemd/user"
CONFIG_DIR="${HOME}/.config/bread"
MODULES_DIR="${CONFIG_DIR}/modules"

# ── build ──────────────────────────────────────────────────────────────────────
echo "building bread (release)..."
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"
echo ""

# ── symlinks ───────────────────────────────────────────────────────────────────
echo "symlinking binaries into $BIN_DIR..."
mkdir -p "$BIN_DIR"
ln -sf "$REPO_ROOT/target/release/breadd" "$BIN_DIR/breadd"
ln -sf "$REPO_ROOT/target/release/bread"  "$BIN_DIR/bread"
echo "  $BIN_DIR/breadd -> $REPO_ROOT/target/release/breadd"
echo "  $BIN_DIR/bread  -> $REPO_ROOT/target/release/bread"

if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    echo ""
    echo "  note: $BIN_DIR is not in PATH — add to your shell profile:"
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
fi
echo ""

# ── config ─────────────────────────────────────────────────────────────────────
echo "setting up config..."
mkdir -p "$CONFIG_DIR" "$MODULES_DIR"

if [[ ! -f "$CONFIG_DIR/breadd.toml" ]]; then
    cat > "$CONFIG_DIR/breadd.toml" << 'EOF'
[daemon]
log_level = "info"

[lua]
entry_point = "~/.config/bread/init.lua"
module_path  = "~/.config/bread/modules"

[adapters.hyprland]
enabled = true

[adapters.udev]
enabled = true

[adapters.power]
enabled = true

[adapters.network]
enabled = true
EOF
    echo "  created $CONFIG_DIR/breadd.toml"
else
    echo "  $CONFIG_DIR/breadd.toml already exists, skipping"
fi

if [[ ! -f "$CONFIG_DIR/init.lua" ]]; then
    cat > "$CONFIG_DIR/init.lua" << 'EOF'
-- bread init.lua — loaded before modules, use for global setup
bread.log("bread started")
EOF
    echo "  created $CONFIG_DIR/init.lua"
else
    echo "  $CONFIG_DIR/init.lua already exists, skipping"
fi
echo ""

# ── systemd user service ───────────────────────────────────────────────────────
echo "installing systemd user service..."
mkdir -p "$SERVICE_DIR"
# Patch ExecStart to match the actual install location rather than hardcoding /usr/bin.
sed "s|ExecStart=.*|ExecStart=$BIN_DIR/breadd|" \
    "$REPO_ROOT/packaging/systemd/breadd.service" \
    > "$SERVICE_DIR/breadd.service"
echo "  installed $SERVICE_DIR/breadd.service (ExecStart=$BIN_DIR/breadd)"

systemctl --user daemon-reload

if systemctl --user is-active --quiet breadd 2>/dev/null; then
    systemctl --user restart breadd
    echo "  breadd restarted"
else
    systemctl --user enable --now breadd
    echo "  breadd enabled and started"
fi
echo ""

# ── verify ─────────────────────────────────────────────────────────────────────
sleep 0.5
if "$BIN_DIR/bread" ping &>/dev/null; then
    "$BIN_DIR/bread" doctor
else
    echo "warning: daemon did not respond to ping"
    echo "  check: journalctl --user -u breadd -n 20"
fi
