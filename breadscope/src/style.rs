//! Window-local CSS for breadscope.
//!
//! The shared bread-theme sheet owns the palette, typography, and common
//! controls. These rules add only this app's layout language: the rail, stream
//! rows, inspectors, cards, and flow side panel. All colors reference the
//! shared `@define-color` names so a wallpaper change recolors the app live.

pub const CSS: &str = r#"
window.scope, .scope {
    background-color: @bg;
    color: @fg;
}

.mono { font-family: monospace; }
.dim { color: alpha(@fg, 0.55); }
.faint { color: alpha(@fg, 0.38); }
.accent { color: @accent; }
.ok { color: @green; }
.warn { color: @yellow; }
.bad { color: @red; }
.small { font-size: 11px; }
.tiny { font-size: 10px; }
.bold { font-weight: 700; }

.topbar {
    background-color: @surface;
    border-bottom: 1px solid alpha(@fg, 0.09);
    padding: 9px 14px;
}
.brand-name { font-size: 17px; font-weight: 700; }
.brand-sub { font-size: 9px; color: alpha(@fg, 0.42); letter-spacing: 2px; }
.mock-tag {
    color: @accent;
    border: 1px solid alpha(@accent, 0.45);
    background-color: alpha(@accent, 0.10);
    border-radius: 999px;
    padding: 2px 8px;
    font-size: 9px;
    letter-spacing: 1px;
}
.status-pill {
    border: 1px solid alpha(@fg, 0.12);
    background-color: @bg;
    border-radius: 999px;
    padding: 4px 10px;
    font-size: 11px;
    color: alpha(@fg, 0.65);
}
.status-pill.ok { color: @green; }
.status-pill.warn { color: @yellow; }
.status-pill.bad { color: @red; }
.top-meta { font-family: monospace; font-size: 10px; color: alpha(@fg, 0.42); }

.sidebar {
    background-color: shade(@bg, 0.92);
    border-right: 1px solid alpha(@fg, 0.08);
    padding: 10px 8px;
}
.nav-group {
    font-size: 9px;
    letter-spacing: 1.5px;
    color: alpha(@fg, 0.30);
    padding: 12px 10px 5px;
}
.nav-row {
    background: transparent;
    border: 0;
    border-radius: 7px;
    padding: 7px 9px;
    color: alpha(@fg, 0.58);
    min-height: 24px;
}
.nav-row:hover { background-color: alpha(@fg, 0.05); color: @fg; }
.nav-row.active {
    background-color: alpha(@accent, 0.13);
    color: @accent;
    border-left: 2px solid @accent;
}
.nav-row .nav-count {
    font-family: monospace;
    font-size: 9px;
    color: alpha(@fg, 0.35);
}
.nav-row.active .nav-count { color: @accent; }

.rail-card {
    background-color: alpha(@fg, 0.04);
    border: 1px solid alpha(@fg, 0.07);
    border-radius: 8px;
    padding: 9px;
}
.socket { font-family: monospace; font-size: 9px; color: alpha(@fg, 0.38); }

.toolbar {
    background-color: shade(@bg, 0.94);
    border-bottom: 1px solid alpha(@fg, 0.08);
    padding: 8px 12px;
}
entry, .toolbar entry {
    background-color: @surface;
    color: @fg;
    border: 1px solid alpha(@fg, 0.12);
    border-radius: 6px;
    padding: 5px 8px;
    font-family: monospace;
    font-size: 11px;
}
entry:focus { border-color: alpha(@accent, 0.65); }
button { border-radius: 6px; }
.btn {
    background-color: alpha(@fg, 0.06);
    color: @fg;
    border: 1px solid alpha(@fg, 0.10);
    padding: 5px 10px;
    font-size: 11px;
}
.btn:hover { background-color: alpha(@fg, 0.10); }
.btn.primary { background-color: @accent; color: @on-accent; border-color: transparent; font-weight: 700; }
.btn.primary:hover { background-color: alpha(@accent, 0.85); }
.btn.ghost { background: transparent; }
.btn.small { padding: 3px 8px; font-size: 10px; }
button.pill {
    border-radius: 999px;
    padding: 3px 9px;
    font-size: 10px;
    background-color: alpha(@fg, 0.05);
    border: 1px solid transparent;
    color: alpha(@fg, 0.55);
}
button.pill:hover { color: @fg; background-color: alpha(@fg, 0.09); }
button.pill.active, .pill.active {
    color: @accent;
    background-color: alpha(@accent, 0.18);
    border-color: alpha(@accent, 0.45);
}
button.pill:checked, togglebutton.pill:checked, .pill:checked {
    color: @fg;
    background-color: alpha(@fg, 0.13);
    border-color: alpha(@fg, 0.15);
}
togglebutton.chip {
    border-radius: 999px;
    padding: 2px 8px;
    font-size: 10px;
    background-color: alpha(@fg, 0.04);
    border: 1px solid transparent;
    color: alpha(@fg, 0.55);
}
button.chip:checked, togglebutton.chip:checked, .chip:checked {
    color: @fg;
    background-color: alpha(@fg, 0.13);
    border-color: alpha(@fg, 0.16);
}

.events-scroll { background-color: @bg; }
button.event-row {
    background: transparent;
    border: 0;
    border-radius: 0;
    border-bottom: 1px solid alpha(@fg, 0.05);
    padding: 6px 10px;
    min-height: 38px;
}
button.event-row:hover { background-color: alpha(@fg, 0.035); }
button.event-row.selected {
    background-color: alpha(@accent, 0.10);
    border-left: 3px solid @accent;
    padding-left: 7px;
}
.event-time { font-family: monospace; font-size: 9px; color: alpha(@fg, 0.38); }
.event-name { font-family: monospace; font-size: 11px; color: alpha(@fg, 0.88); }
button.event-row.selected .event-name { color: @accent; }
.event-summary { font-size: 9.5px; color: alpha(@fg, 0.42); }
.event-dot { font-size: 9px; }
.mini-tag {
    font-family: monospace;
    font-size: 8px;
    color: alpha(@fg, 0.38);
    border: 1px solid alpha(@fg, 0.10);
    border-radius: 4px;
    padding: 0 4px;
}
.mini-tag.by { color: alpha(@accent, 0.85); border-color: alpha(@accent, 0.30); }
.mini-tag.builtin { color: @green; border-color: alpha(@green, 0.25); }

.inspector { background-color: shade(@bg, 0.96); border-left: 1px solid alpha(@fg, 0.08); padding: 14px; }
.insp-title { font-family: monospace; font-size: 13px; color: @fg; }
.insp-source { font-size: 10px; color: alpha(@fg, 0.55); border: 1px solid alpha(@fg, 0.10); border-radius: 999px; padding: 1px 8px; }
.section-label {
    font-size: 9px;
    letter-spacing: 1.2px;
    color: alpha(@fg, 0.35);
    margin-top: 14px;
}
.section-rule { background-color: alpha(@fg, 0.07); min-height: 1px; }
.chain-btn {
    background: transparent;
    border: 1px solid transparent;
    border-radius: 5px;
    padding: 4px 6px;
    font-family: monospace;
    font-size: 10px;
    color: alpha(@fg, 0.65);
}
.chain-btn:hover { background-color: alpha(@fg, 0.05); color: @fg; }
.chain-btn.current { background-color: alpha(@accent, 0.12); border-color: alpha(@accent, 0.35); color: @accent; }
.handler-row { background-color: alpha(@fg, 0.03); border-radius: 5px; padding: 5px 7px; }
.handler-name { font-family: monospace; font-size: 10.5px; color: @fg; }
.handler-pattern { font-family: monospace; font-size: 8.5px; color: alpha(@fg, 0.40); }
.duration { font-family: monospace; font-size: 9px; color: @green; }
.duration.slow { color: @yellow; }
.note { font-size: 10px; color: alpha(@fg, 0.42); }
.json-view {
    background-color: shade(@bg, 0.90);
    border: 1px solid alpha(@fg, 0.07);
    border-radius: 6px;
    padding: 8px;
    font-family: monospace;
    font-size: 10px;
    color: alpha(@fg, 0.78);
}
.json-view text { background-color: transparent; color: alpha(@fg, 0.78); }

.volume { background-color: shade(@bg, 0.94); }

.page { padding: 14px 18px; }
.page-title { font-size: 17px; font-weight: 700; }
.page-sub { font-size: 10.5px; color: alpha(@fg, 0.45); }
.stat-card {
    background-color: @surface;
    border: 1px solid alpha(@fg, 0.09);
    border-radius: 8px;
    padding: 8px 11px;
}
.stat-value { font-size: 20px; font-weight: 700; }
.stat-label { font-size: 9px; color: alpha(@fg, 0.38); }

.module-card {
    background-color: @surface;
    border: 1px solid alpha(@fg, 0.09);
    border-radius: 8px;
    padding: 10px;
    min-width: 260px;
}
.module-card:hover { border-color: alpha(@fg, 0.20); background-color: shade(@surface, 1.06); }
.module-card.selected { border-color: alpha(@accent, 0.55); background-color: alpha(@accent, 0.07); }
.module-card.degraded { border-color: alpha(@red, 0.35); }
.module-name { font-family: monospace; font-size: 11px; color: @fg; }
.card-stat { font-family: monospace; font-size: 8.5px; color: alpha(@fg, 0.38); }
.module-error {
    font-family: monospace;
    font-size: 8.5px;
    color: alpha(@red, 0.90);
    background-color: alpha(@red, 0.07);
    border: 1px solid alpha(@red, 0.18);
    border-radius: 4px;
    padding: 3px 5px;
}
.pattern-chip {
    font-family: monospace;
    font-size: 8px;
    color: alpha(@fg, 0.50);
    background-color: shade(@bg, 0.90);
    border: 1px solid alpha(@fg, 0.08);
    border-radius: 4px;
    padding: 1px 4px;
}
.detail-panel { background-color: @surface; border: 1px solid alpha(@fg, 0.09); border-radius: 8px; padding: 12px; }
.detail-key { font-size: 9px; color: alpha(@fg, 0.36); }
.detail-value { font-family: monospace; font-size: 9.5px; color: alpha(@fg, 0.65); }

.flow-side { background-color: shade(@bg, 0.96); border-left: 1px solid alpha(@fg, 0.08); padding: 14px; }
.path-row { border-bottom: 1px solid alpha(@fg, 0.05); padding: 4px 0; }
.path-name { font-family: monospace; font-size: 9px; color: alpha(@fg, 0.62); }
.path-count { font-family: monospace; font-size: 8.5px; color: alpha(@fg, 0.35); }
.path-bar { background-color: alpha(@fg, 0.08); border-radius: 2px; min-height: 3px; }
.path-fill { background-color: alpha(@accent, 0.55); border-radius: 2px; min-height: 3px; }

.badge {
    font-family: monospace;
    font-size: 8.5px;
    border-radius: 999px;
    padding: 1px 6px;
    border: 1px solid alpha(@fg, 0.12);
    color: alpha(@fg, 0.55);
}
.badge.green { color: @green; border-color: alpha(@green, 0.30); background-color: alpha(@green, 0.06); }
.badge.red { color: @red; border-color: alpha(@red, 0.30); background-color: alpha(@red, 0.06); }
.badge.yellow { color: @yellow; border-color: alpha(@yellow, 0.30); background-color: alpha(@yellow, 0.06); }
.badge.blue { color: @blue; border-color: alpha(@blue, 0.30); background-color: alpha(@blue, 0.06); }

.state-key { background: transparent; border: 1px solid transparent; border-radius: 5px; padding: 4px 7px; font-family: monospace; font-size: 10px; color: alpha(@fg, 0.55); }
.state-key:hover { background-color: alpha(@fg, 0.05); }
.state-key.active { background-color: alpha(@accent, 0.12); border-color: alpha(@accent, 0.30); color: @accent; }
.table-head { font-size: 8.5px; letter-spacing: 1px; color: alpha(@fg, 0.35); }
.table-row { border-bottom: 1px solid alpha(@fg, 0.05); padding: 5px 2px; }
.table-text { font-size: 10px; color: alpha(@fg, 0.62); }
.widget-card { background-color: @surface; border: 1px solid alpha(@fg, 0.09); border-radius: 8px; }
.widget-preview { background-color: shade(@surface, 0.92); border-bottom: 1px solid alpha(@fg, 0.07); padding: 18px; }
.wlabel { font-size: 14px; }
.wlabel.fg { color: @fg; } .wlabel.dim { color: alpha(@fg, 0.50); }
.wlabel.accent { color: @accent; } .wlabel.red { color: @red; }
.wlabel.green { color: @green; } .wlabel.yellow { color: @yellow; }
.wlabel.blue { color: @blue; } .wlabel.pink { color: @pink; } .wlabel.teal { color: @teal; }
.wlabel.bold { font-weight: 700; }
.wlabel.xs { font-size: 10px; } .wlabel.sm { font-size: 12px; }
.wlabel.md { font-size: 14px; } .wlabel.lg { font-size: 16px; } .wlabel.xl { font-size: 20px; }
.wlabel.card-bg { background-color: @surface; border-radius: 8px; padding: 8px; }
.wlabel.surface-bg { background-color: shade(@surface, 0.94); }
.wlabel.radius-sm { border-radius: 4px; } .wlabel.radius-md { border-radius: 8px; } .wlabel.radius-full { border-radius: 999px; }
.wlabel.pad-xs { padding: 2px; } .wlabel.pad-sm { padding: 4px; } .wlabel.pad-md { padding: 8px; }
.wprogress trough { background-color: alpha(@fg, 0.10); min-height: 5px; min-width: 54px; border-radius: 3px; }
.wprogress progress { background-color: @accent; min-height: 5px; border-radius: 3px; }

.toast {
    background-color: shade(@surface, 1.10);
    border: 1px solid alpha(@fg, 0.18);
    border-radius: 7px;
    padding: 7px 12px;
    font-size: 11px;
    color: @fg;
}
.emit-dialog { background-color: @surface; border: 1px solid alpha(@fg, 0.18); border-radius: 10px; padding: 16px; }
.dialog-title { font-size: 15px; font-weight: 700; }
.error-box {
    background-color: alpha(@red, 0.08);
    border: 1px solid alpha(@red, 0.30);
    border-radius: 5px;
    padding: 6px 8px;
    font-family: monospace;
    font-size: 9.5px;
    color: alpha(@red, 0.95);
}
"#;

/// Apply the app sheet globally as well as per window. The per-window bind
/// needs a resolved monitor; a headless Broadway server has none, so applying
/// at display priority keeps screenshots faithful to a real session. Matches
/// breadbug-gui's approach.
pub fn apply_global() {
    static APPLIED: std::sync::Once = std::sync::Once::new();
    APPLIED.call_once(|| {
        let Some(display) = gtk4::gdk::Display::default() else {
            return;
        };
        let palette = bread_theme::load_palette();
        let provider = gtk4::CssProvider::new();
        provider.load_from_string(&format!("{}\n{}", bread_theme::css_vars(&palette), CSS));
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}
