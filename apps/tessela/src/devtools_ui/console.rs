//! Console: guest `log(...)`, load errors and host log lines.

use std::collections::HashSet;

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, UniformListScrollHandle, div, prelude::*, px, uniform_list,
};

use super::widgets::{FieldEvent, Fonts, TextField, button, chip, theme};
use super::{Devtools, Rec, field};

const LEVELS: [&str; 5] = ["error", "warn", "info", "log", "debug"];

pub(super) struct Console {
    filter: Entity<TextField>,
    hidden: HashSet<&'static str>,
    /// Only this source (a surface name or "host").
    source: Option<String>,
    scroll: UniformListScrollHandle,
    /// Records up to this one are cleared.
    cleared: u64,
    /// Indices into the console list that pass the filters.
    shown: Vec<usize>,
}

impl Console {
    pub fn new(fonts: &Fonts, cx: &mut Context<Devtools>) -> Self {
        let filter = field("Filter", fonts, cx);
        cx.subscribe(&filter, |_: &mut Devtools, _, _: &FieldEvent, cx| {
            cx.notify()
        })
        .detach();
        Self {
            filter,
            hidden: HashSet::new(),
            source: None,
            scroll: UniformListScrollHandle::new(),
            cleared: 0,
            shown: vec![],
        }
    }
}

fn level(r: &Rec) -> &'static str {
    match r.v["level"].as_str().unwrap_or("log") {
        "error" => "error",
        "warn" | "warning" => "warn",
        "info" => "info",
        "debug" | "trace" => "debug",
        _ => "log",
    }
}

impl Devtools {
    pub(super) fn console_tab(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let q = self.con.filter.read(cx).text.to_lowercase();
        // Following the tail unless the user scrolled up.
        let follow = self.con.scroll.is_scrolled_to_end().unwrap_or(true);
        self.con.shown = self
            .console
            .iter()
            .enumerate()
            .filter(|(_, r)| r.seq > self.con.cleared)
            .filter(|(_, r)| !self.con.hidden.contains(level(r)))
            .filter(|(_, r)| {
                self.con
                    .source
                    .as_ref()
                    .is_none_or(|s| r.v["source"].as_str() == Some(s))
            })
            .filter(|(_, r)| q.is_empty() || r.v.to_string().to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        if follow && !self.con.shown.is_empty() {
            self.con.scroll.scroll_to_bottom();
        }
        let mut counts = std::collections::HashMap::new();
        for r in self.console.iter().filter(|r| r.seq > self.con.cleared) {
            *counts.entry(level(r)).or_insert(0usize) += 1;
        }
        let levels = LEVELS.iter().map(|l| {
            let on = !self.con.hidden.contains(l);
            let tint = match *l {
                "error" => t.error,
                "warn" => t.warn,
                _ => t.accent,
            };
            let l2: &'static str = l;
            chip(
                SharedString::from(format!("level-{l}")),
                format!("{l} {}", counts.get(l).unwrap_or(&0)),
                on,
                Some(tint),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.con.hidden.remove(l2) {
                    this.con.hidden.insert(l2);
                }
                cx.notify();
            }))
        });
        let mut sources: Vec<String> = vec!["host".into()];
        sources.extend(self.surfaces());
        let source_chips = std::iter::once(None)
            .chain(sources.into_iter().map(Some))
            .map(|s| {
                let on = self.con.source == s;
                let label = s.clone().unwrap_or_else(|| "all".into());
                chip(
                    SharedString::from(format!("source-{label}")),
                    label,
                    on,
                    None,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.con.source = s.clone();
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
                button("console-clear", "⊘ Clear").on_click(cx.listener(|this, _, _, cx| {
                    this.con.cleared = this.seq;
                    cx.notify();
                })),
            )
            .child(div().w(px(220.0)).child(self.con.filter.clone()))
            .children(levels)
            .child(div().w(px(1.0)).h(px(18.0)).bg(t.border))
            .children(source_chips);
        let n = self.con.shown.len();
        let mono = self.fonts.mono.clone();
        let list = uniform_list(
            "console",
            n,
            cx.processor(|this: &mut Devtools, range: std::ops::Range<usize>, _, _| {
                range
                    .filter_map(|i| this.con.shown.get(i).and_then(|j| this.console.get(*j)))
                    .map(console_row)
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.con.scroll)
        .font_family(mono)
        .size_full();
        div().flex().flex_col().size_full().child(toolbar).child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .when(n == 0, |d| {
                    d.child(div().p_4().text_color(t.muted).child("Nothing logged."))
                })
                .child(list),
        )
    }
}

fn console_row(r: &Rec) -> AnyElement {
    let t = theme();
    let lvl = level(r);
    let (fg, bg, mark) = match lvl {
        "error" => (t.error, Some(t.error_bg), "✖"),
        "warn" => (t.warn, Some(t.warn_bg), "▲"),
        "info" => (t.text, None, "ℹ"),
        "debug" => (t.muted, None, "·"),
        _ => (t.text, None, " "),
    };
    let text = r.v["text"].as_str().unwrap_or("").replace('\n', " ⏎ ");
    div()
        .id(SharedString::from(format!("log-{}", r.seq)))
        .flex()
        .w_full()
        .items_center()
        .gap_2()
        .h(px(20.0))
        .px_2()
        .border_b_1()
        .border_color(t.border.opacity(0.5))
        .when_some(bg, |d, b| d.bg(b))
        .child(
            div()
                .w(px(64.0))
                .flex_shrink_0()
                .text_color(t.faint)
                .child(format!("{:>8.3}", r.v["t"].as_f64().unwrap_or(0.0))),
        )
        .child(div().w(px(12.0)).flex_shrink_0().text_color(fg).child(mark))
        .child(
            div()
                .flex_shrink_0()
                .px_1()
                .rounded(px(3.0))
                .bg(t.raised)
                .text_color(t.muted)
                .text_size(px(11.0))
                .child(r.v["source"].as_str().unwrap_or("").to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(fg)
                .child(text),
        )
        .when_some(r.v["at"].as_str(), |d, at| {
            d.child(
                div()
                    .flex_shrink_0()
                    .text_color(t.accent)
                    .child(at.to_string()),
            )
        })
        .into_any_element()
}
