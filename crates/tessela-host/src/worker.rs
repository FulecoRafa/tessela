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
    EngineFactory, Inbound, Instance, InstanceSpec, InvokeTrace, Module, Node, PersistStore, Sink,
};

use crate::probe::{GraphStats, Probe};
use crate::sources::{FileSources, Origin};
use crate::{Frame, Slot, UiEvent};

pub(crate) struct Shared {
    pub engines: Vec<Arc<dyn EngineFactory>>,
    pub modules: Vec<Arc<dyn Module>>,
    pub vocabulary: Arc<Vocabulary>,
    pub supported: HashSet<String>,
    pub frames: Sender<Frame>,
    pub probe: Option<Arc<dyn Probe>>,
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
    origins: Vec<Origin>,
    workers: usize,
) -> (Vec<Slot>, usize) {
    let workers = workers.min(origins.len().max(1));
    let channels: Vec<(Sender<Msg>, Receiver<Msg>)> = (0..workers).map(|_| unbounded()).collect();
    let mut assigned: Vec<Vec<(usize, Origin)>> = vec![Vec::new(); workers];
    let mut slots = Vec::with_capacity(origins.len());
    for (index, origin) in origins.into_iter().enumerate() {
        let w = index % workers;
        slots.push(Slot {
            name: origin.name(),
            index,
            tx: channels[w].0.clone(),
        });
        assigned[w].push((index, origin));
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
    origin: Origin,
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
    fn new(shared: Arc<Shared>, me: Sender<Msg>, assigned: Vec<(usize, Origin)>) -> Self {
        let exts = assigned
            .into_iter()
            .map(|(slot, origin)| Ext {
                slot,
                name: origin.name(),
                origin,
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
        let probe = self.shared.probe.clone();
        let started = Instant::now();
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
                let seen = probe.as_ref().map(|_| inbound.clone());
                let r = inst.deliver(inbound);
                if let (Some(p), Some(inbound)) = (&probe, seen) {
                    p.delivered(
                        slot,
                        &inbound,
                        started.elapsed(),
                        r.as_ref().err().map(|e| e.0.as_str()),
                    );
                }
                (slot, r)
            }
            Msg::Ui { slot, event } => {
                let Some(ext) = self.exts.iter_mut().find(|e| e.slot == slot) else {
                    return;
                };
                let Some(inst) = ext.inst.as_mut() else {
                    return;
                };
                let r = inst.trigger(&event.handler, event.payload.clone());
                if let Some(p) = &probe {
                    p.event(
                        slot,
                        &event,
                        started.elapsed(),
                        r.as_ref().err().map(|e| e.0.as_str()),
                    );
                }
                (slot, r)
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
            Self::logs(&probe, slot, inst);
            ext.dirty |= inst.needs_render();
        }
    }

    fn logs(probe: &Option<Arc<dyn Probe>>, slot: usize, inst: &mut Instance) {
        for line in inst.take_log_lines() {
            log::info!(target: "tessela::guest", "[{}] {}", inst.name, line.text);
            if let Some(p) = probe {
                p.log_at(slot, &line.text, line.at.as_deref());
            }
        }
    }

    fn load(&mut self, i: usize) {
        let shared = self.shared.clone();
        let me = self.me.clone();
        let ext = &mut self.exts[i];
        // Drop the old instance first: it releases its module resources.
        ext.inst = None;
        ext.generation += 1;
        let (slot, generation) = (ext.slot, ext.generation);
        let sink = Sink::new(move |inbound| {
            let _ = me.send(Msg::Inbound {
                slot,
                generation,
                inbound,
            });
        });
        let trace: Option<InvokeTrace> = shared.probe.clone().map(|p| {
            let trace: InvokeTrace = Arc::new(move |record| p.invoke(slot, record));
            trace
        });
        let started = Instant::now();
        let mut watched = Vec::new();
        let mut name = ext.name.clone();
        let result = (|| -> Result<Instance, String> {
            let r = ext.origin.resolve()?;
            name = r.name.clone();
            watched.extend(r.config);
            watched.push(r.root.join(&r.file));
            let sources = Arc::new(FileSources::with_packages(&r.root, &r.packages));
            let read = sources.clone();
            let source = tessela_core::Sources::read(&*sources, &r.file)
                .ok_or_else(|| format!("cannot read {}", r.root.join(&r.file).display()))?;
            let inst = Instance::new(InstanceSpec {
                name: &r.name,
                file: &r.file,
                source: &source,
                grants: r.grants,
                limits: r.limits,
                engines: &shared.engines,
                modules: shared.modules.clone(),
                vocabulary: shared.vocabulary.clone(),
                sink,
                persist: Some(ext.persist.clone()),
                sources: Some(sources),
                trace,
            });
            // Imports read so far are watched even if loading failed, so
            // fixing any of them reloads.
            watched.extend(read.read_files());
            inst.map_err(|e| e.0)
        })();
        watched.sort();
        watched.dedup();
        ext.watched = watched;
        ext.loaded_at = stamp(&ext.watched);
        match result {
            Ok(mut inst) => {
                if let Some(p) = &shared.probe {
                    p.loaded(slot, &name, &ext.watched, started.elapsed());
                }
                Self::logs(&shared.probe, slot, &mut inst);
                ext.inst = Some(inst);
                ext.dirty = true;
            }
            Err(message) => {
                log::warn!("{}: {message}", ext.name);
                if let Some(p) = &shared.probe {
                    p.failed(slot, &name, &message);
                }
                let node = Node::error(&ext.name, &message);
                Self::emit(&shared, ext, node, Instant::now(), GraphStats::default());
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
            let started = Instant::now();
            let node = inst.render();
            Self::logs(&shared.probe, ext.slot, inst);
            if node.is(tessela_core::view::ERROR) {
                log::warn!("{}: {}", ext.name, node.str("message").unwrap_or(""));
            }
            let stats = if shared.probe.is_some() {
                let (bindings, signals) = inst.live_counts();
                GraphStats {
                    bindings,
                    signals,
                    resources: inst.live_resources(),
                }
            } else {
                GraphStats::default()
            };
            Self::emit(&shared, ext, node, started, stats);
        }
    }

    fn emit(shared: &Shared, ext: &mut Ext, node: Node, started: Instant, stats: GraphStats) {
        let lowered = shared
            .vocabulary
            .lower(node, &|k| shared.supported.contains(k));
        let patches = ext.reconciler.update(&lowered);
        let frame = (!patches.is_empty()).then(|| Frame {
            slot: ext.slot,
            patches,
        });
        if let Some(p) = &shared.probe {
            p.rendered(ext.slot, started.elapsed(), frame.as_ref(), stats);
        }
        if let Some(frame) = frame {
            let _ = shared.frames.send(frame);
        }
    }
}
