//! Network: everything that crosses the sandbox. Module calls (guest →
//! module), deliveries (module → guest: signal values, callbacks, finished
//! resources) and events (user → guest), with size and time.

use std::collections::HashSet;

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, UniformListScrollHandle, div, prelude::*, px, uniform_list,
};
use serde_json::Value;

use super::widgets::{FieldEvent, Fonts, TextField, button, chip, json_lines, section, theme};
use super::{Devtools, Rec, Split, divider, field};

const KINDS: [&str; 5] = ["call", "signal", "callback", "finished", "event"];

pub(super) struct Network {
    filter: Entity<TextField>,
    hidden: HashSet<&'static str>,
    selected: Option<u64>,
    scroll: UniformListScrollHandle,
    cleared: u64,
    shown: Vec<usize>,
    pub split: f32,
}

impl Network {
    pub fn new(fonts: &Fonts, cx: &mut Context<Devtools>) -> Self {
        let filter = field("Filter by name, surface or payload", fonts, cx);
        cx.subscribe(&filter, |_: &mut Devtools, _, _: &FieldEvent, cx| {
            cx.notify()
        })
        .detach();
        Self {
            filter,
            hidden: HashSet::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            cleared: 0,
            shown: vec![],
            split: 0.6,
        }
    }
}

/// One row: (kind, name, bytes, ms, error).
fn summarize(r: &Rec) -> (&'static str, String, usize, f64, Option<String>) {
    let v = &r.v;
    let ms = v["ms"].as_f64().unwrap_or(0.0);
    let err = |e: &Value| e.as_str().map(String::from);
    match r.channel {
        "calls" => (
            "call",
            // `service.subscribe audio/state`: what was asked, in short.
            match &v["args"] {
                Value::Object(a) if !a.is_empty() => {
                    let text = |x: &Value| {
                        x.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| x.to_string())
                    };
                    // Service paths read as `service/path`; other args in key order.
                    let what = match (a.get("service"), a.get("path")) {
                        (Some(s), Some(p)) => format!("{}/{}", text(s), text(p)),
                        _ => a.values().map(text).collect::<Vec<_>>().join(" "),
                    };
                    format!("{} {what}", v["call"].as_str().unwrap_or(""))
                }
                _ => v["call"].as_str().unwrap_or("").to_string(),
            },
            v["args"].to_string().len() + v["result"].to_string().len(),
            ms,
            (v["ok"] == false).then(|| v["result"].as_str().unwrap_or("error").to_string()),
        ),
        "events" => (
            "event",
            v["handler"].as_str().unwrap_or("").to_string(),
            if v["payload"].is_null() {
                0
            } else {
                v["payload"].to_string().len()
            },
            ms,
            err(&v["error"]),
        ),
        _ => {
            let d = &v["detail"];
            let (kind, name, bytes) = match v["kind"].as_str().unwrap_or("") {
                "signal" => (
                    "signal",
                    d["from"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| format!("signal #{}", d["sig"])),
                    d["bytes"].as_u64().unwrap_or(0) as usize,
                ),
                "callback" => (
                    "callback",
                    format!("fn#{}", d["fn"]),
                    d["value"].to_string().len(),
                ),
                _ => (
                    "finished",
                    format!(
                        "{} {}",
                        d["module"].as_str().unwrap_or(""),
                        d["key"].as_str().unwrap_or("")
                    ),
                    0,
                ),
            };
            (kind, name, bytes, ms, err(&v["error"]))
        }
    }
}

fn size(bytes: usize) -> String {
    if bytes >= 1024 {
        format!("{:.1} kB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

impl Devtools {
    pub(super) fn network_tab(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let q = self.net.filter.read(cx).text.to_lowercase();
        let follow = self.net.scroll.is_scrolled_to_end().unwrap_or(true);
        self.net.shown = self
            .network
            .iter()
            .enumerate()
            .filter(|(_, r)| r.seq > self.net.cleared)
            .filter(|(_, r)| !self.net.hidden.contains(summarize(r).0))
            .filter(|(_, r)| q.is_empty() || r.v.to_string().to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        if follow && self.net.selected.is_none() && !self.net.shown.is_empty() {
            self.net.scroll.scroll_to_bottom();
        }
        let max_ms = self
            .net
            .shown
            .iter()
            .filter_map(|i| self.network.get(*i))
            .map(|r| r.v["ms"].as_f64().unwrap_or(0.0))
            .fold(1.0_f64, f64::max);
        let kinds = KINDS.iter().map(|k| {
            let k2: &'static str = k;
            chip(
                SharedString::from(format!("kind-{k}")),
                *k,
                !self.net.hidden.contains(k),
                None,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.net.hidden.remove(k2) {
                    this.net.hidden.insert(k2);
                }
                cx.notify();
            }))
        });
        let total: usize = self
            .net
            .shown
            .iter()
            .filter_map(|i| self.network.get(*i))
            .map(|r| summarize(r).2)
            .sum();
        let toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(34.0))
            .border_b_1()
            .border_color(t.border)
            .child(
                button("net-clear", "⊘ Clear").on_click(cx.listener(|this, _, _, cx| {
                    this.net.cleared = this.seq;
                    this.net.selected = None;
                    cx.notify();
                })),
            )
            .child(div().w(px(240.0)).child(self.net.filter.clone()))
            .children(kinds)
            .child(div().flex_1())
            .child(div().text_color(t.muted).child(format!(
                "{} requests · {}",
                self.net.shown.len(),
                size(total)
            )));
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(22.0))
            .px_2()
            .bg(t.panel)
            .border_b_1()
            .border_color(t.border)
            .text_color(t.muted)
            .text_size(px(11.0))
            .child(div().w(px(64.0)).child("Time"))
            .child(div().w(px(70.0)).child("Surface"))
            .child(div().w(px(64.0)).child("Type"))
            .child(div().flex_1().child("Name"))
            .child(div().w(px(64.0)).child("Size"))
            .child(div().w(px(60.0)).child("ms"))
            .child(div().w(px(90.0)).child("Duration"));
        let n = self.net.shown.len();
        let selected = self.net.selected;
        let list = uniform_list(
            "network",
            n,
            cx.processor(
                move |this: &mut Devtools, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|i| this.net.shown.get(i).and_then(|j| this.network.get(*j)))
                        .cloned()
                        .collect::<Vec<_>>()
                        .iter()
                        .map(|r| net_row(r, selected == Some(r.seq), max_ms, cx))
                        .collect::<Vec<_>>()
                },
            ),
        )
        .track_scroll(&self.net.scroll)
        .font_family(self.fonts.mono.clone())
        .size_full();
        let detail = self.net_detail(cx);
        div()
            .flex()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .w(gpui::relative(self.net.split))
                    .child(toolbar)
                    .child(header)
                    .child(div().flex_1().min_h(px(0.0)).child(list)),
            )
            .child(divider(Split::Network, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .bg(t.panel)
                    .child(detail),
            )
    }

    fn net_detail(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let Some(r) = self
            .net
            .selected
            .and_then(|s| self.network.iter().find(|r| r.seq == s))
            .cloned()
        else {
            return div()
                .p_4()
                .text_color(t.muted)
                .child("Select a row to see its payload.")
                .into_any_element();
        };
        let (kind, name, bytes, ms, error) = summarize(&r);
        let v = &r.v;
        let parts: Vec<(&str, Value)> = match r.channel {
            "calls" => vec![
                ("Arguments", v["args"].clone()),
                ("Result", v["result"].clone()),
            ],
            "events" => vec![("Payload", v["payload"].clone())],
            _ => vec![("Detail", v["detail"].clone())],
        };
        div()
            .id("net-detail")
            .size_full()
            .overflow_y_scroll()
            .font_family(self.fonts.mono.clone())
            .child(section("General"))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(kv("Name", &name))
                    .child(kv("Type", kind))
                    .child(kv("Surface", v["surface"].as_str().unwrap_or("")))
                    .child(kv(
                        "At",
                        &format!("{:.3} s", v["t"].as_f64().unwrap_or(0.0)),
                    ))
                    .child(kv("Duration", &format!("{ms:.3} ms")))
                    .child(kv("Size", &size(bytes)))
                    .when_some(error, |d, e| {
                        d.child(div().text_color(t.error).child(format!("Error: {e}")))
                    }),
            )
            .children(parts.into_iter().map(|(title, val)| {
                div()
                    .child(section(title))
                    .child(div().px_2().py_1().children(json_lines(&val, 400)))
            }))
            .into_any_element()
    }
}

fn kv(k: &str, v: &str) -> impl IntoElement {
    let t = theme();
    div()
        .flex()
        .child(div().w(px(90.0)).text_color(t.muted).child(k.to_string()))
        .child(div().text_color(t.text).child(v.to_string()))
}

fn net_row(r: &Rec, selected: bool, max_ms: f64, cx: &mut Context<Devtools>) -> AnyElement {
    let t = theme();
    let (kind, name, bytes, ms, error) = summarize(r);
    let seq = r.seq;
    let tint = match kind {
        "call" => t.accent,
        "event" => t.keyword,
        "signal" => t.ok,
        _ => t.muted,
    };
    let frac = (ms / max_ms).clamp(0.0, 1.0) as f32;
    div()
        .id(SharedString::from(format!("net-{seq}")))
        .flex()
        .w_full()
        .items_center()
        .gap_2()
        .h(px(20.0))
        .px_2()
        .border_b_1()
        .border_color(t.border.opacity(0.5))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.selected))
        .when(!selected, |d| d.hover(|s| s.bg(t.hover)))
        .when(error.is_some() && !selected, |d| d.bg(t.error_bg))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.net.selected = if this.net.selected == Some(seq) {
                None
            } else {
                Some(seq)
            };
            cx.notify();
        }))
        .child(
            div()
                .w(px(64.0))
                .text_color(t.faint)
                .child(format!("{:>8.3}", r.v["t"].as_f64().unwrap_or(0.0))),
        )
        .child(
            div()
                .w(px(70.0))
                .overflow_hidden()
                .text_color(t.muted)
                .child(r.v["surface"].as_str().unwrap_or("").to_string()),
        )
        .child(div().w(px(64.0)).text_color(tint).child(kind))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(if error.is_some() { t.error } else { t.text })
                .child(name),
        )
        .child(div().w(px(64.0)).text_color(t.muted).child(size(bytes)))
        .child(
            div()
                .w(px(60.0))
                .text_color(t.muted)
                .child(format!("{ms:.2}")),
        )
        .child(
            div().w(px(90.0)).h(px(8.0)).bg(t.raised).child(
                div()
                    .h_full()
                    .w(px((90.0 * frac).max(1.0)))
                    .bg(if ms > 16.0 { t.warn } else { tint }),
            ),
        )
        .into_any_element()
}
