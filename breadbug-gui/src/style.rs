//! Window-local CSS for controls the shared theme leaves unset.
//!
//! The shared sheet owns the palette and window background. Plain GTK widgets
//! (buttons, dropdowns, text views) otherwise fall back to the Adwaita
//! defaults, which are light and unreadable against the fixed-dark base. These
//! rules keep every control on the shared palette so a headless snapshot shows
//! the same contrast a real session does.

pub const CSS: &str = r#"
.breadbug button {
    background-image: none;
    background-color: @surface;
    color: @on-surface;
    border: 1px solid alpha(@on-bg, 0.12);
    box-shadow: none;
    text-shadow: none;
}
.breadbug button label { color: @on-surface; text-shadow: none; }
.breadbug button.suggested-action {
    background-color: @accent;
    color: @on-accent;
}
.breadbug button.suggested-action label { color: @on-accent; }
.breadbug button:hover { background-color: alpha(@on-bg, 0.08); }
.breadbug button.suggested-action:hover { background-color: alpha(@accent, 0.85); }
.breadbug button:focus-visible { outline: 2px solid @accent; outline-offset: 3px; }
.breadbug dropdown > button {
    background-image: none;
    background-color: @surface;
    color: @on-surface;
}
.breadbug dropdown > button label { color: @on-surface; }
.breadbug textview,
.breadbug textview text {
    background-color: @surface;
    color: @on-surface;
    caret-color: @on-surface;
}
"#;

/// Apply the rules above at display priority as well as per window.
///
/// `bind_window_auto_with_app_css` only attaches the window's sheet once a
/// monitor output is resolved. An isolated headless display (the walkthrough's
/// Broadway server) has none, so the window sheet never loads and controls
/// would fall back to the light Adwaita defaults in a snapshot. The same rules
/// applied globally keep headless evidence faithful to a real BOS session.
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
