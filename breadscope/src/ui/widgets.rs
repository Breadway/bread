//! Renderer for the typed `bread.widget` node trees returned by
//! `widgets.list`. Mirrors breadbar's interpretation of the shared style
//! enums so the preview in breadscope matches what the bar would draw.

use bread_shared::widget::{
    Align as WidgetAlign, Background, FontWeight, Orientation as WidgetOrientation, Padding,
    Radius, SemanticColor, TextSize, WidgetNode, WidgetStyle,
};
use gtk4::prelude::*;
use gtk4::{Align, Box as GBox, Image, Label, Orientation, ProgressBar};

pub fn render(node: &WidgetNode) -> gtk4::Widget {
    match node {
        WidgetNode::Box {
            orientation,
            spacing,
            style,
            children,
            ..
        } => {
            let gtk_orientation = match orientation {
                WidgetOrientation::Horizontal => Orientation::Horizontal,
                WidgetOrientation::Vertical => Orientation::Vertical,
            };
            let container = GBox::new(gtk_orientation, spacing.unwrap_or(6));
            container.set_valign(Align::Center);
            apply_box_style(&container, style.as_ref());
            for child in children {
                container.append(&render(child));
            }
            container.upcast()
        }
        WidgetNode::Label { text, style, .. } => {
            let label = Label::new(Some(text));
            label.set_xalign(match style.as_ref().and_then(|style| style.align) {
                Some(WidgetAlign::Center) => 0.5,
                Some(WidgetAlign::End) => 1.0,
                _ => 0.0,
            });
            apply_label_style(&label, style.as_ref());
            label.upcast()
        }
        WidgetNode::Icon {
            name,
            path,
            size,
            style,
            ..
        } => {
            let image = if let Some(path) = path {
                Image::from_file(path)
            } else {
                let icon = name.as_deref().unwrap_or("image-x-generic-symbolic");
                Image::from_icon_name(map_icon(icon))
            };
            image.set_pixel_size(size.unwrap_or(16));
            image.set_valign(Align::Center);
            apply_common_style(&image, style.as_ref());
            image.upcast()
        }
        WidgetNode::Progress { value, style, .. } => {
            let bar = ProgressBar::new();
            bar.set_fraction(value.clamp(0.0, 1.0));
            bar.set_show_text(false);
            bar.set_size_request(70, 6);
            bar.set_valign(Align::Center);
            bar.add_css_class("wprogress");
            apply_common_style(&bar, style.as_ref());
            bar.upcast()
        }
    }
}

fn apply_box_style(container: &GBox, style: Option<&WidgetStyle>) {
    if let Some(style) = style {
        apply_background(container, style.background);
        apply_padding(container, style.padding);
        apply_radius(container, style.radius);
    }
}

fn apply_label_style(label: &Label, style: Option<&WidgetStyle>) {
    label.add_css_class("wlabel");
    let Some(style) = style else {
        return;
    };
    if let Some(color) = style.color {
        label.add_css_class(match color {
            SemanticColor::Fg => "fg",
            SemanticColor::Dim => "dim",
            SemanticColor::Accent => "accent",
            SemanticColor::Red => "red",
            SemanticColor::Green => "green",
            SemanticColor::Yellow => "yellow",
            SemanticColor::Blue => "blue",
            SemanticColor::Pink => "pink",
            SemanticColor::Teal => "teal",
        });
    }
    if style.weight == Some(FontWeight::Bold) {
        label.add_css_class("bold");
    }
    if let Some(size) = style.size {
        label.add_css_class(match size {
            TextSize::Xs => "xs",
            TextSize::Sm => "sm",
            TextSize::Md => "md",
            TextSize::Lg => "lg",
            TextSize::Xl => "xl",
        });
    }
    apply_background(label, style.background);
    apply_padding(label, style.padding);
    apply_radius(label, style.radius);
}

fn apply_common_style(widget: &impl IsA<gtk4::Widget>, style: Option<&WidgetStyle>) {
    let Some(style) = style else {
        return;
    };
    apply_background(widget, style.background);
    apply_padding(widget, style.padding);
    apply_radius(widget, style.radius);
}
fn apply_background(widget: &impl IsA<gtk4::Widget>, background: Option<Background>) {
    match background {
        Some(Background::Surface) => widget.add_css_class("surface-bg"),
        Some(Background::Card) => widget.add_css_class("card-bg"),
        _ => {}
    }
}

fn apply_radius(widget: &impl IsA<gtk4::Widget>, radius: Option<Radius>) {
    match radius {
        Some(Radius::Sm) => widget.add_css_class("radius-sm"),
        Some(Radius::Md) => widget.add_css_class("radius-md"),
        Some(Radius::Full) => widget.add_css_class("radius-full"),
        _ => {}
    }
}

fn apply_padding(widget: &impl IsA<gtk4::Widget>, padding: Option<Padding>) {
    match padding {
        Some(Padding::Xs) => widget.add_css_class("pad-xs"),
        Some(Padding::Sm) => widget.add_css_class("pad-sm"),
        Some(Padding::Md) => widget.add_css_class("pad-md"),
        _ => {}
    }
}

fn map_icon(name: &str) -> &str {
    match name {
        "cloud" => "weather-overcast-symbolic",
        "sun" | "weather" => "weather-clear-symbolic",
        "cpu" => "utilities-system-monitor-symbolic",
        "battery" => "battery-good-symbolic",
        "wifi" | "network" => "network-wireless-symbolic",
        "volume" | "audio" => "audio-volume-high-symbolic",
        "clock" => "preferences-system-time-symbolic",
        other => other,
    }
}
