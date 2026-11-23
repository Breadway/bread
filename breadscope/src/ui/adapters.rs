//! Adapter liveness, daemon metadata, and recent handler errors from `health`.

use gtk4::prelude::*;
use gtk4::{pango, Align, Box as GBox, FlowBox, Label};

use super::{
    button, dim_label, hbox, horizontal_spacer, label, page_sub, section_label, vbox, Action,
    Shared,
};
use crate::model;

pub struct AdaptersView {
    root: GBox,
    content: GBox,
}

impl AdaptersView {
    pub fn new(shared: &Shared, actions: std::sync::mpsc::Sender<Action>) -> Self {
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
        let refresh = button("Refresh", "small");
        refresh.set_valign(Align::Center);
        {
            let actions = actions.clone();
            refresh.connect_clicked(move |_| {
                let _ = actions.send(Action::RefreshAdapters);
            });
        }
        header.append(&refresh);
        root.append(&header);

        let content = vbox(14);
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
            let empty = vbox(6);
            empty.set_margin_top(30);
            let title = label("breadd is not reachable.");
            title.add_css_class("bold");
            empty.append(&title);
            empty.append(&dim_label(
                "Start the daemon or check the socket path in the sidebar. The stream view reconnects automatically.",
            ));
            self.content.append(&empty);
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

        self.content.append(&section_label("Adapters"));
        let grid = FlowBox::new();
        grid.set_selection_mode(gtk4::SelectionMode::None);
        grid.set_homogeneous(true);
        grid.set_min_children_per_line(2);
        grid.set_max_children_per_line(3);
        grid.set_column_spacing(10);
        grid.set_row_spacing(10);
        grid.set_valign(Align::Start);
        grid.set_hexpand(true);
        for (name, status) in &adapters {
            let card = vbox(4);
            card.add_css_class("adapter-card");
            card.set_hexpand(true);
            let top = hbox(7);
            let (r, g, b) = super::hex_rgb(match status.as_str() {
                "connected" => "#98c379",
                "unavailable" => "#e5c07b",
                _ => "#e06c75",
            })
            .unwrap_or((0.6, 0.6, 0.6));
            let dot = gtk4::DrawingArea::new();
            dot.set_content_width(8);
            dot.set_content_height(8);
            dot.set_valign(Align::Center);
            dot.set_draw_func(move |_, cr, width, height| {
                cr.set_source_rgb(r, g, b);
                cr.arc(
                    width as f64 / 2.0,
                    height as f64 / 2.0,
                    3.2,
                    0.0,
                    std::f64::consts::TAU,
                );
                let _ = cr.fill();
            });
            top.append(&dot);
            let name = Label::new(Some(name.as_str()));
            name.add_css_class("module-name");
            name.set_hexpand(true);
            name.set_xalign(0.0);
            top.append(&name);
            let class = match status.as_str() {
                "connected" => "green",
                "unavailable" => "yellow",
                _ => "red",
            };
            top.append(&super::badge(status, class));
            card.append(&top);
            let meaning = Label::new(Some(match status.as_str() {
                "connected" => "running, delivering events",
                "unavailable" => "dependency missing; terminal until reconfigured",
                "disconnected" => "enabled but not connected; retrying",
                _ => "unknown state",
            }));
            meaning.add_css_class("tiny");
            meaning.add_css_class("faint");
            meaning.set_xalign(0.0);
            meaning.set_wrap(true);
            card.append(&meaning);
            grid.insert(&card, -1);
        }
        self.content.append(&grid);

        self.content.append(&section_label("Daemon"));
        let daemon = vbox(4);
        daemon.add_css_class("detail-panel");
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
            key_label.set_width_chars(10);
            key_label.set_xalign(0.0);
            row.append(&key_label);
            let value_label = Label::new(Some(&value));
            value_label.add_css_class("detail-value");
            value_label.set_xalign(0.0);
            value_label.set_ellipsize(pango::EllipsizeMode::Middle);
            value_label.set_hexpand(true);
            row.append(&value_label);
            daemon.append(&row);
        }
        self.content.append(&daemon);

        self.content.append(&section_label("Recent handler errors"));
        if health.recent_errors.is_empty() {
            self.content
                .append(&dim_label("No handler errors reported."));
        }
        for error in &health.recent_errors {
            let card = vbox(3);
            card.add_css_class("error-card");
            let top = hbox(7);
            let time = Label::new(Some(&model::timestamp_label(error.timestamp)));
            time.add_css_class("tiny");
            time.add_css_class("faint");
            top.append(&time);
            let module = Label::new(Some(error.module.as_deref().unwrap_or("init.lua")));
            module.add_css_class("mono");
            module.add_css_class("small");
            top.append(&module);
            let age = Label::new(Some(&model::age_label(error.timestamp)));
            age.add_css_class("tiny");
            age.add_css_class("faint");
            top.append(&age);
            card.append(&top);
            let message = Label::new(Some(&error.message));
            message.add_css_class("tiny");
            message.add_css_class("bad");
            message.set_wrap(true);
            message.set_xalign(0.0);
            message.set_selectable(true);
            card.append(&message);
            self.content.append(&card);
        }

        self.content.append(&dim_label(
            "Health is polled every two seconds. Adapter status comes from breadd's health response; this view never probes a device directly.",
        ));
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
