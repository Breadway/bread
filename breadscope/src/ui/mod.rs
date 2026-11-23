//! Application shell, shared state, and cross-view wiring.

pub mod adapters;
pub mod emit;
pub mod flow;
pub mod modules;
pub mod state;
pub mod stream;
pub mod widgets;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use bread_shared::BreadEvent;
use gtk4::prelude::*;
use gtk4::{
    glib, pango, Align, Application, ApplicationWindow, Box as GBox, Button, Label, Orientation,
    Revealer, RevealerTransitionType, ScrolledWindow, Stack, ToggleButton,
};

use crate::ipc::{self, StreamUpdate};
use crate::model::{
    self, Health, Module, Snapshot, SubscriptionEntry, TraceEntry, WidgetSummary, Workflow,
};
use crate::style;

pub const EVENT_CAP: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    Stream,
    Flow,
    Modules,
    State,
    Adapters,
}

impl View {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stream" => Some(View::Stream),
            "flow" => Some(View::Flow),
            "modules" => Some(View::Modules),
            "state" => Some(View::State),
            "adapters" => Some(View::Adapters),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            View::Stream => "stream",
            View::Flow => "flow",
            View::Modules => "modules",
            View::State => "state",
            View::Adapters => "adapters",
        }
    }
}

pub type Shared = Rc<RefCell<AppState>>;

#[derive(Debug)]
pub enum Action {
    Switch(View),
    Emit,
    SetTrace(bool),
    ReloadModules,
    FilterStream(String),
    RefreshStream,
    RefreshState,
    Toast(String),
}

pub struct AppState {
    pub events: VecDeque<BreadEvent>,
    pub seen: HashSet<String>,
    pub paused_buffer: Vec<BreadEvent>,
    pub paused: bool,
    pub filter: String,
    pub source: String,
    pub causal: bool,
    pub selected_event: Option<String>,
    /// True once the user explicitly picks an event; until then the inspector
    /// follows the newest buffered event.
    pub selection_locked: bool,
    pub view: View,
    pub modules: Vec<Module>,
    pub subscriptions: Vec<SubscriptionEntry>,
    pub traces: Vec<TraceEntry>,
    pub health: Option<Health>,
    pub state: serde_json::Value,
    pub workflows: Vec<Workflow>,
    pub widgets: Vec<WidgetSummary>,
    pub daemon_up: bool,
    pub stream_error: Option<String>,
    pub selected_module: Option<String>,
    pub selected_flow: Option<String>,
    pub state_key: String,
    pub trace_enabled: bool,
    pub status: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            seen: HashSet::new(),
            paused_buffer: Vec::new(),
            paused: false,
            filter: String::new(),
            source: "all".to_string(),
            causal: true,
            selected_event: None,
            selection_locked: false,
            view: View::Stream,
            modules: Vec::new(),
            subscriptions: Vec::new(),
            traces: Vec::new(),
            health: None,
            state: serde_json::Value::Null,
            workflows: Vec::new(),
            widgets: Vec::new(),
            daemon_up: false,
            stream_error: None,
            selected_module: None,
            selected_flow: None,
            state_key: String::new(),
            trace_enabled: true,
            status: "connecting".to_string(),
        }
    }
}

impl AppState {
    pub fn push_events(&mut self, events: Vec<BreadEvent>) {
        if self.paused {
            for event in events {
                if self.seen.insert(event.id.clone()) {
                    self.paused_buffer.push(event);
                }
            }
            return;
        }
        for event in events {
            if !self.seen.insert(event.id.clone()) {
                continue;
            }
            self.events.push_back(event);
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.events.len() > EVENT_CAP {
            if let Some(old) = self.events.pop_front() {
                self.seen.remove(&old.id);
            }
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        if !paused && !self.paused_buffer.is_empty() {
            let buffered = std::mem::take(&mut self.paused_buffer);
            for event in buffered {
                self.events.push_back(event);
            }
            self.trim();
        }
    }

    pub fn pending_count(&self) -> usize {
        self.paused_buffer.len()
    }

    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.daemon_up = snapshot.daemon_up;
        if let Some(health) = snapshot.health {
            self.health = Some(health);
            self.stream_error = None;
        } else if !snapshot.daemon_up {
            self.health = None;
        }
        self.modules = snapshot.modules;
        self.subscriptions = snapshot.subscriptions;
        self.traces = snapshot.traces;
        if !snapshot.state.is_null() {
            if self.state_key.is_empty() {
                if let Some(object) = snapshot.state.as_object() {
                    self.state_key = if object.contains_key("devices") {
                        "devices".to_string()
                    } else {
                        object.keys().next().cloned().unwrap_or_default()
                    };
                }
            }
            self.state = snapshot.state;
        }
        self.workflows = snapshot.workflows;
        self.widgets = snapshot.widgets;

        if self.selected_module.is_none() {
            self.selected_module = self
                .modules
                .iter()
                .find(|module| module.name == "bread.devices")
                .or_else(|| self.modules.first())
                .map(|module| module.name.clone());
        }
    }

    pub fn module(&self, name: &str) -> Option<&Module> {
        self.modules.iter().find(|module| module.name == name)
    }

    pub fn matching_subscriptions(&self, event_name: &str) -> Vec<&SubscriptionEntry> {
        let mut matches: Vec<&SubscriptionEntry> = self
            .subscriptions
            .iter()
            .filter(|entry| model::matches_pattern(&entry.pattern, event_name))
            .collect();
        matches.sort_by(|a, b| {
            a.module_label()
                .cmp(b.module_label())
                .then_with(|| a.pattern.cmp(&b.pattern))
        });
        matches
    }

    pub fn traces_for(&self, event_id: &str) -> Vec<&TraceEntry> {
        self.traces
            .iter()
            .filter(|trace| trace.event_id == event_id)
            .collect()
    }

    pub fn event(&self, id: &str) -> Option<&BreadEvent> {
        self.events.iter().find(|event| event.id == id)
    }

    pub fn event_status_line(&self) -> String {
        if let Some(error) = self.stream_error.as_deref() {
            return error.to_string();
        }
        if self.daemon_up {
            "connected".to_string()
        } else {
            "daemon not reachable".to_string()
        }
    }

    pub fn source_matches(&self, event: &BreadEvent) -> bool {
        match self.source.as_str() {
            "all" => true,
            "app" => matches!(event.source, bread_shared::AdapterSource::App(_)),
            other => model::source_key(&event.source) == other,
        }
    }

    pub fn matches_filter(&self, event: &BreadEvent) -> bool {
        if !self.source_matches(event) {
            return false;
        }
        let pattern = self.filter.trim();
        if pattern.is_empty() {
            return true;
        }
        if pattern.contains('*') || pattern.contains('?') {
            model::matches_pattern(pattern, &event.event)
        } else {
            event.event.to_lowercase().contains(&pattern.to_lowercase())
                || model::source_label(&event.source)
                    .to_lowercase()
                    .contains(&pattern.to_lowercase())
        }
    }

    /// Filtered references, sorted the way the active mode reads best:
    /// chronological for a causal tree (parents before children), newest
    /// first otherwise. Depth is counted only through visible ancestors, so a
    /// filtered-out parent does not leave orphaned indentation.
    pub fn visible_events(&self) -> Vec<(&BreadEvent, usize)> {
        let mut visible: Vec<&BreadEvent> = self
            .events
            .iter()
            .filter(|event| self.matches_filter(event))
            .collect();
        if self.causal {
            visible.sort_by_key(|event| event.timestamp);
        } else {
            visible.sort_by_key(|event| std::cmp::Reverse(event.timestamp));
        }
        let ids: HashSet<&str> = visible.iter().map(|event| event.id.as_str()).collect();
        let by_id: HashMap<&str, &BreadEvent> = self
            .events
            .iter()
            .map(|event| (event.id.as_str(), event))
            .collect();
        visible
            .into_iter()
            .map(|event| {
                let mut depth = 0usize;
                let mut current = event;
                let mut guard = 0usize;
                while let Some(parent_id) = current.caused_by.as_deref() {
                    if guard > 64 || !ids.contains(parent_id) {
                        break;
                    }
                    match by_id.get(parent_id) {
                        Some(parent) if !std::ptr::eq(*parent, event) => {
                            depth += 1;
                            current = parent;
                            guard += 1;
                        }
                        _ => break,
                    }
                }
                (event, depth)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// small widget helpers
// ---------------------------------------------------------------------------

pub fn label(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label
}

pub fn mono(text: &str) -> Label {
    let label = label(text);
    label.add_css_class("mono");
    label
}

pub fn dim_label(text: &str) -> Label {
    let label = label(text);
    label.add_css_class("dim");
    label
}

pub fn page_sub(text: &str) -> Label {
    let label = dim_label(text);
    label.set_wrap(true);
    label.set_max_width_chars(96);
    label
}

pub fn small_label(text: &str) -> Label {
    let label = label(text);
    label.add_css_class("small");
    label
}

pub fn section_label(text: &str) -> Label {
    let label = Label::new(Some(&text.to_uppercase()));
    label.set_xalign(0.0);
    label.add_css_class("section-label");
    label
}

pub fn badge(text: &str, class: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("badge");
    if !class.is_empty() {
        label.add_css_class(class);
    }
    label
}

pub fn hbox(spacing: i32) -> GBox {
    GBox::new(Orientation::Horizontal, spacing)
}

pub fn vbox(spacing: i32) -> GBox {
    GBox::new(Orientation::Vertical, spacing)
}

pub fn horizontal_spacer() -> GBox {
    let spacer = GBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    spacer
}

pub fn button(text: &str, class: &str) -> Button {
    let button = Button::with_label(text);
    button.add_css_class("btn");
    if !class.is_empty() {
        button.add_css_class(class);
    }
    button
}

pub fn scrolled(child: &impl IsA<gtk4::Widget>) -> ScrolledWindow {
    let scrolled = ScrolledWindow::new();
    scrolled.set_child(Some(child));
    scrolled.set_vexpand(true);
    scrolled.set_hexpand(true);
    scrolled
}

pub fn ellipsize(label: &Label, chars: i32) {
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(chars);
}

pub fn dot_rgba(source: &bread_shared::AdapterSource) -> (f64, f64, f64) {
    hex_rgb(model::source_color(source)).unwrap_or((0.55, 0.55, 0.55))
}

pub fn hex_rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let value = hex.trim_start_matches('#');
    if value.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&value[0..2], 16).ok()? as f64 / 255.0;
    let g = u8::from_str_radix(&value[2..4], 16).ok()? as f64 / 255.0;
    let b = u8::from_str_radix(&value[4..6], 16).ok()? as f64 / 255.0;
    Some((r, g, b))
}

/// A painted dot, so each adapter source has a stable colour without relying
/// on a stylesheet class per source.
pub fn dot_widget(source: &bread_shared::AdapterSource) -> gtk4::DrawingArea {
    let (r, g, b) = dot_rgba(source);
    let area = gtk4::DrawingArea::new();
    area.set_content_width(9);
    area.set_content_height(9);
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

pub fn text_view(content: &str, height: i32) -> ScrolledWindow {
    let view = gtk4::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk4::WrapMode::None);
    view.set_top_margin(7);
    view.set_bottom_margin(7);
    view.set_left_margin(8);
    view.set_right_margin(8);
    view.buffer().set_text(content);
    view.add_css_class("json-view");
    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&view));
    scroll.set_min_content_height(height);
    scroll.set_max_content_height(height);
    scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
    scroll.set_hexpand(true);
    scroll
}

// ---------------------------------------------------------------------------
// shell
// ---------------------------------------------------------------------------

pub struct Topbar {
    status: Label,
    meta: Label,
    trace: ToggleButton,
}

impl Topbar {
    pub fn refresh(&self, shared: &Shared) {
        let state = shared.borrow();
        self.status.remove_css_class("ok");
        self.status.remove_css_class("warn");
        self.status.remove_css_class("bad");
        let (text, class) = if state.daemon_up {
            ("connected".to_string(), "ok")
        } else if state.stream_error.is_some() {
            ("disconnected".to_string(), "bad")
        } else {
            ("connecting".to_string(), "warn")
        };
        self.status.set_text(&text);
        self.status.add_css_class(class);

        let meta = match state.health.as_ref() {
            Some(health) => format!(
                "pid {}   v{}   up {}   api {}",
                health.pid,
                if health.version.is_empty() {
                    "?"
                } else {
                    &health.version
                },
                model::uptime_label(health.uptime_ms),
                if health.api_version.is_empty() {
                    "?"
                } else {
                    &health.api_version
                }
            ),
            None => "no daemon metadata".to_string(),
        };
        self.meta.set_text(&meta);
        self.trace.set_active(state.trace_enabled);
        self.trace.set_label(if state.trace_enabled {
            "Trace on"
        } else {
            "Trace"
        });
        if state.trace_enabled {
            self.trace.add_css_class("active");
        } else {
            self.trace.remove_css_class("active");
        }
    }
}

pub struct Sidebar {
    counts: HashMap<View, Label>,
    footer: Label,
}

impl Sidebar {
    pub fn refresh(&self, shared: &Shared) {
        let state = shared.borrow();
        if let Some(count) = self.counts.get(&View::Stream) {
            count.set_text(&state.events.len().to_string());
        }
        if let Some(count) = self.counts.get(&View::Modules) {
            count.set_text(&state.modules.len().to_string());
        }
        if let Some(count) = self.counts.get(&View::Adapters) {
            let issues = state
                .health
                .as_ref()
                .map(|health| {
                    health
                        .adapters
                        .values()
                        .filter(|status| status.as_str() != "connected")
                        .count()
                })
                .unwrap_or(0);
            count.set_text(&issues.to_string());
        }
        self.footer.set_text(if state.daemon_up {
            "daemon reachable"
        } else if state.stream_error.is_some() {
            "daemon unreachable"
        } else {
            "connecting"
        });
    }
}

pub struct Hub {
    pub shared: Shared,
    pub window: ApplicationWindow,
    pub stack: Stack,
    pub topbar: Topbar,
    pub sidebar: Sidebar,
    pub stream: stream::StreamView,
    pub flow: flow::FlowView,
    pub modules: modules::ModulesView,
    pub state: state::StateView,
    pub adapters: adapters::AdaptersView,
    nav: Vec<(View, Button)>,
    toast: Revealer,
    toast_label: Label,
    toast_source: RefCell<Option<glib::SourceId>>,
    current: Cell<View>,
    actions: Sender<Action>,
}

impl Hub {
    pub fn switch_view(&self, view: View) {
        self.current.set(view);
        self.shared.borrow_mut().view = view;
        self.stack.set_visible_child_name(view.id());
        for (candidate, button) in &self.nav {
            if *candidate == view {
                button.add_css_class("active");
            } else {
                button.remove_css_class("active");
            }
        }
        if view == View::Stream {
            self.stream.refresh(&self.shared);
        } else if view == View::Flow {
            self.flow.refresh(&self.shared);
        } else if view == View::Modules {
            self.modules.refresh(&self.shared);
        } else if view == View::State {
            self.state.refresh(&self.shared);
        } else if view == View::Adapters {
            self.adapters.refresh(&self.shared);
        }
    }

    pub fn toast(&self, message: &str) {
        self.toast_label.set_text(message);
        self.toast.set_reveal_child(true);
        if let Some(source) = self.toast_source.borrow_mut().take() {
            source.remove();
        }
        let reveal = self.toast.clone();
        let source = glib::timeout_add_seconds_local(3, move || {
            reveal.set_reveal_child(false);
            glib::ControlFlow::Break
        });
        *self.toast_source.borrow_mut() = Some(source);
    }

    pub fn handle_action(&self, action: Action) {
        match action {
            Action::Switch(view) => self.switch_view(view),
            Action::Emit => emit::open(&self.window, &self.shared, &self.actions),
            Action::SetTrace(enabled) => {
                self.shared.borrow_mut().trace_enabled = enabled;
                let actions = self.actions.clone();
                std::thread::spawn(move || {
                    let message = match ipc::set_trace(enabled) {
                        Ok(_) if enabled => "Handler tracing enabled".to_string(),
                        Ok(_) => "Handler tracing disabled".to_string(),
                        Err(err) => format!("trace.set failed: {err}"),
                    };
                    let _ = actions.send(Action::Toast(message));
                });
            }
            Action::ReloadModules => {
                let actions = self.actions.clone();
                std::thread::spawn(move || {
                    let message = match ipc::request("modules.reload", serde_json::json!({})) {
                        Ok(_) => "Lua runtime reloaded".to_string(),
                        Err(err) => format!("modules.reload failed: {err}"),
                    };
                    let _ = actions.send(Action::Toast(message));
                });
            }
            Action::FilterStream(pattern) => {
                self.shared.borrow_mut().filter = pattern.clone();
                self.stream.set_filter(&pattern);
                self.switch_view(View::Stream);
            }
            Action::RefreshStream => self.stream.refresh(&self.shared),
            Action::RefreshState => self.state.refresh(&self.shared),
            Action::Toast(message) => self.toast(&message),
        }
    }
}

fn nav_button(icon: &str, text: &str, count: &str) -> (Button, Label) {
    let button = Button::new();
    button.add_css_class("nav-row");
    button.set_hexpand(true);
    let row = hbox(8);
    let image = gtk4::Image::from_icon_name(icon);
    image.set_pixel_size(15);
    row.append(&image);
    row.append(&label(text));
    row.append(&horizontal_spacer());
    let count_label = mono(count);
    count_label.add_css_class("nav-count");
    row.append(&count_label);
    button.set_child(Some(&row));
    (button, count_label)
}

pub fn build_window(app: &Application, initial_view: Option<View>) -> ApplicationWindow {
    bread_theme::gtk::apply_shared();
    style::apply_global();

    let shared: Shared = Rc::new(RefCell::new(AppState::default()));
    let (action_tx, action_rx) = mpsc::channel::<Action>();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("breadscope")
        .default_width(1360)
        .default_height(860)
        .build();
    window.add_css_class("scope");

    let root = vbox(0);
    root.add_css_class("scope");

    // ---- top bar ---------------------------------------------------------
    let topbar_box = hbox(12);
    topbar_box.add_css_class("topbar");

    let loaf = gtk4::DrawingArea::new();
    loaf.set_content_width(30);
    loaf.set_content_height(24);
    loaf.set_valign(Align::Center);
    loaf.set_draw_func(|_, cr, width, height| {
        let (r, g, b) = hex_rgb("#d9a05b").unwrap_or((0.85, 0.62, 0.36));
        rounded_rect(cr, 0.5, 1.0, width as f64 - 1.0, height as f64 - 2.0, 7.0);
        cr.set_source_rgb(r, g, b);
        let _ = cr.fill();
        cr.set_source_rgba(0.26, 0.15, 0.04, 0.45);
        cr.set_line_width(1.6);
        for index in 0..3 {
            let y = 7.0 + index as f64 * 5.0;
            cr.move_to(6.0, y);
            cr.line_to(width as f64 - 6.0, y - 1.0);
        }
        let _ = cr.stroke();
    });

    let brand = vbox(0);
    let name = label("breadscope");
    name.add_css_class("brand-name");
    let sub = label("BREAD OBSERVABILITY");
    sub.add_css_class("brand-sub");
    brand.append(&name);
    brand.append(&sub);
    topbar_box.append(&loaf);
    topbar_box.append(&brand);

    let mock_tag = Label::new(Some("GTK4"));
    mock_tag.add_css_class("mock-tag");
    mock_tag.set_valign(Align::Center);
    topbar_box.append(&mock_tag);
    topbar_box.append(&horizontal_spacer());

    let status = Label::new(Some("connecting"));
    status.add_css_class("status-pill");
    status.set_valign(Align::Center);
    topbar_box.append(&status);

    let meta = label("");
    meta.add_css_class("top-meta");
    meta.set_valign(Align::Center);
    topbar_box.append(&meta);

    let trace = ToggleButton::with_label("Trace");
    trace.add_css_class("pill");
    trace.set_valign(Align::Center);
    trace.set_tooltip_text(Some(
        "Record which Lua handlers run for each event, with timing. Off clears the ring.",
    ));
    {
        let tx = action_tx.clone();
        trace.connect_toggled(move |button| {
            let _ = tx.send(Action::SetTrace(button.is_active()));
        });
    }
    topbar_box.append(&trace);

    let emit_button = button("Emit event", "primary");
    {
        let tx = action_tx.clone();
        emit_button.connect_clicked(move |_| {
            let _ = tx.send(Action::Emit);
        });
    }
    topbar_box.append(&emit_button);
    root.append(&topbar_box);

    // ---- body: rail + stack ---------------------------------------------
    let body = hbox(0);
    body.set_vexpand(true);
    root.append(&body);

    let rail = vbox(0);
    rail.add_css_class("sidebar");
    rail.set_width_request(210);
    rail.set_hexpand(false);
    rail.set_halign(Align::Start);

    let mut nav = Vec::new();
    let nav_buttons = [
        (
            View::Stream,
            "utilities-system-monitor-symbolic",
            "Event stream",
            "0",
        ),
        (
            View::Flow,
            "network-transmit-receive-symbolic",
            "Interaction map",
            "",
        ),
        (
            View::Modules,
            "application-x-executable-symbolic",
            "Modules",
            "0",
        ),
        (View::State, "drive-harddisk-symbolic", "Runtime state", ""),
        (
            View::Adapters,
            "preferences-system-symbolic",
            "Adapters and health",
            "0",
        ),
    ];
    let mut counts = HashMap::new();
    let mut current_group = "";
    for (view, icon, text, initial) in nav_buttons {
        let group = match view {
            View::Stream | View::Flow => "OBSERVE",
            View::Modules | View::State => "INSPECT",
            View::Adapters => "SYSTEM",
        };
        if group != current_group {
            let heading = Label::new(Some(group));
            heading.set_xalign(0.0);
            heading.add_css_class("nav-group");
            rail.append(&heading);
            current_group = group;
        }
        let (button, count) = nav_button(icon, text, initial);
        {
            let tx = action_tx.clone();
            button.connect_clicked(move |_| {
                let _ = tx.send(Action::Switch(view));
            });
        }
        rail.append(&button);
        counts.insert(view, count);
        nav.push((view, button));
    }

    rail.append(&horizontal_spacer());
    let rail_card = vbox(3);
    rail_card.add_css_class("rail-card");
    let footer = label("connecting");
    footer.add_css_class("small");
    rail_card.append(&footer);
    let socket = Label::new(Some(&bread_shared::resolve_socket_path().to_string_lossy()));
    socket.add_css_class("socket");
    socket.set_wrap(true);
    socket.set_max_width_chars(28);
    socket.set_xalign(0.0);
    rail_card.append(&socket);
    let about = Button::with_label("What is this?");
    about.add_css_class("btn");
    about.add_css_class("small");
    about.set_margin_top(6);
    {
        let parent = window.clone();
        about.connect_clicked(move |_| show_about(&parent));
    }
    rail_card.append(&about);
    rail.append(&rail_card);
    body.append(&rail);

    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.set_transition_type(gtk4::StackTransitionType::Crossfade);
    stack.set_transition_duration(120);

    let stream_view = stream::StreamView::new(&shared, &window, action_tx.clone());
    let flow_view = flow::FlowView::new(&shared);
    let modules_view = modules::ModulesView::new(&shared, action_tx.clone());
    let state_view = state::StateView::new(&shared, &window, action_tx.clone());
    let adapters_view = adapters::AdaptersView::new(&shared);

    stack.add_named(stream_view.root(), Some(View::Stream.id()));
    stack.add_named(flow_view.root(), Some(View::Flow.id()));
    stack.add_named(modules_view.root(), Some(View::Modules.id()));
    stack.add_named(state_view.root(), Some(View::State.id()));
    stack.add_named(adapters_view.root(), Some(View::Adapters.id()));
    body.append(&stack);

    // ---- toast -----------------------------------------------------------
    let toast_layer = gtk4::Overlay::new();
    toast_layer.set_child(Some(&root));
    let toast_anchor = vbox(0);
    toast_anchor.set_valign(Align::End);
    toast_anchor.set_halign(Align::Center);
    toast_anchor.set_margin_bottom(18);
    let toast = Revealer::new();
    toast.set_transition_type(RevealerTransitionType::SlideUp);
    toast.set_transition_duration(140);
    let toast_label = Label::new(Some(""));
    toast_label.add_css_class("toast");
    toast.set_child(Some(&toast_label));
    toast_anchor.append(&toast);
    toast_layer.add_overlay(&toast_anchor);
    window.set_child(Some(&toast_layer));

    let hub = Rc::new(Hub {
        shared: shared.clone(),
        window: window.clone(),
        stack,
        topbar: Topbar {
            status,
            meta,
            trace,
        },
        sidebar: Sidebar { counts, footer },
        stream: stream_view,
        flow: flow_view,
        modules: modules_view,
        state: state_view,
        adapters: adapters_view,
        nav,
        toast,
        toast_label,
        toast_source: RefCell::new(None),
        current: Cell::new(View::Stream),
        actions: action_tx.clone(),
    });
    hub.switch_view(initial_view.unwrap_or(View::Stream));

    {
        let focus = gtk4::gio::SimpleAction::new("focus-filter", None);
        let hub_for_focus = hub.clone();
        focus.connect_activate(move |_, _| {
            hub_for_focus.switch_view(View::Stream);
            hub_for_focus.stream.focus_filter();
        });
        window.add_action(&focus);
        let emit_action = gtk4::gio::SimpleAction::new("emit-event", None);
        let hub_for_emit = hub.clone();
        emit_action.connect_activate(move |_, _| {
            hub_for_emit.handle_action(Action::Emit);
        });
        window.add_action(&emit_action);
        if let Some(application) = window.application() {
            application.set_accels_for_action("win.focus-filter", &["<Ctrl>f"]);
            application.set_accels_for_action("win.emit-event", &["<Ctrl><Shift>e", "F2"]);
        }
    }

    // ---- background loops -------------------------------------------------
    let (stream_tx, stream_rx) = mpsc::channel::<StreamUpdate>();
    let stream_handle = ipc::Stream::spawn(stream_tx);
    let (snapshot_tx, snapshot_rx) = mpsc::channel::<Snapshot>();
    let poller = ipc::Poller::spawn(snapshot_tx);

    std::thread::spawn(|| {
        if let Err(err) = ipc::set_trace(true) {
            eprintln!("breadscope: could not enable tracing: {err}");
        }
    });

    let tick_hub = hub.clone();
    let tick_actions: Receiver<Action> = action_rx;
    let last_stream_refresh = Cell::new(Instant::now());
    glib::timeout_add_local(Duration::from_millis(80), move || {
        let mut got_events = false;
        for update in ipc::try_drain(&stream_rx, 256) {
            match update {
                StreamUpdate::Connected(connected) => {
                    let mut state = tick_hub.shared.borrow_mut();
                    state.daemon_up = connected;
                    if connected {
                        state.stream_error = None;
                    }
                }
                StreamUpdate::Events(events) => {
                    got_events = true;
                    tick_hub.shared.borrow_mut().push_events(events);
                }
                StreamUpdate::Error(error) => {
                    let mut state = tick_hub.shared.borrow_mut();
                    state.daemon_up = false;
                    state.stream_error = Some(error);
                }
            }
        }

        let mut got_snapshot = false;
        for snapshot in ipc::try_drain(&snapshot_rx, 4) {
            tick_hub.shared.borrow_mut().apply_snapshot(snapshot);
            got_snapshot = true;
        }

        for action in ipc::try_drain(&tick_actions, 32) {
            tick_hub.handle_action(action);
        }

        tick_hub.topbar.refresh(&tick_hub.shared);
        tick_hub.sidebar.refresh(&tick_hub.shared);

        let view = tick_hub.current.get();
        if view == View::Stream
            && (got_events || got_snapshot)
            && (got_events && last_stream_refresh.get().elapsed() > Duration::from_millis(180)
                || got_snapshot)
        {
            last_stream_refresh.set(Instant::now());
            tick_hub.stream.refresh(&tick_hub.shared);
        } else if view == View::Stream && got_events {
            tick_hub.stream.refresh_pending(&tick_hub.shared);
        }
        if view == View::Flow && (got_events || got_snapshot) {
            tick_hub.flow.refresh(&tick_hub.shared);
        }
        if view == View::Modules && got_snapshot {
            tick_hub.modules.refresh(&tick_hub.shared);
        }
        if view == View::Adapters && got_snapshot {
            tick_hub.adapters.refresh(&tick_hub.shared);
        }
        if view == View::State && got_snapshot {
            tick_hub.state.refresh(&tick_hub.shared);
        }

        glib::ControlFlow::Continue
    });

    {
        let stream_handle = stream_handle;
        let poller = poller;
        window.connect_close_request(move |_| {
            stream_handle.stop();
            poller.stop();
            std::thread::spawn(|| {
                let _ = ipc::set_trace(false);
            });
            glib::Propagation::Proceed
        });
    }

    window.present();
    window
}

fn show_about(parent: &ApplicationWindow) {
    let dialog = gtk4::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("About breadscope")
        .default_width(440)
        .build();
    dialog.add_css_class("scope");
    let content = vbox(10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let title = label("breadscope");
    title.add_css_class("dialog-title");
    content.append(&title);
    let body = label(
        "A local GTK4 view of breadd: live events and causal chains, module subscriptions and handler timing, the interaction map, runtime state, workflows, widgets, and adapter health.\n\nEverything comes from the daemon's Unix socket. No network listener, no state edits, no always-on tracing: handler timing runs only while this window is open and is cleared when it closes.",
    );
    body.set_wrap(true);
    body.set_max_width_chars(52);
    content.append(&body);
    let hint = mono("breadscope 0.11.0");
    hint.add_css_class("small");
    hint.add_css_class("faint");
    content.append(&hint);
    let close = button("Close", "");
    close.set_halign(Align::End);
    {
        let dialog = dialog.clone();
        close.connect_clicked(move |_| dialog.close());
    }
    content.append(&close);
    dialog.set_child(Some(&content));
    dialog.present();
}

pub fn rounded_rect(
    cr: &gtk4::cairo::Context,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    radius: f64,
) {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    cr.new_sub_path();
    cr.arc(
        x + width - radius,
        y + radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        0.0,
    );
    cr.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        x + radius,
        y + height - radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + radius,
        y + radius,
        radius,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    cr.close_path();
}

pub fn fit_text(cr: &gtk4::cairo::Context, text: &str, max_width: f64) -> String {
    if cr
        .text_extents(text)
        .map(|extents| extents.width())
        .unwrap_or(0.0)
        <= max_width
    {
        return text.to_string();
    }
    let mut truncated = String::new();
    for character in text.chars() {
        let candidate = format!("{truncated}{character}…");
        if cr
            .text_extents(&candidate)
            .map(|extents| extents.width())
            .unwrap_or(0.0)
            > max_width
        {
            break;
        }
        truncated.push(character);
    }
    if truncated.is_empty() {
        "…".to_string()
    } else {
        format!("{truncated}…")
    }
}
