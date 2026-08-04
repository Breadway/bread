# Deprecations

Tracks API surface currently in its deprecation window per
[`Documentation.md`'s API Stability & Versioning](Documentation.md#api-stability--versioning)
policy — marked `Deprecated` and still functioning, pending removal in a
future major version.

## Hyprland legacy flat event names (since v1.5)

**What's deprecated:** the 10 pre-namespace Hyprland event names emitted by
`normalize_hyprland()` in `breadd/src/core/normalizer.rs`:

- `bread.workspace.changed`, `bread.workspace.created`, `bread.workspace.destroyed`
- `bread.monitor.connected`, `bread.monitor.disconnected`
- `bread.window.focus.changed`, `bread.window.focused`, `bread.window.opened`,
  `bread.window.closed`, `bread.window.moved`

**Recommended form going forward:** their `bread.hyprland.<rest>` equivalents
(e.g. `bread.hyprland.workspace.changed`), which make explicit that these
events are Hyprland-specific rather than portable across a future second
compositor backend — see `Documentation.md`'s
[Hyprland event reference](Documentation.md#hyprland) for the full mapping.

**Current behavior:** both names fire by default (`[compat]
legacy_hyprland_event_names = true`). Setting that flag to `false` suppresses
the legacy names; only `bread.hyprland.*` fires.

**Deferred follow-up (not yet scheduled):**

1. Flip `[compat] legacy_hyprland_event_names`'s *default* to `false` in a
   later minor release, once downstream modules have had a full deprecation
   window to migrate.
2. Remove the legacy flat names and the `[compat]` flag entirely in the next
   major version (v2) — at that point `normalize_hyprland()` only ever
   produces `bread.hyprland.*` names and the dual-emit machinery
   (`emit_hyprland_dual` in `normalizer.rs`) can be deleted.

Neither step is scheduled yet; this file exists so the removal isn't
forgotten once the window closes. No issue tracker is wired up to this repo,
so this note is the tracking mechanism until one exists.
