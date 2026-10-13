//! [`Shell`]: a renderer for projects (docs/adr/0015). Every slot is an
//! entry whose root is a `Surface`; each becomes its own layer-shell window,
//! opened and closed as the surface's `visible` prop changes. `Popup`s open
//! their own windows. Styled primitives are drawn natively; colors may be
//! theme tokens resolved for the current color scheme.
//!
//! Windows that must close on an outside click (`Popup`, a `Surface` with
//! `onDismiss`) are transparent overlays covering the output: the content
//! sits where its anchors put it, and a press anywhere else dismisses.
//! Hyprland's focus grab would be the alternative, but GPUI does not speak
//! that protocol.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::{
    AnyElement, App, AppContext, AsyncApp, Bounds, Context, Div, ElementId, Entity, FocusHandle,
    FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    PathBuilder, Pixels, Render, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Size, Stateful,
    StatefulInteractiveElement, Styled, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, canvas, div, img, point, prelude::*, px, relative,
    rgb,
};
use serde_json::{Value, json};
use tessela_host::{Frame, Renderer, Slot, Surface, Trees, UiEvent};
use tessela_view::patch::Retained;
use tessela_view::styled::{self, Length, STYLED};
use tessela_view::text::{icon, short_error};
use tessela_view::{CORE, ERROR, Handler, NodeId, Tone, ViewTree};

use crate::inspect::{Inspector, WindowInfo};

// ---------------------------------------------------------------- options

/// Theme tokens per color scheme: `"$surface"` → `0x232834`.
#[derive(Debug, Clone, Default)]
pub struct Theme {
    pub dark: HashMap<String, u32>,
    pub light: HashMap<String, u32>,
}

/// Turns an `<Image src>` into a file to draw, or `None`.
pub type ImageResolver = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

pub struct Shell {
    /// Wayland app id; layer namespaces are `<app_id>-<surface>`.
    pub app_id: String,
    /// Default font family for every surface.
    pub font: Option<String>,
    pub theme: Theme,
    pub images: ImageResolver,
    /// Run next to the desktop's own bar: top and bottom anchors swap, and no
    /// surface reserves space.
    pub dev_placement: bool,
    /// "dark" | "light" as the desktop changes it.
    pub scheme: Option<crossbeam_channel::Receiver<String>>,
    /// Devtools hooks; see [`Inspector`].
    pub inspector: Option<Inspector>,
}

impl Shell {
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            font: None,
            theme: Theme::default(),
            images: Arc::new(|_| None),
            dev_placement: false,
            scheme: None,
            inspector: None,
        }
    }
}

// ---------------------------------------------------------------- model

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WindowKey {
    Surface(usize),
    Popup(usize, NodeId),
}

/// Where a surface's content sits, in output coordinates once the output
/// size is known.
#[derive(Debug, Clone, PartialEq)]
struct Placement {
    layer: Layer,
    top: bool,
    bottom: bool,
    left: bool,
    right: bool,
    width: f32,
    height: f32,
    margin: [f32; 4],
    exclusive: Option<f32>,
    /// Covers the output and draws the content inside (outside click dismisses).
    overlay: bool,
    keyboard: bool,
}

impl Placement {
    fn of(r: &Retained, dev: bool) -> Self {
        let p = P(r);
        let anchor = p.str("anchor").unwrap_or_default();
        let has = |e: &str| anchor.split_whitespace().any(|a| a == e);
        let (mut top, mut bottom) = (has("top"), has("bottom"));
        let mut margin = r
            .props
            .get("margin")
            .and_then(styled::sides)
            .unwrap_or([0.0; 4]);
        if dev {
            std::mem::swap(&mut top, &mut bottom);
            margin.swap(0, 2);
        }
        let layer = match p.str("layer").as_deref() {
            Some("background") => Layer::Background,
            Some("bottom") => Layer::Bottom,
            Some("overlay") => Layer::Overlay,
            _ => Layer::Top,
        };
        Placement {
            layer,
            top,
            bottom,
            left: has("left"),
            right: has("right"),
            width: p.f32("width").unwrap_or(0.0),
            height: p.f32("height").unwrap_or(0.0),
            margin,
            exclusive: if dev { None } else { p.f32("exclusive") },
            overlay: r.props.contains_key("on_dismiss"),
            keyboard: false,
        }
    }

    /// Placement for a slot that failed or has no `Surface` root.
    fn fallback(dev: bool) -> Self {
        Placement {
            layer: Layer::Overlay,
            top: !dev,
            bottom: dev,
            left: true,
            right: false,
            width: 640.0,
            height: 40.0,
            margin: [8.0, 0.0, 8.0, 8.0],
            exclusive: None,
            overlay: false,
            keyboard: false,
        }
    }

    fn popup() -> Self {
        Placement {
            layer: Layer::Overlay,
            top: true,
            bottom: true,
            left: true,
            right: true,
            width: 0.0,
            height: 0.0,
            margin: [0.0; 4],
            exclusive: None,
            overlay: true,
            keyboard: false,
        }
    }

    fn options(&self, app_id: &str, namespace: String) -> WindowOptions {
        let mut anchor = Anchor::empty();
        let (top, bottom, left, right) = if self.overlay {
            (true, true, true, true)
        } else {
            (self.top, self.bottom, self.left, self.right)
        };
        for (on, a) in [
            (top, Anchor::TOP),
            (bottom, Anchor::BOTTOM),
            (left, Anchor::LEFT),
            (right, Anchor::RIGHT),
        ] {
            if on {
                anchor |= a;
            }
        }
        // Stretching along an axis anchored on both ends: size 0.
        let w = if left && right { 0.0 } else { self.width };
        let h = if top && bottom { 0.0 } else { self.height };
        let [mt, mr, mb, ml] = if self.overlay { [0.0; 4] } else { self.margin };
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.0), px(0.0)),
                size: Size::new(px(w), px(h)),
            })),
            app_id: Some(app_id.to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace,
                layer: self.layer,
                anchor,
                // Overlays ignore other surfaces' reserved space (-1).
                exclusive_zone: if self.overlay {
                    Some(px(-1.0))
                } else {
                    self.exclusive.map(px)
                },
                margin: Some((px(mt), px(mr), px(mb), px(ml))),
                keyboard_interactivity: if self.keyboard {
                    KeyboardInteractivity::OnDemand
                } else {
                    KeyboardInteractivity::None
                },
                ..Default::default()
            }),
            focus: self.keyboard,
            ..Default::default()
        }
    }
}

/// Text being edited in an `Input`; the renderer owns it while focused.
struct InputState {
    text: String,
    focus: FocusHandle,
}

pub(crate) struct Model {
    slots: Vec<Slot>,
    trees: Trees,
    theme: Theme,
    light: bool,
    font: Option<SharedString>,
    images: ImageResolver,
    dev: bool,
    /// Popup anchors: bounds in their window, written while painting.
    anchors: Rc<RefCell<HashMap<(usize, NodeId), Bounds<Pixels>>>>,
    inputs: HashMap<(usize, NodeId), InputState>,
    pub(crate) inspector: Option<Inspector>,
    warned: RefCell<HashSet<String>>,
}

impl Model {
    fn color(&self, s: &str) -> Hsla {
        if let Some(token) = s.strip_prefix('$') {
            let map = if self.light {
                &self.theme.light
            } else {
                &self.theme.dark
            };
            return match map.get(token).or_else(|| self.theme.dark.get(token)) {
                Some(c) => Rgba::from(rgb(*c)).into(),
                None => {
                    if self.warned.borrow_mut().insert(s.to_string()) {
                        log::warn!("theme: no token {s}");
                    }
                    Rgba::from(rgb(0xff00ff)).into()
                }
            };
        }
        match styled::parse_hex(s) {
            Some([r, g, b, a]) => Rgba {
                r: r as f32 / 255.0,
                g: g as f32 / 255.0,
                b: b as f32 / 255.0,
                a: a as f32 / 255.0,
            }
            .into(),
            None => Rgba::from(rgb(0xff00ff)).into(),
        }
    }

    /// A token if the theme has it, else a fixed color (core components in
    /// apps without a theme).
    fn token_or(&self, token: &str, fallback: u32) -> Hsla {
        let map = if self.light {
            &self.theme.light
        } else {
            &self.theme.dark
        };
        Rgba::from(rgb(map.get(token).copied().unwrap_or(fallback))).into()
    }

    fn tone(&self, t: Tone) -> Hsla {
        match t {
            Tone::Normal => self.token_or("text", 0xbfbdb6),
            Tone::Muted => self.token_or("textMuted", 0x565b66),
            Tone::Accent => self.token_or("yellow", 0xe6b450),
            Tone::Warning => self.token_or("orange", 0xffb454),
            Tone::Critical => self.token_or("red", 0xd95757),
        }
    }

    fn root(&self, slot: usize) -> Option<(&ViewTree, NodeId, &Retained)> {
        let tree = self.trees.tree(slot)?;
        let id = tree.root()?;
        Some((tree, id, tree.get(id)?))
    }

    /// Every window that should exist now, with its placement.
    fn wanted(&self) -> Vec<(WindowKey, Placement)> {
        let mut out = Vec::new();
        for slot in 0..self.slots.len() {
            let Some((tree, id, root)) = self.root(slot) else {
                continue;
            };
            if root.kind != styled::SURFACE {
                out.push((WindowKey::Surface(slot), Placement::fallback(self.dev)));
                continue;
            }
            if root.props.get("visible").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let mut placement = Placement::of(root, self.dev);
            placement.keyboard = has_kind(tree, id, styled::INPUT);
            out.push((WindowKey::Surface(slot), placement));
            for popup in open_popups(tree, id) {
                let mut p = Placement::popup();
                p.keyboard = has_kind(tree, popup, styled::INPUT);
                out.push((WindowKey::Popup(slot, popup), p));
            }
        }
        out
    }
}

fn has_kind(tree: &ViewTree, id: NodeId, kind: &str) -> bool {
    let Some(r) = tree.get(id) else { return false };
    r.kind == kind || r.children.iter().any(|c| has_kind(tree, *c, kind))
}

fn open_popups(tree: &ViewTree, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        let Some(r) = tree.get(id) else { continue };
        if r.kind == styled::POPUP {
            if r.props.get("open").and_then(Value::as_bool) == Some(true) {
                out.push(id);
            }
            continue; // popups inside closed popups stay closed
        }
        stack.extend(r.children.iter().copied());
    }
    out
}

// ---------------------------------------------------------------- props

struct P<'a>(&'a Retained);

impl P<'_> {
    fn str(&self, k: &str) -> Option<String> {
        self.0
            .props
            .get(k)
            .and_then(Value::as_str)
            .map(String::from)
    }
    fn f32(&self, k: &str) -> Option<f32> {
        self.0
            .props
            .get(k)
            .and_then(Value::as_f64)
            .map(|v| v as f32)
    }
    fn bool(&self, k: &str) -> bool {
        self.0
            .props
            .get(k)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    fn handler(&self, k: &str) -> Option<Handler> {
        self.0.props.get(k).and_then(Handler::from_value)
    }
    fn tone(&self) -> Tone {
        self.str("tone")
            .and_then(|t| Tone::parse(&t))
            .unwrap_or_default()
    }
}

fn eid(slot: usize, node: NodeId) -> ElementId {
    ElementId::Name(SharedString::from(format!("s{slot}-n{node}")))
}

fn scroll_delta(e: &ScrollWheelEvent) -> i32 {
    let y = match e.delta {
        ScrollDelta::Lines(l) => l.y,
        ScrollDelta::Pixels(p) => f32::from(p.y),
    };
    (y > 0.0) as i32 - (y < 0.0) as i32
}

fn send(slot: &Slot, h: &Handler, payload: Value) {
    slot.send(UiEvent::new(h.clone(), payload));
}

// ---------------------------------------------------------------- drawing

struct Draw<'a> {
    m: &'a Model,
    tree: &'a ViewTree,
    slot: usize,
    model: &'a Entity<Model>,
}

impl Draw<'_> {
    fn ui(&self) -> &Slot {
        &self.m.slots[self.slot]
    }

    fn node(&self, id: NodeId, window: &mut Window, cx: &mut App) -> AnyElement {
        let Some(r) = self.tree.get(id) else {
            return div().into_any_element();
        };
        match r.kind.as_str() {
            styled::BOX | styled::SURFACE => self.boxed(id, r, window, cx),
            styled::LABEL => self.label(r),
            styled::IMAGE => self.image(r),
            styled::RING => self.ring(id, r, window, cx),
            styled::INPUT => self.input(id, r, window, cx),
            // Drawn in its own window; here it only marks the anchor.
            styled::POPUP => self.anchor_probe(id),
            _ => self.core(id, r, window, cx),
        }
    }

    fn children(&self, r: &Retained, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
        r.children
            .iter()
            .map(|c| self.node(*c, window, cx))
            .collect()
    }

    /// Records where a popup's parent is, so its window can open next to it.
    fn anchor_probe(&self, id: NodeId) -> AnyElement {
        let anchors = self.m.anchors.clone();
        let key = (self.slot, id);
        canvas(
            move |bounds, _, _| {
                anchors.borrow_mut().insert(key, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }

    fn events(&self, mut el: Stateful<Div>, r: &Retained) -> Stateful<Div> {
        let p = P(r);
        let ui = self.ui().clone();
        let mut clickable = false;
        if let Some(h) = p.handler("on_click") {
            let ui = ui.clone();
            clickable = true;
            el = el.on_click(move |_, _, _| send(&ui, &h, Value::Null));
        }
        for (prop, button) in [
            ("on_right_click", MouseButton::Right),
            ("on_middle_click", MouseButton::Middle),
        ] {
            if let Some(h) = p.handler(prop) {
                let ui = ui.clone();
                clickable = true;
                el = el.on_mouse_down(button, move |_, _, cx| {
                    cx.stop_propagation();
                    send(&ui, &h, Value::Null)
                });
            }
        }
        for (prop, down) in [("on_press", true), ("on_release", false)] {
            if let Some(h) = p.handler(prop) {
                let ui = ui.clone();
                el = if down {
                    el.on_mouse_down(MouseButton::Left, move |_, _, _| send(&ui, &h, Value::Null))
                } else {
                    el.on_mouse_up(MouseButton::Left, move |_, _, _| send(&ui, &h, Value::Null))
                };
            }
        }
        if let Some(h) = p.handler("on_scroll") {
            el = el.on_scroll_wheel(move |e, _, cx| {
                let d = scroll_delta(e);
                if d != 0 {
                    cx.stop_propagation();
                    send(&ui, &h, json!({ "delta": d }));
                }
            });
        }
        if clickable {
            el = el.cursor_pointer();
        }
        el
    }

    fn boxed(&self, id: NodeId, r: &Retained, window: &mut Window, cx: &mut App) -> AnyElement {
        let p = P(r);
        let m = self.m;
        let mut el = div().id(eid(self.slot, id)).relative().flex();
        el = if p.str("direction").as_deref() == Some("column") {
            el.flex_col()
        } else {
            el.flex_row()
        };
        if let Some(g) = p.f32("gap") {
            el = el.gap(px(g));
        }
        if let Some([t, rt, b, l]) = r.props.get("padding").and_then(styled::sides) {
            el = el.pt(px(t)).pr(px(rt)).pb(px(b)).pl(px(l));
        }
        el = match p.str("align").as_deref() {
            Some("start") => el.items_start(),
            Some("center") => el.items_center(),
            Some("end") => el.items_end(),
            _ => el,
        };
        el = match p.str("justify").as_deref() {
            Some("center") => el.justify_center(),
            Some("end") => el.justify_end(),
            Some("between") => el.justify_between(),
            _ => el,
        };
        match r.props.get("width").and_then(Length::parse) {
            Some(Length::Px(w)) => el = el.w(px(w)).flex_shrink_0(),
            Some(Length::Percent(f)) => el = el.w(relative(f / 100.0)),
            None => {}
        }
        match r.props.get("height").and_then(Length::parse) {
            Some(Length::Px(h)) => el = el.h(px(h)).flex_shrink_0(),
            Some(Length::Percent(f)) => el = el.h(relative(f / 100.0)),
            None => {}
        }
        if let Some(w) = p.f32("minWidth") {
            el = el.min_w(px(w));
        }
        if let Some(w) = p.f32("maxWidth") {
            el = el.max_w(px(w)).overflow_hidden();
        }
        if p.bool("grow") {
            el = el.flex_grow(1.0).flex_basis(px(0.0)).min_w(px(0.0));
        }
        if let Some(c) = p.str("bg") {
            el = el.bg(m.color(&c));
        }
        if let Some(c) = p.str("border") {
            el = el.border_1().border_color(m.color(&c));
        }
        if let Some([tl, tr, br, bl]) = r.props.get("radius").and_then(styled::radius) {
            el = el
                .rounded_tl(px(tl))
                .rounded_tr(px(tr))
                .rounded_br(px(br))
                .rounded_bl(px(bl));
        }
        if let Some(o) = p.f32("opacity") {
            el = el.opacity(o.clamp(0.0, 1.0));
        }
        if let Some([t, rt, b, l]) = r.props.get("place").and_then(styled::place) {
            el = el.absolute();
            if let Some(v) = t {
                el = el.top(px(v));
            }
            if let Some(v) = rt {
                el = el.right(px(v));
            }
            if let Some(v) = b {
                el = el.bottom(px(v));
            }
            if let Some(v) = l {
                el = el.left(px(v));
            }
        }
        let hover_bg = p.str("hoverBg").map(|c| m.color(&c));
        let hover_opacity = p.f32("hoverOpacity");
        if hover_bg.is_some() || hover_opacity.is_some() {
            el = el.hover(move |s| {
                let s = match hover_bg {
                    Some(c) => s.bg(c),
                    None => s,
                };
                match hover_opacity {
                    Some(o) => s.opacity(o),
                    None => s,
                }
            });
        }
        if let Some(t) = p.str("tooltip").filter(|t| !t.is_empty()) {
            let colors = (
                m.token_or("surface", 0x232834),
                m.token_or("surfaceBorder", 0x2e3542),
                m.token_or("text", 0xcccac2),
            );
            let font = m.font.clone();
            el = el.tooltip(move |_, cx| {
                let (t, font) = (SharedString::from(t.clone()), font.clone());
                cx.new(|_| Tooltip {
                    text: t,
                    colors,
                    font,
                })
                .into()
            });
        }
        if p.bool("scroll") {
            el = el.overflow_y_scroll();
        }
        el = self.events(el, r);
        el.children(self.children(r, window, cx)).into_any_element()
    }

    fn label(&self, r: &Retained) -> AnyElement {
        let p = P(r);
        let m = self.m;
        let mut el = div().flex_shrink_0().whitespace_nowrap();
        if let Some(c) = p.str("color") {
            el = el.text_color(m.color(&c));
        }
        if let Some(s) = p.f32("size") {
            el = el.text_size(px(s)).line_height(relative(1.25));
        }
        if let Some(f) = p.str("font") {
            el = el.font_family(SharedString::from(f));
        }
        el = match p.str("weight").as_deref() {
            Some("medium") => el.font_weight(FontWeight::MEDIUM),
            Some("semibold") => el.font_weight(FontWeight::SEMIBOLD),
            Some("bold") => el.font_weight(FontWeight::BOLD),
            _ => el,
        };
        if let Some(w) = p.f32("maxWidth") {
            el = el
                .max_w(px(w))
                .overflow_hidden()
                .text_ellipsis()
                .flex_shrink(1.0);
        }
        el.child(p.str("text").unwrap_or_default())
            .into_any_element()
    }

    fn image(&self, r: &Retained) -> AnyElement {
        let p = P(r);
        let size = px(p.f32("size").unwrap_or(16.0));
        let mut el = div().size(size).flex_shrink_0();
        if let Some(o) = p.f32("opacity") {
            el = el.opacity(o.clamp(0.0, 1.0));
        }
        let src = p.str("src").unwrap_or_default();
        match (self.m.images)(&src) {
            Some(path) => el.child(img(path).size(size)).into_any_element(),
            None => el.into_any_element(),
        }
    }

    fn ring(&self, id: NodeId, r: &Retained, window: &mut Window, cx: &mut App) -> AnyElement {
        let p = P(r);
        let size = p.f32("size").unwrap_or(32.0);
        let thickness = p.f32("thickness").unwrap_or(3.0);
        let value = p.f32("value").unwrap_or(0.0).clamp(0.0, 1.0);
        let color = self
            .m
            .color(&p.str("color").unwrap_or_else(|| "$cyan".into()));
        let track = self
            .m
            .color(&p.str("track").unwrap_or_else(|| "$track".into()));
        let paint = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let c = bounds.center();
                let radius = (size - thickness) / 2.0;
                let arc = |from: f32, to: f32| {
                    let steps = ((to - from).abs() / std::f32::consts::TAU * 64.0)
                        .ceil()
                        .max(2.0) as usize;
                    let mut b = PathBuilder::stroke(px(thickness));
                    for i in 0..=steps {
                        let a = from + (to - from) * i as f32 / steps as f32;
                        let pt = point(c.x + px(radius * a.cos()), c.y + px(radius * a.sin()));
                        if i == 0 { b.move_to(pt) } else { b.line_to(pt) }
                    }
                    b.build().ok()
                };
                let top = -std::f32::consts::FRAC_PI_2;
                if let Some(path) = arc(top, top + std::f32::consts::TAU) {
                    window.paint_path(path, track);
                }
                if value > 0.0
                    && let Some(path) = arc(top, top + std::f32::consts::TAU * value)
                {
                    window.paint_path(path, color);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        div()
            .id(eid(self.slot, id))
            .relative()
            .size(px(size))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(paint)
            .children(self.children(r, window, cx))
            .into_any_element()
    }

    fn input(&self, id: NodeId, r: &Retained, window: &mut Window, cx: &mut App) -> AnyElement {
        let p = P(r);
        let key = (self.slot, id);
        let state = self.m.inputs.get(&key);
        let focused = state.is_some_and(|s| s.focus.is_focused(window));
        let text = match state {
            Some(s) if focused => s.text.clone(),
            _ => p.str("value").unwrap_or_default(),
        };
        let size = p.f32("size").unwrap_or(13.0);
        let color = self
            .m
            .color(&p.str("color").unwrap_or_else(|| "$text".into()));
        let muted = self.m.token_or("textFaint", 0x707a8c);
        let caret = self.m.token_or("yellow", 0xffcc66);
        let (on_change, on_submit) = (p.handler("on_change"), p.handler("on_submit"));
        let model = self.model.clone();
        let ui = self.ui().clone();
        let initial = p.str("value").unwrap_or_default();
        let autofocus = p.bool("autofocus");
        // The focus handle lives in the model so it survives re-renders.
        let focus = match state {
            Some(s) => s.focus.clone(),
            None => {
                let f = cx.focus_handle();
                let f2 = f.clone();
                let init = initial.clone();
                window.defer(cx, move |window, cx| {
                    model.update(cx, |m, _| {
                        m.inputs.entry(key).or_insert(InputState {
                            text: init,
                            focus: f2.clone(),
                        });
                    });
                    if autofocus {
                        window.focus(&f2, cx);
                    }
                });
                f
            }
        };
        let model = self.model.clone();
        let shown: AnyElement = if text.is_empty() && !focused {
            div()
                .text_color(muted)
                .child(p.str("placeholder").unwrap_or_default())
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .text_color(color)
                .child(text.clone())
                .when(focused, |d| {
                    d.child(div().w(px(1.5)).h(px(size * 1.2)).bg(caret))
                })
                .into_any_element()
        };
        let f = focus.clone();
        div()
            .id(eid(self.slot, id))
            .track_focus(&focus)
            .flex_grow(1.0)
            .min_w(px(20.0))
            .text_size(px(size))
            .whitespace_nowrap()
            .overflow_hidden()
            .cursor_text()
            .on_click(move |_, window, cx| window.focus(&f, cx))
            .on_key_down(move |e: &KeyDownEvent, window, cx| {
                let ks = &e.keystroke;
                let mut submit = None;
                let mut changed = None;
                model.update(cx, |m, _| {
                    let Some(s) = m.inputs.get_mut(&key) else {
                        return;
                    };
                    match ks.key.as_str() {
                        "backspace" => {
                            s.text.pop();
                            changed = Some(s.text.clone());
                        }
                        "enter" => submit = Some(s.text.clone()),
                        "escape" => {}
                        _ if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt => {}
                        _ => {
                            if let Some(ch) = &ks.key_char {
                                s.text.push_str(ch);
                                changed = Some(s.text.clone());
                            }
                        }
                    }
                });
                if ks.key == "escape" {
                    window.blur(cx);
                }
                if let (Some(v), Some(h)) = (changed, &on_change) {
                    send(&ui, h, json!({ "value": v }));
                }
                if let (Some(v), Some(h)) = (submit, &on_submit) {
                    send(&ui, h, json!({ "value": v }));
                }
                cx.stop_propagation();
            })
            .child(shown)
            .into_any_element()
    }

    /// Core components, themed.
    fn core(&self, id: NodeId, r: &Retained, window: &mut Window, cx: &mut App) -> AnyElement {
        let p = P(r);
        let m = self.m;
        match r.kind.as_str() {
            "Row" | "Column" => {
                let base = div()
                    .flex()
                    .items_center()
                    .gap(px(p.f32("gap").unwrap_or(0.0)));
                let base = if r.kind == "Column" {
                    base.flex_col()
                } else {
                    base.flex_row()
                };
                base.children(self.children(r, window, cx))
                    .into_any_element()
            }
            "Text" => div()
                .text_color(m.tone(p.tone()))
                .child(p.str("text").unwrap_or_default())
                .into_any_element(),
            "Icon" => {
                let color = if r.props.contains_key("tone") {
                    m.tone(p.tone())
                } else {
                    m.tone(Tone::Accent)
                };
                div()
                    .text_color(color)
                    .child(icon(&p.str("name").unwrap_or_default()))
                    .into_any_element()
            }
            "Pill" | "Button" => {
                let active = p.bool("active");
                let el = div()
                    .id(eid(self.slot, id))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .rounded_full()
                    .when(r.kind == "Pill", |d| {
                        d.bg(if active {
                            m.token_or("cyan", 0x39bae6)
                        } else {
                            m.token_or("raised", 0x272d38)
                        })
                    })
                    .text_color(if active {
                        m.token_or("onAccent", 0x0b0e14)
                    } else {
                        m.tone(Tone::Normal)
                    })
                    .when_some(p.str("icon"), |d, i| d.child(icon(&i)))
                    .child(p.str("label").unwrap_or_default());
                self.events(el, r).into_any_element()
            }
            "Progress" => {
                let v = p.f32("value").unwrap_or(0.0).clamp(0.0, 1.0);
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when_some(p.str("label"), |d, l| d.child(l))
                    .child(
                        div()
                            .w(px(48.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(m.token_or("track", 0x272d38))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(m.tone(Tone::Accent))
                                    .w(px(48.0 * v)),
                            ),
                    )
                    .into_any_element()
            }
            "Spacer" => div().flex_1().into_any_element(),
            ERROR => div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(m.token_or("redTint", 0x3a2a2e))
                .text_color(m.tone(Tone::Critical))
                .font_weight(FontWeight::BOLD)
                .child(short_error(
                    &p.str("widget").unwrap_or_default(),
                    &p.str("message").unwrap_or_default(),
                ))
                .into_any_element(),
            other => div()
                .text_color(m.tone(Tone::Critical))
                .child(format!("⚠ {other}"))
                .into_any_element(),
        }
    }
}

struct Tooltip {
    text: SharedString,
    colors: (Hsla, Hsla, Hsla),
    font: Option<SharedString>,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (bg, border, fg) = self.colors;
        div()
            .when_some(self.font.clone(), |d, f| d.font_family(f))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(bg)
            .border_1()
            .border_color(border)
            .text_color(fg)
            .text_size(px(11.0))
            .child(self.text.clone())
    }
}

// ---------------------------------------------------------------- windows

pub(crate) struct SurfaceView {
    model: Entity<Model>,
    key: WindowKey,
    placement: Placement,
}

impl SurfaceView {
    fn new(
        model: Entity<Model>,
        key: WindowKey,
        placement: Placement,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Self {
            model,
            key,
            placement,
        }
    }
}

impl Render for SurfaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = Instant::now();
        let (key, placement) = (self.key, self.placement.clone());
        let model = self.model.clone();
        // Drawing inside `update` lends the model and the app context as
        // disjoint borrows: elements read the model while building with `cx`.
        self.model.update(cx, |m, cx| {
            draw_window(m, cx, window, &model, key, &placement, started)
        })
    }
}

fn draw_window(
    m: &mut Model,
    cx: &mut App,
    window: &mut Window,
    model: &Entity<Model>,
    key: WindowKey,
    placement: &Placement,
    started: Instant,
) -> AnyElement {
    let m: &Model = m;
    let base = div()
        .size_full()
        .relative()
        .when_some(m.font.clone(), |d, f| d.font_family(f))
        .text_color(m.token_or("text", 0xcccac2))
        .text_size(px(12.0));
    let slot = match key {
        WindowKey::Surface(s) | WindowKey::Popup(s, _) => s,
    };
    let Some(tree) = m.trees.tree(slot) else {
        return base.into_any_element();
    };
    let draw = Draw {
        m,
        tree,
        slot,
        model,
    };
    let el = match key {
        WindowKey::Surface(_) => {
            let Some(root) = tree.root() else {
                return base.into_any_element();
            };
            let content = draw.node(root, window, cx);
            if !placement.overlay {
                base.child(content).into_any_element()
            } else {
                // Outside click: the backdrop under the content dismisses.
                let pl = placement;
                let [mt, mr, mb, ml] = pl.margin;
                let dismiss = tree.get(root).and_then(|r| P(r).handler("on_dismiss"));
                let ui = m.slots[slot].clone();
                let mut frame = div().absolute().occlude();
                if pl.top {
                    frame = frame.top(px(mt));
                }
                if pl.bottom {
                    frame = frame.bottom(px(mb));
                }
                if pl.left {
                    frame = frame.left(px(ml));
                }
                if pl.right {
                    frame = frame.right(px(mr));
                }
                if !(pl.left && pl.right) && pl.width > 0.0 {
                    frame = frame.w(px(pl.width));
                }
                if !(pl.top && pl.bottom) && pl.height > 0.0 {
                    frame = frame.h(px(pl.height));
                }
                base.id("backdrop")
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        if let Some(h) = &dismiss {
                            send(&ui, h, Value::Null)
                        }
                    })
                    .child(frame.flex().child(content))
                    .into_any_element()
            }
        }
        WindowKey::Popup(_, popup) => {
            let Some(r) = tree.get(popup) else {
                return base.into_any_element();
            };
            let width = P(r).f32("width").unwrap_or(240.0);
            let viewport = window.viewport_size();
            let anchor = m.anchors.borrow().get(&(slot, popup)).copied();
            // The anchor's window is the slot's surface; find where that
            // window sits on the output from its placement.
            let host = m.root(slot).map(|(_, _, r)| Placement::of(r, m.dev));
            let (left, top, above) = match (anchor, host) {
                (Some(a), Some(h)) => {
                    let ox = if h.left {
                        h.margin[3]
                    } else if h.right {
                        f32::from(viewport.width) - h.width - h.margin[1]
                    } else {
                        0.0
                    };
                    let oy = if h.top {
                        h.margin[0]
                    } else if h.bottom {
                        f32::from(viewport.height) - h.height - h.margin[2]
                    } else {
                        0.0
                    };
                    let cx_ = ox + f32::from(a.center().x);
                    let x = (cx_ - width / 2.0)
                        .clamp(8.0, (f32::from(viewport.width) - width - 8.0).max(8.0));
                    // From a bar (a surface on one edge with a fixed height),
                    // open past the whole bar, not just past the item.
                    let bar = h.height > 0.0 && h.top != h.bottom;
                    if h.bottom && !h.top {
                        let edge = if bar { 0.0 } else { f32::from(a.top()) };
                        (x, oy + edge.min(f32::from(a.top())) - 6.0, true)
                    } else {
                        let edge = if bar { h.height } else { f32::from(a.bottom()) };
                        (x, oy + edge.max(f32::from(a.bottom())) + 6.0, false)
                    }
                }
                _ => (8.0, 8.0, false),
            };
            let dismiss = P(r).handler("on_dismiss");
            let ui = m.slots[slot].clone();
            let mut frame = div().absolute().left(px(left)).w(px(width)).occlude();
            frame = if above {
                frame.bottom(px(f32::from(viewport.height) - top))
            } else {
                frame.top(px(top))
            };
            base.id("backdrop")
                .on_mouse_down(MouseButton::Left, {
                    let (ui, dismiss) = (ui.clone(), dismiss.clone());
                    move |_, _, _| {
                        if let Some(h) = &dismiss {
                            send(&ui, h, Value::Null)
                        }
                    }
                })
                .on_mouse_down(MouseButton::Right, move |_, _, _| {
                    if let Some(h) = &dismiss {
                        send(&ui, h, Value::Null)
                    }
                })
                .child(
                    frame
                        .flex()
                        .flex_col()
                        .children(draw.children(r, window, cx)),
                )
                .into_any_element()
        }
    };
    if let Some(i) = &m.inspector {
        i.rendered(slot, matches!(key, WindowKey::Popup(..)), started.elapsed());
    }
    el
}

/// Opens, closes and reopens windows to match what the model wants.
struct Windows {
    open: HashMap<WindowKey, (Placement, WindowHandle<SurfaceView>)>,
    app_id: String,
}

impl Windows {
    fn sync(&mut self, model: &Entity<Model>, cx: &mut AsyncApp) {
        let (wanted, names) = model.read_with(cx, |m, _| {
            (
                m.wanted(),
                m.slots.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            )
        });
        let keys: HashSet<WindowKey> = wanted.iter().map(|(k, _)| *k).collect();
        let stale: Vec<WindowKey> = self
            .open
            .iter()
            .filter(|(k, (p, _))| {
                !keys.contains(k) || wanted.iter().any(|(wk, wp)| wk == *k && wp != p)
            })
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            if let Some((_, handle)) = self.open.remove(&k) {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        }
        for (key, placement) in wanted {
            if self.open.contains_key(&key) {
                continue;
            }
            let (slot, suffix) = match key {
                WindowKey::Surface(s) => (s, String::new()),
                WindowKey::Popup(s, n) => (s, format!("-popup{n}")),
            };
            let namespace = format!("{}-{}{suffix}", self.app_id, names[slot]);
            let options = placement.options(&self.app_id, namespace.clone());
            let (m, p) = (model.clone(), placement.clone());
            match cx.update(|cx| {
                cx.open_window(options, |_, cx| {
                    cx.new(|cx| SurfaceView::new(m, key, p, cx))
                })
            }) {
                Ok(handle) => {
                    log::info!("shell: opened {namespace}");
                    self.open.insert(key, (placement, handle));
                }
                Err(e) => log::error!("shell: cannot open {namespace} (layer-shell?): {e}"),
            }
        }
        model.read_with(cx, |m, _| {
            if let Some(i) = &m.inspector {
                i.set_windows(
                    self.open
                        .iter()
                        .map(|(k, (p, _))| {
                            let (slot, popup) = match k {
                                WindowKey::Surface(s) => (*s, None),
                                WindowKey::Popup(s, n) => (*s, Some(*n)),
                            };
                            WindowInfo {
                                slot,
                                popup,
                                layer: format!("{:?}", p.layer).to_lowercase(),
                                anchors: [p.top, p.right, p.bottom, p.left],
                                size: [p.width, p.height],
                                margin: p.margin,
                                overlay: p.overlay,
                                keyboard: p.keyboard,
                            }
                        })
                        .collect(),
                );
            }
        });
    }
}

impl Renderer for Shell {
    fn supports(&self, kind: &str) -> bool {
        CORE.contains(&kind) || STYLED.contains(&kind)
    }

    fn run(self, surface: Surface) -> anyhow::Result<()> {
        let Surface { slots, frames, .. } = surface;
        let (tx, rx) = unbounded();
        std::thread::Builder::new()
            .name("frame-bridge".into())
            .spawn(move || {
                while let Ok(f) = frames.recv() {
                    if tx.unbounded_send(f).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn frame bridge");
        let (scheme_tx, scheme_rx) = unbounded::<String>();
        if let Some(scheme) = self.scheme {
            std::thread::spawn(move || {
                while let Ok(s) = scheme.recv() {
                    if scheme_tx.unbounded_send(s).is_err() {
                        break;
                    }
                }
            });
        }
        let mut rx: Option<UnboundedReceiver<Frame>> = Some(rx);
        let mut scheme_rx = Some(scheme_rx);
        let Shell {
            app_id,
            font,
            theme,
            images,
            dev_placement,
            inspector,
            ..
        } = self;
        let mut init = Some((slots, theme, font, images, inspector));
        gpui_platform::application().run(move |cx: &mut App| {
            let (slots, theme, font, images, inspector) = init.take().expect("runs once");
            let n = slots.len();
            let model = cx.new(|_| Model {
                slots,
                trees: Trees::new(n),
                theme,
                light: false,
                font: font.map(SharedString::from),
                images,
                dev: dev_placement,
                anchors: Rc::default(),
                inputs: HashMap::new(),
                inspector,
                warned: RefCell::default(),
            });
            let mut frames = rx.take().expect("one run");
            let m = model.clone();
            let mut windows = Windows {
                open: HashMap::new(),
                app_id: app_id.clone(),
            };
            cx.spawn(async move |cx| {
                while let Some(first) = frames.next().await {
                    let mut batch = vec![first];
                    while let Ok(f) = frames.try_recv() {
                        batch.push(f);
                    }
                    m.update(cx, |m, cx| {
                        for f in &batch {
                            m.trees.apply(f);
                        }
                        cx.notify();
                    });
                    windows.sync(&m, cx);
                }
            })
            .detach();
            let mut schemes = scheme_rx.take().expect("one run");
            let m = model.clone();
            cx.spawn(async move |cx| {
                while let Some(s) = schemes.next().await {
                    m.update(cx, |m, cx| {
                        m.light = s == "light";
                        log::info!("shell: color scheme {s}");
                        cx.notify();
                    });
                }
            })
            .detach();
        });
        Ok(())
    }
}
