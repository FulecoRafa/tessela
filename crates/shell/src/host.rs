//! One OS thread per widget.
//!
//! Engines are `!Send`, so each widget is created and lives entirely on its
//! own thread. The thread multiplexes three inputs — service deliveries,
//! UI events from the renderer, and a hot-reload poll — and emits frames.
//! A widget that stalls only stalls its own thread; the CPU budget then
//! interrupts it and the rest of the bar keeps updating.

use std::collections::HashSet;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime};

use connector::{Connector, Delivery};
use crossbeam_channel::{Receiver, Sender, select, tick, unbounded};
use runtime::{Effect, FnId, Input, Manifest, Node, Widget};
use serde_json::Value;

pub struct Frame {
    pub slot: usize,
    pub node: Node,
}

pub enum UiEvent {
    Click(FnId),
    Scroll(FnId, i32),
}

pub struct WidgetHandle {
    pub name: String,
    pub ui: Sender<UiEvent>,
}

pub fn spawn(
    slot: usize,
    dir: PathBuf,
    connector: Connector,
    frames: Sender<Frame>,
) -> WidgetHandle {
    let (ui_tx, ui_rx) = unbounded();
    let name = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let thread_name = format!("widget-{name}");
    thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            Worker {
                slot,
                dir,
                connector,
                frames,
                subs: HashSet::new(),
            }
            .run(ui_rx)
        })
        .expect("spawn widget thread");
    WidgetHandle { name, ui: ui_tx }
}

struct Worker {
    slot: usize,
    dir: PathBuf,
    connector: Connector,
    frames: Sender<Frame>,
    /// (service, path, token) currently subscribed, for cleanup on reload.
    subs: HashSet<(String, String, u64)>,
}

fn delivery_input(d: Delivery) -> Input {
    match d {
        Delivery::Event { token, value } => Input::Event { token, value },
        Delivery::Unavailable { token } => Input::Unavailable { token },
        Delivery::CallResult { token, result } => Input::CallResult { token, result },
    }
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Worker {
    fn run(mut self, ui: Receiver<UiEvent>) {
        let poll = tick(Duration::from_millis(400));
        loop {
            // Each (re)load gets a fresh delivery channel, so values addressed
            // to the previous generation's tokens can never reach this one.
            let (dtx, drx) = unbounded::<Delivery>();
            let (mut widget, watched) = match self.load() {
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
            let stamp = |files: &[PathBuf]| files.iter().map(|f| mtime(f)).collect::<Vec<_>>();
            let loaded_at = stamp(&watched);
            self.flush(&mut widget, &dtx);
            // Cached values for already-live subscriptions arrive within
            // microseconds; take them before the first frame to avoid a
            // one-frame "no data" flicker on hot reload.
            let settle = std::time::Instant::now() + Duration::from_millis(15);
            while let Ok(d) = drx.recv_deadline(settle) {
                self.input(&mut widget, delivery_input(d));
            }
            self.flush(&mut widget, &dtx);
            self.emit(widget.render());

            loop {
                let mut changed = false;
                select! {
                    recv(drx) -> d => {
                        self.input(&mut widget, delivery_input(d.expect("we hold a sender")));
                        changed = true;
                    }
                    recv(ui) -> ev => {
                        let Ok(ev) = ev else { return };
                        let input = match ev {
                            UiEvent::Click(handler) => Input::Ui { handler, args: Value::Null },
                            UiEvent::Scroll(handler, delta) => Input::Ui { handler, args: serde_json::json!({ "delta": delta }) },
                        };
                        self.input(&mut widget, input);
                        changed = true;
                    }
                    recv(poll) -> _ => {
                        if stamp(&watched) != loaded_at {
                            log::info!("{}: source changed, reloading", self.name());
                            break;
                        }
                    }
                }
                self.flush(&mut widget, &dtx);
                if changed && widget.needs_render() {
                    self.emit(widget.render());
                }
            }
            self.unsubscribe_all();
        }
    }

    fn name(&self) -> String {
        self.dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }

    fn load(&self) -> Result<(Widget, Vec<PathBuf>), (String, Vec<PathBuf>)> {
        let manifest_path = self.dir.join("widget.toml");
        let mut watched = vec![manifest_path];
        let manifest =
            Manifest::load(&self.dir).map_err(|e| (format!("manifest: {e}"), watched.clone()))?;
        watched.push(manifest.entry.clone());
        let widget =
            Widget::from_manifest(&manifest).map_err(|e| (e.to_string(), watched.clone()))?;
        Ok((widget, watched))
    }

    fn wait_for_change(&self, watched: &[PathBuf], poll: &Receiver<std::time::Instant>) {
        let before: Vec<_> = watched.iter().map(|f| mtime(f)).collect();
        loop {
            let _ = poll.recv();
            if watched.iter().map(|f| mtime(f)).collect::<Vec<_>>() != before {
                return;
            }
        }
    }

    fn input(&self, widget: &mut Widget, input: Input) {
        if let Err(e) = widget.input(input) {
            log::warn!("{}: {e}", self.name());
        }
    }

    /// Carry out the widget's requested effects through the connector.
    fn flush(&mut self, widget: &mut Widget, dtx: &Sender<Delivery>) {
        for effect in widget.take_effects() {
            match effect {
                Effect::Subscribe {
                    service,
                    path,
                    token,
                } => {
                    self.connector
                        .subscribe(&service, &path, dtx.clone(), token);
                    self.subs.insert((service, path, token));
                }
                Effect::Unsubscribe {
                    service,
                    path,
                    token,
                } => {
                    self.connector.unsubscribe(&service, &path, token);
                    self.subs.remove(&(service, path, token));
                }
                Effect::Call {
                    service,
                    method,
                    args,
                    token,
                } => {
                    self.connector
                        .call(&service, &method, args, dtx.clone(), token);
                }
                Effect::Log { widget, message } => log::info!("[{widget}] {message}"),
            }
        }
    }

    fn unsubscribe_all(&mut self) {
        for (service, path, token) in self.subs.drain() {
            self.connector.unsubscribe(&service, &path, token);
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
