//! Application: shared values (editable), desktop topics, windows with
//! their placement (and captures), plugins.

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, img, prelude::*, px,
};
use serde_json::{Value, json};

use super::link::Cmd;
use super::widgets::{
    FieldEvent, Fonts, TextField, button, json_lines, section, theme, value_inline,
};
use super::{Devtools, field};

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Shared,
    Desktop,
    Windows,
    Plugins,
}

pub(super) struct Application {
    pane: Pane,
    /// Selected shared key or desktop topic.
    key: Option<String>,
    editor: Entity<TextField>,
    new_key: Entity<TextField>,
    /// Last capture: (surface, png).
    pub shot: Option<(String, String)>,
}

impl Application {
    pub fn new(fonts: &Fonts, cx: &mut Context<Devtools>) -> Self {
        let editor = field("JSON value; Enter applies", fonts, cx);
        cx.subscribe(&editor, |this: &mut Devtools, _, e: &FieldEvent, cx| {
            if let FieldEvent::Submit = e {
                this.apply_value(cx);
            }
        })
        .detach();
        Self {
            pane: Pane::Shared,
            key: None,
            editor,
            new_key: field("new key", fonts, cx),
            shot: None,
        }
    }
}

impl Devtools {
    /// Sends the editor's value for the selected (or new) key.
    fn apply_value(&mut self, cx: &mut Context<Self>) {
        let new_key = self.app.new_key.read(cx).text.trim().to_string();
        let key = if new_key.is_empty() {
            self.app.key.clone()
        } else {
            Some(new_key)
        };
        let Some(key) = key else {
            self.status = Some(("pick a key, or type a new one".into(), true));
            cx.notify();
            return;
        };
        let text = self.app.editor.read(cx).text.clone();
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => {
                self.act("set", json!({ "op": "set", "key": key, "value": v }));
                self.app.key = Some(key);
                self.app.new_key.update(cx, |f, cx| f.set("", cx));
            }
            Err(e) => {
                self.status = Some((
                    format!("not JSON ({e}); strings need quotes: \"drawer\""),
                    true,
                ))
            }
        }
        cx.notify();
    }

    pub(super) fn application_tab(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let body = match self.app.pane {
            Pane::Shared => self.shared_pane(cx),
            Pane::Desktop => self.desktop_pane(cx),
            Pane::Windows => self.windows_pane(cx),
            Pane::Plugins => self.plugins_pane(),
        };
        let nav = [
            (Pane::Shared, "Shared state"),
            (Pane::Desktop, "Desktop topics"),
            (Pane::Windows, "Windows"),
            (Pane::Plugins, "Plugins"),
        ]
        .into_iter()
        .map(|(pane, label)| {
            let on = self.app.pane == pane;
            div()
                .id(SharedString::from(format!("pane-{label}")))
                .px_3()
                .py_1()
                .cursor_pointer()
                .when(on, |d| d.bg(t.selected).text_color(t.text))
                .when(!on, |d| d.text_color(t.muted).hover(|s| s.bg(t.hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.app.pane = pane;
                    this.app.key = None;
                    cx.notify();
                }))
                .child(label)
        })
        .collect::<Vec<_>>();
        div()
            .flex()
            .size_full()
            .child(
                div()
                    .w(px(180.0))
                    .flex_shrink_0()
                    .h_full()
                    .bg(t.panel)
                    .border_r_1()
                    .border_color(t.border)
                    .py_1()
                    .children(nav),
            )
            .child(div().flex_1().min_w(px(0.0)).h_full().child(body))
    }

    /// A clickable key list on the left, the selection on the right.
    fn keyed(
        &mut self,
        id: &str,
        entries: Vec<(String, Value)>,
        right: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme();
        let rows = entries.into_iter().map(|(k, v)| {
            let on = self.app.key.as_deref() == Some(k.as_str());
            let key = k.clone();
            div()
                .id(SharedString::from(format!("{id}-{k}")))
                .flex()
                .gap_2()
                .px_2()
                .h(px(22.0))
                .items_center()
                .cursor_pointer()
                .border_b_1()
                .border_color(t.border.opacity(0.5))
                .when(on, |d| d.bg(t.selected))
                .when(!on, |d| d.hover(|s| s.bg(t.hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.app.key = Some(key.clone());
                    let v = this.app_value(&key);
                    this.app.editor.update(cx, |f, cx| f.set(v.to_string(), cx));
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(180.0))
                        .flex_shrink_0()
                        .text_color(t.prop)
                        .child(k),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(value_inline(&v, 80)),
                )
        });
        div()
            .flex()
            .size_full()
            .font_family(self.fonts.mono.clone())
            .child(
                div()
                    .id(SharedString::from(format!("{id}-list")))
                    .w(gpui::relative(0.45))
                    .h_full()
                    .overflow_y_scroll()
                    .border_r_1()
                    .border_color(t.border)
                    .children(rows),
            )
            .child(div().flex_1().min_w(px(0.0)).h_full().child(right))
            .into_any_element()
    }

    fn app_value(&self, key: &str) -> Value {
        let src = match self.app.pane {
            Pane::Shared => &self.state["shared"],
            _ => &self.state["desktop"],
        };
        src[key].clone()
    }

    fn shared_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let entries: Vec<(String, Value)> = self.state["shared"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let current = self
            .app
            .key
            .clone()
            .map(|k| (k.clone(), self.app_value(&k)));
        let right = div()
            .id("shared-edit")
            .size_full()
            .overflow_y_scroll()
            .child(section("Edit"))
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_color(t.muted)
                            .child(match &current {
                                Some((k, _)) => format!("{k} — or a new key:"),
                                None => "Select a key, or type a new one:".into(),
                            }),
                    )
                    .child(self.app.new_key.clone())
                    .child(self.app.editor.clone())
                    .child(
                        div().flex().gap_2().child(
                            button("apply", "✓ Apply").on_click(
                                cx.listener(|this, _, _, cx| this.apply_value(cx)),
                            ),
                        ),
                    )
                    .child(
                        div()
                            .text_color(t.faint)
                            .text_size(px(11.0))
                            .child("Sets the value as an extension would: every surface reading it re-renders."),
                    ),
            )
            .when_some(current, |d, (k, v)| {
                d.child(section(format!("Current value of {k}")))
                    .child(div().p_2().children(json_lines(&v, 300)))
            })
            .into_any_element();
        if entries.is_empty() {
            return div()
                .p_4()
                .text_color(t.muted)
                .child("No shared values yet.")
                .child(right)
                .into_any_element();
        }
        self.keyed("shared", entries, right, cx)
    }

    fn desktop_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let entries: Vec<(String, Value)> = self.state["desktop"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let right = match self.app.key.clone() {
            Some(k) => {
                let v = self.app_value(&k);
                div()
                    .id("desktop-value")
                    .size_full()
                    .overflow_y_scroll()
                    .child(section(k))
                    .child(div().p_2().children(json_lines(&v, 600)))
                    .into_any_element()
            }
            None => div()
                .p_4()
                .text_color(t.muted)
                .child("Topics the desktop modules publish; select one.")
                .into_any_element(),
        };
        self.keyed("desktop", entries, right, cx)
    }

    fn windows_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let names = self.surfaces();
        let header = div()
            .flex()
            .gap_2()
            .px_2()
            .h(px(22.0))
            .items_center()
            .bg(t.panel)
            .text_color(t.muted)
            .text_size(px(11.0))
            .child(div().w(px(110.0)).child("Surface"))
            .child(div().w(px(70.0)).child("Layer"))
            .child(div().w(px(150.0)).child("Anchors"))
            .child(div().w(px(100.0)).child("Size"))
            .child(div().w(px(130.0)).child("Margin"))
            .child(div().w(px(110.0)).child("Flags"))
            .child(div().flex_1());
        let rows = self.overview["windows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|w| {
                let slot = w["slot"].as_u64().unwrap_or(0) as usize;
                let name = names
                    .get(slot)
                    .cloned()
                    .unwrap_or_else(|| format!("#{slot}"));
                let anchors: Vec<&str> = ["top", "right", "bottom", "left"]
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| w["anchors"][*i] == true)
                    .map(|(_, a)| a)
                    .collect();
                let mut flags = vec![];
                if w["overlay"] == true {
                    flags.push("overlay");
                }
                if w["keyboard"] == true {
                    flags.push("keyboard");
                }
                let popup = w["popup"].as_u64();
                let shoot = name.clone();
                div()
                    .flex()
                    .gap_2()
                    .px_2()
                    .h(px(26.0))
                    .items_center()
                    .border_b_1()
                    .border_color(t.border.opacity(0.5))
                    .child(div().w(px(110.0)).text_color(t.prop).child(match popup {
                        Some(p) => format!("{name} popup #{p}"),
                        None => name.clone(),
                    }))
                    .child(
                        div()
                            .w(px(70.0))
                            .child(w["layer"].as_str().unwrap_or("").to_string()),
                    )
                    .child(div().w(px(150.0)).child(anchors.join(" ")))
                    .child(div().w(px(100.0)).child(format!(
                        "{} × {}",
                        length(&w["size"][0]),
                        length(&w["size"][1])
                    )))
                    .child(
                        div().w(px(130.0)).child(
                            (0..4)
                                .map(|i| w["margin"][i].as_f64().unwrap_or(0.0).to_string())
                                .collect::<Vec<_>>()
                                .join(" "),
                        ),
                    )
                    .child(
                        div()
                            .w(px(110.0))
                            .text_color(t.muted)
                            .child(flags.join(" ")),
                    )
                    .when(popup.is_none(), |d| {
                        d.child(
                            button(SharedString::from(format!("shot-{name}")), "◉ Capture")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(Cmd::Shot(shoot.clone()));
                                    this.status = Some((format!("capturing {shoot}…"), false));
                                    cx.notify();
                                })),
                        )
                    })
            });
        div()
            .id("windows")
            .size_full()
            .overflow_y_scroll()
            .font_family(self.fonts.mono.clone())
            .child(header)
            .children(rows)
            .when_some(self.app.shot.clone(), |d, (surface, path)| {
                d.child(section(format!("Capture of {surface}: {path}")))
                    .child(
                        div().p_2().child(
                            img(std::path::PathBuf::from(path))
                                .max_w_full()
                                .max_h(px(480.0)),
                        ),
                    )
            })
            .into_any_element()
    }

    fn plugins_pane(&mut self) -> AnyElement {
        let t = theme();
        let rows = self.overview["plugins"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let running = p["pid"].is_u64();
                div()
                    .flex()
                    .gap_2()
                    .px_2()
                    .h(px(24.0))
                    .items_center()
                    .border_b_1()
                    .border_color(t.border.opacity(0.5))
                    .child(div().size(px(8.0)).rounded_full().bg(if running {
                        t.ok
                    } else {
                        t.error
                    }))
                    .child(
                        div()
                            .w(px(140.0))
                            .text_color(t.prop)
                            .child(p["name"].as_str().unwrap_or("").to_string()),
                    )
                    .child(div().w(px(110.0)).child(match p["pid"].as_u64() {
                        Some(pid) => format!("pid {pid}"),
                        None => "stopped".into(),
                    }))
                    .child(
                        div()
                            .w(px(90.0))
                            .text_color(t.muted)
                            .child(format!("{} starts", p["starts"])),
                    )
                    .child(
                        div().text_color(t.muted).child(
                            p["lastExit"]
                                .as_str()
                                .map(|e| format!("last exit: {e}"))
                                .unwrap_or_default(),
                        ),
                    )
            });
        div()
            .id("plugins")
            .size_full()
            .overflow_y_scroll()
            .font_family(self.fonts.mono.clone())
            .child(section(
                "Plugins: services the project ships; the runner restarts them",
            ))
            .children(rows)
            .into_any_element()
    }
}

/// A placement length: 0 stretches (or sizes to content).
fn length(v: &Value) -> String {
    match v.as_f64().unwrap_or(0.0) {
        0.0 => "auto".into(),
        n => format!("{n}"),
    }
}
