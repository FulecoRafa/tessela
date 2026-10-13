//! Colors, fonts and the few controls GPUI does not ship: a one-line text
//! field, buttons, toggles, and JSON shown with colors.

use gpui::{
    App, ClipboardItem, Context, ElementId, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    SharedString, Stateful, Styled, Window, div, prelude::*, px, rgb,
};
use serde_json::Value;

pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Dark, close to Chrome's DevTools.
pub struct Theme {
    pub bg: Hsla,
    pub panel: Hsla,
    pub raised: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    pub selected: Hsla,
    pub hover: Hsla,
    pub error: Hsla,
    pub error_bg: Hsla,
    pub warn: Hsla,
    pub warn_bg: Hsla,
    pub ok: Hsla,
    /// Syntax: element kinds, prop names, strings, numbers, keywords.
    pub kind: Hsla,
    pub prop: Hsla,
    pub string: Hsla,
    pub number: Hsla,
    pub keyword: Hsla,
}

pub fn theme() -> Theme {
    Theme {
        bg: color(0x202124),
        panel: color(0x292a2d),
        raised: color(0x35363a),
        border: color(0x3c4043),
        text: color(0xe8eaed),
        muted: color(0x9aa0a6),
        faint: color(0x6e7378),
        accent: color(0x8ab4f8),
        selected: color(0x1f3a5f),
        hover: color(0x2f3236),
        error: color(0xf28b82),
        error_bg: color(0x3c2626),
        warn: color(0xfdd663),
        warn_bg: color(0x3a3320),
        ok: color(0x81c995),
        kind: color(0x5db0d7),
        prop: color(0x9bbbdc),
        string: color(0xf29766),
        number: color(0x9980ff),
        keyword: color(0xc58af9),
    }
}

/// Font families, picked among the installed ones.
#[derive(Clone)]
pub struct Fonts {
    pub ui: SharedString,
    pub mono: SharedString,
}

impl Fonts {
    pub fn pick(cx: &App) -> Self {
        let have = cx.text_system().all_font_names();
        let first = |wanted: &[&str], fallback: &str| -> SharedString {
            wanted
                .iter()
                .find(|w| have.iter().any(|h| h == *w))
                .map(|w| SharedString::from(w.to_string()))
                .unwrap_or_else(|| fallback.to_string().into())
        };
        Fonts {
            ui: first(
                &[
                    "Inter",
                    "Adwaita Sans",
                    "Cantarell",
                    "Noto Sans",
                    "DejaVu Sans",
                    "IBM Plex Sans",
                ],
                "IBM Plex Sans",
            ),
            mono: first(
                &[
                    "JetBrains Mono",
                    "Adwaita Mono",
                    "CaskaydiaCove Nerd Font Mono",
                    "CaskaydiaCove Nerd Font",
                    "Cascadia Code",
                    "Fira Code",
                    "DejaVu Sans Mono",
                    "Noto Sans Mono",
                    "Liberation Mono",
                ],
                "monospace",
            ),
        }
    }
}

// ---------------------------------------------------------------- text field

pub enum FieldEvent {
    Changed,
    Submit,
}

/// One line of editable text: typing, backspace/delete, ←/→, home/end,
/// ctrl+u (clear), ctrl+c / ctrl+v (whole text), enter (submit).
pub struct TextField {
    pub text: String,
    /// Byte offset of the caret.
    cursor: usize,
    focus: FocusHandle,
    placeholder: SharedString,
    mono: SharedString,
}

impl EventEmitter<FieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextField {
    pub fn new(placeholder: &str, fonts: &Fonts, cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            focus: cx.focus_handle(),
            placeholder: placeholder.to_string().into(),
            mono: fonts.mono.clone(),
        }
    }

    pub fn set(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.cursor = self.text.len();
        cx.notify();
    }

    fn prev(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor)
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &e.keystroke;
        let ctrl = ks.modifiers.control || ks.modifiers.platform;
        let mut changed = false;
        match ks.key.as_str() {
            "enter" => cx.emit(FieldEvent::Submit),
            "escape" => window.blur(cx),
            "backspace" if self.cursor > 0 => {
                let p = self.prev();
                self.text.replace_range(p..self.cursor, "");
                self.cursor = p;
                changed = true;
            }
            "delete" if self.cursor < self.text.len() => {
                let n = self.next();
                self.text.replace_range(self.cursor..n, "");
                changed = true;
            }
            "left" => self.cursor = self.prev(),
            "right" => self.cursor = self.next(),
            "home" => self.cursor = 0,
            "end" => self.cursor = self.text.len(),
            "u" if ctrl => {
                self.text.clear();
                self.cursor = 0;
                changed = true;
            }
            "c" if ctrl => cx.write_to_clipboard(ClipboardItem::new_string(self.text.clone())),
            "v" if ctrl => {
                if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    let t = t.replace('\n', " ");
                    self.text.insert_str(self.cursor, &t);
                    self.cursor += t.len();
                    changed = true;
                }
            }
            _ if ctrl || ks.modifiers.alt => {}
            _ => {
                if let Some(ch) = &ks.key_char {
                    self.text.insert_str(self.cursor, ch);
                    self.cursor += ch.len();
                    changed = true;
                }
            }
        }
        if changed {
            cx.emit(FieldEvent::Changed);
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let focused = self.focus.is_focused(window);
        let content = if self.text.is_empty() && !focused {
            div().text_color(t.faint).child(self.placeholder.clone())
        } else {
            let (before, after) = self.text.split_at(self.cursor);
            div()
                .flex()
                .items_center()
                .text_color(t.text)
                .child(before.to_string())
                .when(focused, |d| {
                    d.child(div().w(px(1.5)).h(px(14.0)).bg(t.accent))
                })
                .child(after.to_string())
        };
        div()
            .id("field")
            .track_focus(&self.focus)
            .key_context("TextField")
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    cx.notify();
                }),
            )
            .flex()
            .items_center()
            .h(px(24.0))
            .px_2()
            .min_w(px(60.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded(px(4.0))
            .bg(t.bg)
            .border_1()
            .border_color(if focused { t.accent } else { t.border })
            .font_family(self.mono.clone())
            .text_size(px(12.0))
            .cursor_text()
            .child(content)
    }
}

// ---------------------------------------------------------------- controls

/// A small flat button.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<gpui::Div> {
    let t = theme();
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap_1()
        .h(px(24.0))
        .px_2()
        .rounded(px(4.0))
        .bg(t.raised)
        .border_1()
        .border_color(t.border)
        .text_color(t.text)
        .text_size(px(12.0))
        .cursor_pointer()
        .hover(|s| s.bg(color(0x44464b)))
        .child(label.into())
}

/// A toggle chip: filters, surfaces, levels.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on: bool,
    tint: Option<Hsla>,
) -> Stateful<gpui::Div> {
    let t = theme();
    let tint = tint.unwrap_or(t.accent);
    div()
        .id(id.into())
        .flex()
        .items_center()
        .h(px(22.0))
        .px_2()
        .rounded(px(11.0))
        .text_size(px(12.0))
        .cursor_pointer()
        .border_1()
        .when(on, |d| {
            d.bg(tint.opacity(0.18))
                .border_color(tint.opacity(0.6))
                .text_color(tint)
        })
        .when(!on, |d| {
            d.border_color(t.border)
                .text_color(t.muted)
                .hover(|s| s.bg(t.hover))
        })
        .child(label.into())
}

/// A section header in a side panel.
pub fn section(title: impl Into<SharedString>) -> gpui::Div {
    let t = theme();
    div()
        .px_2()
        .py_1()
        .bg(t.panel)
        .border_b_1()
        .border_color(t.border)
        .text_color(t.muted)
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(title.into())
}

/// A value in one line, colored by type and cut at `max` chars.
pub fn value_inline(v: &Value, max: usize) -> gpui::Div {
    let t = theme();
    let (text, c) = match v {
        Value::String(s) => (format!("{s:?}"), t.string),
        Value::Number(n) => (n.to_string(), t.number),
        Value::Bool(b) => (b.to_string(), t.keyword),
        Value::Null => ("null".into(), t.keyword),
        other => (other.to_string(), t.text),
    };
    div().text_color(c).child(ellipsize(&text, max))
}

pub fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Pretty JSON as colored lines (keys, strings, numbers, keywords), at
/// most `max_lines`.
pub fn json_lines(v: &Value, max_lines: usize) -> Vec<gpui::AnyElement> {
    let t = theme();
    let pretty = serde_json::to_string_pretty(v).unwrap_or_default();
    let total = pretty.lines().count();
    let mut out: Vec<gpui::AnyElement> = pretty
        .lines()
        .take(max_lines)
        .map(|line| {
            let indent = line.len() - line.trim_start().len();
            let body = line.trim_start();
            let mut row = div().flex().pl(px(indent as f32 * 3.5));
            // `"key": value` or a bare value.
            let (key, rest) = match body.find("\": ") {
                Some(i) if body.starts_with('"') => (Some(&body[..i + 1]), &body[i + 3..]),
                _ => (None, body),
            };
            if let Some(k) = key {
                row = row
                    .child(div().text_color(t.prop).child(k.to_string()))
                    .child(div().text_color(t.muted).child(": "));
            }
            let c = match rest.trim_end_matches(',') {
                s if s.starts_with('"') => t.string,
                "true" | "false" | "null" => t.keyword,
                s if s.starts_with(|c: char| c == '-' || c.is_ascii_digit()) => t.number,
                _ => t.muted,
            };
            row.child(div().text_color(c).child(rest.to_string()))
                .into_any_element()
        })
        .collect();
    if total > max_lines {
        out.push(
            div()
                .text_color(t.faint)
                .child(format!("… {} more lines", total - max_lines))
                .into_any_element(),
        );
    }
    out
}
