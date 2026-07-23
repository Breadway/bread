Packaging
=========

This directory contains distribution packaging for Bread.

```
packaging/
└── systemd/
    └── breadd.service    ← systemd user service unit
```

Prebuilt binaries are distributed exclusively via `bakery` (see the main
README) — there is no Arch/pacman package for `bread` anymore. `bakery`
installs the systemd user service unit itself, matching the `packaging/systemd/breadd.service`
file here.

## systemd user service

The service unit starts `breadd` as a user service after the graphical session is available.

```bash
# Install and enable manually (if not using the PKGBUILD)
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
