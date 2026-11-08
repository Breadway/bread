Packaging
=========

This directory contains distribution packaging for Bread.

```
packaging/
└── systemd/
    └── breadd.service    ← systemd user service unit
```

Prebuilt binaries are distributed exclusively via `bakery` (see the main
README); there is no Arch/pacman package for `bread` anymore. `bakery`
installs the systemd user service unit itself, matching the `packaging/systemd/breadd.service`
file here.

`breadbug-gui` (the GTK4 reporter) and `breadbug-launch` (the display-aware
entry point for keybinds and notification actions) are built from
`breadbug-gui/`, a separate workspace so the daemon build does not need GTK
headers. The `bread` bakery product lists both binaries and `gtk4` in
`system_deps`.

## Windowed reporter

```bash
breadbug-launch            # GUI when a display exists, terminal client otherwise
breadbug-launch --app clip # preselect the app that crashed
breadbug-launch --tty      # always the terminal client
```

Keybinds and notification actions should call `breadbug-launch`; calling
`breadbug` directly skips the windowed front end, and calling `breadbug-gui`
directly fails on a text console.

## systemd user service

The service unit starts `breadd` as a user service after the graphical session is available.

```bash
# Install and enable manually (if not using bakery)
mkdir -p ~/.config/systemd/user
cp systemd/breadd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now breadd

# Check status
systemctl --user status breadd
journalctl --user -u breadd -f
```

The service sets `RUST_LOG=info` by default. To increase verbosity, override it in a drop-in:

```ini
# ~/.config/systemd/user/breadd.service.d/debug.conf
[Service]
Environment=RUST_LOG=debug
```
