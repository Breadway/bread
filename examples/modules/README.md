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
| `active-window-widget.lua` | Shows the focused window next to the workspace pills in breadbar, via `bread.widget` + `bread.state.watch`. | none |
| `cpu-temp-widget.lua` | Live CPU temperature readout in breadbar's stats area, via `bread.widget` + `bread.fs.read` on a timer. | edit `TEMP_PATH` for your hwmon layout |
| `bluetooth-toggle-widget.lua` | One-click Bluetooth power toggle in breadbar's tray, via `bread.widget` + a click handler. | none |
| `focus-mode-widget.lua` | Click-to-toggle "Focus" profile that mutes audio; a widget as an action launcher, not just a readout, and stays in sync with profile changes triggered elsewhere. | none (needs `wpctl`) |
| `workflow-status-widget.lua` | Surfaces `bread.workflow.list()` in breadbar's tray — shows whichever workflow (e.g. `dock-workflow.lua`, below) is currently running or failed, hidden otherwise. | none |

Each module is the standard skeleton — `bread.module{...}`, an `on_load` that
registers subscriptions, `return M` — so they double as references for writing
your own. See [`../../Documentation.md`](../../Documentation.md) for the full
event list and Lua API.
