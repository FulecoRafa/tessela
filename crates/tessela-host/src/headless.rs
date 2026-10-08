use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::Value;
use tessela_core::view::text::{flatten, to_plain};

use crate::{Renderer, Surface, Trees, UiEvent};

/// Prints the surface as one line of text whenever it changes. Optionally
/// stops after a duration and clicks extensions on a schedule, which makes
/// end-to-end tests and benchmarks possible without a display.
pub struct Headless {
    duration: Option<Duration>,
    script: Vec<(String, Duration, Gesture)>,
    out: Box<dyn Write>,
}

#[derive(Debug, Clone, Copy)]
enum Gesture {
    Click,
    Scroll(i32),
}

impl Default for Headless {
    fn default() -> Self {
        Self::new()
    }
}

impl Headless {
    pub fn new() -> Self {
        Self {
            duration: None,
            script: Vec::new(),
            out: Box::new(std::io::stdout()),
        }
    }

    pub fn for_duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }

    /// Click the first clickable element of slot `name` at `at`.
    pub fn click(mut self, name: impl Into<String>, at: Duration) -> Self {
        self.script.push((name.into(), at, Gesture::Click));
        self
    }

    /// Scroll over the first scrollable element of slot `name` at `at`.
    pub fn scroll(mut self, name: impl Into<String>, at: Duration, delta: i32) -> Self {
        self.script.push((name.into(), at, Gesture::Scroll(delta)));
        self
    }

    pub fn output(mut self, w: impl Write + 'static) -> Self {
        self.out = Box::new(w);
        self
    }
}

impl Renderer for Headless {
    fn run(mut self, surface: Surface) -> anyhow::Result<()> {
        let start = Instant::now();
        let mut trees = Trees::new(surface.slots.len());
        let mut last = String::new();
        loop {
            let elapsed = start.elapsed();
            if self.duration.is_some_and(|d| elapsed >= d) {
                return Ok(());
            }
            self.script.retain(|(name, at, gesture)| {
                if elapsed < *at {
                    return true;
                }
                let Some(slot) = surface.slots.iter().position(|s| &s.name == name) else {
                    return false;
                };
                let mut segs = Vec::new();
                if let Some(node) = &trees.nodes()[slot] {
                    flatten(node, &mut segs);
                }
                let (handler, payload) = match gesture {
                    Gesture::Click => (segs.iter().find_map(|s| s.click.clone()), Value::Null),
                    Gesture::Scroll(d) => (
                        segs.iter().find_map(|s| s.scroll.clone()),
                        serde_json::json!({ "delta": d }),
                    ),
                };
                match handler {
                    Some(h) => {
                        log::info!("(headless) {gesture:?} {name}");
                        surface.send(slot, UiEvent::new(h, payload));
                    }
                    None => log::warn!("(headless) {name} does not handle {gesture:?}"),
                }
                false
            });
            if let Ok(f) = surface.frames.recv_timeout(Duration::from_millis(100)) {
                trees.apply(&f);
                while let Ok(f) = surface.frames.try_recv() {
                    trees.apply(&f);
                }
                let line = to_plain(trees.nodes());
                if line != last {
                    writeln!(self.out, "{:>6.2}s  {line}", start.elapsed().as_secs_f64())?;
                    last = line;
                }
            }
        }
    }
}
