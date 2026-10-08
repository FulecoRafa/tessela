//! Runs extensions and connects them to a renderer.
//!
//! The host owns everything with side effects that the core refuses to own:
//! threads, files, hot reload and the main loop handed to a [`Renderer`].
//!
//! ```text
//!   extension dir ──▶ worker thread (engine + graph) ──Frame──▶ Renderer
//!                          ▲            │ invoke                 │
//!                          │ Inbound    ▼                        │ UiEvent
//!                          └────────  modules  ◀─────────────────┘
//! ```

mod headless;
mod manifest;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use tessela_core::{EngineFactory, FnId, Module, Node};

pub use headless::Headless;
pub use manifest::Manifest;

/// A new tree for one slot. Phase 0 sends whole trees; patches come with the
/// remote renderer (docs/adr/0005).
#[derive(Debug, Clone)]
pub struct Frame {
    pub slot: usize,
    pub node: Node,
}

/// Input from the renderer, addressed to a handler in one slot's extension.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    Click(FnId),
    Scroll(FnId, i32),
}

pub struct Slot {
    pub name: String,
    pub ui: Sender<UiEvent>,
}

/// What a renderer gets: one slot per extension, and the stream of frames.
pub struct Surface {
    pub slots: Vec<Slot>,
    pub frames: Receiver<Frame>,
}

impl Surface {
    pub fn send(&self, slot: usize, event: UiEvent) {
        if let Some(s) = self.slots.get(slot) {
            let _ = s.ui.send(event);
        }
    }
}

/// Draws frames and turns user input into [`UiEvent`]s. Takes over the
/// calling thread (GUI toolkits need the main thread) until the app exits.
pub trait Renderer {
    fn run(self, surface: Surface) -> anyhow::Result<()>;
}

#[derive(Default)]
pub struct AppBuilder {
    engines: Vec<Arc<dyn EngineFactory>>,
    modules: Vec<Arc<dyn Module>>,
    extensions: Vec<PathBuf>,
}

impl AppBuilder {
    pub fn engine(mut self, e: impl EngineFactory) -> Self {
        self.engines.push(Arc::new(e));
        self
    }

    pub fn engines(mut self, e: impl IntoIterator<Item = Arc<dyn EngineFactory>>) -> Self {
        self.engines.extend(e);
        self
    }

    pub fn module(mut self, m: impl Module) -> Self {
        self.modules.push(Arc::new(m));
        self
    }

    pub fn shared_module(mut self, m: Arc<dyn Module>) -> Self {
        self.modules.push(m);
        self
    }

    /// A directory with `widget.toml`; becomes the next slot.
    pub fn extension(mut self, dir: impl Into<PathBuf>) -> Self {
        self.extensions.push(dir.into());
        self
    }

    pub fn build(self) -> anyhow::Result<App> {
        anyhow::ensure!(
            !self.engines.is_empty(),
            "no engines registered; enable the `luau` or `quickjs` feature"
        );
        Ok(App {
            engines: self.engines,
            modules: self.modules,
            extensions: self.extensions,
        })
    }
}

pub struct App {
    engines: Vec<Arc<dyn EngineFactory>>,
    modules: Vec<Arc<dyn Module>>,
    extensions: Vec<PathBuf>,
}

impl App {
    pub fn builder() -> AppBuilder {
        AppBuilder::default()
    }

    /// Start one worker per extension and hand the surface to `renderer`.
    pub fn run(self, renderer: impl Renderer) -> anyhow::Result<()> {
        renderer.run(self.start())
    }

    /// Start the workers without a renderer, for embedding and tests.
    pub fn start(self) -> Surface {
        let (frames_tx, frames) = unbounded();
        let slots = self
            .extensions
            .into_iter()
            .enumerate()
            .map(|(slot, dir)| {
                worker::spawn(
                    slot,
                    dir,
                    self.engines.clone(),
                    self.modules.clone(),
                    frames_tx.clone(),
                )
            })
            .collect();
        Surface { slots, frames }
    }
}
