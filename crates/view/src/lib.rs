//! The closed component vocabulary.
//!
//! Guest code (Luau or TSX) never touches the renderer. It produces a tree of
//! plain values; the runtime resolves bindings, and the result is parsed here
//! into [`Node`]. Anything outside this vocabulary is rejected, so a widget can
//! only ask for what every renderer (GPUI, terminal, tests) knows how to draw.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Identifies a guest callback (`on_click`, `onScroll`, ...) inside one widget.
pub type HandlerId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Node {
    Row {
        #[serde(default)]
        gap: f32,
        #[serde(default)]
        children: Vec<Node>,
    },
    Column {
        #[serde(default)]
        gap: f32,
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        text: String,
        #[serde(default)]
        tone: Tone,
    },
    Icon {
        name: String,
    },
    /// The bar's basic unit: icon + short label, optionally highlighted.
    Pill {
        #[serde(default)]
        icon: Option<String>,
        label: String,
        #[serde(default)]
        active: bool,
        #[serde(default)]
        on_click: Option<HandlerId>,
        #[serde(default)]
        on_scroll: Option<HandlerId>,
    },
    Button {
        label: String,
        #[serde(default)]
        on_click: Option<HandlerId>,
    },
    /// 0.0..=1.0
    Progress {
        value: f32,
        #[serde(default)]
        label: Option<String>,
    },
    Spacer,
    /// Rendered in place of a widget that failed; never produced by guests.
    #[serde(skip_deserializing)]
    Error {
        widget: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    #[default]
    Normal,
    Muted,
    Accent,
    Warning,
    Critical,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid view tree: {0}")]
pub struct InvalidTree(String);

/// Children may arrive as a single node, a list, or nested lists (a binding
/// that returns a list inside a list). Flatten before typed parsing.
fn normalize(v: Value) -> Value {
    match v {
        Value::Object(mut map) => {
            if let Some(children) = map.remove("children") {
                let mut flat = Vec::new();
                flatten_into(children, &mut flat);
                map.insert("children".into(), Value::Array(flat));
            }
            for (_, val) in map.iter_mut() {
                if val.is_object() {
                    *val = normalize(std::mem::take(val));
                }
            }
            Value::Object(map)
        }
        other => other,
    }
}

fn flatten_into(v: Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(items) => items.into_iter().for_each(|i| flatten_into(i, out)),
        // `nil`/`false`/`null` children are skipped so `cond and Node{}` works.
        Value::Null | Value::Bool(false) => {}
        Value::String(s) => out.push(serde_json::json!({ "type": "Text", "text": s })),
        Value::Number(n) => out.push(serde_json::json!({ "type": "Text", "text": n.to_string() })),
        obj => out.push(normalize(obj)),
    }
}

pub fn parse(tree: Value) -> Result<Node, InvalidTree> {
    let tree = match tree {
        Value::Array(_) => serde_json::json!({ "type": "Row", "children": tree }),
        other => other,
    };
    serde_json::from_value(normalize(tree)).map_err(|e| InvalidTree(e.to_string()))
}

impl Node {
    /// Every handler id reachable from this node, used to garbage-collect
    /// guest callbacks that are no longer on screen.
    pub fn handlers(&self, out: &mut Vec<HandlerId>) {
        match self {
            Node::Row { children, .. } | Node::Column { children, .. } => {
                children.iter().for_each(|c| c.handlers(out))
            }
            Node::Pill {
                on_click,
                on_scroll,
                ..
            } => out.extend(on_click.iter().chain(on_scroll)),
            Node::Button { on_click, .. } => out.extend(on_click),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_nested_children_and_skips_falsy() {
        let n = parse(json!({
            "type": "Row",
            "children": [[{"type": "Spacer"}], null, false, "hi", 3]
        }))
        .unwrap();
        let Node::Row { children, .. } = n else {
            panic!()
        };
        assert_eq!(children.len(), 3);
        assert_eq!(
            children[1],
            Node::Text {
                text: "hi".into(),
                tone: Tone::Normal
            }
        );
    }

    #[test]
    fn rejects_unknown_components() {
        assert!(parse(json!({"type": "WebView", "url": "https://evil"})).is_err());
    }

    #[test]
    fn guests_cannot_forge_error_nodes() {
        assert!(parse(json!({"type": "Error", "widget": "x", "message": "y"})).is_err());
    }
}
