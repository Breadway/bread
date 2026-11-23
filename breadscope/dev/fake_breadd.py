#!/usr/bin/env python3
"""A small fake breadd for breadscope development.

Speaks the same newline-delimited JSON protocol as the real daemon for the
methods breadscope uses, seeds a realistic desktop, and emits a new causal
chain every few seconds. It exists so the GUI can be exercised on a headless
Broadway display without touching the live daemon or desktop.

    XDG_RUNTIME_DIR=/tmp/scope dev/fake_breadd.py

Not shipped, not used in production. The real daemon is the source of truth.
"""

import json
import os
import random
import re
import socket
import sys
import threading
import time
import uuid

NOW = lambda: int(time.time() * 1000)

MODULES = [
    ("bread.devices", "loaded", True, True, {}, None),
    ("bread.monitors", "loaded", True, True, {}, None),
    ("bread.workspaces", "loaded", True, True, {}, None),
    ("bread.binds", "loaded", True, True, {}, None),
    ("bread.rules", "loaded", True, True, {}, None),
    (
        "dock-layout",
        "loaded",
        False,
        False,
        {"layout": "dock", "last_profile": "performance"},
        None,
    ),
    ("workspace-rules", "loaded", False, False, {"rules": 3}, None),
    (
        "power-saver",
        "loaded",
        False,
        False,
        {"last_ac": False, "saved_brightness": 70},
        None,
    ),
    (
        "presence-inhibitor",
        "loaded",
        False,
        False,
        {"inhibitors": 2, "last_presence": "present"},
        None,
    ),
    (
        "weather",
        "degraded",
        False,
        False,
        {"city": "Perth", "last_temp": 22},
        "on_event: attempt to index a nil value (field 'main')",
    ),
    ("bar", "loaded", False, False, {}, None),
    ("night-light", "disabled", False, False, {"enabled": False}, None),
]

SUBSCRIPTIONS = [
    ("bread.devices", "bread.device.**", "event", False, False),
    ("bread.monitors", "bread.monitor.**", "event", False, False),
    ("bread.workspaces", "bread.workspace.**", "event", False, False),
    ("bread.workspaces", "bread.window.**", "event", False, False),
    ("bread.binds", "bread.workspace.changed", "event", False, False),
    ("bread.rules", "bread.power.ac.**", "event", False, False),
    ("bread.rules", "bread.device.**", "event", False, False),
    ("dock-layout", "bread.device.dock.connected", "event", False, False),
    ("dock-layout", "bread.monitor.connected", "event", False, False),
    ("dock-layout", "bread.monitor.disconnected", "event", False, False),
    ("workspace-rules", "bread.workspace.changed", "event", False, False),
    ("workspace-rules", "bread.monitor.connected", "event", False, False),
    ("power-saver", "bread.power.**", "event", False, False),
    ("power-saver", "bread.monitor.disconnected", "event", False, False),
    ("presence-inhibitor", "bread.device.**", "event", False, False),
    ("presence-inhibitor", "bread.custom.presence.**", "event", False, False),
    ("weather", "bread.network.connected", "event", False, False),
    ("weather", "bread.network.disconnected", "event", False, False),
    ("bar", "bread.bar.widget_clicked", "event", False, False),
    ("bar", "bread.command.bar.**", "event", False, False),
    ("init.lua", None, "state_watch", False, False),
]

WIDGETS = [
    {
        "id": "weather.weather",
        "module": "weather",
        "placement": "left_of_stats",
        "order": 0,
        "visible": True,
        "tooltip": "Perth: partly cloudy",
        "updated_at": NOW() - 60000,
        "root": {
            "type": "box",
            "orientation": "horizontal",
            "spacing": 6,
            "children": [
                {"type": "icon", "name": "cloud", "size": 16},
                {
                    "type": "label",
                    "text": "22C",
                    "style": {"size": "sm", "color": "fg"},
                },
            ],
        },
    },
    {
        "id": "bar.battery",
        "module": "bar",
        "placement": "right_of_clock",
        "order": 0,
        "visible": True,
        "tooltip": "86 percent, on battery",
        "updated_at": NOW() - 1000,
        "root": {
            "type": "box",
            "children": [
                {
                    "type": "label",
                    "text": "86%",
                    "style": {"size": "xs", "color": "yellow", "weight": "bold"},
                },
                {"type": "progress", "value": 0.86},
            ],
        },
    },
    {
        "id": "bar.cpu",
        "module": "bar",
        "placement": "right_of_clock",
        "order": 1,
        "visible": True,
        "tooltip": "CPU load 31 percent",
        "updated_at": NOW() - 500,
        "root": {
            "type": "box",
            "children": [
                {"type": "icon", "name": "cpu"},
                {
                    "type": "label",
                    "text": "31%",
                    "style": {"size": "xs", "color": "dim"},
                },
                {"type": "progress", "value": 0.31},
            ],
        },
    },
    {
        "id": "presence.status",
        "module": "presence-inhibitor",
        "placement": "left_of_stats",
        "order": 10,
        "visible": True,
        "tooltip": "Inhibitor held by bread",
        "updated_at": NOW() - 2000,
        "root": {
            "type": "box",
            "spacing": 5,
            "children": [
                {
                    "type": "label",
                    "text": "present",
                    "style": {"size": "xs", "color": "green", "weight": "bold"},
                },
            ],
        },
    },
]

WORKFLOWS = [
    {
        "name": "dock-connected",
        "state": "running",
        "step": "waiting for monitor",
        "started_at": NOW() - 480000,
        "updated_at": NOW() - 84000,
        "error": None,
    },
    {
        "name": "battery-guard",
        "state": "done",
        "step": "restored performance profile",
        "started_at": NOW() - 320000,
        "updated_at": NOW() - 319000,
        "error": None,
    },
    {
        "name": "profile-transition",
        "state": "failed",
        "step": "matched profile for DP-3",
        "started_at": NOW() - 7200000,
        "updated_at": NOW() - 7190000,
        "error": "no profile matches monitor DP-3",
    },
]

ADAPTERS = {
    "udev": "connected",
    "hyprland": "connected",
    "power": "connected",
    "network": "connected",
    "bluetooth": "connected",
    "systemd": "connected",
    "podman": "unavailable",
    "filesystem": "connected",
    "git": "connected",
    "terminal": "connected",
}

ERRORS = [
    {
        "timestamp": NOW() - 190000,
        "module": "weather",
        "message": "on_event: attempt to index a nil value (field 'main')",
    },
    {
        "timestamp": NOW() - 780000,
        "module": "podman",
        "message": "adapter retry: cannot connect to Podman socket: permission denied",
    },
]


def pattern_regex(pattern):
    out = "^"
    i = 0
    while i < len(pattern):
        char = pattern[i]
        if char == "*":
            if i + 1 < len(pattern) and pattern[i + 1] == "*":
                out += ".*"
                i += 1
            else:
                out += "[^.]+"
        elif char == "?":
            out += "[^.]"
        else:
            out += re.escape(char)
        i += 1
    return re.compile(out + "$")


class Daemon:
    def __init__(self):
        self.lock = threading.Lock()
        self.events = []
        self.clients = []
        self.trace_enabled = False
        self.traces = []
        self.counts = {}
        self.last = {}
        self.last_at = {}
        self.subs = []
        for index, (module, pattern, kind, once, filtered) in enumerate(SUBSCRIPTIONS):
            if pattern is None:
                pattern = "bread.state.changed.power"
            self.subs.append(
                {
                    "id": index + 1,
                    "module": module,
                    "pattern": pattern,
                    "kind": kind,
                    "once": once,
                    "filtered": filtered,
                    "raw_kind": None,
                    "count": 0,
                    "last_event": None,
                    "last_at": None,
                }
            )
        self.regexes = {sub["id"]: pattern_regex(sub["pattern"]) for sub in self.subs}
        self.thread_rng = random.Random(7)
        self.chain_index = 0
        self.seed()

    def matching(self, name):
        return [sub for sub in self.subs if self.regexes[sub["id"]].match(name)]

    def record_dispatch(self, event):
        hits = self.matching(event["event"])
        now = NOW()
        for sub in hits:
            sub["count"] += 1
            sub["last_event"] = event["event"]
            sub["last_at"] = now
            if self.trace_enabled:
                self.traces.append(
                    {
                        "event_id": event["id"],
                        "event": event["event"],
                        "module": sub["module"],
                        "pattern": sub["pattern"],
                        "duration_us": self.thread_rng.randrange(180, 3800),
                        "ok": True,
                        "at": now,
                    }
                )
        if len(self.traces) > 1024:
            self.traces = self.traces[-1024:]
        return len(hits)

    def publish(self, event):
        with self.lock:
            self.events.append(event)
            if len(self.events) > 2000:
                self.events = self.events[-2000:]
            self.record_dispatch(event)
            dead = []
            line = (json.dumps(event) + "\n").encode()
            for client in self.clients:
                try:
                    client.sendall(line)
                except OSError:
                    dead.append(client)
            for client in dead:
                try:
                    self.clients.remove(client)
                except ValueError:
                    pass

    def event(self, name, source, data, caused_by=None):
        return {
            "event": name,
            "timestamp": NOW(),
            "source": source,
            "data": data,
            "id": str(uuid.uuid4()),
            "caused_by": caused_by,
        }

    def seed(self):
        base = NOW() - 112000
        built = []
        counter = [0]

        def add(name, source, data, caused=None):
            event = self.event(name, source, data, caused)
            event["timestamp"] = base + counter[0] * 4600
            counter[0] += 1
            with self.lock:
                self.events.append(event)
                self.record_dispatch(event)
            built.append(event)
            return event

        add("bread.system.startup", "system", {})
        add("bread.profile.activated", "system", {"profile": "default", "previous": None})
        dock = add("bread.device.connected", "udev", {
            "device": "dock", "name": "CalDigit TS4", "subsystem": "usb",
            "vendor_id": "0x35f5", "product_id": "0x0104",
            "id": "/sys/devices/pci0000:00/0000:00:08.3/usb4/4-1",
        })
        add("bread.device.dock.connected", "udev",
            {"device": "dock", "id": "/sys/devices/pci0000:00/0000:00:08.3/usb4/4-1"},
            caused=dock["id"])
        monitor = add("bread.monitor.connected", "hyprland",
                      {"name": "HDMI-A-1", "resolution": "2560x1440", "position": "0x0"})
        layout = add("bread.monitor.layout.applied", "system",
                     {"layout": "dock", "monitors": ["HDMI-A-1", "eDP-1"]},
                     caused=monitor["id"])
        add("bread.workspace.moved", "system",
            {"workspace": "4", "from": "eDP-1", "to": "HDMI-A-1"}, caused=monitor["id"])
        add("bread.profile.activated", "system",
            {"profile": "dock", "previous": "default"}, caused=layout["id"])
        add("bread.command.bar.refresh", {"app": "bar"}, {"app": "bar", "verb": "refresh"},
            caused=layout["id"])
        add("bread.workspace.changed", "hyprland", {"id": "3"})
        add("bread.window.opened", "hyprland",
            {"address": "0x55d3a1c2", "class": "Alacritty", "title": "cargo test -p breadd"})
        ac = add("bread.power.ac.connected", "power",
                 {"ac_connected": True, "battery_percent": 87})
        add("bread.profile.activated", "system",
            {"profile": "performance", "previous": "dock"}, caused=ac["id"])
        paired = add("bread.bluetooth.device.paired", "bluetooth",
                     {"name": "Sony WH-1000XM5", "address": "AC:80:0A:11:22:33",
                      "subsystem": "bluetooth"})
        device = add("bread.device.connected", "bluetooth",
                     {"device": "headphones", "name": "Sony WH-1000XM5",
                      "address": "AC:80:0A:11:22:33", "subsystem": "bluetooth"},
                     caused=paired["id"])
        add("bread.device.bluetooth.connected", "bluetooth",
            {"device": "headphones", "id": "AC:80:0A:11:22:33"}, caused=device["id"])
        add("bread.bar.widget_clicked", {"app": "bar"},
            {"widget_id": "weather.weather", "action": "refresh"})
        add("bread.network.connected", "network", {"online": True, "interfaces": ["eth0"]})
        add("bread.module.crashed", "system",
            {"module": "weather", "pid": 8412, "reason": "panic", "exit_code": 101,
             "signal": None})
        add("bread.custom.test", "manual", {"note": "poke from CLI", "payload": {"n": 1}})
        down = add("bread.monitor.disconnected", "hyprland", {"name": "HDMI-A-1"})
        add("bread.workspace.moved", "system",
            {"workspace": "4", "from": "HDMI-A-1", "to": "eDP-1"}, caused=down["id"])
        add("bread.profile.activated", "system",
            {"profile": "mobile", "previous": "performance"}, caused=down["id"])
        add("bread.power.ac.disconnected", "power",
            {"ac_connected": False, "battery_percent": 86})

    def chain(self):
        chains = [
            [
                (
                    "bread.power.ac.connected",
                    "power",
                    {"ac_connected": True, "battery_percent": 84},
                ),
                (
                    "bread.profile.activated",
                    "system",
                    {"profile": "performance", "previous": "mobile"},
                    0,
                ),
            ],
            [
                ("bread.workspace.changed", "hyprland", {"id": "2"}),
                (
                    "bread.workspace.moved",
                    "system",
                    {"workspace": "2", "from": "eDP-1", "to": "HDMI-A-1"},
                    0,
                ),
                (
                    "bread.command.bar.refresh",
                    {"app": "bar"},
                    {"app": "bar", "verb": "refresh"},
                    1,
                ),
            ],
            [
                (
                    "bread.window.opened",
                    "hyprland",
                    {
                        "address": "0x55d3b8f4",
                        "class": "Alacritty",
                        "title": "bread state | jq",
                    },
                ),
            ],
            [
                (
                    "bread.network.connected",
                    "network",
                    {"online": True, "interfaces": ["eth0"]},
                ),
                ("bread.custom.ping", "manual", {"from": "fake daemon"}, 0),
            ],
            [
                (
                    "bread.device.bluetooth.connected",
                    "bluetooth",
                    {
                        "device": "headphones",
                        "name": "Sony WH-1000XM5",
                        "address": "AC:80:0A:11:22:33",
                        "subsystem": "bluetooth",
                    },
                ),
                (
                    "bread.command.bar.refresh",
                    {"app": "bar"},
                    {"app": "bar", "verb": "refresh"},
                    0,
                ),
            ],
            [
                ("bread.power.battery.low", "power", {"battery_percent": 14}),
                (
                    "bread.profile.activated",
                    "system",
                    {"profile": "battery", "previous": "performance"},
                    0,
                ),
            ],
        ]
        chain = chains[self.chain_index % len(chains)]
        self.chain_index += 1
        built = []
        for index, (name, source, data, *rest) in enumerate(chain):
            parent = rest[0] if rest else None
            caused = (
                built[parent]["id"]
                if parent is not None and parent < len(built)
                else None
            )
            event = self.event(name, source, data, caused)
            built.append(event)
            self.publish(event)

    def start_ticker(self):
        def run():
            while True:
                time.sleep(3.4)
                try:
                    self.chain()
                except Exception as error:  # dev tool: keep ticking
                    print(f"fake_breadd: chain error: {error}", file=sys.stderr)

        threading.Thread(target=run, daemon=True).start()

    # -- protocol ---------------------------------------------------------
    def response(self, req_id, result):
        return {"id": req_id, "result": result}

    def error(self, req_id, message):
        return {"id": req_id, "error": message}

    def health(self):
        return {
            "ok": True,
            "pid": os.getpid(),
            "version": "0.10.1",
            "api_version": "1.9.0",
            "uptime_ms": 11520000,
            "socket": SOCKET_PATH,
            "adapters": ADAPTERS,
            "subscriptions": len(self.subs),
            "modules": [
                {
                    "name": name,
                    "status": status,
                    "last_error": error,
                    "builtin": builtin,
                    "store": store,
                }
                for (name, status, builtin, _ungated, store, error) in MODULES
            ],
            "recent_errors": ERRORS,
            "bugstore": {"available": True},
        }

    def state(self):
        return {
            "monitors": [
                {
                    "name": "HDMI-A-1",
                    "connected": False,
                    "resolution": "2560x1440",
                    "position": "0x0",
                },
                {
                    "name": "eDP-1",
                    "connected": True,
                    "resolution": "1920x1200",
                    "position": "0x0",
                },
            ],
            "workspaces": [
                {"id": "1", "monitor": "eDP-1"},
                {"id": "2", "monitor": "eDP-1"},
                {"id": "3", "monitor": "eDP-1"},
                {"id": "4", "monitor": "eDP-1"},
            ],
            "active_workspace": "3",
            "active_window": "0x55d3a1c2",
            "devices": {
                "connected": [
                    {
                        "id": "/sys/devices/pci0000:00/0000:00:08.3/usb4/4-1",
                        "name": "CalDigit TS4",
                        "device": "dock",
                        "subsystem": "usb",
                        "vendor_id": "0x35f5",
                        "product_id": "0x0104",
                    },
                    {
                        "id": "AC:80:0A:11:22:33",
                        "name": "Sony WH-1000XM5",
                        "device": "headphones",
                        "subsystem": "bluetooth",
                    },
                ]
            },
            "network": {
                "interfaces": {"eth0": {"up": True}, "wlan0": {"up": False}},
                "online": True,
            },
            "power": {
                "ac_connected": False,
                "battery_percent": 86,
                "battery_low": False,
            },
            "profile": {
                "active": "mobile",
                "history": ["performance", "dock", "default"],
                "profiles": {
                    "default": {},
                    "mobile": {},
                    "dock": {},
                    "performance": {},
                },
            },
            "modules": [
                {
                    "name": name,
                    "status": status,
                    "last_error": error,
                    "builtin": builtin,
                    "store": store,
                }
                for (name, status, builtin, _ungated, store, error) in MODULES
            ],
            "workflows": WORKFLOWS,
            "widgets": WIDGETS,
        }

    def handle(self, request):
        method = request.get("method")
        params = request.get("params") or {}
        req_id = request.get("id", "?")
        if method == "ping":
            return self.response(req_id, {"ok": True})
        if method == "startup.ready":
            return self.response(req_id, {"ok": True})
        if method == "health":
            return self.response(req_id, self.health())
        if method == "state.dump":
            return self.response(req_id, self.state())
        if method == "modules.list":
            return self.response(req_id, self.health()["modules"])
        if method == "subscriptions.list":
            with self.lock:
                return self.response(req_id, list(self.subs))
        if method == "modules.reload":
            return self.response(req_id, {"ok": True, "reloaded": True})
        if method == "workflows.list":
            return self.response(req_id, WORKFLOWS)
        if method == "widgets.list":
            return self.response(req_id, WIDGETS)
        if method == "profile.list":
            return self.response(req_id, ["default", "mobile", "dock", "performance"])
        if method == "profile.activate":
            return self.response(req_id, {"active": params.get("name", "default")})
        if method == "events.replay":
            since = int(params.get("since_ms", 0))
            cutoff = NOW() - since
            with self.lock:
                return self.response(
                    req_id, [e for e in self.events if e["timestamp"] >= cutoff]
                )
        if method == "trace.set":
            enabled = bool(params.get("enabled"))
            print(f"fake_breadd: trace.set enabled={enabled}", file=sys.stderr)
            with self.lock:
                self.trace_enabled = enabled
                if not enabled:
                    self.traces = []
            return self.response(req_id, {"enabled": enabled})
        if method == "events.trace":
            since = int(params.get("since_ms", 60000))
            cutoff = NOW() - since
            event_id = params.get("event_id")
            with self.lock:
                entries = [t for t in self.traces if t["at"] >= cutoff]
                if event_id:
                    entries = [t for t in entries if t["event_id"] == event_id]
                return self.response(req_id, entries)
        if method == "emit":
            name = params.get("event", "")
            segments = name.split(".")
            domain = (
                segments[1]
                if segments and segments[0] == "bread" and len(segments) > 1
                else (segments[0] if segments else "")
            )
            reserved = {
                "terminal",
                "git",
                "hyprland",
                "device",
                "power",
                "network",
                "service",
                "container",
                "project",
                "remote",
                "system",
                "profile",
                "notify",
                "command",
                "workflow",
                "bluetooth",
                "workspace",
                "window",
                "monitor",
                "module",
                "state",
                "widget",
                "reload",
                "bug",
            }
            is_command = len(segments) == 4 and segments[:2] == ["bread", "command"]
            if domain in reserved and not is_command:
                return self.error(
                    req_id,
                    f"event '{name}' is in a reserved adapter domain; use a sourced emit or bread.command.<app>.<verb>",
                )
            print(f"fake_breadd: emit {name!r}", file=sys.stderr)
            event = self.event(name, "manual", params.get("data") or {})
            self.publish(event)
            return self.response(req_id, {"emitted": True, "id": event["id"]})
        return self.error(req_id, f"unknown method '{method}'")

    def serve_client(self, conn):
        stream_mode = False
        reader = conn.makefile("rb")
        try:
            for raw in reader:
                if not raw.strip():
                    continue
                try:
                    request = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                if request.get("method") == "events.subscribe":
                    conn.sendall(
                        (
                            json.dumps(
                                self.response(request.get("id"), {"subscribed": True})
                            )
                            + "\n"
                        ).encode()
                    )
                    with self.lock:
                        self.clients.append(conn)
                    stream_mode = True
                    # Hold this connection open for pushes. Reading detects a
                    # client disconnect; breadscope does not write after the
                    # subscribe request.
                    while True:
                        try:
                            if not conn.recv(1):
                                break
                        except OSError:
                            break
                    return
                reply = self.handle(request)
                conn.sendall((json.dumps(reply) + "\n").encode())
        except (OSError, ValueError):
            pass
        finally:
            if stream_mode:
                with self.lock:
                    try:
                        self.clients.remove(conn)
                    except ValueError:
                        pass
            try:
                conn.close()
            except OSError:
                pass


SOCKET_PATH = ""


def main():
    global SOCKET_PATH
    runtime = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
    directory = os.path.join(runtime, "bread")
    os.makedirs(directory, exist_ok=True)
    SOCKET_PATH = os.path.join(directory, "breadd.sock")
    try:
        os.unlink(SOCKET_PATH)
    except FileNotFoundError:
        pass
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(SOCKET_PATH)
    server.listen(32)
    daemon = Daemon()
    daemon.start_ticker()
    print(f"fake_breadd: listening on {SOCKET_PATH}", file=sys.stderr)
    while True:
        conn, _ = server.accept()
        threading.Thread(target=daemon.serve_client, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    main()
