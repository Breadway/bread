#!/usr/bin/env bash
# Render one breadscope view on a private headless Sway output and save a PNG.
#
#   dev/screenshot.sh [stream|flow|modules|state|adapters] [out.png]
#
# Starts a fake breadd and a nested, headless Sway session in a throwaway
# runtime dir. Nothing is drawn on, or read from, the live desktop. Used for
# visual checks in CI-less environments and for reviewing layout changes.
set -euo pipefail

VIEW="${1:-stream}"
OUT="${2:-/tmp/breadscope-$VIEW.png}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$HERE/target/debug/breadscope}"
T="$(mktemp -d)"
PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
    sleep 0.2
    if [ "${KEEP:-0}" = "1" ]; then
        echo "screenshot: kept $T" >&2
    else
        rm -rf "$T"
    fi
}
trap cleanup EXIT

mkdir -p "$T/runtime" "$T/home" "$T/state" "$T/cfg" "$T/cache" "$T/state/breadscope"
chmod 700 "$T/runtime"
if [ -n "${SESSION:-}" ]; then
    printf '%s' "$SESSION" > "$T/state/breadscope/session.json"
fi
cat > "$T/sway.conf" <<EOF
output HEADLESS-1 resolution ${SHOT_WIDTH:-1400}x${SHOT_HEIGHT:-900}
default_border none
gaps inner 0
gaps outer 0
EOF

XDG_RUNTIME_DIR="$T/runtime" python3 "$HERE/dev/fake_breadd.py" >"$T/fake.log" 2>&1 &
PIDS+=($!)

env WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman \
    WLR_LIBINPUT_NO_DEVICES=1 XDG_RUNTIME_DIR="$T/runtime" HOME="$T/home" \
    SWAYSOCK="$T/runtime/sway-ipc.sock" \
    sway -c "$T/sway.conf" >"$T/sway.log" 2>&1 &
PIDS+=($!)

for _ in $(seq 1 50); do
    [ -S "$T/runtime/wayland-1" ] && [ -S "$T/runtime/sway-ipc.sock" ] && break
    sleep 0.1
done
if [ ! -S "$T/runtime/wayland-1" ]; then
    echo "screenshot: sway did not start" >&2
    tail -20 "$T/sway.log" >&2
    exit 1
fi

EXTRA=()
if [ -n "${TAB:-}" ]; then
    EXTRA+=(--tab "$TAB")
fi
dbus-run-session -- env -u HYPRLAND_INSTANCE_SIGNATURE -u DISPLAY \
    WAYLAND_DISPLAY=wayland-1 GDK_BACKEND=wayland GTK_USE_PORTAL=0 \
    HOME="$T/home" XDG_STATE_HOME="$T/state" XDG_CONFIG_HOME="$T/cfg" \
    XDG_CACHE_HOME="$T/cache" XDG_RUNTIME_DIR="$T/runtime" \
    "$BIN" --view "$VIEW" "${EXTRA[@]}" >"$T/app.log" 2>&1 &
PIDS+=($!)

sleep "${SHOT_WAIT:-6}"
if ! kill -0 "${PIDS[2]}" 2>/dev/null; then
    echo "screenshot: breadscope exited early" >&2
    tail -40 "$T/app.log" >&2
    exit 1
fi

env WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR="$T/runtime" \
    grim -t png "$OUT"

echo "$OUT"
