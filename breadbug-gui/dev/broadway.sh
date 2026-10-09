#!/usr/bin/env bash
# Run breadbug-gui in an isolated GTK Broadway session so it can be looked at
# (and driven from a browser) without touching the real desktop: nothing
# draws on your display, your clipboard and compositor are not involved, and
# it reads a throwaway HOME seeded with fake data.
#
#   dev/broadway.sh [extra breadbug-gui args]   then open http://127.0.0.1:8087
#
# Needs gtk4-broadwayd. Stops everything it started on exit.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$HERE/target/debug/breadbug-gui}"
N="${BROADWAY_N:-7}"
T="$(mktemp -d)"
PIDS=()
cleanup() { for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$T"; }
trap cleanup EXIT

# Fake data: a crash in clip, a freeze in pad, a store with a few log lines.
S="$T/state/bread/bugstore/apps"
mkdir -p "$S/clip" "$S/pad" "$T/cfg/bread" "$T/runtime" "$T/state/bakery"
NOW_MS=$(( $(date +%s) * 1000 ))
echo "{\"t\":$((NOW_MS-60000)),\"level\":\"WARN\",\"msg\":\"sync failed for <EMAIL:1> from <IP:1>\"}" > "$S/clip/log.jsonl"
echo "{\"t\":$((NOW_MS-45000)),\"kind\":\"panic\",\"app_version\":\"0.3.1\",\"message\":\"called Option::unwrap() on a None value\",\"frames\":[{\"symbol\":\"clip::sync::run\"}]}" > "$S/clip/markers.jsonl"
echo "{\"t\":$((NOW_MS-3600000)),\"kind\":\"freeze\",\"message\":\"no heartbeat for 20s\"}" > "$S/pad/markers.jsonl"
echo '{"track":"stable","packages":{"breadclip":{"version":"0.3.1","name":"breadclip"},"breadpad":{"version":"0.6.2","name":"breadpad"}}}' > "$T/state/bakery/installed.json"
[ -n "${SENDING:-}" ] && printf 'upload_enabled = true\nendpoint = "%s"\n' "$SENDING" > "$T/cfg/bread/breadbug.toml"
[ -z "${KEEP_CONSENT:-}" ] && { mkdir -p "$T/state/bread/breadbug"; echo 1 > "$T/state/bread/breadbug/consent-shown"; }

# The daemon and the app must agree on XDG_RUNTIME_DIR (the Broadway socket
# lives there), and the daemon listens on loopback only.
XDG_RUNTIME_DIR="$T/runtime" gtk4-broadwayd --address 127.0.0.1 --port "$((8080 + N))" ":$N" >/dev/null 2>&1 &
PIDS+=($!)
sleep 1
echo "open http://127.0.0.1:$((8080 + N))"
env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u DISPLAY \
    GDK_BACKEND=broadway BROADWAY_DISPLAY=":$N" GTK_USE_PORTAL=0 \
    HOME="$T/home" XDG_STATE_HOME="$T/state" XDG_CONFIG_HOME="$T/cfg" XDG_RUNTIME_DIR="$T/runtime" \
    dbus-run-session -- "$BIN" "$@" &
PIDS+=($!)
wait "${PIDS[1]}" || true
