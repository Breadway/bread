//! Wire types for Lua-declared bar widgets.
//!
//! A module running in breadd's Lua runtime can register a small declarative
//! node tree (see [`WidgetNode`]) that gets rendered generically by a
//! sibling `bread*` app (breadbar) in its bar or hamburger-popover free
//! space. These types are the shared contract between `breadd` (which
//! stores/validates/emits them from `bread.widget.*`) and any renderer.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum nesting depth of a widget's node tree (the root counts as depth 1).
pub const MAX_NODE_DEPTH: usize = 4;
/// Maximum total number of nodes (root + all descendants) in a widget's tree.
pub const MAX_NODE_COUNT: usize = 50;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

/// One node in a widget's declarative render tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum WidgetNode {
    Box {
        #[serde(default = "default_orientation")]
        orientation: Orientation,
        #[serde(default)]
        spacing: Option<i32>,
        #[serde(default)]
        class: Option<String>,
        #[serde(default)]
        style: Option<WidgetStyle>,
        #[serde(default)]
        on_click: Option<Value>,
        #[serde(default)]
        children: Vec<WidgetNode>,
    },
    Label {
        text: String,
        #[serde(default)]
        class: Option<String>,
        #[serde(default)]
        style: Option<WidgetStyle>,
        #[serde(default)]
        on_click: Option<Value>,
    },
    Icon {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        size: Option<i32>,
        #[serde(default)]
        class: Option<String>,
        #[serde(default)]
        style: Option<WidgetStyle>,
        #[serde(default)]
        on_click: Option<Value>,
    },
    Progress {
        value: f64,
        #[serde(default)]
        class: Option<String>,
        #[serde(default)]
        style: Option<WidgetStyle>,
        #[serde(default)]
        on_click: Option<Value>,
    },
}

fn default_orientation() -> Orientation {
    Orientation::Horizontal
}

/// A bounded, typed vocabulary for a node's appearance — the alternative to
/// letting Lua hand the renderer a raw CSS/style string. Every field is a
/// small closed enum, so an invalid value is simply a deserialization error,
/// the same as any other malformed field; there is no free-text surface here
/// for a module to smuggle style-string injection through.
///
/// Fields left `None` mean "renderer default" (see `render.rs`'s `build_node`
/// for what that default looks like), not "no style" — a node with no
/// `style` at all is fully equivalent to one whose every field is `None`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct WidgetStyle {
    pub color: Option<SemanticColor>,
    pub weight: Option<FontWeight>,
    pub size: Option<TextSize>,
    pub align: Option<Align>,
    pub background: Option<Background>,
    pub radius: Option<Radius>,
    pub padding: Option<Padding>,
}

/// Foreground/text colors, one per name `bread-theme`'s shared stylesheet
/// defines via `@define-color` (see that crate's `color_pairs()`). Deliberately
/// excludes `bg`/`surface`/`overlay`, which only make sense as backgrounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticColor {
    /// Foreground / body text color.
    Fg,
    /// Muted foreground — the existing `.dim` look, formalized.
    Dim,
    Accent,
    Red,
    Green,
    Yellow,
    Blue,
    Pink,
    Teal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FontWeight {
    Normal,
    Bold,
}

/// Text size scale (10/12/14/16/20px) — `sm`/`md` match `bread-theme::tokens`'
/// existing `FONT_SIZE_SECONDARY`/`FONT_SIZE_BASE` rather than inventing a
/// second set of magic numbers; `xs`/`lg`/`xl` fill out the rest of the scale
/// (the spacing scale's 4/8/12/16/20px is for padding/radius, not text —
/// applying it directly to `font-size` renders illegibly at the small end).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TextSize {
    Xs,
    Sm,
    Md,
    Lg,
    Xl,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Background {
    None,
    Surface,
    Card,
}

/// Reuses `bread-theme::tokens`' radius scale: `sm` = tertiary (4px, small
/// interactive elements), `md` = primary (8px), `full` = pill (999px).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Radius {
    None,
    Sm,
    Md,
    Full,
}

/// Reuses `bread-theme::tokens`' spacing scale (4/8/12px).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Padding {
    None,
    Xs,
    Sm,
    Md,
}

/// Where a widget renders in breadbar. Each variant names a fixed slot in
/// breadbar's existing `CenterBox` layout (workspaces | clock | stats), plus
/// the hamburger control-panel popover's tray section.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WidgetPlacement {
    /// Tucked inside the hamburger control-panel popover, alongside the
    /// existing SNI tray icons.
    Tray,
    /// In the center section, immediately left of the clock label.
    LeftOfClock,
    /// In the center section, immediately right of the clock label.
    RightOfClock,
    /// In the start section, immediately right of the workspace buttons.
    RightOfWorkspaces,
    /// In the end section, immediately left of the CPU/RAM/power/battery group.
    LeftOfStats,
}

/// A full widget declaration, as stored in `RuntimeState.widgets` and
/// returned by the `widgets.list` IPC method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetSpec {
    /// Fully-qualified id: `"<module>.<local_id>"`. Unique across all widgets.
    pub id: String,
    /// Name of the module that registered this widget.
    pub module: String,
    pub placement: WidgetPlacement,
    /// Sort priority within a placement; lower sorts first.
    #[serde(default)]
    pub order: i32,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub tooltip: Option<String>,
    pub root: WidgetNode,
    /// Unix epoch milliseconds of the last register/update.
    pub updated_at: u64,
}

fn default_visible() -> bool {
    true
}

/// Why a [`WidgetNode`] tree failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetValidationError {
    TooDeep { max: usize },
    TooManyNodes { max: usize },
    InvalidClass { class: String },
}

impl std::fmt::Display for WidgetValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooDeep { max } => write!(f, "widget node tree exceeds max depth of {max}"),
            Self::TooManyNodes { max } => {
                write!(f, "widget node tree exceeds max node count of {max}")
            }
            Self::InvalidClass { class } => write!(
                f,
                "invalid css class '{class}': must match ^[a-zA-Z][a-zA-Z0-9_-]{{0,63}}$"
            ),
        }
    }
}

impl std::error::Error for WidgetValidationError {}

/// A CSS class is restricted to a conservative identifier shape so widget
/// styling can only opt into classes predefined in breadbar's stylesheet —
/// there is no raw style/CSS injection surface from Lua.
fn is_valid_class(class: &str) -> bool {
    let mut chars = class.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    class.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl WidgetNode {
    /// The node's CSS class hook, if any — a renderer should apply this via
    /// its GTK equivalent of `add_css_class` rather than injecting raw style.
    pub fn class(&self) -> Option<&str> {
        match self {
            Self::Box { class, .. }
            | Self::Label { class, .. }
            | Self::Icon { class, .. }
            | Self::Progress { class, .. } => class.as_deref(),
        }
    }

    /// The node's typed style vocabulary, if any — a renderer maps each
    /// `Some` field to a predefined CSS class (see `render.rs`'s
    /// `apply_style`), never to raw injected CSS.
    pub fn style(&self) -> Option<&WidgetStyle> {
        match self {
            Self::Box { style, .. }
            | Self::Label { style, .. }
            | Self::Icon { style, .. }
            | Self::Progress { style, .. } => style.as_ref(),
        }
    }

    /// The node's opaque click payload, if any — a renderer attaches a click
    /// handler that reports this value back verbatim (see the Lua API's
    /// "Click events" contract in `Documentation.md`), it never interprets it.
    pub fn on_click(&self) -> Option<&Value> {
        match self {
            Self::Box { on_click, .. }
            | Self::Label { on_click, .. }
            | Self::Icon { on_click, .. }
            | Self::Progress { on_click, .. } => on_click.as_ref(),
        }
    }

    fn children(&self) -> &[WidgetNode] {
        match self {
            Self::Box { children, .. } => children,
            _ => &[],
        }
    }

    /// Validate depth, total node count, and every `class` field. Call this
    /// on any tree received from Lua before storing or broadcasting it.
    pub fn validate(&self) -> Result<(), WidgetValidationError> {
        let mut total = 0usize;
        self.validate_inner(1, &mut total)
    }

    fn validate_inner(
        &self,
        depth: usize,
        total: &mut usize,
    ) -> Result<(), WidgetValidationError> {
        if depth > MAX_NODE_DEPTH {
            return Err(WidgetValidationError::TooDeep { max: MAX_NODE_DEPTH });
        }
        *total += 1;
        if *total > MAX_NODE_COUNT {
            return Err(WidgetValidationError::TooManyNodes { max: MAX_NODE_COUNT });
        }
        if let Some(class) = self.class() {
            if !is_valid_class(class) {
                return Err(WidgetValidationError::InvalidClass {
                    class: class.to_string(),
                });
            }
        }
        for child in self.children() {
            child.validate_inner(depth + 1, total)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn label(class: Option<&str>) -> WidgetNode {
        WidgetNode::Label {
            text: "x".to_string(),
            class: class.map(str::to_string),
            style: None,
            on_click: None,
        }
    }

    fn box_of(children: Vec<WidgetNode>) -> WidgetNode {
        WidgetNode::Box {
            orientation: Orientation::Horizontal,
            spacing: None,
            class: None,
            style: None,
            on_click: None,
            children,
        }
    }

    #[test]
    fn simple_label_is_valid() {
        assert!(label(Some("dim")).validate().is_ok());
    }

    #[test]
    fn rejects_invalid_class_characters() {
        assert_eq!(
            label(Some("dim; color: red")).validate(),
            Err(WidgetValidationError::InvalidClass {
                class: "dim; color: red".to_string()
            })
        );
    }

    #[test]
    fn rejects_class_starting_with_digit() {
        assert!(label(Some("1dim")).validate().is_err());
    }

    #[test]
    fn rejects_empty_class() {
        assert!(label(Some("")).validate().is_err());
    }

    #[test]
    fn accepts_class_with_underscore_and_hyphen() {
        assert!(label(Some("my_class-2")).validate().is_ok());
    }

    #[test]
    fn rejects_tree_deeper_than_max() {
        // depth 1 (root box) -> 2 -> 3 -> 4 -> 5 (label), exceeds MAX_NODE_DEPTH=4
        let tree = box_of(vec![box_of(vec![box_of(vec![box_of(vec![label(None)])])])]);
        assert_eq!(
            tree.validate(),
            Err(WidgetValidationError::TooDeep { max: MAX_NODE_DEPTH })
        );
    }

    #[test]
    fn accepts_tree_at_max_depth() {
        // depth 1 -> 2 -> 3 -> 4 (label), exactly MAX_NODE_DEPTH
        let tree = box_of(vec![box_of(vec![box_of(vec![label(None)])])]);
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn rejects_too_many_nodes() {
        let children: Vec<WidgetNode> = (0..MAX_NODE_COUNT).map(|_| label(None)).collect();
        let tree = box_of(children);
        assert_eq!(
            tree.validate(),
            Err(WidgetValidationError::TooManyNodes { max: MAX_NODE_COUNT })
        );
    }

    #[test]
    fn accepts_node_count_at_max() {
        let children: Vec<WidgetNode> = (0..MAX_NODE_COUNT - 1).map(|_| label(None)).collect();
        let tree = box_of(children);
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn widget_spec_round_trips_through_json() {
        let spec = WidgetSpec {
            id: "weather.temp".to_string(),
            module: "weather".to_string(),
            placement: WidgetPlacement::LeftOfStats,
            order: 10,
            visible: true,
            tooltip: Some("Sydney".to_string()),
            root: box_of(vec![
                WidgetNode::Icon {
                    name: Some("cloud".to_string()),
                    path: None,
                    size: Some(16),
                    class: None,
                    style: None,
                    on_click: None,
                },
                label(Some("dim")),
                WidgetNode::Progress {
                    value: 0.5,
                    class: None,
                    style: Some(WidgetStyle {
                        color: Some(SemanticColor::Accent),
                        ..Default::default()
                    }),
                    on_click: Some(json!({ "action": "refresh" })),
                },
            ]),
            updated_at: 1_700_000_000_000,
        };
        let raw = serde_json::to_string(&spec).unwrap();
        let decoded: WidgetSpec = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded.id, spec.id);
        assert_eq!(decoded.placement, spec.placement);
    }

    #[test]
    fn placement_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&WidgetPlacement::Tray).unwrap(),
            "\"tray\""
        );
        assert_eq!(
            serde_json::to_string(&WidgetPlacement::LeftOfClock).unwrap(),
            "\"left_of_clock\""
        );
        assert_eq!(
            serde_json::to_string(&WidgetPlacement::RightOfClock).unwrap(),
            "\"right_of_clock\""
        );
        assert_eq!(
            serde_json::to_string(&WidgetPlacement::RightOfWorkspaces).unwrap(),
            "\"right_of_workspaces\""
        );
        assert_eq!(
            serde_json::to_string(&WidgetPlacement::LeftOfStats).unwrap(),
            "\"left_of_stats\""
        );
    }

    #[test]
    fn node_serializes_with_type_tag() {
        let value = serde_json::to_value(label(Some("dim"))).unwrap();
        assert_eq!(value["type"], "label");
        assert_eq!(value["text"], "x");
        assert_eq!(value["class"], "dim");
    }

    #[test]
    fn node_without_style_omits_it_when_serialized_and_back() {
        let node = label(None);
        assert!(node.style().is_none());
        let raw = serde_json::to_string(&node).unwrap();
        let decoded: WidgetNode = serde_json::from_str(&raw).unwrap();
        assert!(decoded.style().is_none());
    }

    #[test]
    fn style_field_is_optional_when_absent_from_json() {
        let raw = json!({ "type": "label", "text": "x" });
        let node: WidgetNode = serde_json::from_value(raw).unwrap();
        assert!(node.style().is_none());
    }

    #[test]
    fn style_round_trips_through_json() {
        let node = WidgetNode::Label {
            text: "x".to_string(),
            class: None,
            style: Some(WidgetStyle {
                color: Some(SemanticColor::Red),
                weight: Some(FontWeight::Bold),
                size: Some(TextSize::Lg),
                align: Some(Align::Center),
                background: Some(Background::Card),
                radius: Some(Radius::Sm),
                padding: Some(Padding::Xs),
            }),
            on_click: None,
        };
        let raw = serde_json::to_string(&node).unwrap();
        let decoded: WidgetNode = serde_json::from_str(&raw).unwrap();
        let style = decoded.style().expect("style should round-trip");
        assert_eq!(style.color, Some(SemanticColor::Red));
        assert_eq!(style.weight, Some(FontWeight::Bold));
        assert_eq!(style.size, Some(TextSize::Lg));
        assert_eq!(style.align, Some(Align::Center));
        assert_eq!(style.background, Some(Background::Card));
        assert_eq!(style.radius, Some(Radius::Sm));
        assert_eq!(style.padding, Some(Padding::Xs));
    }

    #[test]
    fn style_enums_serialize_as_snake_case() {
        assert_eq!(serde_json::to_string(&SemanticColor::Dim).unwrap(), "\"dim\"");
        assert_eq!(serde_json::to_string(&Background::None).unwrap(), "\"none\"");
        assert_eq!(serde_json::to_string(&Radius::Full).unwrap(), "\"full\"");
        assert_eq!(serde_json::to_string(&Padding::Xs).unwrap(), "\"xs\"");
        assert_eq!(serde_json::to_string(&Align::End).unwrap(), "\"end\"");
        assert_eq!(serde_json::to_string(&FontWeight::Bold).unwrap(), "\"bold\"");
    }

    #[test]
    fn invalid_style_color_fails_to_deserialize() {
        let raw = json!({ "type": "label", "text": "x", "style": { "color": "bg" } });
        assert!(serde_json::from_value::<WidgetNode>(raw).is_err());
    }

    #[test]
    fn style_with_all_fields_none_is_equivalent_to_default() {
        assert_eq!(
            serde_json::to_value(WidgetStyle::default()).unwrap(),
            json!({
                "color": null, "weight": null, "size": null,
                "align": null, "background": null, "radius": null, "padding": null
            })
        );
    }

    #[test]
    fn style_does_not_affect_validation() {
        let node = WidgetNode::Label {
            text: "x".to_string(),
            class: None,
            style: Some(WidgetStyle {
                color: Some(SemanticColor::Accent),
                ..Default::default()
            }),
            on_click: None,
        };
        assert!(node.validate().is_ok());
    }
}
