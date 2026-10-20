#!/usr/bin/env bash
# Render every page of breadbug-gui to PNGs in an isolated GTK Broadway
# display (nothing appears on your desktop), seeded with fake data.
#
#   dev/walkthrough.sh OUT_DIR            sending off, first-run notice shown
#   SENDING=http://127.0.0.1:PORT dev/walkthrough.sh OUT_DIR    sending on
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:?usage: walkthrough.sh OUT_DIR}"
N="${BROADWAY_N:-9}"
T="$(mktemp -d)"
PIDS=()
cleanup() { for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$T"; }
trap cleanup EXIT

cargo build -q --manifest-path "$HERE/Cargo.toml" --example walkthrough
BIN="$(cargo metadata --manifest-path "$HERE/Cargo.toml" --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/examples/walkthrough"
S="$T/state/bread/bugstore/apps"
mkdir -p "$S/clip" "$S/pad" "$T/cfg/bread" "$T/runtime" "$T/state/bakery"
NOW_MS=$(( $(date +%s) * 1000 ))
echo "{\"t\":$((NOW_MS-60000)),\"level\":\"WARN\",\"msg\":\"sync failed for <EMAIL:1> from <IP:1>\"}" > "$S/clip/log.jsonl"
echo "{\"t\":$((NOW_MS-45000)),\"kind\":\"panic\",\"app_version\":\"0.3.1\",\"message\":\"called Option::unwrap() on a None value\",\"frames\":[{\"symbol\":\"clip::sync::run\"}]}" > "$S/clip/markers.jsonl"
echo "{\"t\":$((NOW_MS-3600000)),\"kind\":\"freeze\",\"message\":\"no heartbeat for 20s\"}" > "$S/pad/markers.jsonl"
echo '{"track":"stable","packages":{"breadclip":{"version":"0.3.1","name":"breadclip"},"breadpad":{"version":"0.6.2","name":"breadpad"}}}' > "$T/state/bakery/installed.json"
[ -n "${SENDING:-}" ] && printf 'upload_enabled = true\nendpoint = "%s"\n' "$SENDING" > "$T/cfg/bread/breadbug.toml"
[ -n "${SKIP_CONSENT:-}" ] && { mkdir -p "$T/state/bread/breadbug"; echo 1 > "$T/state/bread/breadbug/consent-shown"; }

XDG_RUNTIME_DIR="$T/runtime" gtk4-broadwayd --address 127.0.0.1 --port "$((8080 + N))" ":$N" >/dev/null 2>&1 &
PIDS+=($!)
sleep 1
env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u DISPLAY \
    GDK_BACKEND=broadway BROADWAY_DISPLAY=":$N" GTK_USE_PORTAL=0 \
    HOME="$T/home" XDG_STATE_HOME="$T/state" XDG_CONFIG_HOME="$T/cfg" XDG_RUNTIME_DIR="$T/runtime" \
    dbus-run-session -- "$BIN" "$OUT" 2>&1 \
    | grep -v "portal\|dbus-daemon\|xdph\|^$"
