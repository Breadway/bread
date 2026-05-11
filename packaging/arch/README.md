Arch packaging
==============

`PKGBUILD` builds and installs both `breadd` and `bread` from source.

## Local build

```bash
makepkg -si
```

## Before publishing to AUR

1. Tag a release on GitHub.
2. Update `pkgver` to match the tag.
3. Update `source` to the release tarball URL.
4. Run `updpkgsums` (or manually set `sha256sums`).
5. Update `url` if the repository has moved.
6. Set `depends` accurately — at minimum: `glibc`. Add `udev` and `libgit2` if not linking statically.

## Runtime dependencies

| Package | Required | Notes |
|---------|----------|-------|
| `glibc` | yes | always |
| `udev` | yes | device events |
| `dbus` | optional | UPower battery events |
| `libnotify` | optional | `bread.notify()` (uses `notify-send`) |
| `git` | optional | `bread sync` push/pull |
