//! One OS thread per extension (phase 1 replaces this with a worker pool).
//!
//! Engines are `!Send`, so each instance is created and lives entirely on
//! its own thread. The thread multiplexes three inputs — module deliveries,
//! UI events from the renderer and a hot-reload poll — and emits frames. An
//! extension that stalls only stalls its own thread; the CPU budget then
//! interrupts it and the rest keeps updating.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender, select, tick, unbounded};
use serde_json::{Value, json};
use tessela_core::{EngineFactory, Inbound, Instance, InstanceSpec, Module, Node, Sink};

use crate::{Frame, Manifest, Slot, UiEvent};

pub(crate) fn spawn(
    slot: usize,
    dir: PathBuf,
    engines: Vec<Arc<dyn EngineFactory>>,
    modules: Vec<Arc<dyn Module>>,
    frames: Sender<Frame>,
) -> Slot {
    let (ui_tx, ui_rx) = unbounded();
    let name = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    thread::Builder::new()
        .name(format!("ext-{name}"))
        .spawn(move || {
            Worker {
                slot,
                dir,
                engines,
                modules,
                frames,
            }
            .run(ui_rx)
        })
        .expect("spawn extension thread");
    Slot { name, ui: ui_tx }
}

struct Worker {
    slot: usize,
    dir: PathBuf,
    engines: Vec<Arc<dyn EngineFactory>>,
    modules: Vec<Arc<dyn Module>>,
    frames: Sender<Frame>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn stamp(files: &[PathBuf]) -> Vec<Option<SystemTime>> {
    files.iter().map(|f| mtime(f)).collect()
}

impl Worker {
    fn run(self, ui: Receiver<UiEvent>) {
        let poll = tick(Duration::from_millis(400));
        loop {
            // Each (re)load gets a fresh inbound channel, so deliveries meant
            // for the previous generation can never reach this one.
            let (in_tx, in_rx) = unbounded::<Inbound>();
            let sink = Sink::new(move |i| {
                let _ = in_tx.send(i);
            });
            let (mut inst, watched) = match self.load(sink) {
                Ok(ok) => ok,
                Err((message, watched)) => {
                    self.emit(Node::Error {
                        widget: self.name(),
                        message,
                    });
                    self.wait_for_change(&watched, &poll);
                    continue;
                }
            };
            let loaded_at = stamp(&watched);
            self.drain_logs(&mut inst);
            // Cached values for live subscriptions arrive within microseconds;
            // take them before the first frame to avoid a "no data" flicker
            // on hot reload.
            let settle = Instant::now() + Duration::from_millis(15);
            while let Ok(i) = in_rx.recv_deadline(settle) {
                self.deliver(&mut inst, i);
            }
            self.emit(inst.render());

            loop {
                let mut changed = false;
                select! {
                    recv(in_rx) -> i => {
                        self.deliver(&mut inst, i.expect("the sink holds a sender"));
                        changed = true;
                    }
                    recv(ui) -> ev => {
                        let Ok(ev) = ev else { return };
                        let (handler, args) = match ev {
                            UiEvent::Click(h) => (h, Value::Null),
                            UiEvent::Scroll(h, delta) => (h, json!({ "delta": delta })),
                        };
                        if let Err(e) = inst.ui(handler, args) {
                            log::warn!("{}: {e}", self.name());
                        }
                        changed = true;
                    }
                    recv(poll) -> _ => {
                        if stamp(&watched) != loaded_at {
                            log::info!("{}: source changed, reloading", self.name());
                            break;
                        }
                    }
                }
                self.drain_logs(&mut inst);
                if changed && inst.needs_render() {
                    self.emit(inst.render());
                }
            }
            // Dropping the instance releases its module resources.
        }
    }

    fn name(&self) -> String {
        self.dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }

    fn load(&self, sink: Sink) -> Result<(Instance, Vec<PathBuf>), (String, Vec<PathBuf>)> {
        let mut watched = vec![self.dir.join("widget.toml")];
        let manifest =
            Manifest::load(&self.dir).map_err(|e| (format!("manifest: {e}"), watched.clone()))?;
        watched.push(manifest.entry.clone());
        let source = std::fs::read_to_string(&manifest.entry)
            .map_err(|e| (e.to_string(), watched.clone()))?;
        let file = manifest
            .entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let inst = Instance::new(InstanceSpec {
            name: &manifest.name,
            file: &file,
            source: &source,
            grants: manifest.grants(),
            limits: manifest.limits(),
            engines: &self.engines,
            modules: self.modules.clone(),
            sink,
        })
        .map_err(|e| (e.to_string(), watched.clone()))?;
        Ok((inst, watched))
    }

    fn wait_for_change(&self, watched: &[PathBuf], poll: &Receiver<Instant>) {
        let before = stamp(watched);
        loop {
            let _ = poll.recv();
            if stamp(watched) != before {
                return;
            }
        }
    }

    fn deliver(&self, inst: &mut Instance, i: Inbound) {
        if let Err(e) = inst.deliver(i) {
            log::warn!("{}: {e}", self.name());
        }
    }

    fn drain_logs(&self, inst: &mut Instance) {
        for line in inst.take_logs() {
            log::info!("[{}] {line}", inst.name);
        }
    }

    fn emit(&self, node: Node) {
        if let Node::Error { widget, message } = &node {
            log::warn!("{widget}: {message}");
        }
        let _ = self.frames.send(Frame {
            slot: self.slot,
            node,
        });
    }
}
