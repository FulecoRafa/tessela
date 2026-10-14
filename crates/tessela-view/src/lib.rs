//! The UI vocabulary shared by guests, the host and renderers.
//!
//! A [`Node`] is generic — a component `kind`, typed props and children — but
//! the set of kinds is closed: every node is validated against a
//! [`Vocabulary`] of [`ComponentSpec`]s before it reaches a renderer. The core
//! vocabulary is deliberately small and aimed at interfaces, not documents
//! (docs/adr/0003). Apps may register their own components, each with a
//! *fallback* written in core components, so every renderer can draw them.
//!
//! [`patch`] turns successive trees into small, serializable updates with
//! stable node ids — what a remote renderer will receive over the wire.

pub mod patch;
pub mod styled;
pub mod text;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use patch::{NodeId, Patch, Reconciler, ViewTree};

/// Identifies a guest callback inside one extension.
pub type HandlerId = u64;

pub type Props = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub props: Props,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
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

impl Tone {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.into())).ok()
    }
}

/// What happens when the user triggers an event prop (`on_click`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Handler {
    /// A guest function: runs guest code.
    Fn(HandlerId),
    /// A declarative action: the host calls a module directly, no guest code
    /// runs (docs/adr/0012).
    Action {
        #[serde(rename = "$action")]
        action: Action,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub module: String,
    pub func: String,
    #[serde(default)]
    pub args: Value,
}

impl Handler {
    pub fn from_value(v: &Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }
}

impl Node {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            key: None,
            props: Props::new(),
            children: Vec::new(),
        }
    }

    pub fn prop(mut self, k: &str, v: impl Into<Value>) -> Self {
        self.props.insert(k.into(), v.into());
        self
    }

    pub fn child(mut self, c: Node) -> Self {
        self.children.push(c);
        self
    }

    /// Shown in place of an extension that failed. Never constructible by guests.
    pub fn error(widget: &str, message: &str) -> Self {
        Node::new(ERROR)
            .prop("widget", widget)
            .prop("message", message)
    }

    pub fn is(&self, kind: &str) -> bool {
        self.kind == kind
    }

    pub fn str(&self, k: &str) -> Option<&str> {
        self.props.get(k).and_then(Value::as_str)
    }

    pub fn f32(&self, k: &str) -> Option<f32> {
        self.props.get(k).and_then(Value::as_f64).map(|v| v as f32)
    }

    pub fn bool(&self, k: &str) -> bool {
        self.props.get(k).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn tone(&self) -> Tone {
        self.str("tone").and_then(Tone::parse).unwrap_or_default()
    }

    pub fn handler(&self, k: &str) -> Option<Handler> {
        self.props.get(k).and_then(Handler::from_value)
    }

    /// Every guest function referenced from this subtree.
    pub fn handler_ids(&self, out: &mut Vec<HandlerId>) {
        for v in self.props.values() {
            if let Some(Handler::Fn(id)) = Handler::from_value(v) {
                out.push(id);
            }
        }
        self.children.iter().for_each(|c| c.handler_ids(out));
    }
}

// ---------------------------------------------------------------- schemas

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
    String,
    Number,
    Bool,
    Tone,
    /// `#rrggbb[aa]` or a theme token `$name` (styled::is_color).
    Color,
    /// Pixels, or a percentage string `"62%"`.
    Length,
    /// A number, `[vertical, horizontal]` or `[top, right, bottom, left]`.
    Sides,
    /// A number or `[top-left, top-right, bottom-right, bottom-left]`.
    Radius,
    /// `{ top?, right?, bottom?, left? }` with numbers.
    Place,
    /// An absolute path or a list of them (styled::paths).
    Paths,
    /// One of a fixed set of strings.
    Enum(&'static [&'static str]),
    /// An event prop: a guest function or an action. `payload` documents what
    /// the renderer passes along (`null` for clicks).
    Handler {
        payload: &'static str,
    },
}

impl PropType {
    fn accepts(self, v: &Value) -> bool {
        match self {
            PropType::String => v.is_string(),
            PropType::Number => v.is_number(),
            PropType::Bool => v.is_boolean(),
            PropType::Tone => v.as_str().and_then(Tone::parse).is_some(),
            PropType::Color => v.as_str().is_some_and(styled::is_color),
            PropType::Length => styled::Length::parse(v).is_some(),
            PropType::Sides => styled::sides(v).is_some(),
            PropType::Radius => styled::radius(v).is_some(),
            PropType::Place => styled::place(v).is_some(),
            PropType::Paths => styled::paths(v).is_some(),
            PropType::Enum(options) => v.as_str().is_some_and(|s| options.contains(&s)),
            PropType::Handler { .. } => Handler::from_value(v).is_some(),
        }
    }

    fn name(self) -> String {
        match self {
            PropType::String => "string".into(),
            PropType::Number => "number".into(),
            PropType::Bool => "boolean".into(),
            PropType::Tone => "tone (normal|muted|accent|warning|critical)".into(),
            PropType::Color => "color (#rrggbb, #rrggbbaa or $token)".into(),
            PropType::Length => "length (pixels or \"N%\")".into(),
            PropType::Sides => "number, [v, h] or [top, right, bottom, left]".into(),
            PropType::Radius => "number or [tl, tr, br, bl]".into(),
            PropType::Place => "{ top?, right?, bottom?, left? } of numbers".into(),
            PropType::Paths => "absolute path or a list of them".into(),
            PropType::Enum(options) => format!("one of {}", options.join("|")),
            PropType::Handler { .. } => "function or action".into(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PropSpec {
    pub name: &'static str,
    pub ty: PropType,
    pub required: bool,
}

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

/// Rewrites an app component into core components for renderers that do not
/// draw it natively.
pub type Fallback = Arc<dyn Fn(&Node) -> Node + Send + Sync>;

#[derive(Clone)]
pub struct ComponentSpec {
    pub name: &'static str,
    /// The prop a bare value fills: `Text "hi"` in Luau.
    pub shorthand: Option<&'static str>,
    pub props: &'static [PropSpec],
    pub children: bool,
    /// `None` for core components; required for app components.
    pub fallback: Option<Fallback>,
}

impl std::fmt::Debug for ComponentSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentSpec")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

const CLICK: PropType = PropType::Handler { payload: "null" };
const SCROLL: PropType = PropType::Handler {
    payload: "{ delta: 1 | -1 }",
};
const POINTER: PropType = PropType::Handler { payload: "null" };
const INTERACTIVE: [PropSpec; 4] = [
    opt("on_click", CLICK),
    opt("on_press", POINTER),
    opt("on_release", POINTER),
    opt("on_scroll", SCROLL),
];

pub const ERROR: &str = "Error";

fn core_specs() -> Vec<ComponentSpec> {
    const ROW: &[PropSpec] = &[opt("gap", PropType::Number)];
    const TEXT: &[PropSpec] = &[req("text", PropType::String), opt("tone", PropType::Tone)];
    const ICON: &[PropSpec] = &[req("name", PropType::String), opt("tone", PropType::Tone)];
    const PILL: &[PropSpec] = &[
        opt("icon", PropType::String),
        req("label", PropType::String),
        opt("active", PropType::Bool),
        INTERACTIVE[0],
        INTERACTIVE[1],
        INTERACTIVE[2],
        INTERACTIVE[3],
    ];
    const BUTTON: &[PropSpec] = &[
        req("label", PropType::String),
        INTERACTIVE[0],
        INTERACTIVE[1],
        INTERACTIVE[2],
        INTERACTIVE[3],
    ];
    const PROGRESS: &[PropSpec] = &[
        req("value", PropType::Number),
        opt("label", PropType::String),
    ];
    const ERR: &[PropSpec] = &[
        req("widget", PropType::String),
        req("message", PropType::String),
    ];
    let spec = |name, shorthand, props, children| ComponentSpec {
        name,
        shorthand,
        props,
        children,
        fallback: None,
    };
    vec![
        spec("Row", None, ROW, true),
        spec("Column", None, ROW, true),
        spec("Text", Some("text"), TEXT, false),
        spec("Icon", Some("name"), ICON, false),
        spec("Pill", Some("label"), PILL, false),
        spec("Button", Some("label"), BUTTON, false),
        spec("Progress", Some("value"), PROGRESS, false),
        spec("Spacer", None, &[], false),
        spec(ERROR, None, ERR, false),
    ]
}

/// Names of the core components, the ones every renderer must draw.
pub const CORE: &[&str] = &[
    "Row", "Column", "Text", "Icon", "Pill", "Button", "Progress", "Spacer", ERROR,
];

#[derive(Debug, thiserror::Error)]
#[error("invalid view tree: {0}")]
pub struct InvalidTree(String);

/// The closed set of components an app accepts.
#[derive(Debug, Clone)]
pub struct Vocabulary {
    specs: Vec<ComponentSpec>,
}

impl Default for Vocabulary {
    /// The core components plus the styled primitives (docs/adr/0015).
    fn default() -> Self {
        let mut specs = core_specs();
        specs.extend(styled::specs());
        Self { specs }
    }
}

impl Vocabulary {
    /// Add an app component. It must not shadow a core one, and it must come
    /// with a fallback (docs/adr/0003).
    pub fn register(&mut self, spec: ComponentSpec) -> Result<(), String> {
        if self.get(spec.name).is_some() {
            return Err(format!("component '{}' already exists", spec.name));
        }
        if spec.fallback.is_none() {
            return Err(format!(
                "app component '{}' needs a fallback in core components",
                spec.name
            ));
        }
        self.specs.push(spec);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ComponentSpec> {
        self.specs.iter().find(|s| s.name == name)
    }

    /// Components guests may construct (everything but `Error`).
    pub fn guest_components(&self) -> impl Iterator<Item = &ComponentSpec> {
        self.specs.iter().filter(|s| s.name != ERROR)
    }

    /// Parse and validate a guest tree.
    pub fn parse(&self, tree: Value) -> Result<Node, InvalidTree> {
        let tree = match tree {
            Value::Array(_) => serde_json::json!({ "type": "Row", "children": tree }),
            other => other,
        };
        self.node(tree, 0)
    }

    fn node(&self, v: Value, depth: usize) -> Result<Node, InvalidTree> {
        if depth > 64 {
            return Err(InvalidTree("tree too deep".into()));
        }
        let Value::Object(mut map) = v else {
            return Err(InvalidTree(format!("expected a component, got {v}")));
        };
        let kind = match map.remove("type") {
            Some(Value::String(s)) => s,
            _ => return Err(InvalidTree("component without a type".into())),
        };
        let spec = self
            .get(&kind)
            .filter(|s| s.name != ERROR)
            .ok_or_else(|| InvalidTree(format!("unknown component '{kind}'")))?;
        if kind == styled::SURFACE && depth > 0 {
            return Err(InvalidTree(
                "Surface is only valid as the root of an entry".into(),
            ));
        }
        let key = match map.remove("key") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(other) => {
                return Err(InvalidTree(format!(
                    "{kind}: key must be a string or number, got {other}"
                )));
            }
        };
        let mut children = Vec::new();
        if let Some(c) = map.remove("children") {
            let mut flat = Vec::new();
            flatten_into(c, &mut flat);
            if !flat.is_empty() && !spec.children {
                return Err(InvalidTree(format!("{kind} takes no children")));
            }
            for c in flat {
                children.push(self.node(c, depth + 1)?);
            }
        }
        let mut props = Props::new();
        for (k, v) in map {
            if v.is_null() {
                continue; // `nil`/`undefined` props are simply absent
            }
            let p = spec
                .props
                .iter()
                .find(|p| p.name == k)
                .ok_or_else(|| InvalidTree(format!("{kind} has no prop '{k}'")))?;
            if !p.ty.accepts(&v) {
                return Err(InvalidTree(format!(
                    "{kind}.{k} must be a {}, got {v}",
                    p.ty.name()
                )));
            }
            props.insert(k, v);
        }
        if let Some(p) = spec
            .props
            .iter()
            .find(|p| p.required && !props.contains_key(p.name))
        {
            return Err(InvalidTree(format!("{kind} needs '{}'", p.name)));
        }
        Ok(Node {
            kind,
            key,
            props,
            children,
        })
    }

    /// Rewrite every node whose kind `supported` rejects through its
    /// fallback, until only supported kinds remain.
    pub fn lower(&self, node: Node, supported: &dyn Fn(&str) -> bool) -> Node {
        self.lower_depth(node, supported, 0)
    }

    fn lower_depth(&self, node: Node, supported: &dyn Fn(&str) -> bool, depth: usize) -> Node {
        if depth > 16 {
            return Node::error(&node.kind, "fallback recursion too deep");
        }
        let node = if supported(&node.kind) {
            node
        } else {
            match self.get(&node.kind).and_then(|s| s.fallback.clone()) {
                Some(f) => {
                    let key = node.key.clone();
                    let mut lowered = f(&node);
                    lowered.key = lowered.key.or(key);
                    return self.lower_depth(lowered, supported, depth + 1);
                }
                None => Node::error(&node.kind, "no renderer support and no fallback"),
            }
        };
        let Node {
            kind,
            key,
            props,
            children,
        } = node;
        Node {
            kind,
            key,
            props,
            children: children
                .into_iter()
                .map(|c| self.lower_depth(c, supported, depth))
                .collect(),
        }
    }
}

/// Children may arrive as a single node, a list, or nested lists (a binding
/// that returns a list inside a list). `nil`/`false` are skipped so
/// `cond and Node{}` works; bare strings and numbers become `Text`.
fn flatten_into(v: Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(items) => items.into_iter().for_each(|i| flatten_into(i, out)),
        Value::Null | Value::Bool(false) => {}
        Value::String(s) => out.push(serde_json::json!({ "type": "Text", "text": s })),
        Value::Number(n) => out.push(serde_json::json!({ "type": "Text", "text": n.to_string() })),
        obj => out.push(obj),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> Result<Node, InvalidTree> {
        Vocabulary::default().parse(v)
    }

    #[test]
    fn parses_nested_children_and_skips_falsy() {
        let n = parse(
            json!({ "type": "Row", "children": [[{"type": "Spacer"}], null, false, "hi", 3] }),
        )
        .unwrap();
        assert_eq!(n.children.len(), 3);
        assert_eq!(n.children[1], Node::new("Text").prop("text", "hi"));
    }

    #[test]
    fn rejects_unknown_components_props_and_types() {
        assert!(parse(json!({"type": "WebView", "url": "https://evil"})).is_err());
        let e = parse(json!({"type": "Text", "text": "x", "href": "y"}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("no prop 'href'"), "{e}");
        let e = parse(json!({"type": "Progress", "value": "full"}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("must be a number"), "{e}");
        let e = parse(json!({"type": "Pill"})).unwrap_err().to_string();
        assert!(e.contains("needs 'label'"), "{e}");
        assert!(parse(json!({"type": "Text", "text": "x", "children": ["y"]})).is_err());
    }

    #[test]
    fn guests_cannot_forge_error_nodes() {
        assert!(parse(json!({"type": "Error", "widget": "x", "message": "y"})).is_err());
    }

    #[test]
    fn handlers_accept_functions_and_actions() {
        let n = parse(json!({"type": "Button", "label": "a", "on_click": 7,
            "on_press": {"$action": {"module": "service", "func": "call", "args": {"x": 1}}}}))
        .unwrap();
        assert_eq!(n.handler("on_click"), Some(Handler::Fn(7)));
        assert!(matches!(
            n.handler("on_press"),
            Some(Handler::Action { .. })
        ));
        assert!(parse(json!({"type": "Button", "label": "a", "on_click": "alert(1)"})).is_err());
    }

    #[test]
    fn app_components_need_a_fallback_and_lower_to_core() {
        let mut v = Vocabulary::default();
        const P: &[PropSpec] = &[req("value", PropType::Number)];
        let bad = ComponentSpec {
            name: "Gauge",
            shorthand: None,
            props: P,
            children: false,
            fallback: None,
        };
        assert!(v.register(bad.clone()).is_err());
        let fb: Fallback =
            Arc::new(|n: &Node| Node::new("Progress").prop("value", n.f32("value").unwrap_or(0.0)));
        v.register(ComponentSpec {
            fallback: Some(fb),
            ..bad
        })
        .unwrap();
        let tree = v
            .parse(
                json!({"type": "Row", "children": [{"type": "Gauge", "value": 0.5, "key": "g"}]}),
            )
            .unwrap();
        let lowered = v.lower(tree, &|k| CORE.contains(&k));
        assert_eq!(lowered.children[0].kind, "Progress");
        assert_eq!(
            lowered.children[0].key.as_deref(),
            Some("g"),
            "keys survive lowering"
        );
    }
}
