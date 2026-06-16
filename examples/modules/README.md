# Example bread modules

Ready-to-use modules for common desktop automations. Unlike the snippets in
[`../../Examples.md`](../../Examples.md) (which teach the porting patterns),
these are complete files you can drop in as-is.

## Installing

Modules in `~/.config/bread/modules/` are **auto-discovered** — copy a file in
and reload; no `init.lua` edit needed:

```sh
cp low-battery-warning.lua ~/.config/bread/modules/
bread reload
```

## Modules

| File | What it does | Config needed |
|------|--------------|---------------|
| `low-battery-warning.lua` | Critical notification once when the battery runs low; resets on AC. | none |
| `pause-media-on-headphone-unplug.lua` | Runs `playerctl pause` when a headphone/earbud device disconnects. | none (needs `playerctl`) |
| `dock-monitors.lua` | Applies a multi-monitor layout when an external display connects, reverts when removed. | edit output names/resolutions |

Each module is the standard skeleton — `bread.module{...}`, an `on_load` that
registers subscriptions, `return M` — so they double as references for writing
your own. See [`../../Documentation.md`](../../Documentation.md) for the full
event list and Lua API.
