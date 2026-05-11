Packaging
=========

This directory contains distribution packaging for Bread.

```
packaging/
├── arch/
│   └── PKGBUILD          ← Arch Linux package build script
└── systemd/
    └── breadd.service    ← systemd user service unit
```

## Arch Linux

```bash
cd packaging/arch
makepkg -si
```

The PKGBUILD builds both `breadd` and `bread` from source and installs them to `/usr/bin`. It also installs the systemd user service unit to `/usr/lib/systemd/user/`.

Before publishing to the AUR, update `pkgver`, `source`, and `sha256sums` to point at a tagged release tarball.

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
