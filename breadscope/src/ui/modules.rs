//! Module registry: cards for status, subscriptions, store, and errors.

use gtk4::prelude::*;
use gtk4::{pango, Align, Box as GBox, Button, FlowBox, Label, ScrolledWindow};

use super::{
    button, dim_label, ellipsize, hbox, horizontal_spacer, label, page_sub, section_label,
    text_view, vbox, Action, Shared,
};
use crate::model;

pub struct ModulesView {
    root: GBox,
    cards: FlowBox,
    detail: GBox,
    summary: GBox,
    actions: std::sync::mpsc::Sender<Action>,
}

impl ModulesView {
    pub fn new(shared: &Shared, actions: std::sync::mpsc::Sender<Action>) -> Self {
        let root = vbox(0);
        let header = hbox(12);
        header.add_css_class("page");
        let titles = vbox(1);
        let title = label("Modules");
        title.add_css_class("page-title");
        titles.append(&title);
        titles.append(&page_sub(
            "Loaded Lua modules and built-ins. Counts come from the live event buffer; subscription patterns come from the daemon's registry, not parsed source.",
        ));
        header.append(&titles);
        header.append(&horizontal_spacer());
        let reload = button("Reload all", "small");
        {
            let actions = actions.clone();
            reload.connect_clicked(move |_| {
                let _ = actions.send(Action::ReloadModules);
            });
        }
        header.append(&reload);
        root.append(&header);

        let page = vbox(0);
        page.add_css_class("page");
        let summary = hbox(10);
        page.append(&summary);

        let cards = FlowBox::new();
        cards.set_selection_mode(gtk4::SelectionMode::None);
        cards.set_homogeneous(false);
        cards.set_min_children_per_line(2);
        cards.set_max_children_per_line(2);
        cards.set_column_spacing(10);
        cards.set_row_spacing(10);
        cards.set_valign(Align::Start);
        cards.set_hexpand(true);

        let cards_scroll = ScrolledWindow::new();
        cards_scroll.set_child(Some(&cards));
        cards_scroll.set_vexpand(true);
        cards_scroll.set_hexpand(true);

        let detail = vbox(8);
        detail.set_valign(Align::Start);
        let detail_scroll = ScrolledWindow::new();
        detail_scroll.set_child(Some(&detail));
        detail_scroll.set_width_request(330);
        detail_scroll.set_hexpand(false);
        detail_scroll.set_vexpand(true);

        let body = hbox(12);
        body.set_vexpand(true);
        cards_scroll.set_hexpand(true);
        body.append(&cards_scroll);
        body.append(&detail_scroll);
        page.append(&body);
        root.append(&page);

        let view = Self {
            root,
            cards,
            detail,
            summary,
            actions,
        };
        view.refresh(shared);
        view
    }

    pub fn root(&self) -> &GBox {
        &self.root
    }

    pub fn refresh(&self, shared: &Shared) {
        render_summary(&self.summary, shared);
        render_cards(&self.cards, &self.detail, &self.actions, shared);
    }
}

fn status_color(status: &str) -> &'static str {
    match status {
        "loaded" => "#98c379",
        "degraded" => "#e5c07b",
        "load_error" => "#e06c75",
        "disabled" => "#666666",
        _ => "#8d8d8d",
    }
}

fn status_dot(status: &str) -> gtk4::DrawingArea {
    let (r, g, b) = super::hex_rgb(status_color(status)).unwrap_or((0.55, 0.55, 0.55));
    let area = gtk4::DrawingArea::new();
    area.set_content_width(8);
    area.set_content_height(8);
    area.set_valign(Align::Center);
    area.set_draw_func(move |_, cr, width, height| {
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
    area
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

fn render_summary(summary: &GBox, shared: &Shared) {
    while let Some(child) = summary.first_child() {
        summary.remove(&child);
    }
    let state = shared.borrow();
    let loaded = state
        .modules
        .iter()
        .filter(|module| module.status == "loaded")
        .count();
    let degraded = state
        .modules
        .iter()
        .filter(|module| module.is_degraded())
        .count();
    let disabled = state
        .modules
        .iter()
        .filter(|module| module.status == "disabled")
        .count();
    summary.append(&stat_card(&state.modules.len().to_string(), "modules", ""));
    summary.append(&stat_card(&loaded.to_string(), "loaded", ""));
    summary.append(&stat_card(
        &degraded.to_string(),
        "degraded",
        if degraded > 0 { "warn" } else { "" },
    ));
    summary.append(&stat_card(&disabled.to_string(), "disabled", ""));
    summary.append(&stat_card(
        &state.subscriptions.len().to_string(),
        "subscriptions",
        "",
    ));
}

fn render_cards(
    cards: &FlowBox,
    detail: &GBox,
    actions: &std::sync::mpsc::Sender<Action>,
    shared: &Shared,
) {
    while let Some(child) = cards.first_child() {
        cards.remove(&child);
    }
    let state = shared.borrow();
    let selected = state.selected_module.clone();
    for module in &state.modules {
        let card = Button::new();
        card.add_css_class("module-card");
        card.set_hexpand(true);
        card.set_valign(Align::Start);
        if module.is_degraded() {
            card.add_css_class("degraded");
        }
        if selected.as_deref() == Some(module.name.as_str()) {
            card.add_css_class("selected");
        }
        let content = vbox(6);
        let top = hbox(7);
        top.append(&status_dot(&module.status));
        let name = Label::new(Some(&module.name));
        name.add_css_class("module-name");
        name.set_xalign(0.0);
        ellipsize(&name, 36);
        name.set_hexpand(true);
        top.append(&name);
        let kind = if module.builtin {
            "builtin"
        } else if module.ungated {
            "ungated"
        } else {
            "user"
        };
        top.append(&super::badge(
            kind,
            if module.ungated { "yellow" } else { "" },
        ));
        content.append(&top);

        let patterns: Vec<&str> = state
            .subscriptions
            .iter()
            .filter(|entry| entry.module.as_deref() == Some(module.name.as_str()))
            .map(|entry| entry.pattern.as_str())
            .collect();
        let pattern_row = hbox(4);
        if patterns.is_empty() {
            let chip = Label::new(Some("no subscriptions"));
            chip.add_css_class("pattern-chip");
            pattern_row.append(&chip);
        } else {
            for pattern in patterns.iter().take(2) {
                let chip = Label::new(Some(pattern));
                chip.add_css_class("pattern-chip");
                chip.set_ellipsize(pango::EllipsizeMode::End);
                chip.set_max_width_chars(28);
                pattern_row.append(&chip);
            }
            if patterns.len() > 2 {
                let more = Label::new(Some(&format!("+{}", patterns.len() - 2)));
                more.add_css_class("pattern-chip");
                pattern_row.append(&more);
            }
        }
        content.append(&pattern_row);

        if let Some(error) = module.last_error.as_deref() {
            let error = Label::new(Some(error));
            error.add_css_class("module-error");
            error.set_ellipsize(pango::EllipsizeMode::End);
            error.set_max_width_chars(44);
            error.set_xalign(0.0);
            content.append(&error);
        }

        let handled = state
            .events
            .iter()
            .filter(|event| {
                state.subscriptions.iter().any(|entry| {
                    entry.module.as_deref() == Some(module.name.as_str())
                        && model::matches_pattern(&entry.pattern, &event.event)
                })
            })
            .count();
        let lifetime: u64 = state
            .subscriptions
            .iter()
            .filter(|entry| entry.module.as_deref() == Some(module.name.as_str()))
            .map(|entry| entry.count)
            .sum();
        let last_seen = state
            .subscriptions
            .iter()
            .filter(|entry| entry.module.as_deref() == Some(module.name.as_str()))
            .filter_map(|entry| entry.last_at)
            .max();
        let stats = hbox(10);
        let buffer_stat = Label::new(Some(&format!("{handled} handled")));
        buffer_stat.add_css_class("card-stat");
        stats.append(&buffer_stat);
        let lifetime_stat = Label::new(Some(&format!("{lifetime} lifetime")));
        lifetime_stat.add_css_class("card-stat");
        stats.append(&lifetime_stat);
        if let Some(last_seen) = last_seen {
            let last = Label::new(Some(&model::age_label(last_seen)));
            last.add_css_class("card-stat");
            stats.append(&last);
        }
        content.append(&stats);
        card.set_child(Some(&content));

        {
            let shared = shared.clone();
            let detail = detail.clone();
            let actions = actions.clone();
            let cards = cards.clone();
            let name = module.name.clone();
            card.connect_clicked(move |_| {
                shared.borrow_mut().selected_module = Some(name.clone());
                render_cards(&cards, &detail, &actions, &shared);
            });
        }
        cards.insert(&card, -1);
    }

    render_detail(detail, actions, shared);
}

fn render_detail(detail: &GBox, actions: &std::sync::mpsc::Sender<Action>, shared: &Shared) {
    while let Some(child) = detail.first_child() {
        detail.remove(&child);
    }
    let state = shared.borrow();
    let Some(module) = state
        .selected_module
        .as_deref()
        .and_then(|name| state.module(name))
        .or_else(|| state.modules.first())
    else {
        detail.append(&dim_label("No modules reported by the daemon."));
        return;
    };

    let panel = vbox(7);
    panel.add_css_class("detail-panel");
    let header = hbox(7);
    header.append(&status_dot(&module.status));
    let name = Label::new(Some(&module.name));
    name.add_css_class("insp-title");
    name.set_xalign(0.0);
    name.set_wrap(true);
    header.append(&name);
    panel.append(&header);

    let badges = hbox(5);
    let status_class = if module.is_degraded() {
        "yellow"
    } else if module.status == "loaded" {
        "green"
    } else {
        ""
    };
    badges.append(&super::badge(&module.status, status_class));
    badges.append(&super::badge(
        if module.builtin { "builtin" } else { "user" },
        "",
    ));
    if module.ungated {
        badges.append(&super::badge("ungated", "yellow"));
    }
    panel.append(&badges);

    if let Some(error) = module.last_error.as_deref() {
        let error = Label::new(Some(error));
        error.add_css_class("error-box");
        error.set_wrap(true);
        error.set_xalign(0.0);
        panel.append(&error);
    }

    let subs: Vec<&model::SubscriptionEntry> = state
        .subscriptions
        .iter()
        .filter(|entry| entry.module.as_deref() == Some(module.name.as_str()))
        .collect();
    let lifetime: u64 = subs.iter().map(|entry| entry.count).sum();
    let handled = state
        .events
        .iter()
        .filter(|event| {
            subs.iter()
                .any(|entry| model::matches_pattern(&entry.pattern, &event.event))
        })
        .count();
    for (key, value) in [
        ("handled in buffer", handled.to_string()),
        ("lifetime dispatches", lifetime.to_string()),
        ("subscriptions", subs.len().to_string()),
        (
            "kind",
            if module.builtin {
                "builtin"
            } else {
                "user module"
            }
            .to_string(),
        ),
    ] {
        detail_row(&panel, key, &value);
    }

    panel.append(&section_label("Subscriptions"));
    if subs.is_empty() {
        panel.append(&dim_label("No event subscriptions registered."));
    }
    for entry in &subs {
        let row = vbox(1);
        row.add_css_class("handler-row");
        let top = hbox(6);
        let pattern = Label::new(Some(&entry.pattern));
        pattern.add_css_class("handler-name");
        pattern.set_hexpand(true);
        pattern.set_xalign(0.0);
        pattern.set_ellipsize(pango::EllipsizeMode::End);
        top.append(&pattern);
        let count = Label::new(Some(&format!("{}", entry.count)));
        count.add_css_class("duration");
        top.append(&count);
        row.append(&top);
        let meta = hbox(6);
        meta.append(&super::badge(&entry.kind, "blue"));
        if entry.once {
            meta.append(&super::badge("once", ""));
        }
        if entry.filtered {
            meta.append(&super::badge("filtered", ""));
        }
        if let Some(raw) = entry.raw_kind.as_deref() {
            meta.append(&super::badge(raw, ""));
        }
        if let Some(last) = entry.last_at {
            let last = Label::new(Some(&model::age_label(last)));
            last.add_css_class("tiny");
            last.add_css_class("faint");
            meta.append(&last);
        }
        row.append(&meta);
        panel.append(&row);
    }

    panel.append(&section_label("Store"));
    if module.store.is_empty() {
        panel.append(&dim_label("Store is empty."));
    } else {
        let store =
            serde_json::to_string_pretty(&serde_json::to_value(&module.store).unwrap_or_default())
                .unwrap_or_default();
        panel.append(&text_view(&store, 130));
    }

    let buttons = hbox(6);
    let filter = button("Filter stream", "small");
    {
        let pattern = subs.first().map(|entry| entry.pattern.clone());
        let actions = actions.clone();
        filter.connect_clicked(move |_| {
            if let Some(pattern) = pattern.clone() {
                let _ = actions.send(Action::FilterStream(pattern));
            }
        });
    }
    buttons.append(&filter);
    let reload = button("Reload all", "small");
    {
        let actions = actions.clone();
        reload.connect_clicked(move |_| {
            let _ = actions.send(Action::ReloadModules);
        });
    }
    buttons.append(&reload);
    panel.append(&buttons);
    detail.append(&panel);
}

fn detail_row(panel: &GBox, key: &str, value: &str) {
    let row = hbox(8);
    row.set_margin_top(1);
    let key = Label::new(Some(key));
    key.add_css_class("detail-key");
    key.set_hexpand(true);
    key.set_xalign(0.0);
    row.append(&key);
    let value = Label::new(Some(value));
    value.add_css_class("detail-value");
    row.append(&value);
    panel.append(&row);
}
