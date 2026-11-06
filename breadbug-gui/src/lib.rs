//! The breadbug window: pick an app, say what happened, read exactly what a
//! report contains, then save it, send it, or cancel.
//!
//! All behaviour that matters (what is built, what is sent, when sending is
//! refused) lives in `breadbug::session`, which the CLI shares, so this file
//! is only layout and wiring. Slow work (building a report runs helper
//! commands; sending touches the network) runs off the UI thread.

pub mod launch;
pub mod preview;
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
    gio, glib, Application, ApplicationWindow, Box as GBox, Button, CheckButton, DropDown, Label,
    ListBox, ListBoxRow, Orientation, ScrolledWindow, SelectionMode, Stack, TextView, WrapMode,
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
    b.set_margin_top(20);
    b.set_margin_bottom(20);
    b.set_margin_start(24);
    b.set_margin_end(24);
    b
}

fn heading(s: &str) -> Label {
    let l = Label::new(None);
    l.set_markup(&format!(
        "<span size='x-large' weight='bold'>{}</span>",
        glib::markup_escape_text(s)
    ));
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
    b.set_halign(gtk4::Align::End);
    b.set_margin_top(8);
    b
}

/// Apply the `(save, send)` decision from `preview::actions` to the buttons.
fn set_actions(save: &Button, send: &Button, save_on: bool, send_on: bool) {
    save.set_sensitive(save_on);
    send.set_sensitive(send_on);
}

fn mono_view(content: &str) -> ScrolledWindow {
    let tv = TextView::new();
    tv.set_editable(false);
    tv.set_cursor_visible(false);
    tv.set_monospace(true);
    tv.set_wrap_mode(WrapMode::None);
    tv.buffer().set_text(content);
    let sw = ScrolledWindow::new();
    sw.set_child(Some(&tv));
    sw.set_vexpand(true);
    sw.set_min_content_height(220);
    sw
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
    style::apply_global();

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
        .default_width(780)
        .default_height(660)
        .build();
    window.add_css_class("breadbug");
    bread_theme::gtk::bind_window_auto_with_app_css(&window, |_| style::CSS.to_string());

    let stack = Stack::new();
    stack.set_vexpand(true);

    // ------------------------------------------------------------ consent
    let consent_page = page();
    consent_page.append(&heading("Before you start"));
    consent_page.append(&para(CONSENT_TEXT));
    let (c_quit, c_go) = (Button::with_label("Quit"), Button::with_label("Continue"));
    c_go.add_css_class("suggested-action");
    let cb = buttons();
    cb.append(&c_quit);
    cb.append(&c_go);
    consent_page.append(&cb);
    stack.add_named(&consent_page, Some("consent"));

    // --------------------------------------------------------------- pick
    let pick_page = page();
    pick_page.append(&heading("Which app has the problem?"));
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.add_css_class("boxed-list");
    {
        let st = state.borrow();
        let now = now_ms();
        for e in &st.apps {
            let row = ListBoxRow::new();
            let l = Label::new(Some(&text::app_line(e, now)));
            l.set_xalign(0.0);
            l.set_margin_top(8);
            l.set_margin_bottom(8);
            l.set_margin_start(12);
            row.set_child(Some(&l));
            list.append(&row);
        }
    }
    let list_scroll = ScrolledWindow::new();
    list_scroll.set_child(Some(&list));
    list_scroll.set_vexpand(true);
    pick_page.append(&list_scroll);
    let empty_hint = para("No bread apps were found to report on.");
    empty_hint.set_visible(state.borrow().apps.is_empty());
    pick_page.append(&empty_hint);
    let (p_cancel, p_next) = (Button::with_label("Cancel"), Button::with_label("Next"));
    p_next.add_css_class("suggested-action");
    p_next.set_sensitive(false);
    let pb = buttons();
    pb.append(&p_cancel);
    pb.append(&p_next);
    pick_page.append(&pb);
    stack.add_named(&pick_page, Some("pick"));

    // ------------------------------------------------------------ details
    let details_page = page();
    let details_title = heading("");
    details_page.append(&details_title);
    details_page.append(&para("What happened?"));
    let type_dd = DropDown::from_strings(&text::TYPE_LABELS);
    details_page.append(&type_dd);
    details_page.append(&para(
        "Anything you want to add, in your own words (optional). Please do not paste passwords or private text: it is scrubbed like everything else, but you are the best judge.",
    ));
    let notes = TextView::new();
    notes.set_wrap_mode(WrapMode::WordChar);
    notes.set_top_margin(6);
    notes.set_bottom_margin(6);
    notes.set_left_margin(8);
    notes.set_right_margin(8);
    let notes_scroll = ScrolledWindow::new();
    notes_scroll.set_child(Some(&notes));
    notes_scroll.set_vexpand(true);
    notes_scroll.set_min_content_height(120);
    details_page.append(&notes_scroll);
    let live_check = CheckButton::with_label(
        "Include a stack trace of the frozen app (function names and offsets only, no memory)",
    );
    live_check.set_visible(false);
    details_page.append(&live_check);
    let (d_back, d_preview) = (
        Button::with_label("Back"),
        Button::with_label("Preview report"),
    );
    d_preview.add_css_class("suggested-action");
    let db = buttons();
    db.append(&d_back);
    db.append(&d_preview);
    details_page.append(&db);
    stack.add_named(&details_page, Some("details"));

    // ------------------------------------------------------------ preview
    let preview_page = page();
    preview_page.append(&heading("This is exactly what would be reported"));
    let summary = para("");
    preview_page.append(&summary);
    let preview_holder = GBox::new(Orientation::Vertical, 0);
    preview_holder.set_vexpand(true);
    preview_page.append(&preview_holder);
    let status_line = para("");
    preview_page.append(&status_line);
    let (v_back, v_save, v_send, v_cancel) = (
        Button::with_label("Back"),
        Button::with_label("Save to a file…"),
        Button::with_label("Send"),
        Button::with_label("Cancel"),
    );
    v_send.add_css_class("suggested-action");
    let vb = buttons();
    vb.append(&v_back);
    vb.append(&v_cancel);
    vb.append(&v_save);
    vb.append(&v_send);
    preview_page.append(&vb);
    stack.add_named(&preview_page, Some("preview"));

    // --------------------------------------------------------------- done
    let done_page = page();
    let done_title = heading("");
    let done_body = para("");
    done_page.append(&done_title);
    done_page.append(&done_body);
    let d_close = Button::with_label("Close");
    d_close.set_halign(gtk4::Align::End);
    done_page.append(&d_close);
    stack.add_named(&done_page, Some("done"));

    window.set_child(Some(&stack));

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
            title.set_markup(&format!(
                "<span size='x-large' weight='bold'>{}</span>",
                glib::markup_escape_text(&format!("Report a problem with {app}"))
            ));
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
            // The previous report must not stay saveable or sendable while
            // the new one is built, or if that build fails.
            preview::rebuild_started(&mut state.borrow_mut().prepared);
            set_actions(&save_btn, &send_btn, false, false);
            while let Some(c) = holder.first_child() {
                holder.remove(&c);
            }
            summary.set_text("");
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
                            "{} report for {}. {:.1} KiB, sha256 {}. {} log lines, {} markers, {} events, {} journal lines.\n{} Nothing has been sent.",
                            r.report_type.as_str(),
                            r.app.id,
                            prepared.bytes.len() as f64 / 1024.0,
                            &prepared.sha256[..16],
                            r.logs.len(),
                            r.markers.len(),
                            r.events.len(),
                            r.journal.len(),
                            text::redaction_summary(&r.redactions),
                        ));
                        while let Some(c) = holder.first_child() {
                            holder.remove(&c);
                        }
                        holder.append(&mono_view(&String::from_utf8_lossy(&prepared.bytes)));
                        let enabled = session::upload_enabled(&env).unwrap_or(false);
                        let (save, send) = preview::actions(true, enabled);
                        set_actions(&save_btn, &send_btn, save, send);
                        status_line.set_text(if enabled {
                            "Send uploads exactly this report. You will get a receipt you can use to delete it."
                        } else {
                            text::SENDING_OFF_HINT
                        });
                        preview::settle(&mut state.borrow_mut().prepared, Ok::<_, String>(prepared));
                        stack.set_visible_child_name("preview");
                    }
                    Ok(Err(e)) => {
                        preview::settle(&mut state.borrow_mut().prepared, Err::<Prepared, _>(()));
                        set_actions(&save_btn, &send_btn, false, false);
                        summary.set_text("The report could not be prepared. Go back to try again.");
                        status_line.set_text(&format!("Could not build the report: {e}"));
                        stack.set_visible_child_name("preview");
                    }
                    Err(_) => {
                        preview::settle(&mut state.borrow_mut().prepared, Err::<Prepared, _>(()));
                        set_actions(&save_btn, &send_btn, false, false);
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
