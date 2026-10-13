//! `tessela devtools ui`: the devtools in a window of their own, in the
//! spirit of a browser's (docs/adr/0019).
//!
//! A plain GPUI app (an xdg toplevel, not a layer surface) speaking the
//! same socket protocol as the CLI ([`link`]). Panels:
//!
//! | tab | shows | acts |
//! |---|---|---|
//! | Elements | live tree per surface; props, handlers, path and drawn bounds of the selection | outline in the shell on hover/selection, pick mode, invoke handlers |
//! | Console | logs by level, source and surface | filter, clear |
//! | Network | module calls, deliveries and events: size, time, payload | filter, inspect |
//! | Performance | render/draw times per surface, a frame timeline, what each frame changed | pause |
//! | Application | shared values, desktop topics, windows, plugins | edit values, capture surfaces |

mod application;
mod console;
mod elements;
mod link;
mod network;
mod perf;
mod widgets;

use std::collections::{BTreeMap, VecDeque};

use crossbeam_channel::Sender;
use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    MouseMoveEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use serde_json::{Value, json};

use link::{Cmd, Msg};
use widgets::{Fonts, TextField, theme};

/// Records kept per list; older ones fall off.
const KEEP: usize = 5000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Elements,
    Console,
    Network,
    Performance,
    Application,
}

const TABS: [(Tab, &str); 5] = [
    (Tab::Elements, "Elements"),
    (Tab::Console, "Console"),
    (Tab::Network, "Network"),
    (Tab::Performance, "Performance"),
    (Tab::Application, "Application"),
];

/// A divider being dragged.
#[derive(Clone, Copy, PartialEq)]
enum Split {
    Elements,
    Network,
}

/// A record with the order it arrived in, for stable selection.
#[derive(Clone)]
struct Rec {
    seq: u64,
    channel: &'static str,
    v: Value,
}

pub struct Devtools {
    fonts: Fonts,
    link: Sender<Cmd>,
    tab: Tab,
    /// Project name while connected; why not, otherwise.
    connected: Option<String>,
    offline: String,
    /// Last action result: (text, is error).
    status: Option<(String, bool)>,
    overview: Value,
    state: Value,
    trees: BTreeMap<String, Value>,
    seq: u64,
    console: VecDeque<Rec>,
    network: VecDeque<Rec>,
    frames: VecDeque<Rec>,
    el: elements::Elements,
    con: console::Console,
    net: network::Network,
    perf: perf::Perf,
    app: application::Application,
    drag: Option<Split>,
    /// What the shell currently outlines, to send only changes.
    outlined: Option<(String, u64)>,
    /// Took over the project's highlight since connecting.
    adopted: bool,
}

impl Devtools {
    fn new(
        project: Option<String>,
        fonts: Fonts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (link, mut rx) = link::start(project);
        cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(m) = rx.try_recv() {
                    batch.push(m);
                }
                let alive = this.update(cx, |this, cx| {
                    for m in batch {
                        this.receive(m, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            el: elements::Elements::new(&fonts, cx),
            con: console::Console::new(&fonts, cx),
            net: network::Network::new(&fonts, cx),
            perf: perf::Perf::default(),
            app: application::Application::new(&fonts, cx),
            fonts,
            link,
            tab: Tab::Elements,
            connected: None,
            offline: "connecting…".into(),
            status: None,
            overview: Value::Null,
            state: Value::Null,
            trees: BTreeMap::new(),
            seq: 0,
            console: VecDeque::new(),
            network: VecDeque::new(),
            frames: VecDeque::new(),
            drag: None,
            outlined: None,
            adopted: false,
        }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.link.send(cmd);
    }

    /// A request whose outcome goes to the status line.
    fn act(&self, what: &str, req: Value) {
        self.send(Cmd::Ask(what.to_string(), req));
    }

    fn push(&mut self, channel: &'static str, v: Value) {
        self.seq += 1;
        let rec = Rec {
            seq: self.seq,
            channel,
            v,
        };
        let list = match channel {
            "console" => &mut self.console,
            "frames" => &mut self.frames,
            _ => &mut self.network,
        };
        list.push_back(rec);
        while list.len() > KEEP {
            list.pop_front();
        }
    }

    fn receive(&mut self, m: Msg, cx: &mut Context<Self>) {
        match m {
            Msg::Connected(name) => {
                self.connected = Some(name);
                self.console.clear();
                self.network.clear();
                self.frames.clear();
                self.trees.clear();
                self.adopted = false;
                self.outlined = None;
                self.el.picking = false;
            }
            Msg::Disconnected(why) => {
                self.connected = None;
                self.offline = why;
                self.el.picking = false;
            }
            Msg::Overview(v) => {
                // First news from a project: adopt what it already outlines.
                if !self.adopted {
                    self.adopted = true;
                    if let (Some(s), Some(id)) = (
                        v["highlight"]["surface"].as_str(),
                        v["highlight"]["id"].as_u64(),
                    ) {
                        self.outlined = Some((s.to_string(), id));
                        self.overview = v.clone();
                        self.select(Some((s.to_string(), id)));
                    }
                }
                self.overview = v;
            }
            Msg::State(v) => self.state = v,
            Msg::Tree(surface, v) => {
                self.el.saw_tree(&surface, &v);
                self.trees.insert(surface, v);
                if self.tab == Tab::Elements && !self.el.picking {
                    self.sync_outline();
                }
            }
            Msg::Node(v) => {
                // Bounds only come while a node is outlined: remember them.
                if let (Some(s), Some(id)) = (v["surface"].as_str(), v["id"].as_u64())
                    && v["bounds"].is_array()
                {
                    self.el
                        .bounds
                        .insert((s.to_string(), id), v["bounds"].clone());
                }
            }
            Msg::History(channel, records) => {
                let Some(c) = channel_name(&channel) else {
                    return;
                };
                for r in records {
                    if c != "picks" {
                        self.push(c, r);
                    }
                }
                // Channels arrive one after the other; the network list mixes three.
                self.network.make_contiguous().sort_by(|a, b| {
                    let t = |r: &Rec| r.v["t"].as_f64().unwrap_or(0.0);
                    t(a).total_cmp(&t(b))
                });
            }
            Msg::Record(channel, r) => match channel_name(&channel) {
                Some("picks") => self.picked(&r, cx),
                Some("frames") => {
                    if r["patches"].as_u64().unwrap_or(0) > 0
                        && let Some(s) = r["surface"].as_str()
                    {
                        self.send(Cmd::Tree(s.to_string()));
                    }
                    if !self.perf.paused {
                        self.push("frames", r);
                    }
                }
                Some(c) => self.push(c, r),
                None => {}
            },
            // Outlines and pick mode show in the shell itself.
            Msg::Done(what, Ok(_)) if what == "highlight" || what == "pick" => {}
            Msg::Done(what, r) => {
                self.status = Some(match r {
                    Ok(v) => (
                        format!("{what}: {}", widgets::ellipsize(&v.to_string(), 120)),
                        false,
                    ),
                    Err(e) => (format!("{what}: {e}"), true),
                });
                if what == "pick" && self.status.as_ref().is_some_and(|s| s.1) {
                    self.el.picking = false;
                }
            }
            Msg::Shot(surface, r) => {
                self.status = Some(match &r {
                    Ok(path) => (format!("captured {surface}: {path}"), false),
                    Err(e) => (format!("capture {surface}: {e}"), true),
                });
                if let Ok(path) = r {
                    self.app.shot = Some((surface, path));
                }
            }
        }
    }

    /// Names of open surfaces, in project order.
    fn surfaces(&self) -> Vec<String> {
        self.overview["surfaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["name"].as_str().map(String::from))
            .collect()
    }

    /// Outline `target` in the shell, if it changed.
    fn outline(&mut self, target: Option<(String, u64)>) {
        if self.outlined == target {
            return;
        }
        let req = match &target {
            Some((s, id)) => json!({ "op": "highlight", "surface": s, "id": id }),
            None => json!({ "op": "highlight" }),
        };
        self.outlined = target;
        self.act("highlight", req);
    }

    fn top_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let tabs = TABS.iter().map(|(tab, label)| {
            let on = self.tab == *tab;
            let tab = *tab;
            div()
                .id(SharedString::from(format!("tab-{label}")))
                .px_3()
                .h_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .text_color(if on { t.text } else { t.muted })
                .border_b_2()
                .border_color(if on {
                    t.accent
                } else {
                    gpui::transparent_black()
                })
                .hover(|s| s.bg(t.hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = tab;
                    if tab != Tab::Elements {
                        this.el.hovered = None;
                        this.outline(None);
                    }
                    cx.notify();
                }))
                .child(*label)
        });
        let (dot, who) = match &self.connected {
            Some(p) => (
                t.ok,
                format!(
                    "{p} · up {:.0}s",
                    self.overview["uptime"].as_f64().unwrap_or(0.0)
                ),
            ),
            None => (t.error, self.offline.clone()),
        };
        div()
            .flex()
            .items_center()
            .h(px(34.0))
            .bg(t.panel)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .px_3()
                    .text_color(t.accent)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("tessela"),
            )
            .children(tabs)
            .child(div().flex_1())
            .when_some(self.status.clone(), |d, (s, err)| {
                d.child(
                    div()
                        .id("status")
                        .max_w(px(520.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .px_2()
                        .text_size(px(11.0))
                        .font_family(self.fonts.mono.clone())
                        .text_color(if err { t.error } else { t.muted })
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.status = None;
                            cx.notify();
                        }))
                        .child(s),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .text_color(t.muted)
                    .child(div().size(px(8.0)).rounded_full().bg(dot))
                    .child(who),
            )
    }

    fn drag_to(&mut self, e: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(split) = self.drag else { return };
        if e.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let size = window.viewport_size();
        match split {
            Split::Elements => {
                let w = f32::from(size.width);
                self.el.split = (f32::from(e.position.x) / w).clamp(0.2, 0.85);
            }
            Split::Network => {
                let w = f32::from(size.width);
                self.net.split = (f32::from(e.position.x) / w).clamp(0.25, 0.85);
            }
        }
        cx.notify();
    }
}

/// The `'static` name of a known channel.
fn channel_name(c: &str) -> Option<&'static str> {
    crate::devtools::CHANNELS.iter().find(|n| **n == c).copied()
}

/// A vertical divider that resizes the panes beside it.
fn divider(split: Split, cx: &mut Context<Devtools>) -> impl IntoElement {
    let t = theme();
    div()
        .id(SharedString::from(format!("divider-{}", split as u8)))
        .w(px(5.0))
        .h_full()
        .flex_shrink_0()
        .cursor_col_resize()
        .bg(t.bg)
        .border_l_1()
        .border_color(t.border)
        .hover(|s| s.bg(t.accent.opacity(0.4)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.drag = Some(split);
                cx.stop_propagation();
            }),
        )
}

impl Render for Devtools {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let body = match self.tab {
            Tab::Elements => self.elements(window, cx).into_any_element(),
            Tab::Console => self.console_tab(cx).into_any_element(),
            Tab::Network => self.network_tab(cx).into_any_element(),
            Tab::Performance => self.perf_tab(cx).into_any_element(),
            Tab::Application => self.application_tab(cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.text)
            .text_size(px(12.0))
            .font_family(self.fonts.ui.clone())
            .on_mouse_move(cx.listener(Self::drag_to))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag = None),
            )
            .child(self.top_bar(cx))
            .child(div().flex_1().min_h(px(0.0)).flex().child(body))
    }
}

pub fn run(project: Option<String>) -> anyhow::Result<()> {
    let p = project.clone();
    gpui_platform::application().run(move |cx: &mut App| {
        let fonts = Fonts::pick(cx);
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let title = match &p {
            Some(p) => format!("tessela devtools — {p}"),
            None => "tessela devtools".into(),
        };
        let p = p.clone();
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                app_id: Some("tessela-devtools".into()),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| Devtools::new(p, fonts, window, cx)),
        );
        if let Err(e) = opened {
            log::error!("devtools: cannot open a window: {e}");
            cx.quit();
        }
        cx.activate(true);
    });
    // Leave the shell as it was: no outline, no pick mode.
    if let Ok(sock) = crate::devtools::socket(project.as_deref()) {
        let _ = crate::devtools::ask(&sock, json!({ "op": "highlight" }));
        let _ = crate::devtools::ask(&sock, json!({ "op": "pick", "on": false }));
    }
    Ok(())
}

/// Entity helpers shared by the panels.
fn field(placeholder: &str, fonts: &Fonts, cx: &mut Context<Devtools>) -> Entity<TextField> {
    let fonts = fonts.clone();
    let placeholder = placeholder.to_string();
    cx.new(|cx| TextField::new(&placeholder, &fonts, cx))
}
