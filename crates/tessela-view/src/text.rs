//! Lowers a [`Node`] tree to a flat run of styled segments, for renderers
//! that draw text (terminal, headless, logs). Graphical renderers walk the
//! tree directly instead.

use crate::{ERROR, Handler, Node, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain(Tone),
    Pill { active: bool },
    Button,
    Error,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub text: String,
    pub style: Style,
    pub click: Option<Handler>,
    pub press: Option<Handler>,
    pub release: Option<Handler>,
    pub scroll: Option<Handler>,
}

impl Segment {
    fn plain(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            style: Style::Plain(tone),
            click: None,
            press: None,
            release: None,
            scroll: None,
        }
    }

    fn interactive(text: String, style: Style, n: &Node) -> Self {
        Self {
            text,
            style,
            click: n.handler("on_click"),
            press: n.handler("on_press"),
            release: n.handler("on_release"),
            scroll: n.handler("on_scroll"),
        }
    }

    pub fn is_interactive(&self) -> bool {
        self.click.is_some()
            || self.press.is_some()
            || self.release.is_some()
            || self.scroll.is_some()
    }
}

/// Icon names are part of the vocabulary; each renderer maps them. These are
/// plain Unicode so text renderers work without a Nerd Font.
pub fn icon(name: &str) -> &'static str {
    match name {
        "battery" => "▮",
        "charging" => "ϟ",
        "clock" => "◷",
        "calendar" => "▦",
        "cpu" => "◫",
        "memory" => "▤",
        "volume" => "♪",
        "plus" => "+",
        "minus" => "−",
        "bolt" => "⚡",
        "leaf" => "❦",
        "gauge" => "◔",
        "check" => "✓",
        "warning" => "!",
        _ => "•",
    }
}

/// Errors are shown compactly; the host logs the full message.
pub fn short_error(widget: &str, message: &str) -> String {
    const MAX: usize = 32;
    let msg: String = message.chars().take(MAX).collect();
    let ellipsis = if message.chars().count() > MAX {
        "…"
    } else {
        ""
    };
    format!("⚠ {widget}: {msg}{ellipsis}")
}

pub fn flatten(node: &Node, out: &mut Vec<Segment>) {
    match node.kind.as_str() {
        "Row" | "Column" => {
            let gap = node.f32("gap").unwrap_or(0.0);
            for (i, c) in node.children.iter().enumerate() {
                if i > 0 {
                    let spaces = (gap / 4.0).round().max(1.0) as usize;
                    out.push(Segment::plain(" ".repeat(spaces), Tone::Normal));
                }
                flatten(c, out);
            }
        }
        "Text" => out.push(Segment::plain(node.str("text").unwrap_or(""), node.tone())),
        "Icon" => {
            let tone = if node.props.contains_key("tone") {
                node.tone()
            } else {
                Tone::Accent
            };
            out.push(Segment::plain(icon(node.str("name").unwrap_or("")), tone))
        }
        "Pill" => {
            let label = node.str("label").unwrap_or("");
            let text = match node.str("icon") {
                Some(i) => format!(" {} {label} ", icon(i)),
                None => format!(" {label} "),
            };
            out.push(Segment::interactive(
                text,
                Style::Pill {
                    active: node.bool("active"),
                },
                node,
            ));
        }
        "Button" => out.push(Segment::interactive(
            format!("‹{}›", node.str("label").unwrap_or("")),
            Style::Button,
            node,
        )),
        "Progress" => {
            let v = node.f32("value").unwrap_or(0.0).clamp(0.0, 1.0);
            let filled = (v * 8.0).round() as usize;
            let bar = format!("{}{}", "█".repeat(filled), "░".repeat(8 - filled));
            let tone = if v > 0.85 {
                Tone::Critical
            } else if v > 0.6 {
                Tone::Warning
            } else {
                Tone::Normal
            };
            let text = match node.str("label") {
                Some(l) => format!("{l} {bar}"),
                None => bar,
            };
            out.push(Segment::plain(text, tone));
        }
        "Spacer" => out.push(Segment::plain("  ", Tone::Normal)),
        ERROR => out.push(Segment {
            style: Style::Error,
            ..Segment::plain(
                short_error(
                    node.str("widget").unwrap_or("?"),
                    node.str("message").unwrap_or(""),
                ),
                Tone::Critical,
            )
        }),
        other => out.push(Segment {
            style: Style::Error,
            ..Segment::plain(format!("⚠ unsupported component {other}"), Tone::Critical)
        }),
    }
}

pub fn to_plain(nodes: &[Option<Node>]) -> String {
    let mut parts = Vec::new();
    for n in nodes.iter().flatten() {
        let mut segs = Vec::new();
        flatten(n, &mut segs);
        let s: String = segs
            .iter()
            .map(|s| match s.style {
                Style::Pill { active: true } => format!("[*{}]", s.text.trim()),
                Style::Pill { active: false } => format!("[{}]", s.text.trim()),
                _ => s.text.clone(),
            })
            .collect();
        parts.push(s);
    }
    parts.join("  │  ")
}
