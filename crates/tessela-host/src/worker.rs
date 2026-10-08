//! A small pool of threads hosting every extension (docs/adr/0013).
//!
//! Engines are `!Send`, so each extension is pinned to one worker for its
//! whole life and is created there. A worker multiplexes module deliveries
//! and UI events for all its extensions, renders only the ones that became
//! dirty, and polls their files for hot reload. An extension that runs away
//! is interrupted by its CPU budget, so the most it can delay its neighbours
//! is one budget (50 ms by default).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender, select, tick, unbounded};
use tessela_core::view::{Reconciler, Vocabulary};
use tessela_core::{
    EngineFactory, Inbound, Instance, InstanceSpec, Module, Node, PersistStore, Sink,
};

use crate::{Frame, Manifest, Slot, UiEvent};

pub(crate) struct Shared {
    pub engines: Vec<Arc<dyn EngineFactory>>,
    pub modules: Vec<Arc<dyn Module>>,
    pub vocabulary: Arc<Vocabulary>,
    pub supported: HashSet<String>,
    pub frames: Sender<Frame>,
}

pub(crate) enum Msg {
    Inbound {
        slot: usize,
        generation: u64,
        inbound: Inbound,
    },
    Ui {
        slot: usize,
        event: UiEvent,
    },
}

/// Returns the slots and the number of worker threads started.
pub(crate) fn spawn_pool(
    shared: Arc<Shared>,
    dirs: Vec<PathBuf>,
    workers: usize,
) -> (Vec<Slot>, usize) {
    let workers = workers.min(dirs.len().max(1));
    let channels: Vec<(Sender<Msg>, Receiver<Msg>)> = (0..workers).map(|_| unbounded()).collect();
    let mut assigned: Vec<Vec<(usize, PathBuf)>> = vec![Vec::new(); workers];
    let mut slots = Vec::with_capacity(dirs.len());
    for (index, dir) in dirs.into_iter().enumerate() {
        let w = index % workers;
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        slots.push(Slot {
            name,
            index,
            tx: channels[w].0.clone(),
        });
        assigned[w].push((index, dir));
    }
    for (w, ((tx, rx), exts)) in channels.into_iter().zip(assigned).enumerate() {
        let shared = shared.clone();
        thread::Builder::new()
            .name(format!("tessela-w{w}"))
            .spawn(move || Worker::new(shared, tx, exts).run(rx))
            .expect("spawn worker");
    }
    (slots, workers)
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn stamp(files: &[PathBuf]) -> Vec<Option<SystemTime>> {
    files.iter().map(|f| mtime(f)).collect()
}

struct Ext {
    slot: usize,
    dir: PathBuf,
    name: String,
    inst: Option<Instance>,
    generation: u64,
    watched: Vec<PathBuf>,
    loaded_at: Vec<Option<SystemTime>>,
    reconciler: Reconciler,
    persist: PersistStore,
    dirty: bool,
}

struct Worker {
    shared: Arc<Shared>,
    me: Sender<Msg>,
    exts: Vec<Ext>,
}

impl Worker {
    fn new(shared: Arc<Shared>, me: Sender<Msg>, assigned: Vec<(usize, PathBuf)>) -> Self {
        let exts = assigned
            .into_iter()
            .map(|(slot, dir)| Ext {
                slot,
                name: dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                dir,
                inst: None,
                generation: 0,
                watched: Vec::new(),
                loaded_at: Vec::new(),
                reconciler: Reconciler::new(),
                persist: PersistStore::default(),
                dirty: false,
            })
            .collect();
        Self { shared, me, exts }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        for i in 0..self.exts.len() {
            self.load(i);
        }
        // Cached values for live subscriptions arrive within microseconds;
        // take them before the first frames to avoid a "no data" flicker.
        self.drain_for(&rx, Duration::from_millis(15));
        self.render_dirty(true);

        let poll = tick(Duration::from_millis(400));
        loop {
            select! {
                recv(rx) -> m => {
                    let Ok(m) = m else { return };
                    self.handle(m);
                    while let Ok(m) = rx.try_recv() {
                        self.handle(m);
                    }
                }
                recv(poll) -> _ => {
                    let mut reloaded = false;
                    for i in 0..self.exts.len() {
                        if stamp(&self.exts[i].watched) != self.exts[i].loaded_at {
                            log::info!("{}: source changed, reloading", self.exts[i].name);
                            self.load(i);
                            reloaded = true;
                        }
                    }
                    if reloaded {
                        self.drain_for(&rx, Duration::from_millis(15));
                    }
                }
            }
            self.render_dirty(false);
        }
    }

    fn drain_for(&mut self, rx: &Receiver<Msg>, d: Duration) {
        let until = Instant::now() + d;
        while let Ok(m) = rx.recv_deadline(until) {
            self.handle(m);
        }
    }

    fn handle(&mut self, m: Msg) {
        let (slot, result) = match m {
            Msg::Inbound {
                slot,
                generation,
                inbound,
            } => {
                let Some(ext) = self.exts.iter_mut().find(|e| e.slot == slot) else {
                    return;
                };
                if ext.generation != generation {
                    return; // addressed to an instance that was reloaded away
                }
                let Some(inst) = ext.inst.as_mut() else {
                    return;
                };
                (slot, inst.deliver(inbound))
            }
            Msg::Ui { slot, event } => {
                let Some(ext) = self.exts.iter_mut().find(|e| e.slot == slot) else {
                    return;
                };
                let Some(inst) = ext.inst.as_mut() else {
                    return;
                };
                (slot, inst.trigger(&event.handler, event.payload))
            }
        };
        let ext = self
            .exts
            .iter_mut()
            .find(|e| e.slot == slot)
            .expect("found above");
        if let Err(e) = result {
            log::warn!("{}: {e}", ext.name);
        }
        if let Some(inst) = ext.inst.as_mut() {
            for line in inst.take_logs() {
                log::info!("[{}] {line}", inst.name);
            }
            ext.dirty |= inst.needs_render();
        }
    }

    fn load(&mut self, i: usize) {
        let shared = self.shared.clone();
        let me = self.me.clone();
        let ext = &mut self.exts[i];
        // Drop the old instance first: it releases its module resources.
        ext.inst = None;
        ext.generation += 1;
        ext.watched = vec![ext.dir.join("widget.toml")];
        let (slot, generation) = (ext.slot, ext.generation);
        let sink = Sink::new(move |inbound| {
            let _ = me.send(Msg::Inbound {
                slot,
                generation,
                inbound,
            });
        });
        let result = (|| -> Result<Instance, String> {
            let manifest = Manifest::load(&ext.dir).map_err(|e| format!("manifest: {e}"))?;
            ext.watched.push(manifest.entry.clone());
            let source = std::fs::read_to_string(&manifest.entry).map_err(|e| e.to_string())?;
            let file = manifest
                .entry
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            Instance::new(InstanceSpec {
                name: &manifest.name,
                file: &file,
                source: &source,
                grants: manifest.grants(),
                limits: manifest.limits(),
                engines: &shared.engines,
                modules: shared.modules.clone(),
                vocabulary: shared.vocabulary.clone(),
                sink,
                persist: Some(ext.persist.clone()),
            })
            .map_err(|e| e.0)
        })();
        ext.loaded_at = stamp(&ext.watched);
        match result {
            Ok(mut inst) => {
                for line in inst.take_logs() {
                    log::info!("[{}] {line}", inst.name);
                }
                ext.inst = Some(inst);
                ext.dirty = true;
            }
            Err(message) => {
                log::warn!("{}: {message}", ext.name);
                let node = Node::error(&ext.name, &message);
                Self::emit(&shared, ext, node);
            }
        }
    }

    fn render_dirty(&mut self, all: bool) {
        let shared = self.shared.clone();
        for ext in &mut self.exts {
            if !(ext.dirty || all) {
                continue;
            }
            ext.dirty = false;
            let Some(inst) = ext.inst.as_mut() else {
                continue;
            };
            let node = inst.render();
            for line in inst.take_logs() {
                log::info!("[{}] {line}", inst.name);
            }
            if node.is(tessela_core::view::ERROR) {
                log::warn!("{}: {}", ext.name, node.str("message").unwrap_or(""));
            }
            Self::emit(&shared, ext, node);
        }
    }

    fn emit(shared: &Shared, ext: &mut Ext, node: Node) {
        let lowered = shared
            .vocabulary
            .lower(node, &|k| shared.supported.contains(k));
        let patches = ext.reconciler.update(&lowered);
        if !patches.is_empty() {
            let _ = shared.frames.send(Frame {
                slot: ext.slot,
                patches,
            });
        }
    }
}
