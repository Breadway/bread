//! Adapter liveness, daemon metadata, and recent handler errors from `health`.

use gtk4::prelude::*;
use gtk4::{pango, Box as GBox, Label};

use super::{dim_label, hbox, horizontal_spacer, label, page_sub, section_label, vbox, Shared};
use crate::model;

pub struct AdaptersView {
    root: GBox,
    content: GBox,
}

impl AdaptersView {
    pub fn new(shared: &Shared) -> Self {
        let root = vbox(0);
        let header = hbox(12);
        header.add_css_class("page");
        let titles = vbox(1);
        let title = label("Adapters and health");
        title.add_css_class("page-title");
        titles.append(&title);
        titles.append(&page_sub(
            "Adapter status from health, the daemon's subscription count, the client buffer, and the last handler errors breadd reported.",
        ));
        header.append(&titles);
        header.append(&horizontal_spacer());
        root.append(&header);

        let content = vbox(12);
        content.add_css_class("page");
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_child(Some(&content));
        scroll.set_vexpand(true);
        scroll.set_hexpand(true);
        root.append(&scroll);

        let view = Self { root, content };
        view.refresh(shared);
        view
    }

    pub fn root(&self) -> &GBox {
        &self.root
    }

    pub fn refresh(&self, shared: &Shared) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let state = shared.borrow();
        let Some(health) = state.health.as_ref() else {
            self.content.append(&dim_label(
                "breadd is not reachable. Start the daemon or check its socket path in the rail.",
            ));
            return;
        };

        let mut adapters: Vec<(&String, &String)> = health.adapters.iter().collect();
        adapters.sort_by(|a, b| a.0.cmp(b.0));
        let connected = adapters
            .iter()
            .filter(|(_, status)| status.as_str() == "connected")
            .count();
        let degraded = adapters.len().saturating_sub(connected);

        let cards = hbox(10);
        cards.append(&stat_card(&connected.to_string(), "adapters connected", ""));
        cards.append(&stat_card(
            &degraded.to_string(),
            "degraded or unavailable",
            if degraded > 0 { "warn" } else { "" },
        ));
        cards.append(&stat_card(
            &health.subscriptions.to_string(),
            "lua subscriptions",
            "",
        ));
        cards.append(&stat_card(
            &state.events.len().to_string(),
            "events in buffer",
            "",
        ));
        self.content.append(&cards);

        let table = vbox(0);
        let head = hbox(10);
        head.add_css_class("table-head");
        let adapter_head = Label::new(Some("ADAPTER"));
        adapter_head.set_width_chars(18);
        adapter_head.set_xalign(0.0);
        head.append(&adapter_head);
        let status_head = Label::new(Some("STATUS"));
        status_head.set_width_chars(14);
        status_head.set_xalign(0.0);
        head.append(&status_head);
        let note_head = Label::new(Some("MEANING"));
        note_head.set_hexpand(true);
        note_head.set_xalign(0.0);
        head.append(&note_head);
        table.append(&head);
        for (name, status) in &adapters {
            let row = hbox(10);
            row.add_css_class("table-row");
            let name_label = Label::new(Some(name.as_str()));
            name_label.add_css_class("mono");
            name_label.add_css_class("table-text");
            name_label.set_width_chars(18);
            name_label.set_xalign(0.0);
            row.append(&name_label);
            let class = match status.as_str() {
                "connected" => "green",
                "unavailable" => "yellow",
                _ => "red",
            };
            let status_badge = super::badge(status, class);
            status_badge.set_width_chars(13);
            row.append(&status_badge);
            let meaning = Label::new(Some(match status.as_str() {
                "connected" => "running",
                "unavailable" => "dependency missing, terminal until reconfigured",
                "disconnected" => "enabled but not connected, retrying",
                _ => "",
            }));
            meaning.add_css_class("table-text");
            meaning.set_hexpand(true);
            meaning.set_xalign(0.0);
            row.append(&meaning);
            table.append(&row);
        }
        self.content.append(&table);

        self.content.append(&section_label("Daemon"));
        let details = vbox(0);
        for (key, value) in [
            ("socket", health.socket.clone()),
            ("pid", health.pid.to_string()),
            ("version", health.version.clone()),
            ("api", health.api_version.clone()),
            ("uptime", model::uptime_label(health.uptime_ms)),
        ] {
            let row = hbox(8);
            row.set_margin_top(2);
            let key_label = Label::new(Some(key));
            key_label.add_css_class("detail-key");
            key_label.set_width_chars(12);
            key_label.set_xalign(0.0);
            row.append(&key_label);
            let value_label = Label::new(Some(&value));
            value_label.add_css_class("detail-value");
            value_label.set_xalign(0.0);
            value_label.set_ellipsize(pango::EllipsizeMode::Middle);
            value_label.set_hexpand(true);
            row.append(&value_label);
            details.append(&row);
        }
        self.content.append(&details);

        self.content.append(&section_label("Recent handler errors"));
        if health.recent_errors.is_empty() {
            self.content
                .append(&dim_label("No handler errors reported."));
        }
        for error in &health.recent_errors {
            let box_ = vbox(1);
            box_.add_css_class("handler-row");
            let top = hbox(6);
            let time = Label::new(Some(&model::timestamp_label(error.timestamp)));
            time.add_css_class("tiny");
            time.add_css_class("faint");
            top.append(&time);
            let module = Label::new(Some(error.module.as_deref().unwrap_or("init.lua")));
            module.add_css_class("mono");
            module.add_css_class("small");
            top.append(&module);
            box_.append(&top);
            let message = Label::new(Some(&error.message));
            message.add_css_class("tiny");
            message.add_css_class("bad");
            message.set_wrap(true);
            message.set_xalign(0.0);
            box_.append(&message);
            self.content.append(&box_);
        }
    }
}

fn stat_card(value: &str, title: &str, class: &str) -> GBox {
    let card = vbox(0);
    card.add_css_class("stat-card");
    card.set_hexpand(true);
    let value = label(value);
    value.add_css_class("stat-value");
    if !class.is_empty() {
        value.add_css_class(class);
    }
    card.append(&value);
    let title = label(title);
    title.add_css_class("stat-label");
    card.append(&title);
    card
}
