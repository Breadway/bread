//! Aggregate interaction map: sources -> event families -> modules.
//!
//! The graph is recomputed from the client's own event buffer and the live
//! subscription registry, so it is exact for the window the app has seen, with
//! no daemon-side aggregation required.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{Align, Box as GBox, DrawingArea, DropDown, Label, ScrolledWindow};

use super::{button, hbox, horizontal_spacer, label, mono, section_label, vbox, Action, Shared};
use crate::model;

#[derive(Clone)]
struct NodeAgg {
    id: String,
    label: String,
    count: usize,
    color: (f64, f64, f64),
}

#[derive(Clone)]
struct PathAgg {
    module_id: String,
    label: String,
    count: usize,
}

#[derive(Default)]
struct FlowData {
    sources: Vec<NodeAgg>,
    families: Vec<NodeAgg>,
    modules: Vec<NodeAgg>,
    edges: Vec<(String, String, usize)>,
    paths: Vec<PathAgg>,
    total_events: usize,
}

#[derive(Clone)]
struct NodeBox {
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Default)]
struct Geometry {
    nodes: Vec<NodeBox>,
}

pub struct FlowView {
    root: GBox,
    area: DrawingArea,
    side: GBox,
    data: Rc<RefCell<FlowData>>,
    window_filter: DropDown,
    actions: std::sync::mpsc::Sender<Action>,
}

impl FlowView {
    pub fn new(shared: &Shared, actions: std::sync::mpsc::Sender<Action>) -> Self {
        let root = vbox(0);

        let header = hbox(14);
        header.add_css_class("page");
        let titles = vbox(1);
        let title = label("Interaction map");
        title.add_css_class("page-title");
        titles.append(&title);
        titles.append(&super::page_sub(
            "Which sources reach which event families, and which modules subscribe. Edge width is event volume from the current buffer. Select a node to isolate its paths.",
        ));
        header.append(&titles);
        header.append(&horizontal_spacer());
        let window_filter =
            DropDown::from_strings(&["Whole buffer", "Last 1m", "Last 5m", "Last 15m"]);
        window_filter.add_css_class("window-filter");
        window_filter.set_tooltip_text(Some("Only aggregate events from this window"));
        let selected_window = match shared.borrow().flow_window_minutes {
            1 => 1,
            5 => 2,
            15 => 3,
            _ => 0,
        };
        window_filter.set_selected(selected_window);
        header.append(&window_filter);
        let reset = button("Reset", "small");
        reset.set_valign(Align::Center);
        header.append(&reset);
        root.append(&header);

        let area = DrawingArea::new();
        area.set_content_width(620);
        area.set_content_height(560);
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_draw_func(|_, _, _, _| {});

        let side = vbox(6);
        side.set_valign(Align::Start);
        let side_scroll = ScrolledWindow::new();
        side_scroll.set_child(Some(&side));
        side_scroll.set_width_request(260);
        side_scroll.set_vexpand(true);
        side_scroll.add_css_class("flow-side");

        let body = hbox(0);
        body.set_vexpand(true);
        area.set_hexpand(true);
        body.append(&area);
        body.append(&side_scroll);
        root.append(&body);

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
                shared.borrow_mut().flow_window_minutes = minutes;
                let _ = actions.send(Action::RefreshFlow);
            });
        }

        let data = Rc::new(RefCell::new(FlowData::default()));
        let geometry = Rc::new(RefCell::new(Geometry::default()));
        let hover: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        {
            let data = data.clone();
            let geometry = geometry.clone();
            let hover = hover.clone();
            let shared = shared.clone();
            area.set_draw_func(move |_, cr, width, height| {
                draw_map(cr, width, height, &shared, &data, &geometry, &hover);
            });
        }

        {
            let shared = shared.clone();
            let side = side.clone();
            let data = data.clone();
            let geometry = geometry.clone();
            let area_for_click = area.clone();
            let actions = actions.clone();
            let gesture = gtk4::GestureClick::new();
            gesture.connect_pressed(move |_, _, x, y| {
                let hit = geometry
                    .borrow()
                    .nodes
                    .iter()
                    .find(|node| {
                        x >= node.x
                            && x <= node.x + node.width
                            && y >= node.y
                            && y <= node.y + node.height
                    })
                    .map(|node| node.id.clone());
                let Some(hit) = hit else {
                    return;
                };
                let mut state = shared.borrow_mut();
                if state.selected_flow.as_deref() == Some(hit.as_str()) {
                    state.selected_flow = None;
                } else {
                    state.selected_flow = Some(hit);
                }
                drop(state);
                render_side(&side, &area_for_click, &actions, &shared, &data);
                area_for_click.queue_draw();
            });
            area.add_controller(gesture);
        }

        {
            let hover_motion = hover.clone();
            let hover_leave = hover.clone();
            let geometry = geometry.clone();
            let area_motion = area.clone();
            let area_leave = area.clone();
            let motion = gtk4::EventControllerMotion::new();
            motion.connect_motion(move |_, x, y| {
                let hit = geometry
                    .borrow()
                    .nodes
                    .iter()
                    .find(|node| {
                        x >= node.x
                            && x <= node.x + node.width
                            && y >= node.y
                            && y <= node.y + node.height
                    })
                    .map(|node| node.id.clone());
                if *hover_motion.borrow() != hit {
                    *hover_motion.borrow_mut() = hit.clone();
                    area_motion.queue_draw();
                }
                area_motion.set_cursor_from_name(if hit.is_some() {
                    Some("pointer")
                } else {
                    Some("default")
                });
            });
            motion.connect_leave(move |_| {
                if hover_leave.borrow_mut().take().is_some() {
                    area_leave.queue_draw();
                }
            });
            area.add_controller(motion);
        }

        {
            let shared = shared.clone();
            let side_for_reset = side.clone();
            let data = data.clone();
            let area_for_reset = area.clone();
            let actions_for_reset = actions.clone();
            reset.connect_clicked(move |_| {
                shared.borrow_mut().selected_flow = None;
                render_side(
                    &side_for_reset,
                    &area_for_reset,
                    &actions_for_reset,
                    &shared,
                    &data,
                );
                area_for_reset.queue_draw();
            });
        }

        let view = Self {
            root,
            area,
            side,
            data,
            window_filter,
            actions,
        };
        view.refresh(shared);
        view
    }

    pub fn root(&self) -> &GBox {
        &self.root
    }

    pub fn refresh(&self, shared: &Shared) {
        let data = compute_data(shared);
        *self.data.borrow_mut() = data;
        let selection_exists = {
            let state = shared.borrow();
            let data = self.data.borrow();
            state
                .selected_flow
                .as_deref()
                .map(|selected| data.all_nodes().any(|node| node.id == selected))
        };
        if selection_exists == Some(false) {
            shared.borrow_mut().selected_flow = None;
        }
        let minutes = shared.borrow().flow_window_minutes;
        self.window_filter.set_selected(match minutes {
            1 => 1,
            5 => 2,
            15 => 3,
            _ => 0,
        });
        render_side(&self.side, &self.area, &self.actions, shared, &self.data);
        self.area.queue_draw();
    }
}

impl FlowData {
    fn all_nodes(&self) -> impl Iterator<Item = &NodeAgg> {
        self.sources
            .iter()
            .chain(self.families.iter())
            .chain(self.modules.iter())
    }

    fn node(&self, id: &str) -> Option<&NodeAgg> {
        self.all_nodes().find(|node| node.id == id)
    }
}

type SourceAgg = (String, usize, (f64, f64, f64));

fn compute_data(shared: &Shared) -> FlowData {
    let state = shared.borrow();
    let mut sources: HashMap<String, SourceAgg> = HashMap::new();
    let mut families: HashMap<String, usize> = HashMap::new();
    let mut modules: HashMap<String, usize> = HashMap::new();
    let mut edges: HashMap<(String, String), usize> = HashMap::new();
    let mut paths: HashMap<(String, String, String), (String, usize)> = HashMap::new();

    for event in &state.events {
        if !state.in_flow_window(event) {
            continue;
        }
        let source_key = model::source_key(&event.source);
        let source_id = format!("s:{source_key}");
        let source_label = model::source_label(&event.source);
        let color = super::hex_rgb(model::source_color(&event.source)).unwrap_or((0.6, 0.6, 0.6));
        sources
            .entry(source_id.clone())
            .and_modify(|(_, count, _)| *count += 1)
            .or_insert((source_label, 1, color));

        let family = model::event_family(&event.event);
        let family_id = format!("f:{family}");
        *families.entry(family.clone()).or_insert(0) += 1;
        *edges
            .entry((source_id.clone(), family_id.clone()))
            .or_insert(0) += 1;

        for subscription in state
            .subscriptions
            .iter()
            .filter(|entry| model::matches_pattern(&entry.pattern, &event.event))
        {
            let module = subscription.module_label().to_string();
            let module_id = format!("m:{module}");
            *modules.entry(module.clone()).or_insert(0) += 1;
            *edges
                .entry((family_id.clone(), module_id.clone()))
                .or_insert(0) += 1;
            let path = paths
                .entry((source_id.clone(), family_id.clone(), module_id.clone()))
                .or_insert_with(|| {
                    (
                        format!(
                            "{} → {} → {}",
                            model::source_label(&event.source),
                            family,
                            module
                        ),
                        0,
                    )
                });
            path.1 += 1;
        }
    }

    let mut sources: Vec<NodeAgg> = sources
        .into_iter()
        .map(|(id, (label, count, color))| NodeAgg {
            id,
            label,
            count,
            color,
        })
        .collect();
    sources.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    let mut families: Vec<NodeAgg> = families
        .into_iter()
        .map(|(label, count)| NodeAgg {
            id: format!("f:{label}"),
            label,
            count,
            color: (0.85, 0.63, 0.36),
        })
        .collect();
    families.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    let mut modules: Vec<NodeAgg> = modules
        .into_iter()
        .map(|(label, count)| NodeAgg {
            id: format!("m:{label}"),
            label,
            count,
            color: (0.86, 0.68, 0.44),
        })
        .collect();
    modules.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));

    // Keep the map readable: many one-off custom families or modules would
    // otherwise produce a column of unreadable slivers. Fold the tail into a
    // single "other" node and remap its edges so totals stay exact.
    let mut remap: HashMap<String, String> = HashMap::new();
    compact(
        &mut sources,
        8,
        "s:other",
        "other",
        (0.55, 0.55, 0.55),
        &mut remap,
    );
    compact(
        &mut families,
        9,
        "f:other",
        "other",
        (0.65, 0.58, 0.48),
        &mut remap,
    );
    compact(
        &mut modules,
        10,
        "m:other",
        "other",
        (0.65, 0.58, 0.48),
        &mut remap,
    );

    let mut merged: HashMap<(String, String), usize> = HashMap::new();
    for ((from, to), count) in edges {
        let from = remap.get(&from).cloned().unwrap_or(from);
        let to = remap.get(&to).cloned().unwrap_or(to);
        *merged.entry((from, to)).or_insert(0) += count;
    }
    let mut edges: Vec<(String, String, usize)> = merged
        .into_iter()
        .map(|((from, to), count)| (from, to, count))
        .collect();
    edges.sort_by_key(|edge| std::cmp::Reverse(edge.2));
    let mut paths: Vec<PathAgg> = paths
        .into_iter()
        .map(
            |((_source_id, _family_id, module_id), (label, count))| PathAgg {
                module_id,
                label,
                count,
            },
        )
        .collect();
    paths.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    paths.truncate(7);

    FlowData {
        total_events: state.events.len(),
        sources,
        families,
        modules,
        edges,
        paths,
    }
}

fn compact(
    nodes: &mut Vec<NodeAgg>,
    max: usize,
    other_id: &str,
    other_label: &str,
    color: (f64, f64, f64),
    remap: &mut HashMap<String, String>,
) {
    if nodes.len() <= max {
        return;
    }
    let dropped = nodes.split_off(max - 1);
    let count: usize = dropped.iter().map(|node| node.count).sum();
    for node in dropped {
        remap.insert(node.id, other_id.to_string());
    }
    nodes.push(NodeAgg {
        id: other_id.to_string(),
        label: other_label.to_string(),
        count,
        color,
    });
}

fn layout_column(
    nodes: &[NodeAgg],
    top: f64,
    height: f64,
    gap: f64,
    min_height: f64,
) -> Vec<(f64, f64)> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let total: usize = nodes.iter().map(|node| node.count).sum::<usize>().max(1);
    let available = (height - gap * (nodes.len() as f64 - 1.0)).max(40.0);
    let mut heights: Vec<f64> = nodes
        .iter()
        .map(|node| (node.count as f64 / total as f64 * available).max(min_height))
        .collect();
    let sum: f64 = heights.iter().sum();
    if sum > available {
        let excess: f64 = heights.iter().map(|h| (h - min_height).max(0.0)).sum();
        let target = (available - min_height * nodes.len() as f64).max(0.0);
        let scale = if excess > 0.0 { target / excess } else { 0.0 };
        heights = heights
            .into_iter()
            .map(|h| min_height + (h - min_height).max(0.0) * scale)
            .collect();
    }
    let mut y = top;
    heights
        .into_iter()
        .map(|h| {
            let result = (y, h);
            y += h + gap;
            result
        })
        .collect()
}

fn draw_map(
    cr: &gtk4::cairo::Context,
    width: i32,
    height: i32,
    shared: &Shared,
    data: &Rc<RefCell<FlowData>>,
    geometry: &Rc<RefCell<Geometry>>,
    hover: &Rc<RefCell<Option<String>>>,
) {
    let data = data.borrow();
    let state = shared.borrow();
    let width = width as f64;
    let height = height as f64;
    cr.set_source_rgb(0.055, 0.055, 0.055);
    cr.rectangle(0.0, 0.0, width, height);
    let _ = cr.fill();

    let top = 52.0;
    let bottom = 18.0;
    let node_width = 158.0;
    let gap = 9.0;
    let available_height = (height - top - bottom).max(120.0);
    let columns_x = [18.0, (width - node_width) / 2.0, width - node_width - 18.0];

    let source_layout = layout_column(&data.sources, top, available_height, gap, 24.0);
    let family_layout = layout_column(&data.families, top, available_height, gap, 24.0);
    let module_layout = layout_column(&data.modules, top, available_height, gap, 24.0);

    let mut nodes = Vec::new();
    let mut position: HashMap<&str, (f64, f64, f64)> = HashMap::new();
    for (index, node) in data.sources.iter().enumerate() {
        let (y, h) = source_layout[index];
        position.insert(node.id.as_str(), (columns_x[0], y, h));
        nodes.push(NodeBox {
            id: node.id.clone(),
            x: columns_x[0],
            y,
            width: node_width,
            height: h,
        });
    }
    for (index, node) in data.families.iter().enumerate() {
        let (y, h) = family_layout[index];
        position.insert(node.id.as_str(), (columns_x[1], y, h));
        nodes.push(NodeBox {
            id: node.id.clone(),
            x: columns_x[1],
            y,
            width: node_width,
            height: h,
        });
    }
    for (index, node) in data.modules.iter().enumerate() {
        let (y, h) = module_layout[index];
        position.insert(node.id.as_str(), (columns_x[2], y, h));
        nodes.push(NodeBox {
            id: node.id.clone(),
            x: columns_x[2],
            y,
            width: node_width,
            height: h,
        });
    }
    *geometry.borrow_mut() = Geometry {
        nodes: nodes.clone(),
    };

    let selected = state.selected_flow.as_deref();
    let hovered = hover.borrow().clone();
    let focus = selected.or(hovered.as_deref());
    let connected: HashMap<&str, bool> = data
        .edges
        .iter()
        .filter(|(from, to, _)| focus.is_some_and(|focus| focus == from || focus == to))
        .flat_map(|(from, to, _)| [(from.as_str(), true), (to.as_str(), true)])
        .collect();

    for (from, to, count) in &data.edges {
        let (Some((fx, fy, fh)), Some((tx, ty, th))) =
            (position.get(from.as_str()), position.get(to.as_str()))
        else {
            continue;
        };
        let x1 = fx + node_width;
        let y1 = fy + fh / 2.0;
        let x2 = *tx;
        let y2 = ty + th / 2.0;
        let control = ((x2 - x1) * 0.45).max(45.0);
        let active = focus.is_some_and(|focus| focus == from || focus == to);
        let dim = selected.is_some() && !active;
        let (r, g, b) = if from.starts_with("s:") {
            data.node(from)
                .map(|node| node.color)
                .unwrap_or((0.5, 0.5, 0.5))
        } else {
            (0.62, 0.58, 0.52)
        };
        cr.set_source_rgba(
            r,
            g,
            b,
            if dim {
                0.035
            } else if active {
                0.85
            } else {
                0.20
            },
        );
        cr.set_line_width(if active {
            2.4
        } else {
            0.8 + *count as f64 / 18.0
        });
        cr.move_to(x1, y1);
        cr.curve_to(x1 + control, y1, x2 - control, y2, x2, y2);
        let _ = cr.stroke();
    }

    let column_titles = ["Sources", "Event families", "Modules"];
    cr.select_font_face(
        "sans",
        gtk4::cairo::FontSlant::Normal,
        gtk4::cairo::FontWeight::Bold,
    );
    cr.set_font_size(11.0);
    for (index, title) in column_titles.iter().enumerate() {
        cr.set_source_rgba(0.80, 0.80, 0.80, 0.80);
        cr.move_to(columns_x[index], 26.0);
        let _ = cr.show_text(title);
    }
    cr.select_font_face(
        "monospace",
        gtk4::cairo::FontSlant::Normal,
        gtk4::cairo::FontWeight::Normal,
    );
    cr.set_font_size(8.0);
    cr.set_source_rgba(0.55, 0.55, 0.55, 0.70);
    let subtitles = ["event source", "bread.<domain>.*", "subscribed module"];
    for (index, title) in subtitles.iter().enumerate() {
        cr.move_to(columns_x[index], 39.0);
        let _ = cr.show_text(title);
    }

    let draw_node = |node: &NodeAgg, geometry: &NodeBox| {
        let dim = selected.is_some()
            && !connected.contains_key(node.id.as_str())
            && selected != Some(node.id.as_str());
        let (r, g, b) = node.color;
        super::rounded_rect(
            cr,
            geometry.x,
            geometry.y,
            geometry.width,
            geometry.height,
            6.0,
        );
        let shade = if dim { 0.035 } else { 0.085 };
        cr.set_source_rgb(shade + 0.02, shade + 0.02, shade + 0.02);
        let _ = cr.fill_preserve();
        if selected == Some(node.id.as_str()) {
            cr.set_source_rgba(0.85, 0.63, 0.36, 0.95);
            cr.set_line_width(1.5);
        } else if hovered.as_deref() == Some(node.id.as_str()) {
            cr.set_source_rgba(0.85, 0.63, 0.36, 0.65);
            cr.set_line_width(1.2);
        } else {
            cr.set_source_rgba(0.35, 0.35, 0.35, if dim { 0.25 } else { 0.55 });
            cr.set_line_width(0.8);
        }
        let _ = cr.stroke();
        cr.set_source_rgba(r, g, b, if dim { 0.25 } else { 0.85 });
        super::rounded_rect(cr, geometry.x, geometry.y, 3.0, geometry.height, 1.5);
        let _ = cr.fill();
        let alpha = if dim { 0.30 } else { 0.88 };
        cr.set_source_rgba(0.87, 0.87, 0.87, alpha);
        cr.select_font_face(
            "monospace",
            gtk4::cairo::FontSlant::Normal,
            gtk4::cairo::FontWeight::Normal,
        );
        cr.set_font_size(10.0);
        let label = super::fit_text(cr, &node.label, geometry.width - 34.0);
        let baseline = geometry.y + geometry.height / 2.0 + 3.5;
        cr.move_to(geometry.x + 12.0, baseline);
        let _ = cr.show_text(&label);
        cr.set_source_rgba(0.55, 0.55, 0.55, alpha);
        cr.set_font_size(8.5);
        let count = node.count.to_string();
        let count_width = cr
            .text_extents(&count)
            .map(|extents| extents.width())
            .unwrap_or(0.0);
        cr.move_to(geometry.x + geometry.width - 9.0 - count_width, baseline);
        let _ = cr.show_text(&count);
    };

    for node in &data.sources {
        if let Some((x, y, h)) = position.get(node.id.as_str()) {
            draw_node(
                node,
                &NodeBox {
                    id: node.id.clone(),
                    x: *x,
                    y: *y,
                    width: node_width,
                    height: *h,
                },
            );
        }
    }
    for node in &data.families {
        if let Some((x, y, h)) = position.get(node.id.as_str()) {
            draw_node(
                node,
                &NodeBox {
                    id: node.id.clone(),
                    x: *x,
                    y: *y,
                    width: node_width,
                    height: *h,
                },
            );
        }
    }
    for node in &data.modules {
        if let Some((x, y, h)) = position.get(node.id.as_str()) {
            draw_node(
                node,
                &NodeBox {
                    id: node.id.clone(),
                    x: *x,
                    y: *y,
                    width: node_width,
                    height: *h,
                },
            );
        }
    }

    // Hover readout, bottom left, so the map answers "what is this?" without
    // leaving the canvas.
    if let Some(hovered) = hovered.as_deref().and_then(|id| data.node(id)) {
        let text = format!(
            "{}  ·  {} events  ·  click to isolate",
            hovered.label, hovered.count
        );
        cr.select_font_face(
            "monospace",
            gtk4::cairo::FontSlant::Normal,
            gtk4::cairo::FontWeight::Normal,
        );
        cr.set_font_size(9.0);
        let text_width = cr
            .text_extents(&text)
            .map(|extents| extents.width())
            .unwrap_or(0.0);
        super::rounded_rect(cr, 18.0, height - 34.0, text_width + 20.0, 20.0, 5.0);
        cr.set_source_rgba(0.10, 0.10, 0.10, 0.92);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(0.85, 0.63, 0.36, 0.45);
        cr.set_line_width(0.8);
        let _ = cr.stroke();
        cr.set_source_rgba(0.88, 0.88, 0.88, 0.9);
        cr.move_to(28.0, height - 20.0);
        let _ = cr.show_text(&text);
    }

    if data.total_events == 0 {
        cr.set_source_rgba(0.65, 0.65, 0.65, 0.75);
        cr.select_font_face(
            "sans",
            gtk4::cairo::FontSlant::Normal,
            gtk4::cairo::FontWeight::Normal,
        );
        cr.set_font_size(13.0);
        let message = "No events in the buffer yet. Wait for the stream or emit one.";
        let text_width = cr
            .text_extents(message)
            .map(|extents| extents.width())
            .unwrap_or(0.0);
        cr.move_to((width - text_width) / 2.0, height / 2.0);
        let _ = cr.show_text(message);
    }
}

fn render_side(
    side: &GBox,
    area: &DrawingArea,
    actions: &std::sync::mpsc::Sender<Action>,
    shared: &Shared,
    data_rc: &Rc<RefCell<FlowData>>,
) {
    while let Some(child) = side.first_child() {
        side.remove(&child);
    }
    let data_rc = data_rc.clone();
    let (selected, subscriptions) = {
        let state = shared.borrow();
        (state.selected_flow.clone(), state.subscriptions.len())
    };
    let data = data_rc.borrow();

    if let Some(selected) = selected {
        let Some(node) = data.node(&selected) else {
            return;
        };
        let title = label(&node.label);
        title.add_css_class("bold");
        title.set_wrap(true);
        side.append(&title);
        let kind = if selected.starts_with("s:") {
            "source"
        } else if selected.starts_with("f:") {
            "event family"
        } else {
            "module"
        };
        let share = if data.total_events > 0 {
            (node.count as f64 / data.total_events as f64) * 100.0
        } else {
            0.0
        };
        side.append(&super::dim_label(&format!(
            "{kind} · {} events · {share:.0}% of the window",
            node.count
        )));

        let filter_pattern = if selected.starts_with("f:") {
            Some(format!("bread.{}.**", selected.trim_start_matches("f:")))
        } else if selected.starts_with("m:") {
            let module = selected.trim_start_matches("m:");
            let state = shared.borrow();
            state
                .subscriptions
                .iter()
                .find(|entry| entry.module_label() == module)
                .map(|entry| entry.pattern.clone())
        } else {
            None
        };
        if let Some(pattern) = filter_pattern {
            let filter = button("Filter stream", "small primary");
            let actions = actions.clone();
            filter.connect_clicked(move |_| {
                let _ = actions.send(Action::FilterStream(pattern.clone()));
            });
            side.append(&filter);
        }
        if let Some(module) = selected.strip_prefix("m:") {
            let open = button("Open module", "small");
            let actions = actions.clone();
            let module = module.to_string();
            open.connect_clicked(move |_| {
                let _ = actions.send(Action::OpenModule(module.clone()));
            });
            side.append(&open);
        }

        side.append(&section_label("Inbound"));
        let inbound: Vec<(String, String, usize)> = data
            .edges
            .iter()
            .filter(|(_, to, _)| to == &selected)
            .map(|(from, _, count)| {
                let label = data
                    .node(from)
                    .map(|node| node.label.clone())
                    .unwrap_or_else(|| from.clone());
                (from.clone(), label, *count)
            })
            .collect();
        if inbound.is_empty() {
            side.append(&super::dim_label("No inbound edges."));
        }
        for (id, label, count) in inbound {
            side.append(&edge_button(
                "←", &label, count, side, area, actions, shared, &data_rc, &id,
            ));
        }
        side.append(&section_label("Outbound"));
        let outbound: Vec<(String, String, usize)> = data
            .edges
            .iter()
            .filter(|(from, _, _)| from == &selected)
            .map(|(_, to, count)| {
                let label = data
                    .node(to)
                    .map(|node| node.label.clone())
                    .unwrap_or_else(|| to.clone());
                (to.clone(), label, *count)
            })
            .collect();
        if outbound.is_empty() {
            side.append(&super::dim_label("No outbound edges. This is a leaf."));
        }
        for (id, label, count) in outbound {
            side.append(&edge_button(
                "→", &label, count, side, area, actions, shared, &data_rc, &id,
            ));
        }
        side.append(&wrapped_note(
            "Click the selected node again, or Reset, to clear the isolation.",
        ));
        return;
    }

    side.append(&label("Reading the map"));
    side.append(&wrapped_note(
        "Each event walks source, then event family, then every matching subscription. Width is volume in the selected time window.",
    ));
    stat_row(side, "sources", data.sources.len());
    stat_row(side, "event families", data.families.len());
    stat_row(side, "subscribed modules", data.modules.len());
    stat_row(side, "observed events", data.total_events);
    stat_row(side, "live subscriptions", subscriptions);
    side.append(&section_label("Top paths"));
    let max = data
        .paths
        .first()
        .map(|path| path.count)
        .unwrap_or(1)
        .max(1);
    if data.paths.is_empty() {
        side.append(&wrapped_note(
            "No path has been observed yet. Paths appear when a subscription matches a buffered event.",
        ));
    }
    for path in &data.paths {
        let row = gtk4::Button::new();
        row.add_css_class("path-row");
        row.add_css_class("path-button");
        row.set_tooltip_text(Some("Isolate this path in the map"));
        let content = vbox(2);
        let top = hbox(6);
        let name = Label::new(Some(&path.label));
        name.add_css_class("path-name");
        name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        name.set_max_width_chars(24);
        name.set_hexpand(true);
        name.set_xalign(0.0);
        top.append(&name);
        let share = if data.total_events > 0 {
            (path.count as f64 / data.total_events as f64) * 100.0
        } else {
            0.0
        };
        let count = Label::new(Some(&format!("{} · {share:.0}%", path.count)));
        count.add_css_class("path-count");
        top.append(&count);
        content.append(&top);
        let fraction = path.count as f64 / max as f64;
        let bar = DrawingArea::new();
        bar.set_content_height(4);
        bar.set_hexpand(true);
        bar.set_draw_func(move |_, cr, width, height| {
            let width = width as f64;
            let height = height as f64;
            cr.set_source_rgba(0.75, 0.75, 0.75, 0.10);
            super::rounded_rect(cr, 0.0, 0.0, width, height, 2.0);
            let _ = cr.fill();
            cr.set_source_rgba(0.85, 0.63, 0.36, 0.78);
            super::rounded_rect(cr, 0.0, 0.0, width * fraction, height, 2.0);
            let _ = cr.fill();
        });
        content.append(&bar);
        row.set_child(Some(&content));
        {
            let side = side.clone();
            let area = area.clone();
            let actions = actions.clone();
            let shared = shared.clone();
            let data = data_rc.clone();
            let id = path.module_id.clone();
            row.connect_clicked(move |_| {
                select_node(&side, &area, &actions, &shared, &data, id.clone());
            });
        }
        side.append(&row);
    }
    side.append(&wrapped_note(
        "Click a path to isolate its module, then use Filter stream. A module can appear on several paths.",
    ));
}

#[allow(clippy::too_many_arguments)]
fn edge_button(
    arrow: &str,
    label: &str,
    count: usize,
    side: &GBox,
    area: &DrawingArea,
    actions: &std::sync::mpsc::Sender<Action>,
    shared: &Shared,
    data: &Rc<RefCell<FlowData>>,
    id: &str,
) -> gtk4::Button {
    let row = gtk4::Button::new();
    row.add_css_class("path-row");
    row.add_css_class("path-button");
    let content = hbox(8);
    let arrow = Label::new(Some(arrow));
    arrow.add_css_class("accent");
    content.append(&arrow);
    let name = Label::new(Some(label));
    name.add_css_class("path-name");
    name.set_hexpand(true);
    name.set_xalign(0.0);
    name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    content.append(&name);
    let count = Label::new(Some(&count.to_string()));
    count.add_css_class("path-count");
    content.append(&count);
    row.set_child(Some(&content));
    let side = side.clone();
    let area = area.clone();
    let actions = actions.clone();
    let shared = shared.clone();
    let data = data.clone();
    let id = id.to_string();
    row.connect_clicked(move |_| {
        select_node(&side, &area, &actions, &shared, &data, id.clone());
    });
    row
}

fn select_node(
    side: &GBox,
    area: &DrawingArea,
    actions: &std::sync::mpsc::Sender<Action>,
    shared: &Shared,
    data: &Rc<RefCell<FlowData>>,
    id: String,
) {
    shared.borrow_mut().selected_flow = Some(id);
    render_side(side, area, actions, shared, data);
    area.queue_draw();
}
fn wrapped_note(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("note");
    label.set_wrap(true);
    label.set_max_width_chars(32);
    label.set_xalign(0.0);
    label
}

fn stat_row(side: &GBox, key: &str, value: usize) {
    let row = hbox(8);
    row.set_margin_top(2);
    row.append(&label(key));
    row.append(&horizontal_spacer());
    row.append(&mono(&value.to_string()));
    side.append(&row);
}
