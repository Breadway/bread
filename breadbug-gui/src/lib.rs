//! The breadbug window: pick an app, say what happened, read exactly what a
//! report contains, then save it, send it, or cancel.
//!
//! All behaviour that matters (what is built, what is sent, when sending is
//! refused) lives in `breadbug::session`, which the CLI shares, so this file
//! is only layout and wiring. Slow work (building a report runs helper
//! commands; sending touches the network) runs off the UI thread.

mod style;
pub mod text;

use std::cell::RefCell;
use std::rc::Rc;

use bread_bug::consent::{self, CONSENT_TEXT};
use bread_bug::report::{AppEntry, Prepared};
use breadbug::session::{self, SendOutcome};
use breadbug::Env;
use gtk4::prelude::*;
use gtk4::{
    gio, glib, Application, ApplicationWindow, Box as GBox, Button, CheckButton, DropDown,
    Expander, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, SelectionMode, Stack,
    TextView, WrapMode,
};

struct State {
    env: Env,
    apps: Vec<AppEntry>,
    app: Option<String>,
    live_pid: Option<u32>,
    prepared: Option<Prepared>,
}

type Shared = Rc<RefCell<State>>;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn page() -> GBox {
    let b = GBox::new(Orientation::Vertical, 12);
    b.set_margin_top(24);
    b.set_margin_bottom(20);
    b.set_margin_start(24);
    b.set_margin_end(24);
    b
}

fn heading(s: &str) -> Label {
    let l = para(s);
    l.add_css_class("report-title");
    l.set_xalign(0.0);
    l
}

fn para(s: &str) -> Label {
    let l = Label::new(Some(s));
    l.set_wrap(true);
    l.set_xalign(0.0);
    l.set_max_width_chars(90);
    l.set_selectable(false);
    l
}

fn buttons() -> GBox {
    let b = GBox::new(Orientation::Horizontal, 8);
    b.add_css_class("report-footer");
    b.set_margin_top(8);
    b
}

fn spacer() -> GBox {
    let b = GBox::new(Orientation::Vertical, 0);
    b.set_vexpand(true);
    b
}

fn muted(s: &str) -> Label {
    let l = para(s);
    l.add_css_class("muted");
    l
}

fn primary(button: &Button) {
    button.add_css_class("suggested-action");
    button.add_css_class("primary");
}

fn actions(left: Option<&Button>, right: &[&Button]) -> GBox {
    let b = buttons();
    if let Some(button) = left {
        b.append(button);
    }
    let space = GBox::new(Orientation::Horizontal, 0);
    space.set_hexpand(true);
    b.append(&space);
    for button in right {
        b.append(*button);
    }
    b
}

fn notice(s: &str) -> GBox {
    let b = GBox::new(Orientation::Horizontal, 12);
    b.add_css_class("notice");
    let icon = Image::from_icon_name("security-high-symbolic");
    icon.set_valign(gtk4::Align::Start);
    b.append(&icon);
    let label = para(s);
    label.set_hexpand(true);
    b.append(&label);
    b
}

fn section(title: &str, child: &impl IsA<gtk4::Widget>) -> Expander {
    let e = Expander::new(Some(title));
    e.set_child(Some(child));
    e
}

fn metric(value: usize, title: &str) -> GBox {
    let b = GBox::new(Orientation::Vertical, 4);
    b.set_hexpand(true);
    let number = para(&value.to_string());
    number.add_css_class("metric-value");
    let label = para(title);
    label.add_css_class("metric-label");
    b.append(&number);
    b.append(&label);
    b
}

fn mono_view(content: &str) -> ScrolledWindow {
    let tv = TextView::new();
    tv.set_editable(false);
    tv.set_cursor_visible(false);
    tv.set_monospace(true);
    tv.set_wrap_mode(WrapMode::None);
    tv.set_top_margin(12);
    tv.set_bottom_margin(12);
    tv.set_left_margin(12);
    tv.set_right_margin(12);
    tv.buffer().set_text(content);
    let sw = ScrolledWindow::new();
    sw.set_child(Some(&tv));
    sw.set_vexpand(true);
    sw.set_min_content_height(220);
    sw.add_css_class("report-data");
    sw
}

fn report_overview(prepared: &Prepared) -> GBox {
    let report = &prepared.report;
    let b = GBox::new(Orientation::Vertical, 12);
    let counts = GBox::new(Orientation::Horizontal, 16);
    counts.add_css_class("report-card");
    counts.append(&metric(
        report.logs.len() + report.daemon_logs.len(),
        "Log lines",
    ));
    counts.append(&metric(report.markers.len(), "Crash / freeze records"));
    counts.append(&metric(report.events.len(), "Events"));
    counts.append(&metric(report.journal.len(), "Journal lines"));
    b.append(&counts);
    b.append(&notice(&format!(
        "{} Nothing has been sent.",
        text::redaction_summary(&report.redactions)
    )));
    if let Some(notes) = report.notes.as_deref().filter(|s| !s.is_empty()) {
        let card = GBox::new(Orientation::Vertical, 6);
        card.add_css_class("report-card");
        let label = para("Your notes, after scrubbing");
        label.add_css_class("field-label");
        card.append(&label);
        let notes = para(notes);
        notes.set_selectable(true);
        card.append(&notes);
        b.append(&card);
    }
    let system = para(&format!(
        "App: {} {}\nSystem: {} · {}\nKernel: {}\nAlso includes: installed app versions, runtime status and any consented stack trace.",
        text::app_name(&report.app.id),
        report.app.version.as_deref().unwrap_or("(version unknown)"),
        report.system.os_id.as_deref().unwrap_or("unknown"),
        report.system.arch,
        report.system.kernel.as_deref().unwrap_or("unknown"),
    ));
    system.set_selectable(true);
    b.append(&section("App and system details", &system));
    let full = section(
        "Full report · exact data saved or sent",
        &mono_view(&String::from_utf8_lossy(&prepared.bytes)),
    );
    b.append(&full);
    let fingerprint = para(&format!("SHA-256  {}", prepared.sha256));
    fingerprint.add_css_class("hash");
    fingerprint.add_css_class("muted");
    fingerprint.set_selectable(true);
    b.append(&fingerprint);
    b
}

/// The widgets a front-end test or screenshot tool needs to drive the window
/// the way a user would.
pub struct Ui {
    pub window: ApplicationWindow,
    pub stack: Stack,
    pub list: ListBox,
    pub next: Button,
    pub type_dropdown: DropDown,
    pub notes: TextView,
    pub preview: Button,
    pub save: Button,
    pub send: Button,
    pub status_line: Label,
    pub summary: Label,
    pub done_title: Label,
    pub done_body: Label,
    pub consent_continue: Button,
}

/// Build and show the window.
pub fn build_window(app: &Application, env: Env, preselect: Option<String>) -> Ui {
    bread_theme::gtk::apply_shared();

    let apps = session::apps(&env);
    let first_run = !consent::seen(&env.state_dir);
    let state: Shared = Rc::new(RefCell::new(State {
        env,
        apps,
        app: None,
        live_pid: None,
        prepared: None,
    }));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Report a problem")
        .default_width(820)
        .default_height(760)
        .build();
    window.add_css_class("breadbug");
    bread_theme::gtk::bind_window_auto_with_app_css(&window, |_| style::CSS.to_string());

    let stack = Stack::new();
    stack.set_vexpand(true);
    let root = GBox::new(Orientation::Vertical, 0);
    let header = GBox::new(Orientation::Horizontal, 16);
    header.add_css_class("report-header");
    let brand = Label::new(Some("breadbug"));
    brand.add_css_class("brand");
    brand.set_hexpand(true);
    brand.set_xalign(0.0);
    header.append(&brand);
    let steps: Vec<Label> = ["1  Choose app", "2  Describe", "3  Review"]
        .iter()
        .map(|name| {
            let l = Label::new(Some(name));
            l.add_css_class("step");
            l.set_visible(false);
            header.append(&l);
            l
        })
        .collect();
    root.append(&header);
    root.append(&stack);

    // ------------------------------------------------------------ consent
    let consent_page = page();
    let eyebrow = muted("YOUR REPORT, YOUR CHOICE");
    eyebrow.add_css_class("eyebrow");
    consent_page.append(&eyebrow);
    consent_page.append(&heading("Before you start"));
    let consent_card = GBox::new(Orientation::Vertical, 12);
    consent_card.add_css_class("report-card");
    consent_card.append(&para(CONSENT_TEXT));
    let consent_scroll = ScrolledWindow::new();
    consent_scroll.set_child(Some(&consent_card));
    consent_scroll.set_propagate_natural_height(true);
    consent_scroll.set_vexpand(false);
    consent_page.append(&consent_scroll);
    consent_page.append(&spacer());
    consent_page.append(&notice(
        "Review first. Save a copy or choose to send it. You stay in control.",
    ));
    let (c_quit, c_go) = (Button::with_label("Quit"), Button::with_label("Continue"));
    primary(&c_go);
    consent_page.append(&actions(Some(&c_quit), &[&c_go]));
    stack.add_named(&consent_page, Some("consent"));

    // --------------------------------------------------------------- pick
    let pick_page = page();
    pick_page.append(&heading("Which app has the problem?"));
    pick_page.append(&muted(
        "Choose an installed app. Recent crashes and freezes appear first.",
    ));
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.add_css_class("app-list");
    {
        let st = state.borrow();
        let now = now_ms();
        for e in &st.apps {
            let row = ListBoxRow::new();
            let line = GBox::new(Orientation::Horizontal, 14);
            let icon = Image::from_icon_name(text::app_icon(&e.app));
            icon.add_css_class("app-icon");
            icon.set_pixel_size(24);
            line.append(&icon);
            let labels = GBox::new(Orientation::Vertical, 4);
            labels.set_hexpand(true);
            let name = para(&text::app_name(&e.app));
            name.add_css_class("app-name");
            labels.append(&name);
            labels.append(&muted(&text::app_problem(e, now)));
            line.append(&labels);
            let version = muted(e.version.as_deref().unwrap_or("Version unknown"));
            version.add_css_class("version");
            line.append(&version);
            row.set_child(Some(&line));
            list.append(&row);
        }
    }
    let list_scroll = ScrolledWindow::new();
    list_scroll.set_child(Some(&list));
    list_scroll.set_propagate_natural_height(true);
    list_scroll.set_max_content_height(360);
    pick_page.append(&list_scroll);
    let empty_hint = notice("No reportable apps found. Installed bread apps appear here unless excluded in Bug reports settings.");
    empty_hint.set_visible(state.borrow().apps.is_empty());
    pick_page.append(&empty_hint);
    pick_page.append(&spacer());
    pick_page.append(&notice(
        "Nothing is sent automatically. You can inspect the report before deciding.",
    ));
    let (p_cancel, p_next) = (
        Button::with_label("Cancel"),
        Button::with_label("Describe problem"),
    );
    primary(&p_next);
    p_next.set_sensitive(false);
    pick_page.append(&actions(Some(&p_cancel), &[&p_next]));
    stack.add_named(&pick_page, Some("pick"));

    // ------------------------------------------------------------ details
    let details_page = page();
    let details_title = heading("");
    details_page.append(&details_title);
    details_page.append(&muted("A few details help us understand what went wrong."));
    let type_label = para("What happened?");
    type_label.add_css_class("field-label");
    details_page.append(&type_label);
    let type_dd = DropDown::from_strings(&text::TYPE_LABELS);
    type_dd.set_tooltip_text(Some("Choose the kind of problem you experienced"));
    details_page.append(&type_dd);
    let notes_label = para("Anything else? (optional)");
    notes_label.add_css_class("field-label");
    details_page.append(&notes_label);
    details_page.append(&muted(
        "What were you doing, and what did you expect to happen?",
    ));
    let notes = TextView::new();
    notes.set_wrap_mode(WrapMode::WordChar);
    notes.add_css_class("notes");
    notes.set_tooltip_text(Some(
        "Describe the problem in your own words. Do not include passwords or private text.",
    ));
    notes.set_top_margin(12);
    notes.set_bottom_margin(12);
    notes.set_left_margin(12);
    notes.set_right_margin(12);
    let notes_scroll = ScrolledWindow::new();
    notes_scroll.set_child(Some(&notes));
    notes_scroll.set_vexpand(true);
    notes_scroll.set_min_content_height(120);
    notes_scroll.add_css_class("notes-frame");
    details_page.append(&notes_scroll);
    let live_check = CheckButton::with_label(
        "Include a stack trace of the frozen app (function names and offsets only, no memory)",
    );
    live_check.set_visible(false);
    details_page.append(&live_check);
    details_page.append(&notice("Please leave out passwords and private text. Personal details are scrubbed, and you can review the result next."));
    let (d_back, d_preview) = (
        Button::with_label("Back"),
        Button::with_label("Preview report"),
    );
    primary(&d_preview);
    details_page.append(&actions(Some(&d_back), &[&d_preview]));
    stack.add_named(&details_page, Some("details"));

    // ------------------------------------------------------------ preview
    let preview_page = page();
    preview_page.append(&heading("Review your report"));
    let summary = para("");
    summary.add_css_class("muted");
    preview_page.append(&summary);
    let preview_holder = GBox::new(Orientation::Vertical, 0);
    preview_holder.set_vexpand(true);
    let preview_scroll = ScrolledWindow::new();
    preview_scroll.set_child(Some(&preview_holder));
    preview_scroll.set_vexpand(true);
    preview_page.append(&preview_scroll);
    let status_line = para("");
    preview_page.append(&status_line);
    let (v_back, v_save, v_send, v_cancel) = (
        Button::with_label("Back"),
        Button::with_label("Save to a file…"),
        Button::with_label("Send"),
        Button::with_label("Cancel"),
    );
    v_save.set_sensitive(false);
    v_send.set_sensitive(false);
    primary(&v_send);
    preview_page.append(&actions(Some(&v_back), &[&v_cancel, &v_save, &v_send]));
    stack.add_named(&preview_page, Some("preview"));

    // --------------------------------------------------------------- done
    let done_page = page();
    let done_title = heading("");
    let done_body = para("");
    done_page.append(&done_title);
    done_page.append(&done_body);
    done_page.append(&spacer());
    let d_close = Button::with_label("Close");
    d_close.set_halign(gtk4::Align::End);
    primary(&d_close);
    done_page.append(&actions(None, &[&d_close]));
    stack.add_named(&done_page, Some("done"));

    window.set_child(Some(&root));

    // Keep progress and the keyboard default in sync, including preselection.
    {
        let window = window.clone();
        let defaults = [p_next.clone(), d_preview.clone(), v_save.clone()];
        let consent = c_go.clone();
        let close = d_close.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            let name = stack.visible_child_name();
            let index = match name.as_deref() {
                Some("pick") => Some(0),
                Some("details") => Some(1),
                Some("preview") => Some(2),
                _ => None,
            };
            for (i, step) in steps.iter().enumerate() {
                step.set_visible(index.is_some());
                if index == Some(i) {
                    step.add_css_class("current");
                } else {
                    step.remove_css_class("current");
                }
            }
            let default = index.map_or_else(
                || {
                    if name.as_deref() == Some("consent") {
                        &consent
                    } else {
                        &close
                    }
                },
                |i| &defaults[i],
            );
            window.set_default_widget(Some(default));
        });
    }

    // ------------------------------------------------------------ wiring
    {
        let w = window.clone();
        c_quit.connect_clicked(move |_| w.close());
    }
    {
        let w = window.clone();
        p_cancel.connect_clicked(move |_| w.close());
        let w = window.clone();
        v_cancel.connect_clicked(move |_| w.close());
        let w = window.clone();
        d_close.connect_clicked(move |_| w.close());
    }

    let select_index = {
        let (state, p_next) = (state.clone(), p_next.clone());
        move |row: Option<&ListBoxRow>| {
            let idx = row.map(|r| r.index());
            let mut st = state.borrow_mut();
            st.app = idx
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| st.apps.get(i))
                .map(|e| e.app.clone());
            p_next.set_sensitive(st.app.is_some());
        }
    };
    {
        let select_index = select_index.clone();
        list.connect_row_selected(move |_, row| select_index(row));
    }

    let enter_details: Rc<dyn Fn(&str)> = {
        let (state, stack) = (state.clone(), stack.clone());
        let (title, dd, notes, live) = (
            details_title.clone(),
            type_dd.clone(),
            notes.clone(),
            live_check.clone(),
        );
        Rc::new(move |app: &str| {
            let mut st = state.borrow_mut();
            st.app = Some(app.to_string());
            st.live_pid = session::live_stack_pid(&st.env, app);
            title.set_text(&format!("What went wrong with {}?", text::app_name(app)));
            dd.set_selected(text::index_of_type(session::default_type(&st.env, app)));
            notes.buffer().set_text("");
            live.set_active(false);
            live.set_visible(st.live_pid.is_some());
            stack.set_visible_child_name("details");
        })
    };
    {
        let (state, enter) = (state.clone(), enter_details.clone());
        p_next.connect_clicked(move |_| {
            let app = state.borrow().app.clone();
            if let Some(app) = app {
                enter(&app);
            }
        });
    }
    {
        let enter = enter_details.clone();
        let state = state.clone();
        list.connect_row_activated(move |_, row| {
            let app = state
                .borrow()
                .apps
                .get(row.index() as usize)
                .map(|e| e.app.clone());
            if let Some(app) = app {
                enter(&app);
            }
        });
    }
    {
        let st = stack.clone();
        d_back.connect_clicked(move |_| st.set_visible_child_name("pick"));
        let st = stack.clone();
        v_back.connect_clicked(move |_| st.set_visible_child_name("details"));
    }

    // Build the preview off the UI thread.
    {
        let (state, stack) = (state.clone(), stack.clone());
        let (dd, notes, live) = (type_dd.clone(), notes.clone(), live_check.clone());
        let (summary, holder, status_line, send_btn, save_btn, preview_btn) = (
            summary.clone(),
            preview_holder.clone(),
            status_line.clone(),
            v_send.clone(),
            v_save.clone(),
            d_preview.clone(),
        );
        d_preview.connect_clicked(move |_| {
            let (env, app, pid) = {
                let st = state.borrow();
                (st.env.clone(), st.app.clone(), st.live_pid)
            };
            let Some(app) = app else { return };
            let rtype = text::type_from_index(dd.selected());
            let buf = notes.buffer();
            let note_text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
            let live_pid = if live.is_active() { pid } else { None };
            state.borrow_mut().prepared = None;
            save_btn.set_sensitive(false);
            send_btn.set_sensitive(false);
            summary.set_text("");
            while let Some(child) = holder.first_child() {
                holder.remove(&child);
            }
            preview_btn.set_sensitive(false);
            preview_btn.set_label("Preparing…");
            let (state, stack, summary, holder, status_line, send_btn, save_btn, preview_btn) = (
                state.clone(),
                stack.clone(),
                summary.clone(),
                holder.clone(),
                status_line.clone(),
                send_btn.clone(),
                save_btn.clone(),
                preview_btn.clone(),
            );
            glib::spawn_future_local(async move {
                let env2 = env.clone();
                let built = gio::spawn_blocking(move || {
                    session::build_report(&env2, &app, rtype, Some(note_text), live_pid)
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
                preview_btn.set_sensitive(true);
                preview_btn.set_label("Preview report");
                match built {
                    Ok(Ok(prepared)) => {
                        let r = &prepared.report;
                        summary.set_text(&format!(
                            "{} · {} report · {:.1} KiB. Expand the full report to inspect every detail.",
                            text::app_name(&r.app.id),
                            r.report_type.as_str(),
                            prepared.bytes.len() as f64 / 1024.0,
                        ));
                        while let Some(c) = holder.first_child() {
                            holder.remove(&c);
                        }
                        holder.append(&report_overview(&prepared));
                        let enabled = session::upload_enabled(&env).unwrap_or(false);
                        save_btn.set_sensitive(true);
                        send_btn.set_sensitive(enabled);
                        if enabled {
                            send_btn.add_css_class("primary");
                            send_btn.add_css_class("suggested-action");
                            save_btn.remove_css_class("primary");
                            save_btn.remove_css_class("suggested-action");
                        } else {
                            send_btn.remove_css_class("primary");
                            send_btn.remove_css_class("suggested-action");
                            primary(&save_btn);
                        }
                        status_line.set_text(if enabled {
                            "Send uploads exactly this report. You will get a receipt you can use to delete it."
                        } else {
                            text::SENDING_OFF_HINT
                        });
                        state.borrow_mut().prepared = Some(prepared);
                        stack.set_visible_child_name("preview");
                    }
                    Ok(Err(e)) => {
                        summary.set_text("The report could not be prepared. Go back to try again.");
                        status_line.set_text(&format!("Could not build the report: {e}"));
                        stack.set_visible_child_name("preview");
                        send_btn.set_sensitive(false);
                    }
                    Err(_) => {
                        summary.set_text("The report could not be prepared. Go back to try again.");
                        status_line.set_text("Building the report failed unexpectedly.");
                        stack.set_visible_child_name("preview");
                    }
                }
            });
        });
    }

    // Save to a file.
    {
        let (state, window, stack) = (state.clone(), window.clone(), stack.clone());
        let (done_title, done_body) = (done_title.clone(), done_body.clone());
        v_save.connect_clicked(move |_| {
            let Some(p) = state.borrow().prepared.clone() else {
                return;
            };
            let dialog = gtk4::FileDialog::builder()
                .title("Save the report")
                .initial_name(format!(
                    "breadbug-{}-{}.json",
                    p.report.app.id, p.report.created_unix
                ))
                .build();
            let (state, stack, done_title, done_body) = (
                state.clone(),
                stack.clone(),
                done_title.clone(),
                done_body.clone(),
            );
            dialog.save(Some(&window), gio::Cancellable::NONE, move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else { return };
                let env = state.borrow().env.clone();
                match session::save(&env, &p, &path) {
                    Ok(()) => {
                        done_title.set_markup("<span size='x-large' weight='bold'>Saved</span>");
                        done_body.set_text(&format!(
                            "The report was saved to {}.\nNothing was sent.",
                            path.display()
                        ));
                    }
                    Err(e) => {
                        done_title
                            .set_markup("<span size='x-large' weight='bold'>Could not save</span>");
                        done_body.set_text(&format!("{e:#}"));
                    }
                }
                stack.set_visible_child_name("done");
            });
        });
    }

    // Send.
    {
        let (state, stack) = (state.clone(), stack.clone());
        let (done_title, done_body, btn) = (done_title.clone(), done_body.clone(), v_send.clone());
        v_send.connect_clicked(move |_| {
            let (env, prepared) = {
                let st = state.borrow();
                (st.env.clone(), st.prepared.clone())
            };
            let Some(p) = prepared else { return };
            btn.set_sensitive(false);
            btn.set_label("Sending…");
            let (stack, done_title, done_body, btn) =
                (stack.clone(), done_title.clone(), done_body.clone(), btn.clone());
            glib::spawn_future_local(async move {
                let res = gio::spawn_blocking(move || session::send(&env, &p).map_err(|e| format!("{e:#}"))).await;
                btn.set_label("Send");
                btn.set_sensitive(true);
                let (title, body) = match res {
                    Ok(Ok(SendOutcome::Sent { receipt, deletable })) => (
                        "Sent. Thank you.",
                        format!(
                            "Your receipt: {receipt}\n{}",
                            if deletable {
                                "You can delete the report later from Settings, Privacy & security, Bug reports, or with `breadbug delete`."
                            } else {
                                "This report was already on the server."
                            }
                        ),
                    ),
                    Ok(Ok(SendOutcome::Queued { why })) => (
                        "Saved to send later",
                        format!("The server could not be reached ({why}). The report is queued and will be retried; you can see or drop it in Settings."),
                    ),
                    Ok(Ok(SendOutcome::QueueFull { why })) => (
                        "Not sent",
                        format!("The server could not be reached ({why}) and the queue is full. Go back and save the report to a file instead."),
                    ),
                    Ok(Ok(SendOutcome::Rejected { why })) => (
                        "The server refused the report",
                        format!("{why}. Nothing more will be tried; you can go back and save it to a file."),
                    ),
                    Ok(Ok(SendOutcome::Disabled)) => ("Sending is turned off", text::SENDING_OFF_HINT.to_string()),
                    Ok(Err(e)) => ("Could not send", e),
                    Err(_) => ("Could not send", "Sending failed unexpectedly.".to_string()),
                };
                done_title.set_markup(&format!(
                    "<span size='x-large' weight='bold'>{}</span>",
                    glib::markup_escape_text(title)
                ));
                done_body.set_text(&body);
                stack.set_visible_child_name("done");
            });
        });
    }

    // Initial page. An app named on the command line skips the picker, and
    // still waits behind the one-time notice on a first run.
    let preselected = preselect.filter(|p| state.borrow().apps.iter().any(|e| &e.app == p));
    {
        let (state, stack, enter) = (state.clone(), stack.clone(), enter_details.clone());
        let after_consent = preselected.clone();
        c_go.connect_clicked(move |_| {
            consent::mark_seen(&state.borrow().env.state_dir);
            match &after_consent {
                Some(app) => enter(app),
                None => stack.set_visible_child_name("pick"),
            }
        });
    }
    if first_run {
        stack.set_visible_child_name("consent");
        window.set_default_widget(Some(&c_go));
    } else if let Some(app) = &preselected {
        enter_details(app);
    } else {
        stack.set_visible_child_name("pick");
    }

    window.present();

    Ui {
        window,
        stack,
        list,
        next: p_next,
        type_dropdown: type_dd,
        notes,
        preview: d_preview,
        save: v_save,
        send: v_send,
        status_line,
        summary,
        done_title,
        done_body,
        consent_continue: c_go,
    }
}
