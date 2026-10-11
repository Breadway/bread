//! Window-local styling, resolved through the shared monitor palette.

pub const CSS: &str = r#"
.breadbug { background-color: @bg; color: @on-bg; }
.breadbug .report-header { padding: 18px 24px; border-bottom: 1px solid alpha(@on-bg, 0.12); }
.breadbug .brand { font-weight: 700; font-size: 16px; }
.breadbug .muted { opacity: 0.72; }
.breadbug .eyebrow { font-size: 11px; font-weight: 700; letter-spacing: 1px; }
.breadbug .report-title { font-size: 25px; font-weight: 700; }
.breadbug .step { padding: 6px 10px; border-radius: 8px; font-size: 12px; }
.breadbug .step.current { background-color: alpha(@accent, 0.15); color: @on-bg; }
.breadbug .step.current label { color: @on-bg; }
.breadbug .report-card { background-color: @surface; color: @on-surface; border: 1px solid alpha(@on-bg, 0.12); border-radius: 12px; padding: 16px; }
.breadbug .report-card label { color: @on-surface; }
.breadbug .app-list { background-color: transparent; }
.breadbug .app-list row { background-color: @surface; color: @on-surface; border: 1px solid alpha(@on-bg, 0.12); border-radius: 10px; padding: 14px; margin-bottom: 8px; }
.breadbug .app-list row label { color: @on-surface; }
.breadbug .app-list row:selected { background-color: alpha(@accent, 0.12); border-color: @accent; }
.breadbug .app-list row:selected label { color: @on-bg; }
.breadbug .app-list row:focus-visible { outline: 2px solid @accent; outline-offset: -3px; }
.breadbug .app-icon { background-color: alpha(@on-bg, 0.08); color: @on-surface; border-radius: 10px; padding: 10px; }
.breadbug .app-name, .breadbug .field-label { font-weight: 600; }
.breadbug .version { font-size: 12px; }
.breadbug .notice { border-left: 3px solid @accent; padding: 10px 14px; background-color: @surface; border-radius: 6px; }
.breadbug .notice label { color: @on-surface; }
.breadbug .report-footer { padding-top: 14px; border-top: 1px solid alpha(@on-bg, 0.12); }
.breadbug button { min-height: 20px; padding: 10px 16px; background-image: none; background-color: @surface; color: @on-surface; box-shadow: none; border: 1px solid alpha(@on-bg, 0.12); text-shadow: none; }
.breadbug button label { color: @on-surface; text-shadow: none; }
.breadbug button.primary { background-color: @accent; color: @on-accent; }
.breadbug button.primary label { color: @on-accent; }
.breadbug button.primary:hover { background-color: alpha(@accent, 0.85); }
.breadbug button:focus-visible { outline: 2px solid @accent; outline-offset: 3px; }
.breadbug dropdown > button { background-image: none; background-color: @surface; color: @on-surface; }
.breadbug textview, .breadbug textview text { background-color: @surface; color: @on-surface; caret-color: @on-surface; }
.breadbug textview.notes, .breadbug textview.notes text { font-family: sans-serif; background-color: @surface; color: @on-surface; caret-color: @on-surface; }
.breadbug .notes-frame { border: 1px solid alpha(@on-bg, 0.12); border-radius: 10px; }
.breadbug .notes-frame:focus-within { border-color: @accent; }
.breadbug .report-data { border-radius: 8px; padding: 12px; background-color: @surface; }
.breadbug .hash { font-family: monospace; font-size: 11px; }
.breadbug .metric-value { font-size: 22px; font-weight: 600; }
.breadbug .metric-label { font-size: 12px; opacity: 0.72; }
.breadbug expander > title { padding: 10px 0; font-weight: 600; }
"#;
