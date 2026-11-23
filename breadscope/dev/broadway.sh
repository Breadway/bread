#!/usr/bin/env bash
# Run breadscope against a fake breadd on an isolated GTK Broadway display,
# so the window can be viewed and driven in a browser without touching the live
# daemon or desktop.
#
#   dev/broadway.sh        then open http://127.0.0.1:8090
#
# Needs gtk4-broadwayd. Stops everything it started on exit.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$HERE/target/debug/breadscope}"
N="${BROADWAY_N:-10}"
PORT="${BROADWAY_PORT:-8090}"
T="$(mktemp -d)"
PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
    rm -rf "$T"
}
trap cleanup EXIT

mkdir -p "$T/runtime/bread" "$T/home" "$T/state" "$T/cfg"

# The fake daemon speaks breadd's newline-delimited JSON protocol and seeds a
# realistic desktop, then emits a causal chain every few seconds.
XDG_RUNTIME_DIR="$T/runtime" python3 "$HERE/dev/fake_breadd.py" 2>"$T/fake_breadd.log" &
PIDS+=($!)

XDG_RUNTIME_DIR="$T/runtime" gtk4-broadwayd --address 127.0.0.1 --port "$PORT" ":$N" >/dev/null 2>&1 &
PIDS+=($!)
sleep 1

echo "open http://127.0.0.1:$PORT"
env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u DISPLAY \
    GDK_BACKEND=broadway BROADWAY_DISPLAY=":$N" GTK_USE_PORTAL=0 \
    HOME="$T/home" XDG_STATE_HOME="$T/state" XDG_CONFIG_HOME="$T/cfg" \
    XDG_RUNTIME_DIR="$T/runtime" \
    RUST_BACKTRACE=1 dbus-run-session -- "$BIN" &
PIDS+=($!)

# Keep the daemon/broadway alive while the app runs, and surface daemon errors
# if the app exits early.
wait "${PIDS[2]}" || true
if [ -s "$T/fake_breadd.log" ]; then
    echo "--- fake_breadd.log ---" >&2
    tail -40 "$T/fake_breadd.log" >&2
fi
