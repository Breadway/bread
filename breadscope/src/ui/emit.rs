//! Manual `emit` dialog, including the same reserved-domain guard breadd
//! enforces so the rejection is visible before the request is sent.

use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{Box as GBox, Button, Entry, Label, Orientation, TextView, Window};

use super::{button, dim_label, hbox, horizontal_spacer, label, Action, Shared};
use crate::ipc;

const RESERVED_DOMAINS: &[&str] = &[
    "terminal",
    "git",
    "hyprland",
    "device",
    "power",
    "network",
    "service",
    "container",
    "project",
    "remote",
    "system",
    "profile",
    "notify",
    "command",
    "workflow",
    "bluetooth",
    "workspace",
    "window",
    "monitor",
    "module",
    "state",
    "widget",
    "reload",
    "bug",
];

pub fn open(
    parent: &gtk4::ApplicationWindow,
    _shared: &Shared,
    actions: &std::sync::mpsc::Sender<Action>,
) {
    let dialog = Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Emit event")
        .default_width(580)
        .build();
    dialog.add_css_class("scope");

    let content = GBox::new(Orientation::Vertical, 9);
    content.add_css_class("emit-dialog");
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);

    let title = label("Emit an event");
    title.add_css_class("dialog-title");
    content.append(&title);
    content.append(&dim_label(
        "Calls the IPC emit method as a manual source. Adapter-owned domains are rejected by breadd.",
    ));

    let error = Label::new(Some(""));
    error.add_css_class("error-box");
    error.set_wrap(true);
    error.set_xalign(0.0);
    error.set_visible(false);
    content.append(&error);

    let examples = hbox(5);
    let examples_label = Label::new(Some("try:"));
    examples_label.add_css_class("tiny");
    examples_label.add_css_class("faint");
    examples.append(&examples_label);
    content.append(&examples);

    content.append(&field_label("Event name"));
    let name = Entry::new();
    name.set_text("bread.custom.test");
    name.add_css_class("mono");
    content.append(&name);
    let validation = Label::new(Some(""));
    validation.add_css_class("tiny");
    validation.set_xalign(0.0);
    validation.set_wrap(true);
    content.append(&validation);

    let source_row = hbox(8);
    let source_label = field_label("Source");
    source_row.append(&source_label);
    let source = Entry::new();
    source.set_text("Manual");
    source.set_editable(false);
    source.set_hexpand(true);
    source_row.append(&source);
    let source_note = Label::new(Some("breadd assigns the event id"));
    source_note.add_css_class("tiny");
    source_note.add_css_class("faint");
    source_row.append(&source_note);
    content.append(&source_row);

    let data_header = hbox(8);
    data_header.append(&field_label("Data (JSON)"));
    data_header.append(&horizontal_spacer());
    let format = button("Format", "small");
    data_header.append(&format);
    content.append(&data_header);
    let data = TextView::new();
    data.set_monospace(true);
    data.set_wrap_mode(gtk4::WrapMode::WordChar);
    data.set_top_margin(6);
    data.set_bottom_margin(6);
    data.set_left_margin(6);
    data.set_right_margin(6);
    data.buffer()
        .set_text(r#"{"note": "poked from breadscope"}"#);
    let data_scroll = gtk4::ScrolledWindow::new();
    data_scroll.set_child(Some(&data));
    data_scroll.set_min_content_height(100);
    data_scroll.set_max_content_height(150);
    content.append(&data_scroll);

    let hint = dim_label(
        "Accepted events appear in the stream immediately. Reserved examples are rejected locally and by breadd.",
    );
    hint.add_css_class("tiny");
    content.append(&hint);

    let actions_row = hbox(7);
    let cancel = button("Cancel", "ghost");
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    actions_row.append(&cancel);
    actions_row.append(&horizontal_spacer());
    let publish = button("Publish (Ctrl+Enter)", "primary");
    actions_row.append(&publish);
    content.append(&actions_row);
    dialog.set_child(Some(&content));

    for (label_text, event_name, payload, note) in [
        (
            "custom",
            "bread.custom.test",
            r#"{"note": "poked from breadscope"}"#,
            "accepted",
        ),
        (
            "command",
            "bread.command.bar.refresh",
            r#"{"app": "bar"}"#,
            "accepted on the command bus",
        ),
        (
            "reserved",
            "bread.power.ac.connected",
            r#"{"ac_connected": true}"#,
            "rejected: adapter-owned domain",
        ),
    ] {
        let chip = Button::with_label(label_text);
        chip.add_css_class("chip");
        chip.set_tooltip_text(Some(note));
        {
            let name = name.clone();
            let data = data.clone();
            let validation = validation.clone();
            chip.connect_clicked(move |_| {
                name.set_text(event_name);
                data.buffer().set_text(payload);
                update_validation(&validation, event_name);
            });
        }
        examples.append(&chip);
    }

    {
        let validation = validation.clone();
        name.connect_changed(move |entry| {
            update_validation(&validation, &entry.text());
        });
    }
    update_validation(&validation, &name.text());

    {
        let error = error.clone();
        let data = data.clone();
        format.connect_clicked(move |_| {
            let buffer = data.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => {
                    buffer.set_text(
                        &serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string()),
                    );
                    error.set_visible(false);
                }
                Err(err) => {
                    error.set_text(&format!("data is not valid JSON: {err}"));
                    error.set_visible(true);
                }
            }
        });
    }

    {
        let controller = gtk4::EventControllerKey::new();
        controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let publish_for_key = publish.clone();
        let dialog_for_key = dialog.clone();
        controller.connect_key_pressed(move |_, key, _, modifiers| {
            use gtk4::gdk::Key;
            if (key == Key::Return || key == Key::KP_Enter)
                && modifiers.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
            {
                publish_for_key.emit_clicked();
                gtk4::glib::Propagation::Stop
            } else if key == Key::Escape {
                dialog_for_key.close();
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
        dialog.add_controller(controller);
    }

    {
        let error = error.clone();
        let name = name.clone();
        let data = data.clone();
        let dialog = dialog.clone();
        let actions = actions.clone();
        publish.connect_clicked(move |_| {
            error.set_visible(false);
            let event_name = name.text().to_string();
            if event_name.trim().is_empty() {
                show_error(&error, "event name is required");
                return;
            }
            if let Err(message) = validate_event_name(&event_name) {
                show_error(&error, &message);
                return;
            }
            let raw = data.buffer().text(
                &data.buffer().start_iter(),
                &data.buffer().end_iter(),
                false,
            );
            let payload = if raw.trim().is_empty() {
                serde_json::json!({})
            } else {
                match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(value) => value,
                    Err(err) => {
                        show_error(&error, &format!("data is not valid JSON: {err}"));
                        return;
                    }
                }
            };
            let (tx, rx) = mpsc::channel();
            let event_for_thread = event_name.clone();
            std::thread::spawn(move || {
                let _ = tx.send(ipc::emit(&event_for_thread, payload));
            });
            let error_for_poll = error.clone();
            let dialog_for_poll = dialog.clone();
            let actions_for_poll = actions.clone();
            gtk4::glib::timeout_add_local(Duration::from_millis(70), move || match rx.try_recv() {
                Ok(Ok(_)) => {
                    let _ = actions_for_poll.send(Action::Toast(format!(
                        "emit accepted · {event_name} tagged Manual"
                    )));
                    dialog_for_poll.close();
                    gtk4::glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    show_error(&error_for_poll, &err.to_string());
                    gtk4::glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    show_error(&error_for_poll, "emit request ended unexpectedly");
                    gtk4::glib::ControlFlow::Break
                }
            });
        });
    }
    dialog.present();
    name.grab_focus();
}

fn update_validation(label: &Label, name: &str) {
    label.remove_css_class("ok");
    label.remove_css_class("bad");
    let name = name.trim();
    if name.is_empty() {
        label.set_text("Enter an event name.");
        label.add_css_class("bad");
        return;
    }
    match validate_event_name(name) {
        Ok(()) => {
            label.set_text("accepted by the manual-emit guard");
            label.add_css_class("ok");
        }
        Err(message) => {
            label.set_text(&message);
            label.add_css_class("bad");
        }
    }
}

fn field_label(text: &str) -> Label {
    let label = Label::new(Some(&text.to_uppercase()));
    label.set_xalign(0.0);
    label.add_css_class("section-label");
    label
}

fn show_error(error: &Label, message: &str) {
    error.set_text(message);
    error.set_visible(true);
}

fn validate_event_name(name: &str) -> Result<(), String> {
    let segments: Vec<&str> = name.split('.').collect();
    let domain = if segments.first() == Some(&"bread") {
        segments.get(1).copied().unwrap_or("")
    } else {
        segments.first().copied().unwrap_or("")
    };
    let is_known_command = segments.first() == Some(&"bread")
        && segments.get(1) == Some(&"command")
        && segments.len() == 4
        && bread_shared::apps::is_known_app(segments[2]);
    if RESERVED_DOMAINS.contains(&domain) && !is_known_command {
        return Err(format!(
            "rejected: '{domain}' is an adapter-owned reserved domain. A manual emit cannot forge it; use a sourced emit from that adapter or bread.command.<app>.<verb>."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_event_name;

    #[test]
    fn manual_emit_accepts_custom_events() {
        assert!(validate_event_name("bread.custom.test").is_ok());
        assert!(validate_event_name("my.own.event").is_ok());
    }

    #[test]
    fn manual_emit_rejects_adapter_owned_domains() {
        for name in [
            "bread.power.ac.connected",
            "bread.hyprland.event",
            "bread.device.connected",
            "bread.notify.sent",
            "bread.bug.panic",
        ] {
            assert!(
                validate_event_name(name).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn manual_emit_accepts_well_formed_commands_only() {
        assert!(validate_event_name("bread.command.bar.refresh").is_ok());
        assert!(validate_event_name("bread.command.clip.clear").is_ok());
        assert!(validate_event_name("bread.command.notanapp.clear").is_err());
        assert!(validate_event_name("bread.command.bar.history.clear").is_err());
        assert!(validate_event_name("bread.command.bar").is_err());
    }
}
