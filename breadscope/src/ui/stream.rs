//! Live event stream with causal ordering and a per-event inspector.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use bread_shared::AdapterSource;
use gtk4::prelude::*;
use gtk4::{
    pango, Align, ApplicationWindow, Box as GBox, Button, DropDown, Label, ScrolledWindow,
    SearchEntry, ToggleButton,
};

use super::json::JsonView;
use super::{
    badge, button, dim_label, dot_widget, ellipsize, hbox, horizontal_spacer, label, mono,
    section_label, vbox, Action, Shared, View,
};
use crate::model;

pub struct StreamView {
    root: GBox,
    search: SearchEntry,
    causal: ToggleButton,
    pause: Button,
    list: GBox,
    list_scroll: ScrolledWindow,
    inspector: GBox,
    inspector_scroll: ScrolledWindow,
    volume: gtk4::DrawingArea,
    follow: ToggleButton,
    grouped: ToggleButton,
    raw: ToggleButton,
    raw_count: Label,
    legacy: ToggleButton,
    legacy_count: Label,
    chip_buttons: HashMap<String, ToggleButton>,
    list_header: GBox,
    window_filter: DropDown,
    counts: RefCell<HashMap<String, Label>>,
    row_widgets: RefCell<HashMap<String, Button>>,
    pending_scroll: Cell<bool>,
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
    handlers: usize,
    handler_tooltip: String,
    alt: bool,
}

struct GroupRow {
    name: String,
    summary: String,
    source: AdapterSource,
    count: usize,
    handlers: usize,
    handler_tooltip: String,
    latest: u64,
    selected: bool,
}

fn chip(text: &str, color: Option<&str>) -> (ToggleButton, Label) {
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
    let count = Label::new(Some("0"));
    count.add_css_class("chip-count");
    row.append(&count);
    button.set_child(Some(&row));
    (button, count)
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

        let window_filter = DropDown::from_strings(&["All time", "Last 1m", "Last 5m", "Last 15m"]);
        window_filter.add_css_class("window-filter");
        window_filter.set_tooltip_text(Some("Hide events older than this window"));
        let selected_window = match shared.borrow().window_minutes {
            1 => 1,
            5 => 2,
            15 => 3,
            _ => 0,
        };
        window_filter.set_selected(selected_window);
        {
            let shared = shared.clone();
            let actions = actions.clone();
            window_filter.connect_selected_notify(move |dropdown| {
                let minutes = match dropdown.selected() {
                    1 => 1,
                    2 => 5,
                    3 => 15,
                    _ => 0,
                };
                shared.borrow_mut().window_minutes = minutes;
                let _ = actions.send(Action::RefreshStream);
            });
        }
        toolbar.append(&window_filter);

        let follow = ToggleButton::with_label("Follow");
        follow.add_css_class("pill");
        follow.set_active(shared.borrow().follow_tail);
        follow.set_tooltip_text(Some(
            "Keep the newest event selected and pinned to the bottom (f)",
        ));
        {
            let shared = shared.clone();
            let actions = actions.clone();
            follow.connect_toggled(move |button| {
                let mut state = shared.borrow_mut();
                state.follow_tail = button.is_active();
                if button.is_active() {
                    state.selection_locked = false;
                }
                drop(state);
                let _ = actions.send(Action::RefreshStream);
            });
        }
        toolbar.append(&follow);

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
        let mut counts = HashMap::new();
        let mut chip_buttons = HashMap::new();
        let (all_chip, all_count) = chip("all", None);
        all_chip.set_active(true);
        counts.insert("all".to_string(), all_count);
        chip_buttons.insert("all".to_string(), all_chip.clone());
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
            let (button, count) = chip(text, color);
            button.set_group(Some(&all_chip));
            counts.insert(value.to_string(), count);
            chip_buttons.insert(value.to_string(), button.clone());
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
        let raw = ToggleButton::new();
        raw.add_css_class("pill");
        raw.set_active(!shared.borrow().hide_raw);
        raw.set_tooltip_text(Some("Include the raw bread.hyprland.event firehose"));
        let raw_row = hbox(5);
        raw_row.append(&label("Raw"));
        let raw_count = Label::new(Some("0"));
        raw_count.add_css_class("chip-count");
        raw_row.append(&raw_count);
        raw.set_child(Some(&raw_row));
        {
            let shared = shared.clone();
            let actions = actions.clone();
            raw.connect_toggled(move |button| {
                shared.borrow_mut().hide_raw = !button.is_active();
                let _ = actions.send(Action::RefreshStream);
            });
        }
        chips.append(&raw);

        let legacy = ToggleButton::new();
        legacy.add_css_class("pill");
        legacy.set_active(!shared.borrow().hide_legacy);
        legacy.set_tooltip_text(Some("Include legacy bread.hyprland.* compatibility names"));
        let legacy_row = hbox(5);
        legacy_row.append(&label("Legacy"));
        let legacy_count = Label::new(Some("0"));
        legacy_count.add_css_class("chip-count");
        legacy_row.append(&legacy_count);
        legacy.set_child(Some(&legacy_row));
        {
            let shared = shared.clone();
            let actions = actions.clone();
            legacy.connect_toggled(move |button| {
                shared.borrow_mut().hide_legacy = !button.is_active();
                let _ = actions.send(Action::RefreshStream);
            });
        }
        chips.append(&legacy);

        let grouped = ToggleButton::with_label("Grouped");
        grouped.add_css_class("pill");
        grouped.set_active(shared.borrow().grouped);
        grouped.set_tooltip_text(Some(
            "One row per event type; click a row to see its instances",
        ));
        {
            let shared = shared.clone();
            let actions = actions.clone();
            grouped.connect_toggled(move |button| {
                shared.borrow_mut().grouped = button.is_active();
                let _ = actions.send(Action::RefreshStream);
            });
        }
        chips.append(&grouped);

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
        chips.append(&causal);
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
        let list_header = hbox(8);
        list_header.add_css_class("table-header");
        let list_pane = vbox(0);
        list_pane.set_vexpand(true);
        list_pane.append(&list_header);
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
        inspector_scroll.set_width_request(420);
        inspector_scroll.set_hexpand(false);
        inspector_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        inspector_scroll.set_min_content_width(0);
        inspector_scroll.add_css_class("inspector");

        list_pane.append(&list_scroll);
        let body = hbox(0);
        body.set_vexpand(true);
        list_pane.set_hexpand(true);
        body.append(&list_pane);
        body.append(&inspector_scroll);
        root.append(&body);
        let view = Self {
            root,
            search,
            causal,
            pause,
            list,
            list_scroll,
            inspector,
            inspector_scroll,
            volume,
            follow,
            grouped,
            raw,
            raw_count,
            legacy,
            legacy_count,
            chip_buttons,
            list_header,
            window_filter,
            counts: RefCell::new(counts),
            row_widgets: RefCell::new(HashMap::new()),
            pending_scroll: Cell::new(false),
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

    pub fn set_grouped(&self, grouped: bool) {
        self.grouped.set_active(grouped);
    }

    pub fn clear_filter(&self) {
        if !self.search.text().is_empty() {
            self.search.set_text("");
        }
    }

    pub fn toggle_follow(&self, shared: &Shared) {
        let next = !shared.borrow().follow_tail;
        {
            let mut state = shared.borrow_mut();
            state.follow_tail = next;
            if next {
                state.selection_locked = false;
            }
        }
        self.follow.set_active(next);
        let _ = self.actions.send(Action::RefreshStream);
    }

    pub fn select_offset(&self, shared: &Shared, delta: isize) -> bool {
        let (id, causal) = {
            let state = shared.borrow();
            let visible: Vec<&bread_shared::BreadEvent> = state
                .visible_events()
                .into_iter()
                .map(|(event, _)| event)
                .collect();
            if visible.is_empty() {
                return false;
            }
            let current = state
                .selected_event
                .as_deref()
                .and_then(|id| visible.iter().position(|event| event.id == id));
            let base = current.unwrap_or_else(|| if state.causal { visible.len() - 1 } else { 0 });
            let next = (base as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
            (visible[next].id.clone(), state.causal)
        };
        let _ = causal;
        let mut state = shared.borrow_mut();
        state.selected_event = Some(id);
        state.selection_locked = true;
        state.follow_tail = false;
        drop(state);
        self.follow.set_active(false);
        self.pending_scroll.set(true);
        true
    }

    pub fn select_end(&self, shared: &Shared, newest: bool) -> bool {
        let id = {
            let state = shared.borrow();
            let visible: Vec<&bread_shared::BreadEvent> = state
                .visible_events()
                .into_iter()
                .map(|(event, _)| event)
                .collect();
            if visible.is_empty() {
                return false;
            }
            let index = if state.causal == newest {
                visible.len() - 1
            } else {
                0
            };
            visible[index].id.clone()
        };
        let mut state = shared.borrow_mut();
        state.selected_event = Some(id);
        state.selection_locked = true;
        state.follow_tail = false;
        drop(state);
        self.follow.set_active(false);
        self.pending_scroll.set(true);
        true
    }

    fn scroll_to_event(&self, event_id: &str) {
        let Some(row) = self.row_widgets.borrow().get(event_id).cloned() else {
            return;
        };
        let list = self.list.clone();
        let scroll = self.list_scroll.clone();
        gtk4::glib::idle_add_local_once(move || {
            let Some(bounds) = row.compute_bounds(&list) else {
                return;
            };
            let adjustment = scroll.vadjustment();
            let top = bounds.y() as f64;
            let bottom = top + bounds.height() as f64;
            if top < adjustment.value() + 4.0 {
                adjustment.set_value((top - 6.0).max(adjustment.lower()));
            } else if bottom > adjustment.value() + adjustment.page_size() - 4.0 {
                adjustment
                    .set_value((bottom - adjustment.page_size() + 6.0).max(adjustment.lower()));
            }
        });
    }

    pub fn refresh(&self, shared: &Shared) {
        self.last_refresh.set(Instant::now());
        let (
            rows,
            groups,
            shown,
            total,
            paused,
            selected,
            causal,
            grouped,
            hide_raw,
            counts,
            window_minutes,
            follow_tail,
            raw_count,
            legacy_count,
            hide_legacy,
            source_filter,
        ) = {
            let state = shared.borrow();
            let visible = state.visible_events();
            let selected = if state.follow_tail {
                None
            } else if state.selection_locked {
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
            let (_, counts) = state.source_counts();
            let selected_key = selected.as_deref().and_then(|id| {
                visible
                    .iter()
                    .find(|(event, _)| event.id == id)
                    .map(|(event, _)| (event.event.clone(), model::source_key(&event.source)))
            });

            let rows = visible
                .iter()
                .enumerate()
                .map(|(index, (event, depth))| {
                    let (handlers, handler_tooltip) = handler_summary(&state, &event.event);
                    RowData {
                        id: event.id.clone(),
                        timestamp: event.timestamp,
                        name: event.event.clone(),
                        summary: model::event_summary(event),
                        source: event.source.clone(),
                        depth: *depth,
                        selected: selected.as_deref() == Some(event.id.as_str()),
                        handlers,
                        handler_tooltip,
                        alt: index % 2 == 1,
                    }
                })
                .collect::<Vec<_>>();

            let mut group_map: HashMap<(String, String), GroupRow> = HashMap::new();
            for (event, _) in &visible {
                let key = (event.event.clone(), model::source_key(&event.source));
                let (handlers, handler_tooltip) = handler_summary(&state, &event.event);
                let entry = group_map.entry(key).or_insert_with(|| GroupRow {
                    name: event.event.clone(),
                    summary: model::event_summary(event),
                    source: event.source.clone(),
                    count: 0,
                    handlers,
                    handler_tooltip: handler_tooltip.clone(),
                    latest: 0,
                    selected: false,
                });
                entry.count += 1;
                if event.timestamp >= entry.latest {
                    entry.latest = event.timestamp;
                    entry.summary = model::event_summary(event);
                    entry.handlers = handlers;
                    entry.handler_tooltip = handler_tooltip;
                }
            }
            let mut groups: Vec<GroupRow> = group_map.into_values().collect();
            for group in &mut groups {
                group.selected = selected_key.as_ref().is_some_and(|(name, source)| {
                    name == &group.name && source == &model::source_key(&group.source)
                });
            }
            groups.sort_by(|a, b| {
                b.count
                    .cmp(&a.count)
                    .then_with(|| b.latest.cmp(&a.latest))
                    .then_with(|| a.name.cmp(&b.name))
            });

            (
                rows,
                groups,
                visible.len(),
                state.events.len(),
                state.paused,
                selected,
                state.causal,
                state.grouped,
                state.hide_raw,
                counts,
                state.window_minutes,
                state.follow_tail,
                state.raw_count(),
                state.legacy_count(),
                state.hide_legacy,
                state.source.clone(),
            )
        };
        shared.borrow_mut().selected_event = selected.clone();

        let _ = shown;
        self.pause
            .set_label(if paused { "Resume" } else { "Pause" });
        self.causal.set_active(causal);
        self.causal.set_sensitive(!grouped);
        self.grouped.set_active(grouped);
        self.raw.set_active(!hide_raw);
        self.raw_count.set_text(&raw_count.to_string());
        self.raw.set_visible(raw_count > 0 || !hide_raw);
        self.legacy.set_active(!hide_legacy);
        self.legacy_count.set_text(&legacy_count.to_string());
        self.legacy.set_visible(legacy_count > 0 || !hide_legacy);
        self.follow.set_active(follow_tail);
        self.window_filter.set_selected(match window_minutes {
            1 => 1,
            5 => 2,
            15 => 3,
            _ => 0,
        });
        if let Some(all) = self.counts.borrow().get("all") {
            all.set_text(&total.to_string());
        }
        for (key, label) in self.counts.borrow().iter() {
            if key == "all" {
                continue;
            }
            let count = counts.get(key).copied().unwrap_or(0);
            label.set_text(&count.to_string());
            if let Some(button) = self.chip_buttons.get(key) {
                button.set_visible(count > 0 || key == "all" || source_filter == key.as_str());
            }
        }
        self.volume.queue_draw();
        self.update_header(grouped);

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.row_widgets.borrow_mut().clear();
        if grouped {
            if groups.is_empty() {
                self.empty_state(total, raw_count, hide_raw, legacy_count, hide_legacy);
            } else {
                for (index, group) in groups.into_iter().enumerate() {
                    self.list.append(&self.group_row(group, index));
                }
            }
        } else if rows.is_empty() {
            self.empty_state(total, raw_count, hide_raw, legacy_count, hide_legacy);
        } else {
            for row in rows {
                let id = row.id.clone();
                let widget = self.event_row(shared, row);
                self.row_widgets.borrow_mut().insert(id, widget.clone());
                self.list.append(&widget);
            }
        }
        if !grouped && follow_tail && self.list.first_child().is_some() {
            let adjustment = self.list_scroll.vadjustment();
            gtk4::glib::idle_add_local_once(move || {
                adjustment.set_value(
                    (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()),
                );
            });
        } else if !grouped && self.pending_scroll.replace(false) {
            if let Some(selected) = selected.as_deref() {
                self.scroll_to_event(selected);
            }
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

    fn empty_state(
        &self,
        total: usize,
        raw_count: usize,
        hide_raw: bool,
        legacy_count: usize,
        hide_legacy: bool,
    ) {
        let empty = vbox(5);
        empty.set_margin_top(50);
        empty.set_margin_start(20);
        let (title_text, note_text) = if total == 0 {
            (
                "Waiting for events.",
                "breadd is connected. The stream fills as events arrive.",
            )
        } else if (hide_raw && raw_count > 0) || (hide_legacy && legacy_count > 0) {
            (
                "Only hidden Hyprland compatibility events match.",
                "Raw and legacy Hyprland events are hidden by default. Turn on Raw or Legacy in the chip row to see them.",
            )
        } else {
            (
                "No events match.",
                "Try a broader pattern such as bread.** or clear the source filter.",
            )
        };
        let title = label(title_text);
        title.add_css_class("bold");
        empty.append(&title);
        empty.append(&dim_label(note_text));
        self.list.append(&empty);
    }

    fn update_header(&self, grouped: bool) {
        while let Some(child) = self.list_header.first_child() {
            self.list_header.remove(&child);
        }
        if grouped {
            self.list_header.append(&header_cell("COUNT", 7, false));
            self.list_header.append(&header_cell("EVENT TYPE", 0, true));
            self.list_header
                .append(&header_cell("LATEST SUMMARY", 0, true));
            self.list_header.append(&header_cell("SOURCE", 13, false));
            self.list_header.append(&header_cell("HANDLERS", 9, false));
            self.list_header.append(&header_cell("LAST", 8, false));
        } else {
            self.list_header.append(&header_cell("", 3, false));
            self.list_header.append(&header_cell("TIME", 11, false));
            self.list_header.append(&header_cell("SOURCE", 13, false));
            self.list_header.append(&header_cell("EVENT", 0, true));
            self.list_header.append(&header_cell("SUMMARY", 0, true));
            self.list_header.append(&header_cell("HANDLERS", 9, false));
            self.list_header.append(&header_cell("ID", 9, false));
            self.list_header.set_tooltip_text(Some(
                "Chronological order. Toggle Grouped for one row per event type.",
            ));
        }
    }

    fn group_row(&self, group: GroupRow, index: usize) -> Button {
        let button = Button::new();
        button.add_css_class("group-row");
        if group.selected {
            button.add_css_class("selected");
        }
        if index % 2 == 1 {
            button.add_css_class("alt");
        }
        button.set_hexpand(true);
        button.set_tooltip_text(Some(&format!(
            "Show {} instances of {}\nHandlers: {}",
            group.count,
            group.name,
            if group.handler_tooltip.is_empty() {
                "none".to_string()
            } else {
                group.handler_tooltip.clone()
            }
        )));
        let content = hbox(8);
        let count = Label::new(Some(&format!("×{}", group.count)));
        count.add_css_class("group-count");
        count.set_width_chars(6);
        count.set_xalign(0.0);
        content.append(&count);
        let name = Label::new(Some(&group.name));
        name.add_css_class("event-name");
        name.set_hexpand(true);
        name.set_xalign(0.0);
        ellipsize(&name, 40);
        content.append(&name);
        let summary = Label::new(Some(&group.summary));
        summary.add_css_class("event-summary");
        summary.set_hexpand(true);
        summary.set_xalign(0.0);
        summary.set_ellipsize(pango::EllipsizeMode::End);
        summary.set_max_width_chars(60);
        content.append(&summary);
        content.append(&source_cell(&group.source));
        content.append(&handler_cell(group.handlers, &group.handler_tooltip, false));
        let last = Label::new(Some(&model::age_label(group.latest)));
        last.add_css_class("event-time");
        last.set_width_chars(8);
        last.set_xalign(1.0);
        content.append(&last);
        let chevron = Label::new(Some("›"));
        chevron.add_css_class("accent");
        content.append(&chevron);
        button.set_child(Some(&content));
        let actions = self.actions.clone();
        let name = group.name.clone();
        button.connect_clicked(move |_| {
            let _ = actions.send(Action::ShowInstances(name.clone()));
        });
        button
    }
    fn event_row(&self, shared: &Shared, row: RowData) -> Button {
        let button = Button::new();
        button.add_css_class("event-row");
        if row.selected {
            button.add_css_class("selected");
        }
        if row.alt {
            button.add_css_class("alt");
        }
        button.set_hexpand(true);
        button.set_tooltip_text(Some(&format!(
            "{}\n{}\nHandlers: {}",
            row.name,
            row.summary,
            if row.handler_tooltip.is_empty() {
                "none".to_string()
            } else {
                row.handler_tooltip.clone()
            }
        )));

        let content = hbox(8);
        let gutter = Label::new(Some(if row.depth > 0 { "↳" } else { "" }));
        gutter.add_css_class("tree-gutter");
        gutter.set_width_chars(3);
        content.append(&gutter);
        let time = Label::new(Some(&model::timestamp_label(row.timestamp)));
        time.add_css_class("event-time");
        time.set_width_chars(11);
        time.set_xalign(0.0);
        content.append(&time);
        content.append(&source_cell(&row.source));
        let name = Label::new(Some(&row.name));
        name.add_css_class("event-name");
        name.set_hexpand(true);
        name.set_xalign(0.0);
        name.set_margin_start((row.depth as i32) * 14);
        ellipsize(&name, 44);
        content.append(&name);
        let summary = Label::new(Some(&row.summary));
        summary.add_css_class("event-summary");
        summary.set_hexpand(true);
        summary.set_xalign(0.0);
        summary.set_ellipsize(pango::EllipsizeMode::End);
        summary.set_max_width_chars(58);
        content.append(&summary);
        content.append(&handler_cell(row.handlers, &row.handler_tooltip, true));
        let id = Label::new(Some(&model::short_id(&row.id)));
        id.add_css_class("event-id");
        id.set_width_chars(9);
        id.set_xalign(1.0);
        content.append(&id);
        button.set_child(Some(&content));

        let actions = self.actions.clone();
        let event_id = row.id.clone();
        let shared = shared.clone();
        button.connect_clicked(move |_| {
            let mut state = shared.borrow_mut();
            state.selected_event = Some(event_id.clone());
            state.selection_locked = true;
            state.follow_tail = false;
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

            let statuses: HashMap<String, String> = state
                .modules
                .iter()
                .map(|module| (module.name.clone(), module.status.clone()))
                .collect();
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
            let instances = state
                .events
                .iter()
                .filter(|candidate| candidate.event == event.event)
                .count();
            let key = format!(
                "{}:{}:{}:{}:{}:{}",
                event_id,
                traces.len(),
                subscriptions.len(),
                children.len(),
                event.data.to_string().len(),
                instances
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
                statuses,
                instances,
                key,
            )
        };
        if *self.last_inspector_key.borrow() == data.11 {
            return;
        }
        *self.last_inspector_key.borrow_mut() = data.11;
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
            statuses,
            instances,
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
        let mut handler_modules: Vec<String> = Vec::new();
        if !traces.is_empty() {
            for (module, pattern, duration_us, ok) in &traces {
                let status = statuses.get(module).map(String::as_str).unwrap_or("init");
                self.inspector.append(&self.handler_button(
                    module,
                    pattern,
                    &format!("{:.1} ms", *duration_us as f64 / 1000.0),
                    *ok,
                    status,
                ));
                if module != "init.lua" && !handler_modules.contains(module) {
                    handler_modules.push(module.clone());
                }
            }
        } else if !subscriptions.is_empty() {
            for (module, pattern, count) in &subscriptions {
                let status = statuses.get(module).map(String::as_str).unwrap_or("init");
                self.inspector.append(&self.handler_button(
                    module,
                    pattern,
                    &format!("{count} lifetime"),
                    true,
                    status,
                ));
                if module != "init.lua" && !handler_modules.contains(module) {
                    handler_modules.push(module.clone());
                }
            }
            self.inspector.append(&wrapped_note(
                "Timing not recorded yet for this event. Tracing records timing only after an event has been dispatched with the window open.",
            ));
        } else {
            let warning = Label::new(Some(
                "No subscription matched. The event was recorded and dropped.",
            ));
            warning.add_css_class("note");
            warning.add_css_class("warn");
            warning.set_wrap(true);
            warning.set_xalign(0.0);
            self.inspector.append(&warning);
        }
        if !traces.is_empty() || !subscriptions.is_empty() {
            self.inspector.append(&wrapped_note(
                "Click a handler to filter the stream to its subscription pattern.",
            ));
        }

        self.inspector.append(&section_label("Data"));
        let json = JsonView::new(200);
        json.set_value(&payload);
        self.inspector.append(&json.root);

        let actions_row = hbox(6);
        let copy_data = button("Copy data", "small");
        {
            let window = self.window.clone();
            let text =
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
            let actions = self.actions.clone();
            copy_data.connect_clicked(move |_| {
                window.clipboard().set_text(&text);
                let _ = actions.send(Action::Toast("Event data copied".to_string()));
            });
        }
        actions_row.append(&copy_data);
        let copy_name = button("Copy event", "small");
        {
            let window = self.window.clone();
            let name = name.clone();
            let actions = self.actions.clone();
            copy_name.connect_clicked(move |_| {
                window.clipboard().set_text(&name);
                let _ = actions.send(Action::Toast("Event name copied".to_string()));
            });
        }
        actions_row.append(&copy_name);
        self.inspector.append(&actions_row);

        let actions_row = hbox(6);
        if instances > 1 {
            let show = button(&format!("Show {instances} instances"), "small");
            let name = name.clone();
            let actions = self.actions.clone();
            show.connect_clicked(move |_| {
                let _ = actions.send(Action::ShowInstances(name.clone()));
            });
            actions_row.append(&show);
        }
        let stream_filter = button("Filter family", "small");
        {
            let family = model::event_family(&name);
            let actions = self.actions.clone();
            stream_filter.connect_clicked(move |_| {
                let _ = actions.send(Action::FilterStream(format!("bread.{family}.**")));
            });
        }
        actions_row.append(&stream_filter);
        if let Some(module) = handler_modules.first() {
            let open = button("Open module", "small");
            let module = module.clone();
            let actions = self.actions.clone();
            open.connect_clicked(move |_| {
                let _ = actions.send(Action::OpenModule(module.clone()));
            });
            actions_row.append(&open);
        }
        self.inspector.append(&actions_row);

        self.inspector.append(&section_label("Event id"));
        let id_label = Label::new(Some(&event_id));
        id_label.add_css_class("mono");
        id_label.add_css_class("tiny");
        id_label.add_css_class("faint");
        id_label.set_selectable(true);
        id_label.set_wrap(true);
        self.inspector.append(&id_label);
        let copy = button("Copy id", "small");
        copy.set_halign(Align::Start);
        {
            let window = self.window.clone();
            let event_id = event_id.clone();
            let actions = self.actions.clone();
            copy.connect_clicked(move |_| {
                window.clipboard().set_text(&event_id);
                let _ = actions.send(Action::Toast("Event id copied".to_string()));
            });
        }
        self.inspector.append(&copy);
    }

    fn handler_button(
        &self,
        module: &str,
        pattern: &str,
        trailing: &str,
        ok: bool,
        status: &str,
    ) -> Button {
        let button = Button::new();
        button.add_css_class("handler-row");
        button.set_tooltip_text(Some(&format!("Filter the stream to {pattern}")));
        let row = vbox(1);
        let top = hbox(6);
        top.append(&mono(module));
        let class = match status {
            "loaded" => "green",
            "degraded" | "load_error" => "yellow",
            _ => "",
        };
        if !class.is_empty() {
            top.append(&badge(status, class));
        }
        top.append(&horizontal_spacer());
        let trailing = Label::new(Some(trailing));
        trailing.add_css_class("duration");
        if !ok {
            trailing.add_css_class("slow");
        }
        top.append(&trailing);
        row.append(&top);
        let pattern_label = Label::new(Some(pattern));
        pattern_label.add_css_class("handler-pattern");
        pattern_label.set_xalign(0.0);
        pattern_label.set_ellipsize(pango::EllipsizeMode::End);
        pattern_label.set_max_width_chars(36);
        row.append(&pattern_label);
        button.set_child(Some(&row));
        let actions = self.actions.clone();
        let pattern = pattern.to_string();
        button.connect_clicked(move |_| {
            let _ = actions.send(Action::FilterStream(pattern.clone()));
        });
        button
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
        ellipsize(&name_label, 34);
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

fn header_cell(text: &str, width_chars: i32, expand: bool) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("col-head");
    label.set_xalign(0.0);
    if width_chars > 0 {
        label.set_width_chars(width_chars);
    }
    if expand {
        label.set_hexpand(true);
    }
    label
}

fn source_cell(source: &AdapterSource) -> GBox {
    let cell = hbox(6);
    cell.set_size_request(112, -1);
    let dot = dot_widget(source);
    dot.set_valign(Align::Center);
    cell.append(&dot);
    let label = Label::new(Some(&model::source_short(source)));
    label.add_css_class("source-text");
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(12);
    label.set_xalign(0.0);
    if let Some((r, g, b)) = super::hex_rgb(model::source_color(source)) {
        label.set_markup(&format!(
            "<span foreground=\"#{:02x}{:02x}{:02x}\">{}</span>",
            (r * 255.0) as u8,
            (g * 255.0) as u8,
            (b * 255.0) as u8,
            model::source_short(source)
        ));
    }
    cell.append(&label);
    cell
}

fn handler_cell(count: usize, tooltip: &str, fixed: bool) -> Label {
    let label = Label::new(Some(&if count == 0 {
        "0".to_string()
    } else {
        format!("{count} sub{}", if count == 1 { "" } else { "s" })
    }));
    label.add_css_class("handler-count");
    if count == 0 {
        label.add_css_class("zero");
    }
    if fixed {
        label.set_width_chars(9);
    }
    label.set_xalign(1.0);
    if !tooltip.is_empty() {
        label.set_tooltip_text(Some(tooltip));
    }
    label
}

fn handler_summary(state: &super::AppState, event_name: &str) -> (usize, String) {
    let subscriptions = state.matching_subscriptions(event_name);
    let mut modules: Vec<&str> = Vec::new();
    for entry in &subscriptions {
        let module = entry.module_label();
        if !modules.contains(&module) {
            modules.push(module);
        }
    }
    (subscriptions.len(), modules.join(", "))
}

fn wrapped_note(text: &str) -> Label {
    let label = dim_label(text);
    label.set_wrap(true);
    label.set_max_width_chars(30);
    label.set_xalign(0.0);
    label
}

fn draw_volume(cr: &gtk4::cairo::Context, width: i32, height: i32, shared: &Shared) {
    let state = shared.borrow();
    let now = bread_shared::now_unix_ms();
    let window_ms = 120_000u64;
    let buckets = 60usize;
    let bar_w = 3.0_f64;
    let gap = 1.0_f64;
    let height = height as f64;
    let width = width as f64;
    let graph_w = buckets as f64 * (bar_w + gap);
    let x0 = (width - 14.0 - graph_w).max(12.0);

    let mut counts = vec![0u32; buckets];
    for event in &state.events {
        let age = now.saturating_sub(event.timestamp);
        if age <= window_ms {
            let index = ((window_ms - age) as f64 / window_ms as f64 * buckets as f64) as usize;
            let index = index.min(buckets - 1);
            counts[index] += 1;
        }
    }
    let total: u32 = counts.iter().sum();
    let max = counts.iter().copied().max().unwrap_or(1).max(1) as f64;

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.07);
    cr.rectangle(x0, height - 3.5, graph_w, 1.0);
    let _ = cr.fill();

    for (index, count) in counts.iter().enumerate() {
        let bar_height = 2.0 + (*count as f64 / max) * (height - 11.0);
        let x = x0 + index as f64 * (bar_w + gap);
        let y = height - 4.5 - bar_height;
        let alpha = if index + 12 >= buckets { 0.60 } else { 0.22 };
        cr.set_source_rgba(0.85, 0.63, 0.36, alpha);
        super::rounded_rect(cr, x, y, bar_w, bar_height, 1.2);
        let _ = cr.fill();
    }

    let rate = total as f64 / (window_ms as f64 / 1000.0);
    cr.select_font_face(
        "Sans",
        gtk4::cairo::FontSlant::Normal,
        gtk4::cairo::FontWeight::Bold,
    );
    cr.set_font_size(13.0);
    cr.set_source_rgba(0.93, 0.93, 0.93, 0.92);
    let number = format!("{rate:.1}");
    cr.move_to(14.0, height / 2.0 + 4.5);
    let _ = cr.show_text(&number);
    let advance = cr
        .text_extents(&number)
        .map(|ext| ext.x_advance())
        .unwrap_or(20.0);

    cr.select_font_face(
        "Sans",
        gtk4::cairo::FontSlant::Normal,
        gtk4::cairo::FontWeight::Normal,
    );
    cr.set_font_size(9.5);
    cr.set_source_rgba(0.80, 0.80, 0.80, 0.62);
    let caption = if state.paused {
        "events/s · paused"
    } else {
        "events/s · last 2 minutes"
    };
    cr.move_to(14.0 + advance + 6.0, height / 2.0 + 4.0);
    let _ = cr.show_text(caption);
}
