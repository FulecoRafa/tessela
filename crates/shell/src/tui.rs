//! Terminal renderer: the bar as one line, keyboard and mouse driven.
//!
//! This exists so the whole pipeline (services → connector → sandboxed widgets
//! → frames → input → handlers) can be exercised anywhere, including over SSH
//! and in CI. The GPUI renderer (feature `gpui`) consumes the same frames.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Receiver;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style as RStyle};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use runtime::Node;
use view::Tone;

use crate::flatten::{Segment, Style, flatten};
use crate::host::{Frame, UiEvent, WidgetHandle};

// Ayu-dark-ish palette, matching the Marathon Shell sketches.
const BG: Color = Color::Rgb(0x0b, 0x0e, 0x14);
const PANEL: Color = Color::Rgb(0x1c, 0x21, 0x2b);
const FG: Color = Color::Rgb(0xbf, 0xbd, 0xb6);
const MUTED: Color = Color::Rgb(0x56, 0x5b, 0x66);
const ACCENT: Color = Color::Rgb(0xe6, 0xb4, 0x50);
const CYAN: Color = Color::Rgb(0x39, 0xba, 0xe6);
const WARN: Color = Color::Rgb(0xff, 0xb4, 0x54);
const CRIT: Color = Color::Rgb(0xd9, 0x57, 0x57);

pub type LogBuffer = Arc<Mutex<VecDeque<String>>>;

struct Target {
    x0: u16,
    x1: u16,
    slot: usize,
    seg: Segment,
}

pub fn run(
    widgets: Vec<WidgetHandle>,
    frames: Receiver<Frame>,
    logs: LogBuffer,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = event_loop(&mut terminal, &widgets, &frames, &logs);
    execute!(std::io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    widgets: &[WidgetHandle],
    frames: &Receiver<Frame>,
    logs: &LogBuffer,
) -> anyhow::Result<()> {
    let mut tree: Vec<Option<Node>> = vec![None; widgets.len()];
    let mut focus = 0usize;
    let mut targets: Vec<Target> = Vec::new();

    loop {
        while let Ok(f) = frames.try_recv() {
            tree[f.slot] = Some(f.node);
        }

        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Block::new().style(RStyle::new().bg(BG)), area);
            let [bar, _, body] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(area);

            let (line, t) = bar_line(&tree, widgets, focus);
            targets = t;
            frame.render_widget(Paragraph::new(line).style(RStyle::new().bg(PANEL)), bar);

            let help = Line::from(vec![
                Span::styled(" ←/→ ", RStyle::new().fg(ACCENT)),
                Span::styled("focus  ", RStyle::new().fg(MUTED)),
                Span::styled("enter ", RStyle::new().fg(ACCENT)),
                Span::styled("click  ", RStyle::new().fg(MUTED)),
                Span::styled("↑/↓ ", RStyle::new().fg(ACCENT)),
                Span::styled("scroll  ", RStyle::new().fg(MUTED)),
                Span::styled("q ", RStyle::new().fg(ACCENT)),
                Span::styled("quit   ", RStyle::new().fg(MUTED)),
                Span::styled(
                    "edit any widget file to hot-reload it",
                    RStyle::new().fg(MUTED).add_modifier(Modifier::ITALIC),
                ),
            ]);
            let logs: Vec<Line> = {
                let buf = logs.lock().unwrap();
                let height = body.height.saturating_sub(3) as usize;
                buf.iter()
                    .rev()
                    .take(height)
                    .rev()
                    .map(|l| Line::styled(l.clone(), RStyle::new().fg(FG)))
                    .collect()
            };
            let mut lines = vec![help, Line::raw("")];
            lines.extend(logs);
            frame.render_widget(
                Paragraph::new(lines).block(
                    Block::new()
                        .borders(Borders::TOP)
                        .border_style(RStyle::new().fg(MUTED))
                        .title(Span::styled(" marathon · log ", RStyle::new().fg(MUTED))),
                ),
                body,
            );
        })?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let n = targets.len().max(1);
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => focus = (focus + 1) % n,
                KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                    focus = (focus + n - 1) % n
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    fire(widgets, targets.get(focus), Fire::Click)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    fire(widgets, targets.get(focus), Fire::Scroll(1))
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    fire(widgets, targets.get(focus), Fire::Scroll(-1))
                }
                _ => {}
            },
            Event::Mouse(m) if m.row == 0 => {
                let hit = targets
                    .iter()
                    .position(|t| (t.x0..t.x1).contains(&m.column));
                if let Some(i) = hit {
                    focus = i;
                    match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            fire(widgets, targets.get(i), Fire::Click)
                        }
                        MouseEventKind::ScrollUp => fire(widgets, targets.get(i), Fire::Scroll(1)),
                        MouseEventKind::ScrollDown => {
                            fire(widgets, targets.get(i), Fire::Scroll(-1))
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

enum Fire {
    Click,
    Scroll(i32),
}

fn fire(widgets: &[WidgetHandle], target: Option<&Target>, what: Fire) {
    let Some(t) = target else { return };
    let ev = match what {
        Fire::Click => t.seg.click.map(UiEvent::Click),
        Fire::Scroll(d) => t.seg.scroll.map(|h| UiEvent::Scroll(h, d)),
    };
    if let Some(ev) = ev {
        let _ = widgets[t.slot].ui.send(ev);
    }
}

fn tone_color(t: Tone) -> Color {
    match t {
        Tone::Normal => FG,
        Tone::Muted => MUTED,
        Tone::Accent => ACCENT,
        Tone::Warning => WARN,
        Tone::Critical => CRIT,
    }
}

fn bar_line(
    tree: &[Option<Node>],
    widgets: &[WidgetHandle],
    focus: usize,
) -> (Line<'static>, Vec<Target>) {
    let mut spans = vec![
        Span::styled(
            " marathon ",
            RStyle::new().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    let mut x: u16 = spans.iter().map(|s| s.width() as u16).sum();
    let mut targets = Vec::new();

    for (slot, node) in tree.iter().enumerate() {
        if slot > 0 {
            spans.push(Span::styled(" │ ", RStyle::new().fg(MUTED)));
            x += 3;
        }
        let Some(node) = node else {
            let s = format!("{}…", widgets[slot].name);
            x += s.chars().count() as u16;
            spans.push(Span::styled(s, RStyle::new().fg(MUTED)));
            continue;
        };
        let mut segs = Vec::new();
        flatten(node, &mut segs);
        for seg in segs {
            let w = Span::raw(seg.text.clone()).width() as u16;
            let focused = seg.interactive() && targets.len() == focus;
            let mut style = match seg.style {
                Style::Plain(t) => RStyle::new().fg(tone_color(t)),
                Style::Pill { active: false } => {
                    RStyle::new().fg(FG).bg(Color::Rgb(0x27, 0x2d, 0x38))
                }
                Style::Pill { active: true } => {
                    RStyle::new().fg(BG).bg(CYAN).add_modifier(Modifier::BOLD)
                }
                Style::Button => RStyle::new().fg(ACCENT),
                Style::Error => RStyle::new().fg(CRIT).add_modifier(Modifier::BOLD),
            };
            if focused {
                style = style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
            }
            if seg.interactive() {
                targets.push(Target {
                    x0: x,
                    x1: x + w,
                    slot,
                    seg: seg.clone(),
                });
            }
            spans.push(Span::styled(seg.text, style));
            x += w;
        }
    }
    (Line::from(spans), targets)
}
