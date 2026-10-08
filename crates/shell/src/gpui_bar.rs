//! GPUI renderer: the bar as a Wayland layer-shell surface.
//!
//! Consumes exactly the same frames as the terminal renderer. Widgets never
//! see GPUI types; this file is the only place the vocabulary meets pixels.
//! Requires a compositor with `zwlr_layer_shell_v1` (niri, sway, river,
//! Hyprland, KWin, Wayfire, ...).

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::{
    AnyElement, App, Bounds, Context, ElementId, FontWeight, Hsla, IntoElement, ParentElement,
    Render, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Size, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*,
    px, rgb,
};
use runtime::Node;
use view::Tone;

use crate::flatten::{icon, short_error};
use crate::host::{Frame, UiEvent, WidgetHandle};

const HEIGHT: f32 = 32.0;

/// Renders since start; `MARATHON_FRAME_STATS=1` logs the rate every 5 s.
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
    widgets: Vec<Sender<UiEvent>>,
    tree: Vec<Option<Node>>,
}

impl Bar {
    fn new(
        widgets: Vec<WidgetHandle>,
        mut frames: UnboundedReceiver<Frame>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Event-driven: the UI thread sleeps until a widget emits a frame, then
        // takes everything queued and notifies once. GPUI coalesces notifies
        // into the next compositor frame, so 100 updates/s cost at most 60 renders.
        cx.spawn(async move |this, cx| {
            while let Some(first) = frames.next().await {
                let mut batch = vec![first];
                while let Ok(f) = frames.try_recv() {
                    batch.push(f);
                }
                let alive = this.update(cx, |bar, cx| {
                    for f in batch {
                        bar.tree[f.slot] = Some(f.node);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        let n = widgets.len();
        Self {
            widgets: widgets.into_iter().map(|w| w.ui).collect(),
            tree: vec![None; n],
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
            .child(
                div()
                    .px_2()
                    .rounded_md()
                    .bg(p.accent)
                    .text_color(p.bg)
                    .font_weight(FontWeight::BOLD)
                    .child("marathon"),
            );
        for (slot, node) in self.tree.iter().enumerate() {
            let el = match node {
                Some(n) => element(n, slot, &self.widgets[slot], &p),
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

fn id(slot: usize, handler: u64) -> ElementId {
    ElementId::Name(SharedString::from(format!("w{slot}-h{handler}")))
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

fn element(node: &Node, slot: usize, ui: &Sender<UiEvent>, p: &Palette) -> AnyElement {
    match node {
        Node::Row { gap, children } | Node::Column { gap, children } => {
            let base = div().flex().items_center().gap(px(*gap));
            let base = if matches!(node, Node::Column { .. }) {
                base.flex_col()
            } else {
                base.flex_row()
            };
            base.children(children.iter().map(|c| element(c, slot, ui, p)))
                .into_any_element()
        }
        Node::Text { text, tone: t } => div()
            .text_color(tone(*t, p))
            .child(text.clone())
            .into_any_element(),
        Node::Icon { name } => div()
            .text_color(p.accent)
            .child(icon(name))
            .into_any_element(),
        Node::Pill {
            icon: ic,
            label,
            active,
            on_click,
            on_scroll,
        } => {
            let (bg, fg) = if *active {
                (p.cyan, p.bg)
            } else {
                (p.pill, p.fg)
            };
            let handler = on_click.or(*on_scroll).unwrap_or(0);
            let mut pill = div()
                .id(id(slot, handler))
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py(px(2.0))
                .rounded_full()
                .bg(bg)
                .text_color(fg)
                .when_some(ic.as_ref(), |d, i| d.child(icon(i)))
                .child(label.clone());
            if let Some(h) = *on_click {
                let tx = ui.clone();
                pill = pill
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .on_click(move |_, _, _| {
                        let _ = tx.send(UiEvent::Click(h));
                    });
            }
            if let Some(h) = *on_scroll {
                let tx = ui.clone();
                pill = pill.on_scroll_wheel(move |e, _, _| {
                    let d = scroll_delta(e);
                    if d != 0 {
                        let _ = tx.send(UiEvent::Scroll(h, d));
                    }
                });
            }
            pill.into_any_element()
        }
        Node::Button { label, on_click } => {
            let mut b = div()
                .id(id(slot, on_click.unwrap_or(0)))
                .px_2()
                .text_color(p.accent)
                .child(label.clone());
            if let Some(h) = *on_click {
                let tx = ui.clone();
                b = b.cursor_pointer().on_click(move |_, _, _| {
                    let _ = tx.send(UiEvent::Click(h));
                });
            }
            b.into_any_element()
        }
        Node::Progress { value, label } => {
            let v = value.clamp(0.0, 1.0);
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
                .when_some(label.as_ref(), |d, l| d.child(l.clone()))
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
        Node::Spacer => div().flex_1().into_any_element(),
        Node::Error { widget, message } => div()
            .text_color(p.crit)
            .font_weight(FontWeight::BOLD)
            .child(short_error(widget, message))
            .into_any_element(),
    }
}

pub fn run(widgets: Vec<WidgetHandle>, frames: Receiver<Frame>) -> anyhow::Result<()> {
    if std::env::var_os("MARATHON_FRAME_STATS").is_some() {
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
    // Bridge the widget threads' channel into an async stream the UI can await.
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
    gpui_platform::application().run(move |cx: &mut App| {
        let options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.0), px(0.0)),
                // Width 0 + LEFT|RIGHT anchors = stretch across the output.
                size: Size::new(px(0.0), px(HEIGHT)),
            })),
            app_id: Some("marathon".into()),
            window_background: WindowBackgroundAppearance::Opaque,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "marathon-bar".into(),
                layer: Layer::Top,
                anchor: Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(HEIGHT)),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        };
        if let Err(e) = cx.open_window(options, |_, cx| {
            cx.new(|cx| Bar::new(widgets, rx.take().expect("one window"), cx))
        }) {
            log::error!("could not open the bar (does the compositor support layer-shell?): {e}");
            cx.quit();
        }
    });
    Ok(())
}
