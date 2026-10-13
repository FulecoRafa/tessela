//! Styled primitives (docs/adr/0015): what an app needs to draw its own
//! design — a styled `Box`, `Label`, `Image`, a `Ring` meter, a `Slider`, an
//! anchored `Popup`, a one-line `Input` and the `Surface` an entry draws into.
//!
//! They are part of the default vocabulary but not of [`CORE`]: each one has a
//! fallback in core components, so renderers that only draw the core (the
//! terminal) still show something sensible, while GPUI draws them natively.
//! Styles are data, validated like any prop: colors are hex or theme tokens
//! (`"$surface"`), never CSS, so no renderer has to parse a style language.
//!
//! [`CORE`]: crate::CORE

use std::sync::Arc;

use serde_json::Value;

use crate::{ComponentSpec, Fallback, Node, PropSpec, PropType};

pub const BOX: &str = "Box";
pub const LABEL: &str = "Label";
pub const IMAGE: &str = "Image";
pub const RING: &str = "Ring";
pub const POPUP: &str = "Popup";
pub const INPUT: &str = "Input";
/// A horizontal level the user sets by clicking, dragging or scrolling.
pub const SLIDER: &str = "Slider";
/// Root of an entry; only valid as the root of a tree.
pub const SURFACE: &str = "Surface";

/// Names of the styled primitives.
pub const STYLED: &[&str] = &[BOX, LABEL, IMAGE, RING, SLIDER, POPUP, INPUT, SURFACE];

const fn opt(name: &'static str, ty: PropType) -> PropSpec {
    PropSpec {
        name,
        ty,
        required: false,
    }
}
const fn req(name: &'static str, ty: PropType) -> PropSpec {
    PropSpec {
        name,
        ty,
        required: true,
    }
}

const CLICK: PropType = PropType::Handler { payload: "null" };
const SCROLL: PropType = PropType::Handler {
    payload: "{ delta: 1 | -1 }",
};
const TEXT_EVENT: PropType = PropType::Handler {
    payload: "{ value: string }",
};
const VALUE_EVENT: PropType = PropType::Handler {
    payload: "{ value: number }",
};
const ALIGN: PropType = PropType::Enum(&["start", "center", "end", "stretch"]);
const JUSTIFY: PropType = PropType::Enum(&["start", "center", "end", "between"]);

const BOX_PROPS: &[PropSpec] = &[
    opt("direction", PropType::Enum(&["row", "column"])),
    opt("gap", PropType::Number),
    opt("padding", PropType::Sides),
    opt("align", ALIGN),
    opt("justify", JUSTIFY),
    opt("width", PropType::Length),
    opt("height", PropType::Length),
    opt("minWidth", PropType::Number),
    opt("maxWidth", PropType::Number),
    opt("maxHeight", PropType::Number),
    opt("grow", PropType::Bool),
    opt("bg", PropType::Color),
    opt("border", PropType::Color),
    opt("radius", PropType::Radius),
    opt("opacity", PropType::Number),
    opt("hoverBg", PropType::Color),
    opt("hoverOpacity", PropType::Number),
    opt("tooltip", PropType::String),
    opt("place", PropType::Place),
    opt("scroll", PropType::Bool),
    opt("on_click", CLICK),
    opt("on_right_click", CLICK),
    opt("on_middle_click", CLICK),
    opt("on_scroll", SCROLL),
];

const LABEL_PROPS: &[PropSpec] = &[
    req("text", PropType::String),
    opt("color", PropType::Color),
    opt("size", PropType::Number),
    opt(
        "weight",
        PropType::Enum(&["normal", "medium", "semibold", "bold"]),
    ),
    opt("font", PropType::String),
    opt("maxWidth", PropType::Number),
    // Wrap onto as many lines as the parent's width needs, cut with an
    // ellipsis past `lines` when given.
    opt("wrap", PropType::Bool),
    opt("lines", PropType::Number),
];

const IMAGE_PROPS: &[PropSpec] = &[
    req("src", PropType::String),
    req("size", PropType::Number),
    opt("opacity", PropType::Number),
    // Rounded corners, clipping the picture (album covers).
    opt("radius", PropType::Radius),
    // "contain" (default) shows all of it; "cover" fills the square, cropping.
    opt("fit", PropType::Enum(&["contain", "cover"])),
];

const RING_PROPS: &[PropSpec] = &[
    req("value", PropType::Number),
    req("size", PropType::Number),
    opt("thickness", PropType::Number),
    opt("color", PropType::Color),
    opt("track", PropType::Color),
];

// `value` is 0..=1. The renderer moves the level under the pointer while
// dragging and reports each whole percent; the guest sets `value` back.
const SLIDER_PROPS: &[PropSpec] = &[
    req("value", PropType::Number),
    opt("width", PropType::Length),
    opt("thickness", PropType::Number),
    opt("color", PropType::Color),
    opt("track", PropType::Color),
    // What one wheel notch adds or takes (0.05 by default).
    opt("step", PropType::Number),
    opt("on_change", VALUE_EVENT),
];

const POPUP_PROPS: &[PropSpec] = &[
    req("open", PropType::Bool),
    req("width", PropType::Number),
    opt("on_dismiss", CLICK),
];

const INPUT_PROPS: &[PropSpec] = &[
    req("value", PropType::String),
    opt("placeholder", PropType::String),
    opt("size", PropType::Number),
    opt("color", PropType::Color),
    opt("autofocus", PropType::Bool),
    // Drawn as dots (passwords).
    opt("secret", PropType::Bool),
    opt("on_change", TEXT_EVENT),
    opt("on_submit", TEXT_EVENT),
];

const SURFACE_PROPS: &[PropSpec] = &[
    opt(
        "layer",
        PropType::Enum(&["background", "bottom", "top", "overlay"]),
    ),
    req("anchor", PropType::String),
    opt("width", PropType::Number),
    opt("height", PropType::Number),
    opt("margin", PropType::Sides),
    opt("exclusive", PropType::Number),
    opt("visible", PropType::Bool),
    opt("on_dismiss", CLICK),
];

fn container(n: &Node, direction: &str) -> Node {
    let kind = if direction == "column" {
        "Column"
    } else {
        "Row"
    };
    let mut out = Node::new(kind);
    if let Some(g) = n.props.get("gap") {
        out.props.insert("gap".into(), g.clone());
    }
    out.children = n.children.clone();
    out
}

fn fallback(f: fn(&Node) -> Node) -> Option<Fallback> {
    Some(Arc::new(f))
}

pub(crate) fn specs() -> Vec<ComponentSpec> {
    let spec = |name, shorthand, props, children, f: fn(&Node) -> Node| ComponentSpec {
        name,
        shorthand,
        props,
        children,
        fallback: fallback(f),
    };
    vec![
        spec(BOX, None, BOX_PROPS, true, |n| {
            container(n, n.str("direction").unwrap_or("row"))
        }),
        spec(LABEL, Some("text"), LABEL_PROPS, false, |n| {
            Node::new("Text").prop("text", n.str("text").unwrap_or(""))
        }),
        spec(IMAGE, Some("src"), IMAGE_PROPS, false, |_| {
            Node::new("Text").prop("text", "▣")
        }),
        spec(RING, Some("value"), RING_PROPS, true, |n| {
            Node::new("Progress").prop("value", n.f32("value").unwrap_or(0.0))
        }),
        spec(SLIDER, Some("value"), SLIDER_PROPS, false, |n| {
            Node::new("Progress").prop("value", n.f32("value").unwrap_or(0.0))
        }),
        spec(POPUP, None, POPUP_PROPS, true, |n| {
            if n.bool("open") {
                container(n, "column")
            } else {
                Node::new("Spacer")
            }
        }),
        spec(INPUT, Some("value"), INPUT_PROPS, false, |n| {
            let v = n.str("value").filter(|v| !v.is_empty());
            let text = match v {
                Some(v) if n.bool("secret") => "•".repeat(v.chars().count()),
                Some(v) => v.to_string(),
                None => n.str("placeholder").unwrap_or("").to_string(),
            };
            Node::new("Text").prop("text", text)
        }),
        spec(SURFACE, None, SURFACE_PROPS, true, |n| container(n, "row")),
    ]
}

// ---------------------------------------------------------------- values

/// `#rgb`, `#rrggbb`, `#rrggbbaa` or a theme token `$name`.
pub fn is_color(s: &str) -> bool {
    if let Some(token) = s.strip_prefix('$') {
        return !token.is_empty()
            && token.len() <= 64
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    }
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Parse a hex color to RGBA (0..=255). Tokens are resolved by the renderer.
pub fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let hex = s.strip_prefix('#')?;
    let digit = |i: usize, len: usize| u8::from_str_radix(&hex[i..i + len], 16).ok();
    match hex.len() {
        3 => {
            let d = |i| digit(i, 1).map(|v| v * 17);
            Some([d(0)?, d(1)?, d(2)?, 255])
        }
        6 => Some([digit(0, 2)?, digit(2, 2)?, digit(4, 2)?, 255]),
        8 => Some([digit(0, 2)?, digit(2, 2)?, digit(4, 2)?, digit(6, 2)?]),
        _ => None,
    }
}

/// A length: pixels or a percentage of the parent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    Percent(f32),
}

impl Length {
    pub fn parse(v: &Value) -> Option<Self> {
        if let Some(n) = v.as_f64() {
            return Some(Length::Px(n as f32));
        }
        let s = v.as_str()?.strip_suffix('%')?;
        s.trim().parse::<f32>().ok().map(Length::Percent)
    }
}

fn numbers(v: &Value, lens: &[usize]) -> Option<Vec<f32>> {
    if let Some(n) = v.as_f64() {
        return Some(vec![n as f32]);
    }
    let a = v.as_array()?;
    if !lens.contains(&a.len()) {
        return None;
    }
    a.iter().map(|x| x.as_f64().map(|f| f as f32)).collect()
}

/// `[top, right, bottom, left]` from a number, `[v, h]` or `[t, r, b, l]`.
pub fn sides(v: &Value) -> Option<[f32; 4]> {
    let n = numbers(v, &[2, 4])?;
    Some(match n.as_slice() {
        [a] => [*a; 4],
        [v, h] => [*v, *h, *v, *h],
        [t, r, b, l] => [*t, *r, *b, *l],
        _ => return None,
    })
}

/// `[top-left, top-right, bottom-right, bottom-left]`.
pub fn radius(v: &Value) -> Option<[f32; 4]> {
    let n = numbers(v, &[4])?;
    Some(match n.as_slice() {
        [a] => [*a; 4],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => return None,
    })
}

/// Offsets of a `place`d box: `[top, right, bottom, left]`, each optional.
pub fn place(v: &Value) -> Option<[Option<f32>; 4]> {
    let m = v.as_object()?;
    if m.keys()
        .any(|k| !matches!(k.as_str(), "top" | "right" | "bottom" | "left"))
    {
        return None;
    }
    let get = |k: &str| match m.get(k) {
        None | Some(Value::Null) => Some(None),
        Some(v) => v.as_f64().map(|f| Some(f as f32)),
    };
    Some([get("top")?, get("right")?, get("bottom")?, get("left")?])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vocabulary;
    use serde_json::json;

    #[test]
    fn colors_are_hex_or_tokens() {
        for ok in ["#fff", "#1A1F29", "#00000000", "$surface", "$text-muted"] {
            assert!(is_color(ok), "{ok}");
        }
        for bad in ["red", "#12345", "$", "$a b", "url(x)", "#ggg"] {
            assert!(!is_color(bad), "{bad}");
        }
        assert_eq!(parse_hex("#ff8000"), Some([255, 128, 0, 255]));
        assert_eq!(parse_hex("#0f08"), None);
        assert_eq!(parse_hex("#0f0"), Some([0, 255, 0, 255]));
    }

    #[test]
    fn shorthand_values_expand() {
        assert_eq!(sides(&json!(4)), Some([4.0; 4]));
        assert_eq!(sides(&json!([1, 2])), Some([1.0, 2.0, 1.0, 2.0]));
        assert_eq!(sides(&json!([1, 2, 3])), None);
        assert_eq!(radius(&json!([1, 2, 3, 4])), Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(Length::parse(&json!("62%")), Some(Length::Percent(62.0)));
        assert_eq!(Length::parse(&json!("62px")), None);
        assert_eq!(
            place(&json!({"top": -1, "left": 8})),
            Some([Some(-1.0), None, None, Some(8.0)])
        );
        assert_eq!(place(&json!({"z": 1})), None);
    }

    #[test]
    fn styled_trees_validate_and_lower_to_core() {
        let v = Vocabulary::default();
        let tree = v
            .parse(json!({"type": "Surface", "anchor": "top", "children": [
                {"type": "Box", "direction": "column", "gap": 4, "bg": "$surface", "radius": [1, 2, 3, 4],
                 "place": {"top": 2}, "width": "50%", "on_right_click": 3, "children": [
                    {"type": "Label", "text": "hi", "color": "#fff", "weight": "bold"},
                    {"type": "Ring", "value": 0.5, "size": 32, "children": ["50"]},
                    {"type": "Slider", "value": 0.3, "width": "50%", "on_change": 4},
                    {"type": "Image", "src": "file:/a.png", "size": 40, "radius": 8, "fit": "cover"},
                    {"type": "Label", "text": "a long body", "wrap": true, "lines": 3},
                    {"type": "Input", "value": "abc", "secret": true},
                ]},
            ]}))
            .unwrap();
        let lowered = v.lower(tree, &|k| crate::CORE.contains(&k));
        assert_eq!(lowered.kind, "Row");
        assert_eq!(lowered.children[0].kind, "Column");
        assert_eq!(lowered.children[0].children[1].kind, "Progress");
        assert_eq!(lowered.children[0].children[2].kind, "Progress");
        assert_eq!(
            lowered.children[0].children[5].props["text"],
            json!("•••"),
            "secret input text never reaches a fallback"
        );

        let bad = |t| v.parse(t).unwrap_err().to_string();
        assert!(bad(json!({"type": "Box", "bg": "red"})).contains("color"));
        assert!(bad(json!({"type": "Box", "direction": "diagonal"})).contains("one of"));
        assert!(bad(json!({"type": "Slider"})).contains("value"));
        assert!(
            bad(json!({"type": "Image", "src": "a", "size": 4, "fit": "fill"})).contains("one of")
        );
        assert!(bad(json!({"type": "Slider", "value": 1, "width": "wide"})).contains("width"));
        assert!(
            bad(json!({"type": "Box", "children": [{"type": "Surface", "anchor": "top"}]}))
                .contains("root")
        );
    }
}
