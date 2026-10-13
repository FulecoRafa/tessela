//! The window's connection to a running project: the same socket and
//! requests as the CLI, off the UI thread.
//!
//! One thread follows every channel (`watch`); another polls what has no
//! stream (overview, state, trees, the selected node) and runs the window's
//! requests. Both reconnect on their own: the project may not be running
//! yet, or restart.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded as ui_channel};
use serde_json::{Value, json};

use crate::devtools::{CHANNELS, ask, shot, socket};

/// From the project to the window.
pub enum Msg {
    Connected(String),
    Disconnected(String),
    Overview(Value),
    State(Value),
    Tree(String, Value),
    Node(Value),
    /// Ring contents at connect time, oldest first.
    History(String, Vec<Value>),
    Record(String, Value),
    /// Result of a request the window made, for its status line.
    Done(String, Result<Value, String>),
    Shot(String, Result<String, String>),
}

/// From the window to the project.
pub enum Cmd {
    /// A request whose reply only goes to the status line.
    Ask(String, Value),
    /// Refresh a surface's tree on the next tick (coalesced).
    Tree(String),
    /// Follow this node's details (path, drawn bounds).
    Node(Option<(String, u64)>),
    Shot(String),
}

const TICK: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_secs(1);

pub fn start(project: Option<String>) -> (Sender<Cmd>, UnboundedReceiver<Msg>) {
    let (cmd_tx, cmd_rx) = unbounded();
    let (tx, rx) = ui_channel();
    std::thread::Builder::new()
        .name("devtools-link".into())
        .spawn(move || supervise(project, cmd_rx, tx))
        .expect("spawn devtools link");
    (cmd_tx, rx)
}

fn supervise(project: Option<String>, cmds: Receiver<Cmd>, tx: UnboundedSender<Msg>) {
    let mut last_error = String::new();
    loop {
        match socket(project.as_deref()) {
            Ok(sock) if UnixStream::connect(&sock).is_ok() => {
                let name = sock
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if tx.unbounded_send(Msg::Connected(name)).is_err() {
                    return;
                }
                let reason = session(&sock, &cmds, &tx);
                last_error.clear();
                if tx.unbounded_send(Msg::Disconnected(reason)).is_err() {
                    return;
                }
            }
            Ok(sock) => report(
                &tx,
                &mut last_error,
                format!("waiting for {}", sock.display()),
            ),
            Err(e) => report(&tx, &mut last_error, e.to_string()),
        }
        // Requests made while disconnected fail right away.
        while let Ok(c) = cmds.try_recv() {
            if let Cmd::Ask(what, _) = c {
                let _ = tx.unbounded_send(Msg::Done(what, Err("not connected".into())));
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn report(tx: &UnboundedSender<Msg>, last: &mut String, e: String) {
    if *last != e {
        let _ = tx.unbounded_send(Msg::Disconnected(e.clone()));
        *last = e;
    }
}

/// One connection's life; returns why it ended.
fn session(sock: &Path, cmds: &Receiver<Cmd>, tx: &UnboundedSender<Msg>) -> String {
    let alive = Arc::new(AtomicBool::new(true));
    // History first, then the stream: a record may show up twice, never
    // go missing... unless it lands between the two, which is rare enough.
    for c in CHANNELS {
        if let Ok(Value::Array(records)) = ask(sock, json!({ "op": c, "limit": 1000 })) {
            let _ = tx.unbounded_send(Msg::History(c.to_string(), records));
        }
    }
    {
        let (sock, tx, alive) = (sock.to_path_buf(), tx.clone(), alive.clone());
        std::thread::spawn(move || {
            let _ = follow(&sock, &tx);
            alive.store(false, Ordering::Relaxed);
        });
    }
    let mut trees: BTreeSet<String> = BTreeSet::new();
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut node: Option<(String, u64)> = None;
    let mut polled = Instant::now() - POLL;
    let mut node_polled = Instant::now() - POLL;
    loop {
        if !alive.load(Ordering::Relaxed) {
            return "the project stopped".into();
        }
        if polled.elapsed() >= POLL {
            polled = Instant::now();
            match ask(sock, json!({ "op": "overview" })) {
                Ok(v) => {
                    // Trees of surfaces seen for the first time.
                    for s in v["surfaces"].as_array().into_iter().flatten() {
                        if let Some(n) = s["name"].as_str()
                            && known.insert(n.to_string())
                        {
                            trees.insert(n.to_string());
                        }
                    }
                    let _ = tx.unbounded_send(Msg::Overview(v));
                }
                Err(e) => return e.to_string(),
            }
            if let Ok(v) = ask(sock, json!({ "op": "state" })) {
                let _ = tx.unbounded_send(Msg::State(v));
            }
        }
        for surface in std::mem::take(&mut trees) {
            if let Ok(v) = ask(sock, json!({ "op": "tree", "surface": surface })) {
                let _ = tx.unbounded_send(Msg::Tree(surface, v));
            }
        }
        if let Some((surface, id)) = &node
            && node_polled.elapsed() >= Duration::from_millis(500)
        {
            node_polled = Instant::now();
            if let Ok(v) = ask(sock, json!({ "op": "node", "surface": surface, "id": id })) {
                let _ = tx.unbounded_send(Msg::Node(v));
            }
        }
        let deadline = Instant::now() + TICK;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match cmds.recv_timeout(left) {
                Ok(Cmd::Ask(what, req)) => {
                    let r = ask(sock, req).map_err(|e| e.to_string());
                    let _ = tx.unbounded_send(Msg::Done(what, r));
                }
                Ok(Cmd::Tree(s)) => {
                    trees.insert(s);
                }
                Ok(Cmd::Node(n)) => {
                    node = n;
                    node_polled = Instant::now() - POLL;
                }
                Ok(Cmd::Shot(surface)) => {
                    let file = std::env::temp_dir().join(format!(
                        "tessela-{surface}-{}.png",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or(0)
                    ));
                    let r = shot(sock, &surface, Some(file.to_string_lossy().to_string()))
                        .map(|_| file.to_string_lossy().to_string())
                        .map_err(|e| e.to_string());
                    let _ = tx.unbounded_send(Msg::Shot(surface, r));
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return "window closed".into(),
            }
        }
    }
}

fn follow(sock: &Path, tx: &UnboundedSender<Msg>) -> anyhow::Result<()> {
    let mut s = UnixStream::connect(sock)?;
    writeln!(s, "{}", json!({ "op": "watch", "channels": CHANNELS }))?;
    for line in BufReader::new(s).lines() {
        let v: Value = serde_json::from_str(&line?)?;
        let channel = v["channel"].as_str().unwrap_or("").to_string();
        if tx
            .unbounded_send(Msg::Record(channel, v["record"].clone()))
            .is_err()
        {
            break;
        }
    }
    Ok(())
}
