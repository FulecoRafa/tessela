//! Performance: per-surface totals, a timeline of renders and what each one
//! changed.

use gpui::{
    Context, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use serde_json::Value;

use super::Devtools;
use super::widgets::{button, color, section, theme};

/// Frames in the timeline.
const BARS: usize = 180;
const CHART_H: f32 = 140.0;

#[derive(Default)]
pub(super) struct Perf {
    pub paused: bool,
    selected: Option<u64>,
}

/// A stable color per surface.
fn tint(i: usize) -> Hsla {
    color([0x8ab4f8, 0x81c995, 0xfdd663, 0xc58af9, 0xf29766, 0x78d9ec][i % 6])
}

impl Devtools {
    pub(super) fn perf_tab(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let surfaces = self.surfaces();
        let index = |name: &str| surfaces.iter().position(|s| s == name).unwrap_or(0);
        let cards = self.overview["surfaces"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, s)| card(i, s));
        let frames: Vec<_> = self
            .frames
            .iter()
            .skip(self.frames.len().saturating_sub(BARS))
            .cloned()
            .collect();
        // Scaled to the 90th percentile (at least two frame budgets), so one
        // slow load does not flatten everything else; taller bars are capped.
        let mut times: Vec<f64> = frames
            .iter()
            .map(|r| r.v["ms"].as_f64().unwrap_or(0.0))
            .collect();
        times.sort_by(f64::total_cmp);
        let max = times
            .get(times.len() * 9 / 10)
            .copied()
            .unwrap_or(0.0)
            .max(33.4);
        let selected = self.perf.selected;
        let bars = frames.iter().map(|r| {
            let ms = r.v["ms"].as_f64().unwrap_or(0.0);
            let capped = ms > max;
            let h = ((ms.min(max) / max) as f32 * CHART_H).max(2.0);
            let c = tint(index(r.v["surface"].as_str().unwrap_or("")));
            let seq = r.seq;
            let patches = r.v["patches"].as_u64().unwrap_or(0);
            div()
                .id(SharedString::from(format!("bar-{seq}")))
                .flex()
                .flex_col()
                .justify_end()
                .h_full()
                .w(px(8.0))
                .flex_shrink_0()
                .cursor_pointer()
                .when(selected == Some(seq), |d| d.bg(t.selected))
                .hover(|s| s.bg(t.hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    // Clicking the selected bar again goes back to the list.
                    this.perf.selected = (this.perf.selected != Some(seq)).then_some(seq);
                    cx.notify();
                }))
                .child(
                    div()
                        .h(px(h))
                        .w_full()
                        // Renders that changed nothing are dimmed.
                        .bg(if patches == 0 { c.opacity(0.35) } else { c })
                        .when(capped, |d| d.border_t_2().border_color(t.error)),
                )
        });
        let budget = CHART_H - (16.7 / max as f32) * CHART_H;
        let legend = surfaces.iter().enumerate().map(|(i, s)| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().size(px(8.0)).rounded(px(2.0)).bg(tint(i)))
                .child(div().text_color(t.muted).child(s.clone()))
        });
        let chart = div()
            .relative()
            .h(px(CHART_H))
            .mx_2()
            .border_b_1()
            .border_color(t.border)
            .flex()
            .items_end()
            .gap(px(2.0))
            .overflow_hidden()
            .children(bars)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(budget))
                    .h(px(1.0))
                    .bg(t.warn.opacity(0.5)),
            )
            .child(
                div()
                    .absolute()
                    .right(px(4.0))
                    .top(px((budget - 14.0).max(0.0)))
                    .text_size(px(10.0))
                    .text_color(t.warn)
                    .child("16.7 ms"),
            );
        let detail = match selected.and_then(|s| self.frames.iter().find(|r| r.seq == s)) {
            Some(r) => frame_detail(&r.v, &self.fonts.mono).into_any_element(),
            None => self.recent_frames(cx).into_any_element(),
        };
        div()
            .id("perf")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(34.0))
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        button("perf-pause", if self.perf.paused { "▶ Resume" } else { "⏸ Pause" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.perf.paused = !this.perf.paused;
                                cx.notify();
                            })),
                    )
                    .child(button("perf-clear", "⊘ Clear").on_click(cx.listener(|this, _, _, cx| {
                        this.frames.clear();
                        this.perf.selected = None;
                        cx.notify();
                    })))
                    .child(div().flex_1())
                    .children(legend),
            )
            .child(div().flex().flex_wrap().gap_2().p_2().children(cards))
            .child(section(format!(
                "Renders (last {}): height is render time, dim bars changed nothing, red tops are off the scale. Click one.",
                frames.len()
            )))
            .child(div().pt_2().child(chart))
            .child(detail)
    }
}

fn stat(label: &str, value: String) -> impl IntoElement {
    let t = theme();
    div()
        .flex()
        .justify_between()
        .gap_4()
        .child(div().text_color(t.muted).child(label.to_string()))
        .child(div().child(value))
}

fn card(i: usize, s: Value) -> impl IntoElement {
    let t = theme();
    let f = |k: &str| s[k].as_f64().unwrap_or(0.0);
    let error = s["error"].as_str().map(String::from);
    div()
        .w(px(250.0))
        .p_2()
        .rounded(px(6.0))
        .bg(t.panel)
        .border_1()
        .border_color(if error.is_some() { t.error } else { t.border })
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(8.0)).rounded(px(2.0)).bg(tint(i)))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(s["name"].as_str().unwrap_or("").to_string()),
                ),
        )
        .when_some(error, |d, e| {
            d.child(div().text_color(t.error).text_size(px(11.0)).child(e))
        })
        .child(stat(
            "render avg / max",
            format!("{:.2} / {:.2} ms", f("renderAvgMs"), f("renderMaxMs")),
        ))
        .child(stat(
            "draw avg",
            format!("{:.2} ms × {}", f("drawAvgMs"), s["draws"]),
        ))
        .child(stat(
            "renders / frames",
            format!("{} / {}", s["renders"], s["frames"]),
        ))
        .child(stat("patches", s["patches"].to_string()))
        .child(stat(
            "graph",
            format!(
                "{} b · {} s · {} r",
                s["bindings"], s["signals"], s["resources"]
            ),
        ))
        .child(stat(
            "calls / deliveries",
            format!("{} / {}", s["calls"], s["deliveries"]),
        ))
        .child(stat(
            "load",
            format!("{:.0} ms, {} files", f("loadMs"), s["files"]),
        ))
}

fn frame_detail(v: &Value, mono: &SharedString) -> impl IntoElement {
    let t = theme();
    let changes: Vec<String> = v["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str().map(String::from))
        .collect();
    div()
        .mt_2()
        .child(section(format!(
            "{} at {:.3} s: render {:.2} ms, {} patches · graph {} bindings {} signals {} resources",
            v["surface"].as_str().unwrap_or(""),
            v["t"].as_f64().unwrap_or(0.0),
            v["ms"].as_f64().unwrap_or(0.0),
            v["patches"],
            v["bindings"],
            v["signals"],
            v["resources"]
        )))
        .child(
            div()
                .px_2()
                .py_1()
                .font_family(mono.clone())
                .when(changes.is_empty(), |d| {
                    d.child(div().text_color(t.muted).child("Nothing changed: the render produced no patches."))
                })
                .children(changes.into_iter().map(|c| {
                    let fg = if c.starts_with('+') {
                        t.ok
                    } else if c.starts_with('-') {
                        t.error
                    } else {
                        t.text
                    };
                    div().text_color(fg).child(c)
                })),
        )
}

impl Devtools {
    /// The latest renders that changed something, newest first.
    fn recent_frames(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let rows = self
            .frames
            .iter()
            .rev()
            .filter(|r| r.v["patches"].as_u64().unwrap_or(0) > 0)
            .take(40)
            .map(|r| {
                let seq = r.seq;
                let first = r.v["changes"][0].as_str().unwrap_or("").to_string();
                let more = r.v["changes"]
                    .as_array()
                    .map_or(0, |c| c.len().saturating_sub(1));
                div()
                    .id(SharedString::from(format!("frame-{seq}")))
                    .flex()
                    .w_full()
                    .gap_2()
                    .px_2()
                    .h(px(20.0))
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.perf.selected = Some(seq);
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
                            .w(px(80.0))
                            .text_color(t.muted)
                            .child(r.v["surface"].as_str().unwrap_or("").to_string()),
                    )
                    .child(
                        div()
                            .w(px(70.0))
                            .child(format!("{:.2} ms", r.v["ms"].as_f64().unwrap_or(0.0))),
                    )
                    .child(
                        div()
                            .w(px(80.0))
                            .text_color(t.muted)
                            .child(format!("{} patches", r.v["patches"])),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(if more > 0 {
                                format!("{first}  (+{more})")
                            } else {
                                first
                            }),
                    )
            });
        div()
            .mt_2()
            .font_family(self.fonts.mono.clone())
            .child(section("Recent renders with changes"))
            .children(rows)
    }
}
