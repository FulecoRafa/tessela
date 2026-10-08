use std::io::Write;
use std::time::{Duration, Instant};

use tessela_view::text::{flatten, to_plain};

use crate::{Renderer, Surface, UiEvent};

/// Prints the surface as one line of text whenever it changes. Optionally
/// stops after a duration and clicks extensions on a schedule, which makes
/// end-to-end tests and benchmarks possible without a display.
pub struct Headless {
    duration: Option<Duration>,
    clicks: Vec<(String, Duration)>,
    out: Box<dyn Write>,
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
            clicks: Vec::new(),
            out: Box::new(std::io::stdout()),
        }
    }

    pub fn for_duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }

    /// Click the first clickable element of slot `name` at `at`.
    pub fn click(mut self, name: impl Into<String>, at: Duration) -> Self {
        self.clicks.push((name.into(), at));
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
        let mut tree = vec![None; surface.slots.len()];
        let mut last = String::new();
        loop {
            let elapsed = start.elapsed();
            if self.duration.is_some_and(|d| elapsed >= d) {
                return Ok(());
            }
            self.clicks.retain(|(name, at)| {
                if elapsed < *at {
                    return true;
                }
                let Some(slot) = surface.slots.iter().position(|s| &s.name == name) else {
                    return false;
                };
                let mut segs = Vec::new();
                if let Some(node) = &tree[slot] {
                    flatten(node, &mut segs);
                }
                match segs.iter().find_map(|s| s.click) {
                    Some(h) => {
                        log::info!("(headless) click {name}");
                        surface.send(slot, UiEvent::Click(h));
                    }
                    None => log::warn!("(headless) {name} has nothing clickable"),
                }
                false
            });
            if let Ok(f) = surface.frames.recv_timeout(Duration::from_millis(100)) {
                tree[f.slot] = Some(f.node);
                while let Ok(f) = surface.frames.try_recv() {
                    tree[f.slot] = Some(f.node);
                }
                let line = to_plain(&tree);
                if line != last {
                    writeln!(self.out, "{:>6.2}s  {line}", start.elapsed().as_secs_f64())?;
                    last = line;
                }
            }
        }
    }
}
