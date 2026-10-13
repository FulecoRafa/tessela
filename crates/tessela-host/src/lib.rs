//! Runs extensions and connects them to a renderer.
//!
//! The host owns everything with side effects that the core refuses to own:
//! threads, files, hot reload and the main loop handed to a [`Renderer`].
//!
//! ```text
//!   extension dirs ──▶ worker pool (engines + graphs) ──Frame(patches)──▶ Renderer
//!                          ▲             │ invoke                            │
//!                          │ Inbound     ▼                                   │ UiEvent
//!                          └─────────  modules  ◀────────────────────────────┘
//! ```

mod headless;
mod manifest;
mod sources;
mod worker;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::Value;
use tessela_core::view::{CORE, ComponentSpec, Patch, ViewTree, Vocabulary};
use tessela_core::{EngineFactory, Handler, Module, Node};

pub use headless::Headless;
pub use manifest::Manifest;
pub use sources::Entry;

use sources::Origin;

/// Changes to one slot's tree, as patches with stable node ids
/// (docs/adr/0011). Apply them with [`Trees`].
#[derive(Debug, Clone)]
pub struct Frame {
    pub slot: usize,
    pub patches: Vec<Patch>,
}

/// The user triggered an event prop: send back the prop's [`Handler`] and the
/// event data (`null` for clicks, `{delta}` for scroll).
#[derive(Debug, Clone, PartialEq)]
pub struct UiEvent {
    pub handler: Handler,
    pub payload: Value,
}

impl UiEvent {
    pub fn new(handler: Handler, payload: Value) -> Self {
        Self { handler, payload }
    }
}

/// One extension's place on the surface.
#[derive(Clone)]
pub struct Slot {
    pub name: String,
    index: usize,
    tx: Sender<worker::Msg>,
}

impl Slot {
    pub fn send(&self, event: UiEvent) {
        let _ = self.tx.send(worker::Msg::Ui {
            slot: self.index,
            event,
        });
    }
}

/// What a renderer gets: one slot per extension, and the stream of frames.
pub struct Surface {
    pub slots: Vec<Slot>,
    pub frames: Receiver<Frame>,
    /// Worker threads hosting the slots' extensions.
    pub workers: usize,
}

impl Surface {
    pub fn send(&self, slot: usize, event: UiEvent) {
        if let Some(s) = self.slots.get(slot) {
            s.send(event);
        }
    }
}

/// Renderer-side mirror of every slot's tree.
pub struct Trees {
    trees: Vec<ViewTree>,
    nodes: Vec<Option<Node>>,
}

impl Trees {
    pub fn new(slots: usize) -> Self {
        Self {
            trees: vec![ViewTree::default(); slots],
            nodes: vec![None; slots],
        }
    }

    pub fn apply(&mut self, frame: &Frame) {
        let Some(tree) = self.trees.get_mut(frame.slot) else {
            return;
        };
        if let Err(e) = tree.apply(&frame.patches) {
            log::error!("slot {}: bad patch stream: {e}", frame.slot);
            return;
        }
        self.nodes[frame.slot] = tree.to_node();
    }

    /// Current tree of every slot (`None` until the first frame).
    pub fn nodes(&self) -> &[Option<Node>] {
        &self.nodes
    }

    pub fn tree(&self, slot: usize) -> Option<&ViewTree> {
        self.trees.get(slot)
    }
}

/// Draws frames and turns user input into [`UiEvent`]s. Takes over the
/// calling thread (GUI toolkits need the main thread) until the app exits.
pub trait Renderer {
    /// Component kinds this renderer draws natively. Anything else is
    /// rewritten through its fallback before frames are sent.
    fn supports(&self, kind: &str) -> bool {
        CORE.contains(&kind)
    }

    fn run(self, surface: Surface) -> anyhow::Result<()>;
}

pub struct AppBuilder {
    engines: Vec<Arc<dyn EngineFactory>>,
    modules: Vec<Arc<dyn Module>>,
    vocabulary: Vocabulary,
    extensions: Vec<Origin>,
    workers: usize,
}

impl Default for AppBuilder {
    fn default() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        Self {
            engines: Vec::new(),
            modules: Vec::new(),
            vocabulary: Vocabulary::default(),
            extensions: Vec::new(),
            workers: cores.clamp(1, 4),
        }
    }
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

    /// An app component: guests can construct it; renderers that do not draw
    /// it get its fallback (docs/adr/0003).
    pub fn component(mut self, spec: ComponentSpec) -> anyhow::Result<Self> {
        self.vocabulary.register(spec).map_err(anyhow::Error::msg)?;
        Ok(self)
    }

    /// A directory with `widget.toml`; becomes the next slot.
    pub fn extension(mut self, dir: impl Into<PathBuf>) -> Self {
        self.extensions.push(Origin::Widget(dir.into()));
        self
    }

    /// An entry of a project (docs/adr/0015); becomes the next slot. Its
    /// file may import other files under `entry.root`.
    pub fn entry(mut self, entry: Entry) -> Self {
        self.extensions.push(Origin::Entry(entry));
        self
    }

    /// Worker threads shared by all extensions (default: cores, at most 4).
    pub fn workers(mut self, n: usize) -> Self {
        self.workers = n.max(1);
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
            vocabulary: Arc::new(self.vocabulary),
            extensions: self.extensions,
            workers: self.workers,
        })
    }
}

pub struct App {
    engines: Vec<Arc<dyn EngineFactory>>,
    modules: Vec<Arc<dyn Module>>,
    vocabulary: Arc<Vocabulary>,
    extensions: Vec<Origin>,
    workers: usize,
}

impl App {
    pub fn builder() -> AppBuilder {
        AppBuilder::default()
    }

    pub fn vocabulary(&self) -> &Vocabulary {
        &self.vocabulary
    }

    /// Start the workers and hand the surface to `renderer`.
    pub fn run(self, renderer: impl Renderer) -> anyhow::Result<()> {
        let supported: HashSet<String> = self
            .vocabulary
            .guest_components()
            .map(|c| c.name)
            .chain(CORE.iter().copied())
            .filter(|k| renderer.supports(k))
            .map(String::from)
            .collect();
        renderer.run(self.start_with(supported))
    }

    /// Start the workers for a renderer that draws the core components only,
    /// for embedding and tests.
    pub fn start(self) -> Surface {
        self.start_with(CORE.iter().map(|s| s.to_string()).collect())
    }

    fn start_with(self, supported: HashSet<String>) -> Surface {
        let (frames_tx, frames) = unbounded();
        let shared = Arc::new(worker::Shared {
            engines: self.engines,
            modules: self.modules,
            vocabulary: self.vocabulary,
            supported,
            frames: frames_tx,
        });
        let (slots, workers) = worker::spawn_pool(shared, self.extensions, self.workers);
        Surface {
            slots,
            frames,
            workers,
        }
    }
}
