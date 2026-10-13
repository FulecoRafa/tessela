//! Elements: the live tree of each surface, and the selected node.
//!
//! Hovering a row outlines its node in the shell; without a hover, the
//! selection stays outlined. Pick mode hands the next click in the shell to
//! devtools: the node under the pointer is revealed while moving, selected
//! on click.

use std::collections::HashSet;

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, UniformListScrollHandle, Window, div,
    prelude::*, px, uniform_list,
};
use serde_json::{Value, json};

use super::widgets::{
    FieldEvent, Fonts, TextField, button, chip, json_lines, section, theme, value_inline,
};
use super::{Devtools, Split, divider, field};

/// Props worth showing inline in a row.
const SUMMARY: &[&str] = &[
    "text",
    "label",
    "src",
    "name",
    "value",
    "tooltip",
    "placeholder",
];

pub(super) struct Elements {
    pub surface: Option<String>,
    expanded: HashSet<(String, u64)>,
    /// Nodes seen before: only new ones get expanded by default.
    seen: HashSet<(String, u64)>,
    pub selected: Option<(String, u64)>,
    pub hovered: Option<(String, u64)>,
    /// Last drawn bounds seen per node.
    pub bounds: std::collections::HashMap<(String, u64), Value>,
    pub picking: bool,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    /// Tree pane width, as a fraction of the window.
    pub split: f32,
    search: Entity<TextField>,
    payload: Entity<TextField>,
    /// Scroll to the selection on the next render.
    reveal: bool,
}

#[derive(Clone)]
struct Row {
    id: u64,
    depth: usize,
    kind: String,
    key: Option<String>,
    summary: Vec<(String, Value)>,
    handlers: Vec<String>,
    children: usize,
    expanded: bool,
    matched: bool,
}

impl Elements {
    pub fn new(fonts: &Fonts, cx: &mut Context<Devtools>) -> Self {
        let search = field("Find by kind, prop or text", fonts, cx);
        cx.subscribe(&search, |this: &mut Devtools, _, e: &FieldEvent, cx| {
            if let FieldEvent::Submit = e {
                this.next_match(cx);
            }
            cx.notify();
        })
        .detach();
        Self {
            surface: None,
            expanded: HashSet::new(),
            seen: HashSet::new(),
            selected: None,
            hovered: None,
            bounds: Default::default(),
            picking: false,
            rows: vec![],
            scroll: UniformListScrollHandle::new(),
            split: 0.58,
            search,
            payload: field("payload JSON (optional)", fonts, cx),
            reveal: false,
        }
    }

    /// A new tree arrived: expand the first levels of nodes never seen.
    pub fn saw_tree(&mut self, surface: &str, tree: &Value) {
        fn walk(e: &mut Elements, surface: &str, v: &Value, depth: usize) {
            let Some(id) = v["id"].as_u64() else { return };
            let key = (surface.to_string(), id);
            if e.seen.insert(key.clone()) && depth < 4 {
                e.expanded.insert(key);
            }
            for c in v["children"].as_array().into_iter().flatten() {
                walk(e, surface, c, depth + 1);
            }
        }
        walk(self, surface, tree, 0);
    }
}

/// Ids from the root to `id`, both included.
fn path_to(v: &Value, id: u64, out: &mut Vec<u64>) -> bool {
    let Some(me) = v["id"].as_u64() else {
        return false;
    };
    out.push(me);
    if me == id {
        return true;
    }
    for c in v["children"].as_array().into_iter().flatten() {
        if path_to(c, id, out) {
            return true;
        }
    }
    out.pop();
    false
}

fn find(v: &Value, id: u64) -> Option<&Value> {
    if v["id"].as_u64() == Some(id) {
        return Some(v);
    }
    v["children"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|c| find(c, id))
}

fn matches(v: &Value, q: &str) -> bool {
    !q.is_empty()
        && (v["kind"].as_str().unwrap_or("").to_lowercase().contains(q)
            || v["props"].to_string().to_lowercase().contains(q)
            || v["id"]
                .as_u64()
                .is_some_and(|id| q.trim_start_matches('#') == id.to_string()))
}

impl Devtools {
    fn tree_surface(&self) -> Option<String> {
        let surfaces = self.surfaces();
        match &self.el.surface {
            Some(s) if surfaces.contains(s) => Some(s.clone()),
            _ => surfaces.first().cloned(),
        }
    }

    /// Flattens what is expanded into rows; with a search, also opens the
    /// way to every match.
    fn build_rows(&mut self, surface: &str, cx: &Context<Self>) {
        let q = self.el.search.read(cx).text.to_lowercase();
        let Some(tree) = self.trees.get(surface) else {
            self.el.rows.clear();
            return;
        };
        if !q.is_empty() {
            let mut open = vec![];
            fn collect(v: &Value, q: &str, path: &mut Vec<u64>, open: &mut Vec<u64>) {
                if matches(v, q) {
                    open.extend(path.iter().copied());
                }
                let Some(id) = v["id"].as_u64() else { return };
                path.push(id);
                for c in v["children"].as_array().into_iter().flatten() {
                    collect(c, q, path, open);
                }
                path.pop();
            }
            collect(tree, &q, &mut vec![], &mut open);
            for id in open {
                self.el.expanded.insert((surface.to_string(), id));
            }
        }
        let mut rows = vec![];
        fn walk(
            v: &Value,
            depth: usize,
            surface: &str,
            q: &str,
            expanded: &HashSet<(String, u64)>,
            rows: &mut Vec<Row>,
        ) {
            let Some(id) = v["id"].as_u64() else { return };
            let props = v["props"].as_object();
            let mut summary = vec![];
            let mut handlers = vec![];
            for (k, val) in props.into_iter().flatten() {
                if let Some(h) = k.strip_prefix("on_") {
                    handlers.push(h.to_string());
                } else if SUMMARY.contains(&k.as_str()) {
                    summary.push((k.clone(), val.clone()));
                }
            }
            let children = v["childCount"].as_u64().unwrap_or(0) as usize;
            let open = expanded.contains(&(surface.to_string(), id));
            rows.push(Row {
                id,
                depth,
                kind: v["kind"].as_str().unwrap_or("?").to_string(),
                key: v["key"].as_str().map(String::from),
                summary,
                handlers,
                children,
                expanded: open,
                matched: matches(v, q),
            });
            if open {
                for c in v["children"].as_array().into_iter().flatten() {
                    walk(c, depth + 1, surface, q, expanded, rows);
                }
            }
        }
        walk(tree, 0, surface, &q, &self.el.expanded, &mut rows);
        self.el.rows = rows;
    }

    /// Selects the next search match after the selection.
    fn next_match(&mut self, cx: &Context<Self>) {
        let Some(surface) = self.tree_surface() else {
            return;
        };
        self.build_rows(&surface, cx);
        let at = self
            .el
            .selected
            .as_ref()
            .and_then(|(_, id)| self.el.rows.iter().position(|r| r.id == *id));
        let n = self.el.rows.len();
        let start = at.map_or(0, |a| a + 1);
        let next = (0..n)
            .map(|i| (start + i) % n)
            .find(|i| self.el.rows.get(*i).is_some_and(|r| r.matched));
        if let Some(i) = next {
            let id = self.el.rows[i].id;
            self.select(Some((surface, id)));
        }
    }

    pub(super) fn select(&mut self, node: Option<(String, u64)>) {
        if let Some((s, _)) = &node {
            self.el.surface = Some(s.clone());
            self.el.reveal = true;
        }
        self.el.selected = node.clone();
        self.send(super::link::Cmd::Node(node));
        self.sync_outline();
    }

    /// Hover wins over the selection.
    pub(super) fn sync_outline(&mut self) {
        // Only nodes that still exist: a closed panel takes its nodes along.
        let exists =
            |n: &(String, u64)| self.trees.get(&n.0).is_some_and(|t| find(t, n.1).is_some());
        let target = self.el.hovered.clone().filter(exists).or(self
            .el
            .selected
            .clone()
            .filter(exists));
        self.outline(target);
    }

    /// A record from the `picks` channel.
    pub(super) fn picked(&mut self, r: &Value, cx: &mut Context<Self>) {
        let surface = r["surface"].as_str().unwrap_or("").to_string();
        let done = r["done"] == true;
        if done {
            self.el.picking = false;
        }
        match r["id"].as_u64() {
            Some(id) => {
                // The renderer already outlines it.
                self.outlined = Some((surface.clone(), id));
                self.el.hovered = None;
                self.select(Some((surface, id)));
                if done {
                    self.tab = super::Tab::Elements;
                }
            }
            None => self.sync_outline(),
        }
        cx.notify();
    }

    fn toggle_pick(&mut self) {
        self.el.picking = !self.el.picking;
        self.act("pick", json!({ "op": "pick", "on": self.el.picking }));
        if !self.el.picking {
            self.sync_outline();
        }
    }

    pub(super) fn elements(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme();
        let surface = self.tree_surface();
        // Open the way to the selection (its tree may arrive after it).
        if self.el.reveal
            && let Some((s, id)) = self.el.selected.clone()
            && let Some(tree) = self.trees.get(&s)
        {
            let mut path = vec![];
            path_to(tree, id, &mut path);
            path.pop();
            for p in path {
                self.el.expanded.insert((s.clone(), p));
            }
        }
        if let Some(s) = &surface {
            self.build_rows(s, cx);
        } else {
            self.el.rows.clear();
        }
        if self.el.reveal
            && let Some(i) = self
                .el
                .selected
                .as_ref()
                .and_then(|(_, id)| self.el.rows.iter().position(|r| r.id == *id))
        {
            self.el.reveal = false;
            self.el.scroll.scroll_to_item(i, ScrollStrategy::Center);
        }
        let open: HashSet<usize> = self.overview["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| w["slot"].as_u64().map(|s| s as usize))
            .collect();
        let chips = self.surfaces().into_iter().enumerate().map(|(i, name)| {
            let on = surface.as_deref() == Some(name.as_str());
            let label = if open.contains(&i) {
                name.clone()
            } else {
                format!("{name} (hidden)")
            };
            let n = name.clone();
            chip(
                SharedString::from(format!("surface-{name}")),
                label,
                on,
                None,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.el.surface = Some(n.clone());
                cx.notify();
            }))
        });
        let toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(34.0))
            .border_b_1()
            .border_color(t.border)
            .child(
                chip("pick", "⌖ Pick", self.el.picking, Some(t.accent)).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.toggle_pick();
                        cx.notify();
                    },
                )),
            )
            .child(div().w(px(1.0)).h(px(18.0)).bg(t.border))
            .children(chips)
            .child(div().flex_1())
            .child(div().w(px(220.0)).child(self.el.search.clone()));
        let rows = self.el.rows.len();
        let selected = self.el.selected.clone();
        let hovered = self.el.hovered.clone();
        let s = surface.clone().unwrap_or_default();
        let mono = self.fonts.mono.clone();
        let list = uniform_list(
            "tree",
            rows,
            cx.processor(
                move |this: &mut Devtools, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|i| this.el.rows.get(i).cloned())
                        .map(|r| tree_row(&r, &s, &selected, &hovered, cx))
                        .collect::<Vec<_>>()
                },
            ),
        )
        .track_scroll(&self.el.scroll)
        .font_family(mono)
        .size_full();
        let tree_pane = div()
            .flex()
            .flex_col()
            .h_full()
            .w(gpui::relative(self.el.split))
            .min_w(px(200.0))
            .child(toolbar)
            .child(
                div()
                    .id("tree-pane")
                    .flex_1()
                    .min_h(px(0.0))
                    .on_hover(cx.listener(|this, inside: &bool, _, cx| {
                        if !inside && this.el.hovered.take().is_some() {
                            this.sync_outline();
                            cx.notify();
                        }
                    }))
                    .when(surface.is_none(), |d| {
                        d.child(div().p_4().text_color(t.muted).child("No surfaces yet."))
                    })
                    .child(list),
            );
        div()
            .flex()
            .size_full()
            .child(tree_pane)
            .child(divider(Split::Elements, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .bg(t.panel)
                    .child(self.node_panel(cx)),
            )
    }

    fn node_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let Some((surface, id)) = self.el.selected.clone() else {
            return div()
                .p_4()
                .text_color(t.muted)
                .child("Select a node, or use ⌖ Pick and click something in the shell.")
                .into_any_element();
        };
        let Some(node) = self
            .trees
            .get(&surface)
            .and_then(|tr| find(tr, id))
            .cloned()
        else {
            return div()
                .p_4()
                .text_color(t.muted)
                .child(format!("#{id} is gone from {surface}."))
                .into_any_element();
        };
        let mono = self.fonts.mono.clone();
        let kind = node["kind"].as_str().unwrap_or("?").to_string();
        // Breadcrumb from the root.
        let mut path = vec![];
        if let Some(tree) = self.trees.get(&surface) {
            path_to(tree, id, &mut path);
        }
        let crumbs = path.iter().enumerate().map(|(i, pid)| {
            let k = self
                .trees
                .get(&surface)
                .and_then(|tr| find(tr, *pid))
                .and_then(|n| n["kind"].as_str().map(String::from))
                .unwrap_or_default();
            let s = surface.clone();
            let pid = *pid;
            div()
                .flex()
                .items_center()
                .when(i > 0, |d| {
                    d.child(div().px_1().text_color(t.faint).child("›"))
                })
                .child(
                    div()
                        .id(SharedString::from(format!("crumb-{pid}")))
                        .cursor_pointer()
                        .text_color(if pid == id { t.text } else { t.kind })
                        .hover(|s| s.underline())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select(Some((s.clone(), pid)));
                            cx.notify();
                        }))
                        .child(k),
                )
        });
        let props = node["props"].as_object().cloned().unwrap_or_default();
        let handlers: Vec<(String, String)> = props
            .iter()
            .filter(|(k, _)| k.starts_with("on_"))
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect();
        let plain: Vec<(String, Value)> = props
            .into_iter()
            .filter(|(k, _)| !k.starts_with("on_"))
            .collect();
        let bounds = match self
            .el
            .bounds
            .get(&(surface.clone(), id))
            .and_then(Value::as_array)
        {
            Some(b) => {
                let n: Vec<f64> = b.iter().filter_map(Value::as_f64).collect();
                Some(format!(
                    "{:.0} × {:.0} at ({:.0}, {:.0}) in its window",
                    n.get(2).unwrap_or(&0.0),
                    n.get(3).unwrap_or(&0.0),
                    n.first().unwrap_or(&0.0),
                    n.get(1).unwrap_or(&0.0)
                ))
            }
            _ => None,
        };
        let handler_rows = handlers.into_iter().map(|(prop, label)| {
            let (s, p) = (surface.clone(), prop.clone());
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .child(div().w(px(130.0)).text_color(t.prop).child(prop.clone()))
                .child(div().flex_1().text_color(t.muted).child(label))
                .child(
                    button(SharedString::from(format!("invoke-{prop}")), "▶ Invoke").on_click(
                        cx.listener(move |this, _, _, cx| {
                            let text = this.el.payload.read(cx).text.trim().to_string();
                            let mut req =
                                json!({ "op": "click", "surface": s, "id": id, "prop": p });
                            if !text.is_empty() {
                                match serde_json::from_str::<Value>(&text) {
                                    Ok(v) => req["payload"] = v,
                                    Err(e) => {
                                        this.status =
                                            Some((format!("payload is not JSON: {e}"), true));
                                        cx.notify();
                                        return;
                                    }
                                }
                            }
                            this.act("invoke", req);
                        }),
                    ),
                )
        });
        let has_handlers = node["props"]
            .as_object()
            .is_some_and(|p| p.keys().any(|k| k.starts_with("on_")));
        let prop_rows = plain.into_iter().map(|(k, v)| {
            let long = matches!(v, Value::Object(_) | Value::Array(_));
            div()
                .flex()
                .px_2()
                .py(px(2.0))
                .child(
                    div()
                        .w(px(130.0))
                        .flex_shrink_0()
                        .text_color(t.prop)
                        .child(k),
                )
                .child(if long {
                    div().flex().flex_col().children(json_lines(&v, 40))
                } else {
                    value_inline(&v, 400)
                })
        });
        div()
            .id("node-panel")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .font_family(mono)
            .child(
                div()
                    .px_2()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .text_color(t.kind)
                            .text_size(px(14.0))
                            .child(kind.clone()),
                    )
                    .child(div().text_color(t.muted).child(format!("#{id}")))
                    .when_some(node["key"].as_str(), |d, k| {
                        d.child(div().text_color(t.string).child(format!("key={k:?}")))
                    })
                    .child(div().flex_1())
                    .child(div().text_color(t.faint).child(surface.clone())),
            )
            .child(div().flex().flex_wrap().px_2().py_1().children(crumbs))
            .when(has_handlers, |d| {
                d.child(section("Handlers"))
                    .child(div().px_2().pt_1().child(self.el.payload.clone()))
                    .children(handler_rows)
            })
            .child(section("Props"))
            .children(prop_rows)
            .child(section("Layout"))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(if bounds.is_some() { t.text } else { t.faint })
                    .child(bounds.unwrap_or_else(|| {
                        "not drawn (closed popup, hidden surface) or not measured yet".into()
                    })),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(t.muted)
                    .child(format!("{} children", node["childCount"])),
            )
            .into_any_element()
    }
}

fn tree_row(
    r: &Row,
    surface: &str,
    selected: &Option<(String, u64)>,
    hovered: &Option<(String, u64)>,
    cx: &mut Context<Devtools>,
) -> AnyElement {
    let t = theme();
    let me = (surface.to_string(), r.id);
    let is_sel = selected.as_ref() == Some(&me);
    let is_hover = hovered.as_ref() == Some(&me);
    let arrow = if r.children == 0 {
        " "
    } else if r.expanded {
        "▾"
    } else {
        "▸"
    };
    let (toggle, pick, hover) = (me.clone(), me.clone(), me.clone());
    let mut line = div()
        .flex()
        .items_center()
        .gap_1()
        .whitespace_nowrap()
        .child(div().text_color(t.kind).child(r.kind.clone()))
        .when_some(r.key.clone(), |d, k| {
            d.child(attr("key", &Value::String(k)))
        });
    for (k, v) in &r.summary {
        line = line.child(attr(k, v));
    }
    for h in &r.handlers {
        line = line.child(
            div()
                .px_1()
                .rounded(px(3.0))
                .bg(t.raised)
                .text_color(t.muted)
                .text_size(px(10.0))
                .child(h.clone()),
        );
    }
    if r.children > 0 && !r.expanded {
        line = line.child(div().text_color(t.faint).child(format!("… {}", r.children)));
    }
    line = line.child(div().text_color(t.faint).child(format!("#{}", r.id)));
    div()
        .id(SharedString::from(format!("row-{}", r.id)))
        .flex()
        .w_full()
        .items_center()
        .h(px(20.0))
        .pl(px(6.0 + r.depth as f32 * 14.0))
        .when(is_sel, |d| d.bg(t.selected))
        .when(!is_sel && is_hover, |d| d.bg(t.hover))
        .when(r.matched, |d| d.border_l_2().border_color(t.warn))
        .on_hover(
            cx.listener(move |this: &mut Devtools, inside: &bool, _, cx| {
                if *inside {
                    this.el.hovered = Some(hover.clone());
                } else if this.el.hovered.as_ref() == Some(&hover) {
                    this.el.hovered = None;
                }
                this.sync_outline();
                cx.notify();
            }),
        )
        .on_click(cx.listener(move |this: &mut Devtools, _, _, cx| {
            this.select(Some(pick.clone()));
            cx.notify();
        }))
        .child(
            div()
                .id(SharedString::from(format!("arrow-{}", r.id)))
                .w(px(14.0))
                .flex_shrink_0()
                .text_color(t.muted)
                .cursor_pointer()
                .on_click(cx.listener(move |this: &mut Devtools, _, _, cx| {
                    if !this.el.expanded.remove(&toggle) {
                        this.el.expanded.insert(toggle.clone());
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(arrow),
        )
        .child(line)
        .into_any_element()
}

/// `name="value"`, as in markup.
fn attr(name: &str, v: &Value) -> impl IntoElement {
    let t = theme();
    div()
        .flex()
        .child(div().text_color(t.prop).child(name.to_string()))
        .child(div().text_color(t.muted).child("="))
        .child(value_inline(v, 48))
}
