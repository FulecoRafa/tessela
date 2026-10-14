//! GPUI renderer.
//!
//! Consumes exactly the same frames as every other renderer; extensions
//! never see GPUI types, and this crate is the only place the vocabulary
//! meets pixels. Phase 0 ships one surface kind, a Wayland layer-shell bar
//! ([`LayerBar`]); it needs a compositor with `zwlr_layer_shell_v1` (niri,
//! sway, river, Hyprland, KWin, Wayfire, ...).

mod drag;
mod inspect;
mod motion;
mod shell;
mod svg;

pub use inspect::{InspectCommand, InspectControl, Inspector, RenderProbe, WindowInfo};
pub use shell::{ImageResolver, Shell, Theme};

use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::{
    AnyElement, App, Bounds, Context, Div, ElementId, FontWeight, Hsla, IntoElement, MouseButton,
    ParentElement, Render, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Size, Stateful,
    Styled, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div,
    point, prelude::*, px, rgb,
};
use serde_json::Value;
use tessela_host::{Frame, Renderer, Slot, Surface, Trees, UiEvent};
use tessela_view::patch::Retained;
use tessela_view::text::{icon, short_error};
use tessela_view::{ERROR, Handler, NodeId, Tone, ViewTree};

/// Renders since start; `TESSELA_FRAME_STATS=1` logs the rate every 5 s.
static RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn color(hex: u32) -> Hsla {
    Rgba::from(rgb(hex)).into()
}

struct Palette {
    bg: Hsla,
    pill: Hsla,
    fg: Hsla,
    muted: Hsla,
    accent: Hsla,
    cyan: Hsla,
    warn: Hsla,
    crit: Hsla,
}

fn palette() -> Palette {
    Palette {
        bg: color(0x0b0e14),
        pill: color(0x272d38),
        fg: color(0xbfbdb6),
        muted: color(0x565b66),
        accent: color(0xe6b450),
        cyan: color(0x39bae6),
        warn: color(0xffb454),
        crit: color(0xd95757),
    }
}

struct Bar {
    title: Option<String>,
    slots: Vec<Slot>,
    trees: Trees,
}

impl Bar {
    fn new(
        title: Option<String>,
        slots: Vec<Slot>,
        mut frames: UnboundedReceiver<Frame>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Event-driven: the UI thread sleeps until an extension emits a frame,
        // then takes everything queued and notifies once. GPUI coalesces
        // notifies into the next compositor frame.
        cx.spawn(async move |this, cx| {
            while let Some(first) = frames.next().await {
                let mut batch = vec![first];
                while let Ok(f) = frames.try_recv() {
                    batch.push(f);
                }
                let alive = this.update(cx, |bar, cx| {
                    for f in &batch {
                        bar.trees.apply(f);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        let n = slots.len();
        Self {
            title,
            slots,
            trees: Trees::new(n),
        }
    }
}

impl Render for Bar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = palette();
        let mut bar = div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .bg(p.bg)
            .text_color(p.fg)
            .text_sm()
            .when_some(self.title.clone(), |bar, title| {
                bar.child(
                    div()
                        .px_2()
                        .rounded_md()
                        .bg(p.accent)
                        .text_color(p.bg)
                        .font_weight(FontWeight::BOLD)
                        .child(title),
                )
            });
        for (slot, ui) in self.slots.iter().enumerate() {
            let tree = self.trees.tree(slot);
            let el = match tree.and_then(|t| t.root().map(|r| (t, r))) {
                Some((t, root)) => element(t, root, slot, ui, &p),
                None => div().text_color(p.muted).child("…").into_any_element(),
            };
            bar = bar.child(el);
        }
        bar
    }
}

fn tone(t: Tone, p: &Palette) -> Hsla {
    match t {
        Tone::Normal => p.fg,
        Tone::Muted => p.muted,
        Tone::Accent => p.accent,
        Tone::Warning => p.warn,
        Tone::Critical => p.crit,
    }
}

/// Element ids come from node ids, which the reconciler keeps stable, so
/// hover and press state survive re-renders.
fn id(slot: usize, node: NodeId) -> ElementId {
    ElementId::Name(SharedString::from(format!("s{slot}-n{node}")))
}

fn scroll_delta(e: &ScrollWheelEvent) -> i32 {
    let y = match e.delta {
        ScrollDelta::Lines(l) => l.y,
        ScrollDelta::Pixels(px) => f32::from(px.y),
    };
    if y > 0.0 {
        1
    } else if y < 0.0 {
        -1
    } else {
        0
    }
}

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
    fn tone(&self) -> Tone {
        self.str("tone")
            .and_then(|t| Tone::parse(&t))
            .unwrap_or_default()
    }
    fn handler(&self, k: &str) -> Option<Handler> {
        self.0.props.get(k).and_then(Handler::from_value)
    }
}

/// Wire the event props of `n` onto an interactive element.
fn interactive(mut el: Stateful<Div>, n: &P<'_>, ui: &Slot) -> Stateful<Div> {
    if let Some(h) = n.handler("on_click") {
        let ui = ui.clone();
        el = el
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .on_click(move |_, _, _| ui.send(UiEvent::new(h.clone(), Value::Null)));
    }
    if let Some(h) = n.handler("on_press") {
        let ui = ui.clone();
        el = el.on_mouse_down(MouseButton::Left, move |_, _, _| {
            ui.send(UiEvent::new(h.clone(), Value::Null))
        });
    }
    if let Some(h) = n.handler("on_release") {
        let ui = ui.clone();
        el = el.on_mouse_up(MouseButton::Left, move |_, _, _| {
            ui.send(UiEvent::new(h.clone(), Value::Null))
        });
    }
    if let Some(h) = n.handler("on_scroll") {
        let ui = ui.clone();
        el = el.on_scroll_wheel(move |e, _, _| {
            let d = scroll_delta(e);
            if d != 0 {
                ui.send(UiEvent::new(h.clone(), serde_json::json!({ "delta": d })));
            }
        });
    }
    el
}

fn element(tree: &ViewTree, nid: NodeId, slot: usize, ui: &Slot, p: &Palette) -> AnyElement {
    let Some(r) = tree.get(nid) else {
        return div().into_any_element();
    };
    let n = P(r);
    match r.kind.as_str() {
        "Row" | "Column" => {
            let base = div()
                .flex()
                .items_center()
                .gap(px(n.f32("gap").unwrap_or(0.0)));
            let base = if r.kind == "Column" {
                base.flex_col()
            } else {
                base.flex_row()
            };
            base.children(r.children.iter().map(|c| element(tree, *c, slot, ui, p)))
                .into_any_element()
        }
        "Text" => div()
            .text_color(tone(n.tone(), p))
            .child(n.str("text").unwrap_or_default())
            .into_any_element(),
        "Icon" => {
            let color = if r.props.contains_key("tone") {
                tone(n.tone(), p)
            } else {
                p.accent
            };
            div()
                .text_color(color)
                .child(icon(&n.str("name").unwrap_or_default()))
                .into_any_element()
        }
        "Pill" => {
            let (bg, fg) = if n.bool("active") {
                (p.cyan, p.bg)
            } else {
                (p.pill, p.fg)
            };
            let pill = div()
                .id(id(slot, nid))
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py(px(2.0))
                .rounded_full()
                .bg(bg)
                .text_color(fg)
                .when_some(n.str("icon"), |d, i| d.child(icon(&i)))
                .child(n.str("label").unwrap_or_default());
            interactive(pill, &n, ui).into_any_element()
        }
        "Button" => {
            let b = div()
                .id(id(slot, nid))
                .px_2()
                .text_color(p.accent)
                .child(n.str("label").unwrap_or_default());
            interactive(b, &n, ui).into_any_element()
        }
        "Progress" => {
            let v = n.f32("value").unwrap_or(0.0).clamp(0.0, 1.0);
            let fill = if v > 0.85 {
                p.crit
            } else if v > 0.6 {
                p.warn
            } else {
                p.cyan
            };
            div()
                .flex()
                .items_center()
                .gap_1()
                .when_some(n.str("label"), |d, l| d.child(l))
                .child(
                    div()
                        .w(px(48.0))
                        .h(px(6.0))
                        .rounded_full()
                        .bg(p.pill)
                        .child(div().h_full().rounded_full().bg(fill).w(px(48.0 * v))),
                )
                .into_any_element()
        }
        "Spacer" => div().flex_1().into_any_element(),
        ERROR => div()
            .text_color(p.crit)
            .font_weight(FontWeight::BOLD)
            .child(short_error(
                &n.str("widget").unwrap_or_default(),
                &n.str("message").unwrap_or_default(),
            ))
            .into_any_element(),
        other => div()
            .text_color(p.crit)
            .child(format!("⚠ {other}"))
            .into_any_element(),
    }
}

/// A bar anchored to the top edge as a layer-shell surface, reserving its
/// height (exclusive zone) so windows do not cover it.
pub struct LayerBar {
    pub app_id: String,
    pub namespace: String,
    pub height: f32,
    /// Badge drawn before the first slot.
    pub title: Option<String>,
}

impl LayerBar {
    pub fn new(app_id: impl Into<String>) -> Self {
        let app_id = app_id.into();
        Self {
            namespace: format!("{app_id}-bar"),
            app_id,
            height: 32.0,
            title: None,
        }
    }

    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }

    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
}

impl Renderer for LayerBar {
    fn run(self, surface: Surface) -> anyhow::Result<()> {
        if std::env::var_os("TESSELA_FRAME_STATS").is_some() {
            std::thread::spawn(|| {
                let mut last = 0;
                loop {
                    std::thread::sleep(Duration::from_secs(5));
                    let now = RENDERS.load(std::sync::atomic::Ordering::Relaxed);
                    log::info!("renders/s: {:.1}", (now - last) as f64 / 5.0);
                    last = now;
                }
            });
        }
        let Surface { slots, frames, .. } = surface;
        // Bridge the extension threads' channel into an async stream the UI can await.
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
        let mut rx = Some(rx);
        // `TESSELA_DEV_PLACEMENT=1`: draw at the bottom edge without reserving
        // space, so it can run next to the desktop's own bar without moving
        // windows or that bar.
        let dev = std::env::var_os("TESSELA_DEV_PLACEMENT").is_some();
        let LayerBar {
            app_id,
            namespace,
            height,
            title,
        } = self;
        gpui_platform::application().run(move |cx: &mut App| {
            let options = WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    // Width 0 + LEFT|RIGHT anchors = stretch across the output.
                    size: Size::new(px(0.0), px(height)),
                })),
                app_id: Some(app_id.clone()),
                window_background: WindowBackgroundAppearance::Opaque,
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: namespace.clone(),
                    layer: Layer::Top,
                    anchor: if dev { Anchor::BOTTOM } else { Anchor::TOP }
                        | Anchor::LEFT
                        | Anchor::RIGHT,
                    exclusive_zone: (!dev).then_some(px(height)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let (title, slots) = (title.clone(), slots.clone());
            if let Err(e) = cx.open_window(options, |_, cx| {
                cx.new(|cx| Bar::new(title, slots, rx.take().expect("one window"), cx))
            }) {
                log::error!(
                    "could not open the bar (does the compositor support layer-shell?): {e}"
                );
                cx.quit();
            }
        });
        Ok(())
    }
}
