//! Lowers a [`Node`] tree to a flat run of styled segments, for renderers
//! that draw text (terminal, headless, logs). Graphical renderers walk the
//! tree directly instead.

use crate::{HandlerId as FnId, Node, Tone};

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
    pub click: Option<FnId>,
    pub scroll: Option<FnId>,
}

impl Segment {
    fn plain(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            style: Style::Plain(tone),
            click: None,
            scroll: None,
        }
    }
    pub fn interactive(&self) -> bool {
        self.click.is_some() || self.scroll.is_some()
    }
}

/// Icon names are part of the vocabulary; each renderer maps them. These are
/// plain Unicode so the demo works without a Nerd Font.
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
        "warning" => "!",
        _ => "•",
    }
}

/// Errors are shown compactly in the bar; the host logs the full message.
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
    match node {
        Node::Row { children, gap } | Node::Column { children, gap } => {
            for (i, c) in children.iter().enumerate() {
                if i > 0 {
                    let spaces = (*gap / 4.0).round().max(1.0) as usize;
                    out.push(Segment::plain(" ".repeat(spaces), Tone::Normal));
                }
                flatten(c, out);
            }
        }
        Node::Text { text, tone } => out.push(Segment::plain(text.clone(), *tone)),
        Node::Icon { name } => out.push(Segment::plain(icon(name), Tone::Accent)),
        Node::Pill {
            icon: ic,
            label,
            active,
            on_click,
            on_scroll,
        } => {
            let text = match ic {
                Some(i) => format!(" {} {} ", icon(i), label),
                None => format!(" {label} "),
            };
            out.push(Segment {
                text,
                style: Style::Pill { active: *active },
                click: *on_click,
                scroll: *on_scroll,
            });
        }
        Node::Button { label, on_click } => out.push(Segment {
            text: format!("‹{label}›"),
            style: Style::Button,
            click: *on_click,
            scroll: None,
        }),
        Node::Progress { value, label } => {
            let v = value.clamp(0.0, 1.0);
            let filled = (v * 8.0).round() as usize;
            let bar = format!("{}{}", "█".repeat(filled), "░".repeat(8 - filled));
            let tone = if v > 0.85 {
                Tone::Critical
            } else if v > 0.6 {
                Tone::Warning
            } else {
                Tone::Normal
            };
            let text = match label {
                Some(l) => format!("{l} {bar}"),
                None => bar,
            };
            out.push(Segment::plain(text, tone));
        }
        Node::Spacer => out.push(Segment::plain("  ", Tone::Normal)),
        Node::Error { widget, message } => out.push(Segment {
            text: short_error(widget, message),
            style: Style::Error,
            click: None,
            scroll: None,
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
