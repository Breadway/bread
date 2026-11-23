//! Runtime state, workflows, and rendered widget trees.

use gtk4::prelude::*;
use gtk4::{
    pango, Align, Box as GBox, Button, FlowBox, Label, Orientation, ScrolledWindow, Stack,
    StackSwitcher,
};

use super::{
    button, dim_label, ellipsize, hbox, horizontal_spacer, label, mono, page_sub, text_view, vbox,
    Action, Shared,
};
use crate::model;

pub struct StateView {
    root: GBox,
    stack: Stack,
    keys: GBox,
    runtime_panel: GBox,
    workflows: GBox,
    widgets: FlowBox,
    window: gtk4::ApplicationWindow,
    signature: std::cell::RefCell<String>,
}

impl StateView {
    pub fn new(
        shared: &Shared,
        window: &gtk4::ApplicationWindow,
        actions: std::sync::mpsc::Sender<Action>,
    ) -> Self {
        let root = vbox(0);
        let header = hbox(12);
        header.add_css_class("page");
        let titles = vbox(1);
        let title = label("Runtime state");
        title.add_css_class("page-title");
        titles.append(&title);
        titles.append(&page_sub(
            "The same shapes returned by state.dump, workflows.list, and widgets.list. Read-only: breadscope never edits daemon state.",
        ));
        header.append(&titles);
        header.append(&horizontal_spacer());

        let stack = Stack::new();
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        let switcher = StackSwitcher::new();
        switcher.set_stack(Some(&stack));
        header.append(&switcher);
        let refresh = button("Refresh", "small");
        {
            let actions = actions.clone();
            refresh.connect_clicked(move |_| {
                let _ = actions.send(Action::RefreshState);
            });
        }
        header.append(&refresh);
        root.append(&header);

        let keys = vbox(2);
        keys.set_valign(Align::Start);
        let keys_scroll = ScrolledWindow::new();
        keys_scroll.set_child(Some(&keys));
        keys_scroll.set_width_request(180);
        keys_scroll.set_hexpand(false);
        keys_scroll.set_vexpand(true);

        let runtime_panel = vbox(6);
        runtime_panel.set_valign(Align::Start);
        let runtime_scroll = ScrolledWindow::new();
        runtime_scroll.set_child(Some(&runtime_panel));
        runtime_scroll.set_vexpand(true);
        runtime_scroll.set_hexpand(true);

        let runtime = hbox(0);
        runtime.set_vexpand(true);
        runtime_scroll.set_hexpand(true);
        runtime.append(&keys_scroll);
        runtime.append(&runtime_scroll);

        let workflows = vbox(0);
        workflows.set_valign(Align::Start);
        let workflows_scroll = ScrolledWindow::new();
        workflows_scroll.set_child(Some(&workflows));
        workflows_scroll.set_vexpand(true);
        workflows_scroll.set_hexpand(true);

        let widgets = FlowBox::new();
        widgets.set_selection_mode(gtk4::SelectionMode::None);
        widgets.set_homogeneous(true);
        widgets.set_min_children_per_line(1);
        widgets.set_max_children_per_line(3);
        widgets.set_column_spacing(10);
        widgets.set_row_spacing(10);
        widgets.set_valign(Align::Start);
        widgets.set_hexpand(true);
        let widgets_scroll = ScrolledWindow::new();
        widgets_scroll.set_child(Some(&widgets));
        widgets_scroll.set_vexpand(true);
        widgets_scroll.set_hexpand(true);
        stack.add_titled(&runtime, Some("runtime"), "Runtime");
        stack.add_titled(&workflows_scroll, Some("workflows"), "Workflows");
        stack.add_titled(&widgets_scroll, Some("widgets"), "Widgets");
        root.append(&stack);

        let view = Self {
            root,
            stack,
            keys,
            runtime_panel,
            workflows,
            widgets,
            window: window.clone(),
            signature: std::cell::RefCell::new(String::new()),
        };
        view.refresh(shared);
        view
    }

    pub fn root(&self) -> &GBox {
        &self.root
    }

    pub fn refresh(&self, shared: &Shared) {
        let signature = {
            let state = shared.borrow();
            format!(
                "{}:{}:{}:{}",
                serde_json::to_string(&state.state).unwrap_or_default(),
                state.workflows.len(),
                state.widgets.len(),
                state.state_key
            )
        };
        if *self.signature.borrow() == signature {
            return;
        }
        *self.signature.borrow_mut() = signature;
        render_runtime(&self.keys, &self.runtime_panel, &self.window, shared);
        render_workflows(&self.workflows, shared);
        render_widgets(&self.widgets, shared);
        let _ = &self.stack;
    }
}

fn render_runtime(keys: &GBox, panel: &GBox, window: &gtk4::ApplicationWindow, shared: &Shared) {
    while let Some(child) = keys.first_child() {
        keys.remove(&child);
    }
    while let Some(child) = panel.first_child() {
        panel.remove(&child);
    }
    let (state, selected) = {
        let state = shared.borrow();
        (state.state.clone(), state.state_key.clone())
    };
    let Some(object) = state.as_object() else {
        panel.append(&dim_label("No runtime state reported by the daemon."));
        return;
    };
    let mut sorted: Vec<(&String, &serde_json::Value)> = object.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    if sorted.is_empty() {
        panel.append(&dim_label("Runtime state is empty."));
        return;
    }
    let selected = sorted
        .iter()
        .find(|(key, _)| key.as_str() == selected)
        .map(|(key, _)| key.to_string())
        .unwrap_or_else(|| sorted[0].0.clone());
    shared.borrow_mut().state_key = selected.clone();

    for (key, value) in &sorted {
        let button = Button::new();
        button.add_css_class("state-key");
        if key.as_str() == selected {
            button.add_css_class("active");
        }
        button.set_hexpand(true);
        let row = hbox(6);
        row.append(&label(key));
        row.append(&horizontal_spacer());
        let type_label = Label::new(Some(&type_summary(value)));
        type_label.add_css_class("tiny");
        type_label.add_css_class("faint");
        row.append(&type_label);
        button.set_child(Some(&row));
        {
            let keys = keys.clone();
            let panel = panel.clone();
            let window = window.clone();
            let shared = shared.clone();
            let key = key.to_string();
            button.connect_clicked(move |_| {
                shared.borrow_mut().state_key = key.clone();
                render_runtime(&keys, &panel, &window, &shared);
            });
        }
        keys.append(&button);
    }

    let value = object.get(&selected).cloned().unwrap_or_default();
    let title = hbox(8);
    let title_label = mono(&format!("state.dump().{selected}"));
    title_label.add_css_class("small");
    title.append(&title_label);
    title.append(&horizontal_spacer());
    let copy = button("Copy", "small");
    {
        let window = window.clone();
        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
        copy.connect_clicked(move |_| {
            window.clipboard().set_text(&text);
        });
    }
    title.append(&copy);
    panel.append(&title);
    let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
    panel.append(&text_view(&pretty, 560));
}

fn type_summary(value: &serde_json::Value) -> String {
    if let Some(array) = value.as_array() {
        format!("array[{}]", array.len())
    } else if value.is_object() {
        "object".to_string()
    } else if value.is_string() {
        "string".to_string()
    } else if value.is_boolean() {
        "bool".to_string()
    } else if value.is_number() {
        "number".to_string()
    } else if value.is_null() {
        "null".to_string()
    } else {
        "value".to_string()
    }
}

fn render_workflows(container: &GBox, shared: &Shared) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let state = shared.borrow();
    let header = hbox(10);
    header.add_css_class("table-head");
    for (text, width) in [
        ("NAME", 200),
        ("STATE", 80),
        ("STEP", 260),
        ("STARTED", 90),
        ("UPDATED", 90),
    ] {
        let label = Label::new(Some(text));
        label.set_width_chars(width / 8);
        label.set_xalign(0.0);
        header.append(&label);
    }
    let error_header = Label::new(Some("ERROR"));
    error_header.set_hexpand(true);
    error_header.set_xalign(0.0);
    header.append(&error_header);
    container.append(&header);

    if state.workflows.is_empty() {
        container.append(&dim_label("No workflow instances reported."));
        return;
    }
    for workflow in &state.workflows {
        let row = hbox(10);
        row.add_css_class("table-row");
        let name = Label::new(Some(&workflow.name));
        name.add_css_class("table-text");
        name.add_css_class("mono");
        name.set_width_chars(24);
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_xalign(0.0);
        row.append(&name);

        let class = match workflow.state.as_str() {
            "running" => "blue",
            "done" => "green",
            "failed" | "timed_out" => "red",
            _ => "",
        };
        row.append(&super::badge(&workflow.state, class));

        let step = Label::new(Some(workflow.step.as_deref().unwrap_or("")));
        step.add_css_class("table-text");
        step.set_width_chars(30);
        step.set_ellipsize(pango::EllipsizeMode::End);
        step.set_xalign(0.0);
        row.append(&step);

        let started = Label::new(Some(&model::age_label(workflow.started_at)));
        started.add_css_class("table-text");
        started.add_css_class("mono");
        started.set_width_chars(10);
        started.set_xalign(0.0);
        row.append(&started);

        let updated = Label::new(Some(&model::age_label(workflow.updated_at)));
        updated.add_css_class("table-text");
        updated.add_css_class("mono");
        updated.set_width_chars(10);
        updated.set_xalign(0.0);
        row.append(&updated);

        let error = Label::new(Some(workflow.error.as_deref().unwrap_or("")));
        error.add_css_class("table-text");
        error.set_hexpand(true);
        error.set_ellipsize(pango::EllipsizeMode::End);
        error.set_xalign(0.0);
        row.append(&error);
        container.append(&row);
    }
}

fn render_widgets(container: &FlowBox, shared: &Shared) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let state = shared.borrow();
    if state.widgets.is_empty() {
        let empty = dim_label("No widgets registered by any module.");
        container.insert(&empty, -1);
        return;
    }
    for widget in &state.widgets {
        let card = vbox(0);
        card.add_css_class("widget-card");
        card.set_hexpand(true);
        card.set_valign(Align::Start);

        let preview = vbox(0);
        preview.add_css_class("widget-preview");
        preview.set_halign(Align::Fill);
        let center = GBox::new(Orientation::Horizontal, 0);
        center.set_halign(Align::Center);
        center.set_valign(Align::Center);
        match serde_json::from_value::<bread_shared::widget::WidgetNode>(widget.root.clone()) {
            Ok(node) => center.append(&super::widgets::render(&node)),
            Err(err) => {
                let label = Label::new(Some(&format!("malformed widget: {err}")));
                label.add_css_class("tiny");
                label.add_css_class("bad");
                center.append(&label);
            }
        }
        preview.append(&center);
        card.append(&preview);

        let meta = vbox(3);
        meta.set_margin_top(8);
        meta.set_margin_bottom(9);
        meta.set_margin_start(10);
        meta.set_margin_end(10);
        let name = Label::new(Some(&widget.id));
        name.add_css_class("mono");
        name.add_css_class("small");
        name.set_xalign(0.0);
        ellipsize(&name, 40);
        meta.append(&name);
        let sub = hbox(7);
        sub.append(&super::badge(&widget.placement, ""));
        let order = Label::new(Some(&format!("order {}", widget.order)));
        order.add_css_class("tiny");
        order.add_css_class("faint");
        sub.append(&order);
        if let Some(tooltip) = widget.tooltip.as_deref() {
            let tooltip = Label::new(Some(tooltip));
            tooltip.add_css_class("tiny");
            tooltip.add_css_class("faint");
            tooltip.set_ellipsize(pango::EllipsizeMode::End);
            tooltip.set_max_width_chars(30);
            sub.append(&tooltip);
        }
        meta.append(&sub);
        let module = Label::new(Some(&format!(
            "{}  ·  updated {}",
            widget.module,
            model::age_label(widget.updated_at)
        )));
        module.add_css_class("tiny");
        module.add_css_class("faint");
        module.set_xalign(0.0);
        meta.append(&module);
        card.append(&meta);
        container.insert(&card, -1);
    }
}
