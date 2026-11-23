#!/usr/bin/env bash
# Start a persistent, private headless Sway session with the fake daemon and
# breadscope, for interactive checks and screenshots. Nothing touches the live
# desktop: the compositor and daemon live under a throwaway XDG_RUNTIME_DIR.
#
#   dev/headless.sh            then read /tmp/breadscope-headless.env
#   dev/headless.sh stop       stop the session it started
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$HERE/target/debug/breadscope}"
ENVFILE="/tmp/breadscope-headless.env"

if [ "${1:-}" = "stop" ]; then
    if [ -f "$ENVFILE" ]; then
        # shellcheck disable=SC1090
        source "$ENVFILE"
        for pid in ${PIDS:-}; do kill "$pid" 2>/dev/null || true; done
        rm -rf "${T:-/nonexistent}"
        rm -f "$ENVFILE"
    fi
    exit 0
fi

T="$(mktemp -d)"
mkdir -p "$T/runtime" "$T/home" "$T/state" "$T/cfg" "$T/cache"
chmod 700 "$T/runtime"
cat > "$T/sway.conf" <<'EOF'
output HEADLESS-1 resolution 1400x900
default_border none
gaps inner 0
gaps outer 0
EOF

PIDS=""
if [ "${LIVE:-0}" = "1" ]; then
    # Read-only smoke test against the running daemon: symlink its socket into
    # the throwaway runtime dir. Tracing is enabled and cleared by the app.
    mkdir -p "$T/runtime/bread"
    ln -s "/run/user/$(id -u)/bread/breadd.sock" "$T/runtime/bread/breadd.sock"
elif [ "${REAL:-0}" = "1" ]; then
    # Exercise the real daemon and the real IPC methods. A tiny init.lua
    # chains two custom events and a timer supplies a steady stream.
    mkdir -p "$T/cfg/bread/modules"
    cat > "$T/cfg/bread/init.lua" <<'LUA'
bread.on('bread.custom.tick', function(event)
    bread.emit('bread.custom.pong', { from = 'tick', n = event.data.n })
end)
bread.on('bread.custom.pong', function(event)
    local _ = event.data.from
end)
bread.every(900, function()
    bread.emit('bread.custom.tick', { n = 1 })
end)
LUA
    cat > "$T/cfg/bread/breadd.toml" <<'TOML'
[daemon]
log_level = "error"
[lua]
entry_point = "~/.config/bread/init.lua"
module_path = "~/.config/bread/modules"
[adapters.hyprland]
enabled = false
[adapters.udev]
enabled = false
[adapters.power]
enabled = false
[adapters.network]
enabled = false
[adapters.podman]
enabled = false
TOML
    env XDG_RUNTIME_DIR="$T/runtime" HOME="$T/home" XDG_CONFIG_HOME="$T/cfg"         "$HERE/../target/debug/breadd" >"$T/breadd.log" 2>&1 &
    PIDS="$PIDS $!"
else
    XDG_RUNTIME_DIR="$T/runtime" python3 "$HERE/dev/fake_breadd.py" >"$T/fake.log" 2>&1 &
    PIDS="$PIDS $!"
fi

env WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman \
    WLR_LIBINPUT_NO_DEVICES=1 XDG_RUNTIME_DIR="$T/runtime" HOME="$T/home" \
    SWAYSOCK="$T/runtime/sway-ipc.sock" \
    sway -c "$T/sway.conf" >"$T/sway.log" 2>&1 &
PIDS="$PIDS $!"

for _ in $(seq 1 50); do
    [ -S "$T/runtime/wayland-1" ] && [ -S "$T/runtime/sway-ipc.sock" ] && break
    sleep 0.1
done

dbus-run-session -- env -u HYPRLAND_INSTANCE_SIGNATURE -u DISPLAY \
    WAYLAND_DISPLAY=wayland-1 GDK_BACKEND=wayland GTK_USE_PORTAL=0 \
    HOME="$T/home" XDG_STATE_HOME="$T/state" XDG_CONFIG_HOME="$T/cfg" \
    XDG_CACHE_HOME="$T/cache" XDG_RUNTIME_DIR="$T/runtime" \
    "$BIN" --view "${VIEW:-stream}" >"$T/app.log" 2>&1 &
PIDS="$PIDS $!"
sleep 2

cat > "$ENVFILE" <<EOF
T=$T
PIDS="$PIDS"
SWAYSOCK=$T/runtime/sway-ipc.sock
WAYLAND_DISPLAY=wayland-1
XDG_RUNTIME_DIR=$T/runtime
HOME=$T/home
GDK_BACKEND=wayland
EOF
echo "env: $ENVFILE"
echo "open: WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=$T/runtime grim /tmp/shot.png"
wait $!
