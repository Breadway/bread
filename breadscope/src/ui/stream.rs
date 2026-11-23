//! Live event stream with causal ordering and a per-event inspector.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use bread_shared::AdapterSource;
use gtk4::prelude::*;
use gtk4::{
    pango, Align, ApplicationWindow, Box as GBox, Button, Label, ScrolledWindow, SearchEntry,
    ToggleButton,
};

use super::{
    badge, button, dim_label, dot_widget, ellipsize, hbox, horizontal_spacer, label, mono,
    section_label, text_view, vbox, Action, Shared, View,
};
use crate::model;

pub struct StreamView {
    root: GBox,
    search: SearchEntry,
    causal: ToggleButton,
    pause: Button,
    summary: Label,
    list: GBox,
    list_scroll: ScrolledWindow,
    inspector: GBox,
    inspector_scroll: ScrolledWindow,
    volume: gtk4::DrawingArea,
    window: ApplicationWindow,
    actions: std::sync::mpsc::Sender<Action>,
    last_inspector_key: RefCell<String>,
    last_refresh: Cell<Instant>,
}

struct RowData {
    id: String,
    timestamp: u64,
    name: String,
    summary: String,
    source: AdapterSource,
    depth: usize,
    selected: bool,
}

fn chip(text: &str, color: Option<&str>) -> ToggleButton {
    let button = ToggleButton::new();
    button.add_css_class("chip");
    let row = hbox(5);
    if let Some(color) = color {
        let swatch = gtk4::DrawingArea::new();
        swatch.set_content_width(7);
        swatch.set_content_height(7);
        swatch.set_valign(Align::Center);
        let (r, g, b) = super::hex_rgb(color).unwrap_or((0.55, 0.55, 0.55));
        swatch.set_draw_func(move |_, cr, width, height| {
            cr.set_source_rgb(r, g, b);
            cr.arc(
                width as f64 / 2.0,
                height as f64 / 2.0,
                3.0,
                0.0,
                std::f64::consts::TAU,
            );
            let _ = cr.fill();
        });
        row.append(&swatch);
    }
    row.append(&label(text));
    button.set_child(Some(&row));
    button
}

impl StreamView {
    pub fn new(
        shared: &Shared,
        window: &ApplicationWindow,
        actions: std::sync::mpsc::Sender<Action>,
    ) -> Self {
        let root = vbox(0);

        // ---- toolbar -----------------------------------------------------
        let toolbar = hbox(8);
        toolbar.add_css_class("toolbar");
        let search = SearchEntry::new();
        search.set_placeholder_text(Some("filter: bread.device.** or dock"));
        search.set_width_chars(22);
        search.set_max_width_chars(36);
        search.set_hexpand(true);
        {
            let shared = shared.clone();
            let actions = actions.clone();
            search.connect_search_changed(move |entry| {
                shared.borrow_mut().filter = entry.text().to_string();
                let _ = actions.send(Action::RefreshStream);
            });
        }
        toolbar.append(&search);

        let causal = ToggleButton::with_label("Causal tree");
        causal.add_css_class("pill");
        causal.set_active(shared.borrow().causal);
        {
            let shared = shared.clone();
            let actions = actions.clone();
            causal.connect_toggled(move |button| {
                shared.borrow_mut().causal = button.is_active();
                let _ = actions.send(Action::RefreshStream);
            });
        }
        toolbar.append(&causal);

        let pause = button("Pause", "");
        {
            let shared = shared.clone();
            let actions = actions.clone();
            pause.connect_clicked(move |button| {
                let paused = !shared.borrow().paused;
                shared.borrow_mut().set_paused(paused);
                button.set_label(if paused { "Resume" } else { "Pause" });
                let _ = actions.send(Action::RefreshStream);
            });
        }
        toolbar.append(&pause);

        let clear = button("Clear buffer", "ghost");
        {
            let shared = shared.clone();
            let actions = actions.clone();
            clear.connect_clicked(move |_| {
                let mut state = shared.borrow_mut();
                state.events.clear();
                state.seen.clear();
                state.selected_event = None;
                state.selection_locked = false;
                drop(state);
                let _ = actions.send(Action::RefreshStream);
            });
        }
        toolbar.append(&clear);
        root.append(&toolbar);

        // ---- source chips -------------------------------------------------
        let chips = hbox(5);
        chips.add_css_class("toolbar");
        let values: [(&str, &str, Option<&str>); 9] = [
            ("all", "all", None),
            ("udev", "udev", Some("#61afef")),
            ("hyprland", "hyprland", Some("#c678dd")),
            ("power", "power", Some("#e5c07b")),
            ("network", "network", Some("#56b6c2")),
            ("bluetooth", "bluetooth", Some("#7aa2f7")),
            ("system", "lua / system", Some("#98c379")),
            ("manual", "manual", Some("#8d8d8d")),
            ("app", "app", Some("#d9a05b")),
        ];
        let all_chip = chip("all", None);
        all_chip.set_active(true);
        chips.append(&all_chip);
        {
            let shared = shared.clone();
            let actions = actions.clone();
            all_chip.connect_toggled(move |button| {
                if button.is_active() {
                    shared.borrow_mut().source = "all".to_string();
                    let _ = actions.send(Action::RefreshStream);
                }
            });
        }
        for (value, text, color) in values.into_iter().filter(|(value, _, _)| *value != "all") {
            let button = chip(text, color);
            button.set_group(Some(&all_chip));
            {
                let shared = shared.clone();
                let actions = actions.clone();
                let value = value.to_string();
                button.connect_toggled(move |button| {
                    if !button.is_active() {
                        return;
                    }
                    shared.borrow_mut().source = value.clone();
                    let _ = actions.send(Action::RefreshStream);
                });
            }
            chips.append(&button);
        }
        chips.append(&horizontal_spacer());
        let summary = label("0 shown");
        summary.add_css_class("small");
        summary.add_css_class("dim");
        chips.append(&summary);
        root.append(&chips);

        // ---- volume ------------------------------------------------------
        let volume = gtk4::DrawingArea::new();
        volume.add_css_class("volume");
        volume.set_content_height(30);
        volume.set_draw_func(|_, _, _, _| {});
        {
            let shared = shared.clone();
            volume.set_draw_func(move |_, cr, width, height| {
                draw_volume(cr, width, height, &shared);
            });
        }
        root.append(&volume);

        // ---- list + inspector -------------------------------------------
        let list = vbox(0);
        list.set_valign(Align::Start);
        let list_scroll = ScrolledWindow::new();
        list_scroll.set_child(Some(&list));
        list_scroll.set_vexpand(true);
        list_scroll.set_hexpand(true);
        list_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        list_scroll.set_min_content_width(0);
        list_scroll.add_css_class("events-scroll");

        let inspector = vbox(8);
        inspector.set_valign(Align::Start);
        let inspector_scroll = ScrolledWindow::new();
        inspector_scroll.set_child(Some(&inspector));
        inspector_scroll.set_vexpand(true);
        inspector_scroll.set_width_request(350);
        inspector_scroll.set_hexpand(false);
        inspector_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        inspector_scroll.set_min_content_width(0);
        inspector_scroll.add_css_class("inspector");

        let body = hbox(0);
        body.set_vexpand(true);
        list_scroll.set_hexpand(true);
        body.append(&list_scroll);
        body.append(&inspector_scroll);
        root.append(&body);
        let view = Self {
            root,
            search,
            causal,
            pause,
            summary,
            list,
            list_scroll,
            inspector,
            inspector_scroll,
            volume,
            window: window.clone(),
            actions,
            last_inspector_key: RefCell::new(String::new()),
            last_refresh: Cell::new(Instant::now() - Duration::from_secs(1)),
        };
        view.refresh(shared);
        view
    }

    pub fn root(&self) -> &GBox {
        &self.root
    }

    pub fn set_filter(&self, filter: &str) {
        if self.search.text() != filter {
            self.search.set_text(filter);
        }
    }

    pub fn focus_filter(&self) {
        self.search.grab_focus();
    }

    pub fn refresh_pending(&self, shared: &Shared) {
        let state = shared.borrow();
        let shown = state.visible_events().len();
        let pending = state.pending_count();
        if pending > 0 {
            self.summary
                .set_text(&format!("{shown} shown  ·  paused, {pending} buffered"));
        } else {
            self.summary.set_text(&format!(
                "{shown} shown  ·  {} buffered",
                state.events.len()
            ));
        }
    }

    pub fn refresh(&self, shared: &Shared) {
        self.last_refresh.set(Instant::now());
        let (rows, shown, total, paused, pending, selected, causal) = {
            let state = shared.borrow();
            let visible = state.visible_events();
            let selected = if state.selection_locked {
                state
                    .selected_event
                    .clone()
                    .filter(|id| visible.iter().any(|(event, _)| &event.id == id))
            } else {
                None
            }
            .or_else(|| {
                visible
                    .iter()
                    .filter(|(event, _)| !state.matching_subscriptions(&event.event).is_empty())
                    .max_by_key(|(event, _)| event.timestamp)
                    .or_else(|| visible.iter().max_by_key(|(event, _)| event.timestamp))
                    .map(|(event, _)| event.id.clone())
                    .or_else(|| state.events.back().map(|event| event.id.clone()))
            });
            let rows = visible
                .iter()
                .map(|(event, depth)| RowData {
                    id: event.id.clone(),
                    timestamp: event.timestamp,
                    name: event.event.clone(),
                    summary: model::event_summary(event),
                    source: event.source.clone(),
                    depth: *depth,
                    selected: selected.as_deref() == Some(event.id.as_str()),
                })
                .collect::<Vec<_>>();
            (
                rows,
                visible.len(),
                state.events.len(),
                state.paused,
                state.pending_count(),
                selected,
                state.causal,
            )
        };
        shared.borrow_mut().selected_event = selected.clone();

        self.summary.set_text(&if paused {
            format!("{shown} shown  ·  paused, {pending} buffered")
        } else {
            format!("{shown} shown  ·  {total} buffered")
        });
        self.pause
            .set_label(if paused { "Resume" } else { "Pause" });
        self.causal.set_active(causal);
        self.volume.queue_draw();

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        if rows.is_empty() {
            let empty = vbox(4);
            empty.set_margin_top(50);
            empty.set_margin_start(20);
            let title = label(if total == 0 {
                "Waiting for events."
            } else {
                "No events match."
            });
            title.add_css_class("bold");
            empty.append(&title);
            empty.append(&dim_label(if total == 0 {
                "breadd is connected. The stream fills as events arrive."
            } else {
                "Try a broader pattern such as bread.** or clear the source filter."
            }));
            self.list.append(&empty);
        } else {
            for row in rows {
                self.list.append(&self.event_row(shared, row));
            }
        }
        let follow = !shared.borrow().selection_locked;
        if follow && self.list.first_child().is_some() {
            let adjustment = self.list_scroll.vadjustment();
            gtk4::glib::idle_add_local_once(move || {
                adjustment.set_value(
                    (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()),
                );
            });
        }
        if let Some(selected) = selected {
            self.render_inspector(shared, &selected);
        } else {
            while let Some(child) = self.inspector.first_child() {
                self.inspector.remove(&child);
            }
            self.inspector
                .append(&dim_label("Select an event to inspect it."));
        }
    }

    fn event_row(&self, shared: &Shared, row: RowData) -> Button {
        let button = Button::new();
        button.add_css_class("event-row");
        if row.selected {
            button.add_css_class("selected");
        }
        button.set_hexpand(true);
        button.set_tooltip_text(Some(&row.summary));

        let content = hbox(8);
        content.set_margin_start((row.depth as i32) * 18);
        if row.depth > 0 {
            let connector = Label::new(Some("↳"));
            connector.add_css_class("faint");
            connector.set_valign(Align::Center);
            content.append(&connector);
        }
        let time = Label::new(Some(&model::timestamp_label(row.timestamp)));
        time.add_css_class("event-time");
        time.set_valign(Align::Start);
        time.set_margin_top(2);
        time.set_width_chars(11);
        content.append(&time);
        let dot = dot_widget(&row.source);
        dot.set_valign(Align::Start);
        dot.set_margin_top(4);
        content.append(&dot);

        let main = vbox(0);
        main.set_hexpand(true);
        let name = Label::new(Some(&row.name));
        name.add_css_class("event-name");
        name.set_xalign(0.0);
        ellipsize(&name, 58);
        main.append(&name);
        let summary = Label::new(Some(&row.summary));
        summary.add_css_class("event-summary");
        summary.set_xalign(0.0);
        summary.set_ellipsize(pango::EllipsizeMode::End);
        summary.set_max_width_chars(78);
        main.append(&summary);
        content.append(&main);

        let meta = hbox(5);
        meta.set_valign(Align::Start);
        meta.set_margin_top(2);
        meta.append(&badge(&model::short_id(&row.id), ""));
        content.append(&meta);
        button.set_child(Some(&content));

        let actions = self.actions.clone();
        let event_id = row.id.clone();
        let shared = shared.clone();
        button.connect_clicked(move |_| {
            let mut state = shared.borrow_mut();
            state.selected_event = Some(event_id.clone());
            state.selection_locked = true;
            drop(state);
            let _ = actions.send(Action::RefreshStream);
        });
        button
    }

    fn render_inspector(&self, shared: &Shared, event_id: &str) {
        let data = {
            let state = shared.borrow();
            let Some(event) = state.event(event_id) else {
                return;
            };
            let mut ancestors = Vec::new();
            let mut cursor = event.caused_by.as_deref().and_then(|id| state.event(id));
            let mut guard = 0;
            while let Some(parent) = cursor {
                if guard > 32 {
                    break;
                }
                ancestors.push((parent.id.clone(), parent.event.clone()));
                cursor = parent.caused_by.as_deref().and_then(|id| state.event(id));
                guard += 1;
            }
            ancestors.reverse();
            let mut children: Vec<(String, String)> = state
                .events
                .iter()
                .filter(|candidate| candidate.caused_by.as_deref() == Some(event_id))
                .map(|candidate| (candidate.id.clone(), candidate.event.clone()))
                .collect();
            children.sort_by(|a, b| a.1.cmp(&b.1));

            let traces: Vec<(String, String, u64, bool)> = state
                .traces_for(event_id)
                .into_iter()
                .map(|trace| {
                    (
                        trace.module_label().to_string(),
                        trace.pattern.clone(),
                        trace.duration_us,
                        trace.ok,
                    )
                })
                .collect();
            let subscriptions: Vec<(String, String, u64)> = state
                .matching_subscriptions(&event.event)
                .into_iter()
                .map(|entry| {
                    (
                        entry.module_label().to_string(),
                        entry.pattern.clone(),
                        entry.count,
                    )
                })
                .collect();
            let key = format!(
                "{}:{}:{}:{}",
                event_id,
                traces.len(),
                subscriptions.len(),
                children.len()
            );
            (
                event.event.clone(),
                event.source.clone(),
                event.timestamp,
                event.data.clone(),
                event.id.clone(),
                ancestors,
                children,
                traces,
                subscriptions,
                key,
            )
        };
        if *self.last_inspector_key.borrow() == data.9 {
            return;
        }
        *self.last_inspector_key.borrow_mut() = data.9;
        while let Some(child) = self.inspector.first_child() {
            self.inspector.remove(&child);
        }

        let (
            name,
            source,
            timestamp,
            payload,
            event_id,
            ancestors,
            children,
            traces,
            subscriptions,
            _,
        ) = data;

        let header = hbox(7);
        header.append(&dot_widget(&source));
        let title = Label::new(Some(&name));
        title.add_css_class("insp-title");
        title.set_wrap(true);
        title.set_xalign(0.0);
        title.set_selectable(true);
        header.append(&title);
        self.inspector.append(&header);

        let sub = hbox(6);
        let source_pill = Label::new(Some(&model::source_label(&source)));
        source_pill.add_css_class("insp-source");
        sub.append(&source_pill);
        let when = Label::new(Some(&format!(
            "{}  ·  {}",
            model::timestamp_label(timestamp),
            model::age_label(timestamp)
        )));
        when.add_css_class("tiny");
        when.add_css_class("faint");
        sub.append(&when);
        self.inspector.append(&sub);

        self.inspector.append(&section_label("Causal chain"));
        if ancestors.is_empty() {
            self.inspector
                .append(&dim_label("No parent: this event started the chain."));
        }
        for (id, chain_name) in &ancestors {
            self.inspector
                .append(&self.chain_button(shared, id, chain_name, "caused by", false));
        }
        self.inspector
            .append(&self.chain_button(shared, &event_id, &name, "this event", true));
        for (id, child_name) in &children {
            self.inspector
                .append(&self.chain_button(shared, id, child_name, "effect", false));
        }

        self.inspector.append(&section_label("Handled by"));
        if !traces.is_empty() {
            for (module, pattern, duration_us, ok) in &traces {
                self.inspector.append(&handler_row(
                    module,
                    pattern,
                    &format!("{:.1} ms", *duration_us as f64 / 1000.0),
                    *ok,
                ));
            }
        } else if !subscriptions.is_empty() {
            for (module, pattern, count) in &subscriptions {
                self.inspector.append(&handler_row(
                    module,
                    pattern,
                    &format!("{count} lifetime"),
                    true,
                ));
            }
            self.inspector.append(&dim_label(
                "Timing unavailable for this event (it predates tracing or tracing is off).",
            ));
        } else {
            self.inspector.append(&dim_label(
                "No subscription matched. The event was recorded and dropped.",
            ));
        }

        self.inspector.append(&section_label("Effects"));
        if children.is_empty() {
            self.inspector
                .append(&dim_label("This event emitted nothing further."));
        }

        self.inspector.append(&section_label("Data"));
        let pretty = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
        self.inspector.append(&text_view(&pretty, 190));

        self.inspector.append(&section_label("Event id"));
        let id_label = Label::new(Some(&event_id));
        id_label.add_css_class("mono");
        id_label.add_css_class("tiny");
        id_label.add_css_class("faint");
        id_label.set_selectable(true);
        id_label.set_wrap(true);
        self.inspector.append(&id_label);

        let actions_row = hbox(6);
        let copy = button("Copy id", "small");
        {
            let window = self.window.clone();
            let event_id = event_id.clone();
            let actions = self.actions.clone();
            copy.connect_clicked(move |_| {
                window.clipboard().set_text(&event_id);
                let _ = actions.send(Action::Toast("Event id copied".to_string()));
            });
        }
        actions_row.append(&copy);
        let stream_filter = button("Filter to family", "small");
        {
            let family = model::event_family(&name);
            let actions = self.actions.clone();
            stream_filter.connect_clicked(move |_| {
                let _ = actions.send(Action::FilterStream(format!("bread.{family}.**")));
            });
        }
        actions_row.append(&stream_filter);
        self.inspector.append(&actions_row);
    }

    fn chain_button(
        &self,
        shared: &Shared,
        id: &str,
        name: &str,
        role: &str,
        current: bool,
    ) -> Button {
        let button = Button::new();
        button.add_css_class("chain-btn");
        if current {
            button.add_css_class("current");
        }
        let row = hbox(6);
        row.append(&label(if current { "●" } else { "↳" }));
        let name_label = mono(name);
        ellipsize(&name_label, 40);
        row.append(&name_label);
        row.append(&horizontal_spacer());
        let role = Label::new(Some(role));
        role.add_css_class("tiny");
        role.add_css_class("faint");
        row.append(&role);
        button.set_child(Some(&row));
        button.set_tooltip_text(Some(name));
        let shared = shared.clone();
        let actions = self.actions.clone();
        let id = id.to_string();
        button.connect_clicked(move |_| {
            let mut state = shared.borrow_mut();
            state.selected_event = Some(id.clone());
            state.selection_locked = true;
            drop(state);
            let _ = actions.send(Action::RefreshStream);
        });
        button
    }

    pub fn focus(&self) -> bool {
        self.inspector_scroll.has_focus()
    }

    pub fn scroll_inspector_top(&self) {
        let adjustment = self.inspector_scroll.vadjustment();
        adjustment.set_value(adjustment.lower());
    }

    pub fn view_id(&self) -> View {
        View::Stream
    }
}

fn handler_row(module: &str, pattern: &str, trailing: &str, ok: bool) -> GBox {
    let row = vbox(1);
    row.add_css_class("handler-row");
    let top = hbox(6);
    top.append(&mono(module));
    top.append(&horizontal_spacer());
    let trailing = Label::new(Some(trailing));
    trailing.add_css_class("duration");
    if !ok {
        trailing.add_css_class("slow");
    }
    top.append(&trailing);
    row.append(&top);
    let pattern = Label::new(Some(pattern));
    pattern.add_css_class("handler-pattern");
    pattern.set_xalign(0.0);
    pattern.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    pattern.set_max_width_chars(44);
    row.append(&pattern);
    row
}

fn draw_volume(cr: &gtk4::cairo::Context, width: i32, height: i32, shared: &Shared) {
    let state = shared.borrow();
    let now = bread_shared::now_unix_ms();
    let window_ms = 120_000u64;
    let buckets = 46usize;
    let width = width as f64 - 24.0;
    let height = height as f64;
    let bar_width = (width / buckets as f64).max(2.0);
    let mut counts = vec![0u32; buckets];
    for event in &state.events {
        let age = now.saturating_sub(event.timestamp);
        if age <= window_ms {
            let index = ((window_ms - age) as f64 / window_ms as f64 * buckets as f64) as usize;
            let index = index.min(buckets - 1);
            counts[index] += 1;
        }
    }
    let max = counts.iter().copied().max().unwrap_or(1).max(1) as f64;
    for (index, count) in counts.iter().enumerate() {
        let bar_height = 3.0 + (*count as f64 / max) * (height - 10.0);
        let x = 12.0 + index as f64 * bar_width;
        let y = height - 4.0 - bar_height;
        let alpha = if index + 7 >= buckets { 0.55 } else { 0.20 };
        cr.set_source_rgba(0.85, 0.63, 0.36, alpha);
        super::rounded_rect(cr, x, y, (bar_width - 2.0).max(1.0), bar_height, 1.5);
        let _ = cr.fill();
    }
    cr.set_source_rgba(0.75, 0.75, 0.75, 0.35);
    cr.select_font_face(
        "monospace",
        gtk4::cairo::FontSlant::Normal,
        gtk4::cairo::FontWeight::Normal,
    );
    cr.set_font_size(8.5);
    let label = if state.paused {
        "paused"
    } else {
        "live · events/second"
    };
    cr.move_to(width - 92.0, 11.0);
    let _ = cr.show_text(label);
}
