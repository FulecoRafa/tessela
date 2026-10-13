//! Devtools: watch a running project from another terminal.
//!
//! `tessela run <project> --devtools` serves `<runtime dir>/devtools.sock`,
//! one JSON request per line; `tessela devtools <command>` is the client.
//!
//! | panel | what | source |
//! |---|---|---|
//! | elements | live trees with node ids, props and handlers | a mirror fed by the same patches the renderer gets |
//! | console | guest `log(...)`, load errors, host warnings | worker probe, logger |
//! | calls | every module call: args, result, time | core invoke trace |
//! | events | clicks/scrolls delivered to guest code | worker probe |
//! | frames | renders: time, patches, live graph objects | worker probe |
//! | perf | per surface: renders, render time, window draw time | worker + renderer probes |
//! | state | shared values, desktop topics, plugins, windows | modules, renderer |
//!
//! Actions: `click` triggers a node's handler as if the user did it; `shot`
//! captures a surface's window with grim.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use crossbeam_channel::{Sender, unbounded};
use serde_json::{Value, json};
use tessela::core::{Inbound, InvokeRecord};
use tessela::host::{Frame, GraphStats, Probe, Slot, Trees, UiEvent};
use tessela::view::{Handler, NodeId, ViewTree};

const RING: usize = 1000;
/// Signal values up to this size (JSON bytes) are kept with their delivery.
const SIGNAL_VALUE_MAX: usize = 4096;
const CHANNELS: &[&str] = &["console", "calls", "events", "frames", "deliveries"];

#[derive(Default, Clone)]
struct SurfaceStats {
    loads: u64,
    load_ms: f64,
    files: usize,
    error: Option<String>,
    renders: u64,
    frames: u64,
    patches: u64,
    render_total_ms: f64,
    render_max_ms: f64,
    graph: GraphStats,
    deliveries: u64,
    calls: u64,
    draws: u64,
    draw_total_ms: f64,
}

struct State {
    names: Vec<String>,
    slots: Vec<Slot>,
    trees: Trees,
    surfaces: Vec<SurfaceStats>,
    rings: Vec<(&'static str, VecDeque<Value>)>,
    windows: Value,
    /// (slot, signal) → where it comes from: "hyprland:clients", "service:audio/state".
    signal_names: std::collections::HashMap<(usize, u64), String>,
}

/// What the runner shares with devtools besides the probes.
pub struct Sources {
    pub project: String,
    pub state: Arc<tessela::state::SharedState>,
    pub desktop: Arc<tessela::desktop::Desktop>,
    pub plugins: Arc<crate::plugins::Plugins>,
    /// Logical size of the screen surfaces are on, when it is not the
    /// desktop's (--nested).
    pub screen: Option<[f64; 2]>,
}

pub struct Devtools {
    started: Instant,
    state: Mutex<State>,
    watchers: Mutex<Vec<(Vec<String>, Sender<String>)>>,
    sources: Mutex<Option<Sources>>,
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e6).round() / 1e3
}

/// `fn#12`, `action:service.call(...)`: handlers as devtools show them. Only
/// `on_*` props hold handlers (a bare number would parse as one).
fn handler_label(key: &str, v: &Value) -> Option<String> {
    if !key.starts_with("on_") {
        return None;
    }
    match Handler::from_value(v)? {
        Handler::Fn(id) => Some(format!("fn#{id}")),
        Handler::Action { action } => Some(format!("action:{}.{}", action.module, action.func)),
    }
}

/// One patch in a line: `#45 Label text="22:41"`, `+#12 Box`, `-3 nodes`.
fn describe(tree: Option<&ViewTree>, p: &tessela::view::Patch) -> String {
    use tessela::view::Patch;
    let kind = |id: &NodeId| {
        tree.and_then(|t| t.get(*id))
            .map(|r| r.kind.clone())
            .unwrap_or_default()
    };
    match p {
        Patch::Create { id, kind, .. } => format!("+#{id} {kind}"),
        Patch::Update { id, set, unset } => {
            let mut parts: Vec<String> = set
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        handler_label(k, v).unwrap_or_else(|| short(v, 24))
                    )
                })
                .collect();
            parts.extend(unset.iter().map(|k| format!("-{k}")));
            format!("#{id} {} {}", kind(id), parts.join(" "))
        }
        Patch::Children { id, children } => {
            format!("#{id} {} children({})", kind(id), children.len())
        }
        Patch::Root { id } => format!("root #{id}"),
        Patch::Remove { ids } => format!("-{} nodes", ids.len()),
    }
}

fn tree_json(tree: &ViewTree, id: NodeId, depth: usize, max: usize) -> Value {
    let Some(r) = tree.get(id) else {
        return Value::Null;
    };
    let props: serde_json::Map<String, Value> = r
        .props
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                handler_label(k, v)
                    .map(Value::String)
                    .unwrap_or_else(|| v.clone()),
            )
        })
        .collect();
    let children: Vec<Value> = if depth >= max {
        vec![]
    } else {
        r.children
            .iter()
            .map(|c| tree_json(tree, *c, depth + 1, max))
            .collect()
    };
    json!({
        "id": id,
        "kind": r.kind,
        "key": r.key,
        "props": props,
        "children": children,
        "childCount": r.children.len(),
    })
}

impl Devtools {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            state: Mutex::new(State {
                names: vec![],
                slots: vec![],
                trees: Trees::new(0),
                surfaces: vec![],
                rings: CHANNELS.iter().map(|c| (*c, VecDeque::new())).collect(),
                windows: json!([]),
                signal_names: Default::default(),
            }),
            watchers: Mutex::default(),
            sources: Mutex::default(),
        })
    }

    pub fn attach(&self, sources: Sources) {
        *self.sources.lock().unwrap() = Some(sources);
    }

    fn t(&self) -> f64 {
        (self.started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0
    }

    fn name(st: &State, slot: usize) -> String {
        st.names
            .get(slot)
            .cloned()
            .unwrap_or_else(|| format!("#{slot}"))
    }

    fn push(&self, st: &mut State, channel: &'static str, mut record: Value) {
        record["t"] = json!(self.t());
        let line = record.to_string();
        if let Some((_, ring)) = st.rings.iter_mut().find(|(c, _)| *c == channel) {
            ring.push_back(record);
            while ring.len() > RING {
                ring.pop_front();
            }
        }
        self.watchers.lock().unwrap().retain(|(channels, tx)| {
            !channels.iter().any(|c| c == channel)
                || tx
                    .send(format!("{{\"channel\":\"{channel}\",\"record\":{line}}}"))
                    .is_ok()
        });
    }

    /// Host log lines (warnings and errors from everywhere, info from tessela).
    pub fn host_log(&self, level: log::Level, line: &str) {
        let mut st = self.state.lock().unwrap();
        self.push(
            &mut st,
            "console",
            json!({ "source": "host", "level": level.to_string().to_lowercase(), "text": line }),
        );
    }

    // ------------------------------------------------------------ server

    pub fn serve(self: &Arc<Self>, socket: PathBuf) -> anyhow::Result<()> {
        let _ = std::fs::remove_file(&socket);
        let listener =
            UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        }
        log::info!("devtools: {}", socket.display());
        let me = self.clone();
        std::thread::Builder::new()
            .name("devtools".into())
            .spawn(move || {
                for conn in listener.incoming().flatten() {
                    let me = me.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = me.connection(conn) {
                            log::debug!("devtools client: {e}");
                        }
                    });
                }
            })?;
        Ok(())
    }

    fn connection(&self, conn: UnixStream) -> anyhow::Result<()> {
        let mut out = conn.try_clone()?;
        for line in BufReader::new(conn).lines() {
            let req: Value = serde_json::from_str(&line?).unwrap_or(Value::Null);
            if req["op"] == "watch" {
                let channels: Vec<String> = req["channels"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_else(|| CHANNELS.iter().map(|c| c.to_string()).collect());
                let (tx, rx) = unbounded();
                self.watchers.lock().unwrap().push((channels, tx));
                for line in rx {
                    writeln!(out, "{line}")?;
                }
                return Ok(());
            }
            let reply = self
                .request(&req)
                .unwrap_or_else(|e| json!({ "error": e.to_string() }));
            writeln!(out, "{reply}")?;
        }
        Ok(())
    }

    fn slot_of(st: &State, req: &Value) -> anyhow::Result<usize> {
        let name = req["surface"].as_str().context("which surface?")?;
        st.names
            .iter()
            .position(|n| n == name)
            .with_context(|| format!("no surface '{name}' (have: {})", st.names.join(", ")))
    }

    fn request(&self, req: &Value) -> anyhow::Result<Value> {
        let op = req["op"].as_str().unwrap_or("overview");
        let st = self.state.lock().unwrap();
        Ok(match op {
            "overview" | "perf" => {
                let src = self.sources.lock().unwrap();
                let surfaces: Vec<Value> = st
                    .surfaces
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        json!({
                            "name": Self::name(&st, i),
                            "loads": s.loads, "loadMs": s.load_ms, "files": s.files, "error": s.error,
                            "renders": s.renders, "frames": s.frames, "patches": s.patches,
                            "renderAvgMs": if s.renders > 0 { (s.render_total_ms / s.renders as f64 * 1000.0).round() / 1000.0 } else { 0.0 },
                            "renderMaxMs": s.render_max_ms,
                            "bindings": s.graph.bindings, "signals": s.graph.signals, "resources": s.graph.resources,
                            "deliveries": s.deliveries, "calls": s.calls,
                            "draws": s.draws,
                            "drawAvgMs": if s.draws > 0 { (s.draw_total_ms / s.draws as f64 * 1000.0).round() / 1000.0 } else { 0.0 },
                        })
                    })
                    .collect();
                json!({
                    "project": src.as_ref().map(|s| s.project.clone()),
                    "uptime": self.t(),
                    "surfaces": surfaces,
                    "windows": st.windows,
                    "plugins": src.as_ref().map(|s| s.plugins.status().into_iter().map(|(n, p)| json!({"name": n, "pid": p.pid, "starts": p.starts, "lastExit": p.last_exit})).collect::<Vec<_>>()),
                })
            }
            "tree" => {
                let slot = Self::slot_of(&st, req)?;
                let tree = st.trees.tree(slot).context("no tree yet")?;
                let root = req["id"].as_u64().or(tree.root()).context("empty tree")?;
                tree_json(tree, root, 0, req["depth"].as_u64().unwrap_or(64) as usize)
            }
            "console" | "calls" | "events" | "frames" | "deliveries" => {
                let n = req["limit"].as_u64().unwrap_or(50) as usize;
                let ring = &st
                    .rings
                    .iter()
                    .find(|(c, _)| *c == op)
                    .context("channel")?
                    .1;
                Value::Array(
                    ring.iter()
                        .skip(ring.len().saturating_sub(n))
                        .cloned()
                        .collect(),
                )
            }
            "state" => {
                let src = self.sources.lock().unwrap();
                let src = src.as_ref().context("not attached")?;
                json!({ "shared": src.state.snapshot(), "desktop": src.desktop.snapshot() })
            }
            "windows" => st.windows.clone(),
            "set" => {
                let key = req["key"].as_str().context("which key?")?;
                let src = self.sources.lock().unwrap();
                src.as_ref()
                    .context("not attached")?
                    .state
                    .set(key, req["value"].clone());
                json!({ "set": key, "value": req["value"] })
            }
            "click" => {
                let slot = Self::slot_of(&st, req)?;
                let id = req["id"].as_u64().context("which node id?")?;
                let tree = st.trees.tree(slot).context("no tree yet")?;
                let node = tree.get(id).with_context(|| format!("no node #{id}"))?;
                let (prop, payload) = match req["button"].as_str().unwrap_or("left") {
                    _ if req["prop"].is_string() => {
                        (req["prop"].as_str().unwrap_or(""), req["payload"].clone())
                    }
                    "right" => ("on_right_click", Value::Null),
                    "middle" => ("on_middle_click", Value::Null),
                    "scroll" => (
                        "on_scroll",
                        json!({ "delta": req["delta"].as_i64().unwrap_or(1).signum() }),
                    ),
                    _ => ("on_click", Value::Null),
                };
                let handler = node
                    .props
                    .get(prop)
                    .and_then(Handler::from_value)
                    .with_context(|| format!("#{id} ({}) has no {prop}", node.kind))?;
                st.slots[slot].send(UiEvent::new(handler, payload.clone()));
                json!({ "sent": prop, "to": id, "kind": node.kind, "payload": payload })
            }
            "geometry" => {
                let slot = Self::slot_of(&st, req)?;
                let src = self.sources.lock().unwrap();
                let screen = match src.as_ref() {
                    Some(s) => match s.screen {
                        Some([w, h]) => json!({ "width": w, "height": h }),
                        None => s.desktop.snapshot()["hyprland"]["screen"].clone(),
                    },
                    None => Value::Null,
                };
                let win = st
                    .windows
                    .as_array()
                    .and_then(|ws| {
                        ws.iter()
                            .find(|w| w["slot"] == slot && w["popup"].is_null())
                    })
                    .cloned()
                    .context("that surface has no open window")?;
                // The display it renders on (a nested compositor with --nested).
                json!({ "window": win, "screen": screen, "display": std::env::var("WAYLAND_DISPLAY").ok() })
            }
            other => anyhow::bail!("unknown op '{other}'"),
        })
    }
}

impl Probe for Devtools {
    fn started(&self, slots: &[Slot]) {
        let mut st = self.state.lock().unwrap();
        st.names = slots.iter().map(|s| s.name.clone()).collect();
        st.slots = slots.to_vec();
        st.trees = Trees::new(slots.len());
        st.surfaces = vec![SurfaceStats::default(); slots.len()];
    }

    fn loaded(&self, slot: usize, name: &str, files: &[PathBuf], took: Duration) {
        let mut st = self.state.lock().unwrap();
        if let Some(s) = st.surfaces.get_mut(slot) {
            s.loads += 1;
            s.load_ms = ms(took);
            s.files = files.len();
            s.error = None;
        }
        let rec = json!({ "source": name, "level": "info", "text": format!("loaded {} files in {:.1} ms", files.len(), ms(took)) });
        self.push(&mut st, "console", rec);
    }

    fn failed(&self, slot: usize, name: &str, error: &str) {
        let mut st = self.state.lock().unwrap();
        if let Some(s) = st.surfaces.get_mut(slot) {
            s.error = Some(error.to_string());
        }
        self.push(
            &mut st,
            "console",
            json!({ "source": name, "level": "error", "text": error }),
        );
    }

    fn rendered(&self, slot: usize, took: Duration, frame: Option<&Frame>, stats: GraphStats) {
        let mut st = self.state.lock().unwrap();
        let t = ms(took);
        let patches = frame.map(|f| f.patches.len()).unwrap_or(0);
        // Described against the tree before applying, to name updated kinds.
        let changes: Vec<String> = frame
            .map(|f| {
                f.patches
                    .iter()
                    .map(|p| describe(st.trees.tree(slot), p))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(f) = frame {
            st.trees.apply(f);
        }
        if let Some(s) = st.surfaces.get_mut(slot) {
            s.renders += 1;
            s.render_total_ms += t;
            s.render_max_ms = s.render_max_ms.max(t);
            s.graph = stats;
            if frame.is_some() {
                s.frames += 1;
                s.patches += patches as u64;
            }
        }
        let name = Self::name(&st, slot);
        let rec = json!({
            "surface": name, "ms": t, "patches": patches, "changes": changes,
            "bindings": stats.bindings, "signals": stats.signals, "resources": stats.resources,
        });
        self.push(&mut st, "frames", rec);
    }

    fn delivered(&self, slot: usize, inbound: &Inbound, took: Duration, error: Option<&str>) {
        let mut st = self.state.lock().unwrap();
        if let Some(s) = st.surfaces.get_mut(slot) {
            s.deliveries += 1;
        }
        let (kind, detail) = match inbound {
            Inbound::Signal { sig, value } => {
                let bytes = value.to_string().len();
                // Small values are kept for the devtools network panel.
                let value = if bytes <= SIGNAL_VALUE_MAX {
                    value.clone()
                } else {
                    Value::Null
                };
                (
                    "signal",
                    json!({ "sig": sig, "from": st.signal_names.get(&(slot, *sig)), "bytes": bytes, "value": value }),
                )
            }
            Inbound::Callback { f, value } => ("callback", json!({ "fn": f, "value": value })),
            Inbound::Finished(r) => ("finished", json!({ "module": r.module, "key": r.key })),
        };
        let rec = json!({ "surface": Self::name(&st, slot), "kind": kind, "detail": detail, "ms": ms(took), "error": error });
        self.push(&mut st, "deliveries", rec);
    }

    fn event(&self, slot: usize, event: &UiEvent, took: Duration, error: Option<&str>) {
        let mut st = self.state.lock().unwrap();
        let handler = match &event.handler {
            Handler::Fn(id) => format!("fn#{id}"),
            Handler::Action { action } => format!("action:{}.{}", action.module, action.func),
        };
        let rec = json!({ "surface": Self::name(&st, slot), "handler": handler, "payload": event.payload, "ms": ms(took), "error": error });
        self.push(&mut st, "events", rec);
        if let Some(e) = error {
            let rec = json!({ "source": Self::name(&st, slot), "level": "error", "text": e });
            self.push(&mut st, "console", rec);
        }
    }

    fn log_at(&self, slot: usize, line: &str, at: Option<&str>) {
        let mut st = self.state.lock().unwrap();
        let rec =
            json!({ "source": Self::name(&st, slot), "level": "log", "text": line, "at": at });
        self.push(&mut st, "console", rec);
    }

    fn invoke(&self, slot: usize, r: InvokeRecord) {
        let mut st = self.state.lock().unwrap();
        if let Some(s) = st.surfaces.get_mut(slot) {
            s.calls += 1;
        }
        if let Ok(Some(sig)) = r.result.as_ref().map(|v| v["sig"].as_u64()) {
            let what = r.args["topic"]
                .as_str()
                .or(r.args["key"].as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    format!(
                        "{}/{}",
                        r.args["service"].as_str().unwrap_or("?"),
                        r.args["path"].as_str().unwrap_or("?")
                    )
                });
            st.signal_names
                .insert((slot, sig), format!("{}:{what}", r.module));
        }
        let rec = json!({
            "surface": Self::name(&st, slot),
            "call": format!("{}.{}", r.module, r.func),
            "args": r.args,
            "ok": r.result.is_ok(),
            "result": match &r.result { Ok(v) => v.clone(), Err(e) => json!(e) },
            "ms": ms(r.took),
        });
        self.push(&mut st, "calls", rec);
    }
}

#[cfg(feature = "gpui")]
impl tessela::gpui::RenderProbe for Devtools {
    fn rendered(&self, slot: usize, _popup: bool, took: Duration) {
        let mut st = self.state.lock().unwrap();
        if let Some(s) = st.surfaces.get_mut(slot) {
            s.draws += 1;
            s.draw_total_ms += ms(took);
        }
    }

    fn windows(&self, windows: Vec<tessela::gpui::WindowInfo>) {
        self.state.lock().unwrap().windows = serde_json::to_value(windows).unwrap_or(Value::Null);
    }
}

// ---------------------------------------------------------------- client

pub const USAGE: &str = "\
tessela devtools [--project NAME] <command>
  overview                      surfaces, windows, plugins (default)
  perf                          render and draw times per surface
  tree <surface> [--depth N] [--id N] [--compact]
  find <surface> <text>         nodes whose kind or props contain text
  click <surface> <id> [--right|--middle|--scroll ±1|--handler PROP [--payload JSON]]
  console|calls|events|frames|deliveries [-n N] [-f]
  watch [channel...]            follow channels (all by default)
  state                         shared values and desktop topics
  set <key> <json>              change a shared value, as an extension would
  windows                       open windows and their placement
  shot <surface> [file.png]     capture the surface's window (grim)";

fn socket(project: Option<&str>) -> anyhow::Result<PathBuf> {
    let runtime = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into()))
        .join("tessela");
    if let Some(p) = project {
        return Ok(runtime.join(p).join("devtools.sock"));
    }
    let found: Vec<PathBuf> = std::fs::read_dir(&runtime)
        .map(|d| {
            d.flatten()
                .map(|e| e.path().join("devtools.sock"))
                .filter(|p| p.exists())
                .collect()
        })
        .unwrap_or_default();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => anyhow::bail!(
            "no running project with devtools (start it with `tessela run <project> --devtools`)"
        ),
        many => anyhow::bail!(
            "several projects running; pick one with --project: {}",
            many.iter()
                .filter_map(|p| p.parent()?.file_name()?.to_str().map(String::from))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn ask(sock: &Path, req: Value) -> anyhow::Result<Value> {
    let mut s =
        UnixStream::connect(sock).with_context(|| format!("connecting to {}", sock.display()))?;
    writeln!(s, "{req}")?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    let v: Value = serde_json::from_str(&line)?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        anyhow::bail!("{e}");
    }
    Ok(v)
}

fn short(v: &Value, max: usize) -> String {
    let s = match v {
        Value::String(s) => format!("{s:?}"),
        other => other.to_string(),
    };
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s
    }
}

fn print_tree(v: &Value, indent: usize, compact: bool) {
    let props = v["props"].as_object();
    let mut parts = vec![];
    let mut handlers = vec![];
    for (k, val) in props.into_iter().flatten() {
        if let Some(name) = k.strip_prefix("on_") {
            handlers.push(name.to_string());
        } else if !compact
            || matches!(
                k.as_str(),
                "text" | "src" | "name" | "label" | "value" | "tooltip"
            )
        {
            parts.push(format!("{k}={}", short(val, 40)));
        }
    }
    let key = v["key"]
        .as_str()
        .map(|k| format!(" key={k:?}"))
        .unwrap_or_default();
    let h = if handlers.is_empty() {
        String::new()
    } else {
        format!(" [{}]", handlers.join(" "))
    };
    println!(
        "{}#{} {}{key} {}{h}",
        "  ".repeat(indent),
        v["id"],
        v["kind"].as_str().unwrap_or("?"),
        parts.join(" ")
    );
    let children = v["children"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    for c in children {
        print_tree(c, indent + 1, compact);
    }
    let hidden = v["childCount"].as_u64().unwrap_or(0) as usize
        - children
            .len()
            .min(v["childCount"].as_u64().unwrap_or(0) as usize);
    if hidden > 0 {
        println!("{}… {hidden} more", "  ".repeat(indent + 1));
    }
}

fn find(v: &Value, text: &str, path: &mut Vec<String>, out: &mut Vec<String>) {
    let me = format!("#{} {}", v["id"], v["kind"].as_str().unwrap_or("?"));
    let hay = format!("{} {}", v["kind"], v["props"]).to_lowercase();
    if hay.contains(&text.to_lowercase()) {
        let props: Vec<String> = v["props"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, val)| format!("{k}={}", short(val, 30)))
            .collect();
        out.push(format!(
            "{me} {}\n    in {}",
            props.join(" "),
            if path.is_empty() {
                "(root)".into()
            } else {
                path.join(" › ")
            }
        ));
    }
    path.push(me);
    for c in v["children"].as_array().into_iter().flatten() {
        find(c, text, path, out);
    }
    path.pop();
}

fn print_record(channel: &str, r: &Value) {
    let t = r["t"].as_f64().unwrap_or(0.0);
    let line = match channel {
        "console" => format!(
            "{:<5} [{}] {}{}",
            r["level"].as_str().unwrap_or(""),
            r["source"].as_str().unwrap_or(""),
            r["text"].as_str().unwrap_or(""),
            r["at"]
                .as_str()
                .map(|a| format!("  ({a})"))
                .unwrap_or_default()
        ),
        "calls" => format!(
            "{:<7} {}({}) → {}{} {:.2}ms",
            r["surface"].as_str().unwrap_or(""),
            r["call"].as_str().unwrap_or(""),
            short(&r["args"], 70),
            if r["ok"] == true { "" } else { "ERROR " },
            short(&r["result"], 60),
            r["ms"].as_f64().unwrap_or(0.0)
        ),
        "events" => format!(
            "{:<7} {} {} {:.2}ms{}",
            r["surface"].as_str().unwrap_or(""),
            r["handler"].as_str().unwrap_or(""),
            short(&r["payload"], 40),
            r["ms"].as_f64().unwrap_or(0.0),
            r["error"]
                .as_str()
                .map(|e| format!(" ERROR {e}"))
                .unwrap_or_default()
        ),
        "frames" => format!(
            "{:<7} render {:.2}ms, {} patches · graph {}b/{}s/{}r{}",
            r["surface"].as_str().unwrap_or(""),
            r["ms"].as_f64().unwrap_or(0.0),
            r["patches"],
            r["bindings"],
            r["signals"],
            r["resources"],
            r["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| format!("\n{:>21}{}", "", c.as_str().unwrap_or("")))
                .collect::<String>()
        ),
        "deliveries" => format!(
            "{:<7} {} {} {:.2}ms",
            r["surface"].as_str().unwrap_or(""),
            r["kind"].as_str().unwrap_or(""),
            short(&r["detail"], 60),
            r["ms"].as_f64().unwrap_or(0.0)
        ),
        _ => r.to_string(),
    };
    println!("{t:>9.3}s {line}");
}

fn shot(sock: &Path, surface: &str, file: Option<String>) -> anyhow::Result<()> {
    let g = ask(sock, json!({ "op": "geometry", "surface": surface }))?;
    let (w, s) = (&g["window"], &g["screen"]);
    let (sw, sh) = (
        s["width"].as_f64().context("screen size unknown")?,
        s["height"].as_f64().unwrap_or(0.0),
    );
    let [top, right, bottom, left] = [0, 1, 2, 3].map(|i| w["anchors"][i] == true);
    let [mt, mr, mb, ml] = [0, 1, 2, 3].map(|i| w["margin"][i].as_f64().unwrap_or(0.0));
    let (ww, wh) = (
        w["size"][0].as_f64().unwrap_or(0.0),
        w["size"][1].as_f64().unwrap_or(0.0),
    );
    let overlay = w["overlay"] == true;
    let (x, width) = if overlay || (left && right) {
        (
            if overlay { 0.0 } else { ml },
            if overlay { sw } else { sw - ml - mr },
        )
    } else if right {
        (sw - ww - mr, ww)
    } else if left {
        (ml, ww)
    } else {
        ((sw - ww) / 2.0, ww)
    };
    let (y, height) = if overlay || (top && bottom) {
        (
            if overlay { 0.0 } else { mt },
            if overlay { sh } else { sh - mt - mb },
        )
    } else if bottom {
        (sh - wh - mb, wh)
    } else if top {
        (mt, wh)
    } else {
        ((sh - wh) / 2.0, wh)
    };
    let geometry = format!(
        "{},{} {}x{}",
        x.round(),
        y.round(),
        width.round(),
        height.round()
    );
    let file = file.unwrap_or_else(|| format!("{surface}.png"));
    let mut grim = std::process::Command::new("grim");
    grim.args(["-g", &geometry, &file]);
    if let Some(display) = g["display"].as_str() {
        grim.env("WAYLAND_DISPLAY", display);
    }
    let status = grim.status();
    match status {
        Ok(s) if s.success() => println!("{file} ({geometry})"),
        _ => anyhow::bail!("grim failed or is missing; run: grim -g \"{geometry}\" {file}"),
    }
    Ok(())
}

pub fn client(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let mut project = None;
    let mut rest = vec![];
    while let Some(a) = args.next() {
        if a == "--project" {
            project = args.next();
        } else {
            rest.push(a);
        }
    }
    if rest.first().map(String::as_str) == Some("--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let sock = socket(project.as_deref())?;
    let cmd = rest.first().cloned().unwrap_or_else(|| "overview".into());
    let arg = |i: usize| rest.get(i).cloned();
    let flag = |f: &str| rest.iter().position(|a| a == f);
    let num = |f: &str| {
        flag(f)
            .and_then(|i| rest.get(i + 1))
            .and_then(|v| v.parse::<i64>().ok())
    };
    match cmd.as_str() {
        "overview" | "perf" => {
            let v = ask(&sock, json!({ "op": "overview" }))?;
            println!(
                "project {} · up {:.0}s",
                v["project"].as_str().unwrap_or("?"),
                v["uptime"].as_f64().unwrap_or(0.0)
            );
            for s in v["surfaces"].as_array().into_iter().flatten() {
                println!(
                    "  {:<8} {} · {} loads ({:.0} ms, {} files) · {} renders avg {:.2} ms max {:.2} · {} frames {} patches · draw avg {:.2} ms ×{} · graph {} bindings {} signals {} resources · {} calls {} deliveries",
                    s["name"].as_str().unwrap_or(""),
                    if s["error"].is_null() {
                        "ok".to_string()
                    } else {
                        format!("ERROR {}", s["error"].as_str().unwrap_or(""))
                    },
                    s["loads"],
                    s["loadMs"].as_f64().unwrap_or(0.0),
                    s["files"],
                    s["renders"],
                    s["renderAvgMs"].as_f64().unwrap_or(0.0),
                    s["renderMaxMs"].as_f64().unwrap_or(0.0),
                    s["frames"],
                    s["patches"],
                    s["drawAvgMs"].as_f64().unwrap_or(0.0),
                    s["draws"],
                    s["bindings"],
                    s["signals"],
                    s["resources"],
                    s["calls"],
                    s["deliveries"]
                );
            }
            if cmd == "overview" {
                for w in v["windows"].as_array().into_iter().flatten() {
                    println!(
                        "  window slot {} {} {} size {} margin {}{}",
                        w["slot"],
                        w["layer"].as_str().unwrap_or(""),
                        w["anchors"],
                        w["size"],
                        w["margin"],
                        if w["popup"].is_null() {
                            String::new()
                        } else {
                            format!(" popup #{}", w["popup"])
                        }
                    );
                }
                let plugins: Vec<String> = v["plugins"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| match p["pid"].as_u64() {
                        Some(pid) => format!("{} ({pid})", p["name"].as_str().unwrap_or("")),
                        None => format!(
                            "{} (stopped: {})",
                            p["name"].as_str().unwrap_or(""),
                            p["lastExit"].as_str().unwrap_or("not started")
                        ),
                    })
                    .collect();
                println!("  plugins: {}", plugins.join(", "));
            }
        }
        "tree" => {
            let surface = arg(1).context("tree <surface>")?;
            let mut req = json!({ "op": "tree", "surface": surface });
            if let Some(d) = num("--depth") {
                req["depth"] = json!(d);
            }
            if let Some(id) = num("--id") {
                req["id"] = json!(id);
            }
            print_tree(&ask(&sock, req)?, 0, flag("--compact").is_some());
        }
        "find" => {
            let (surface, text) = (
                arg(1).context("find <surface> <text>")?,
                arg(2).context("find <surface> <text>")?,
            );
            let tree = ask(&sock, json!({ "op": "tree", "surface": surface }))?;
            let mut out = vec![];
            find(&tree, &text, &mut vec![], &mut out);
            if out.is_empty() {
                println!("nothing matches {text:?}");
            }
            for o in out {
                println!("{o}");
            }
        }
        "click" => {
            let (surface, id) = (
                arg(1).context("click <surface> <id>")?,
                arg(2).context("click <surface> <id>")?,
            );
            let id: u64 = id.trim_start_matches('#').parse()?;
            let button = if flag("--right").is_some() {
                "right"
            } else if flag("--middle").is_some() {
                "middle"
            } else if flag("--scroll").is_some() {
                "scroll"
            } else {
                "left"
            };
            let mut req = json!({ "op": "click", "surface": surface, "id": id, "button": button, "delta": num("--scroll").unwrap_or(1) });
            if let Some(prop) = flag("--handler").and_then(|i| rest.get(i + 1)) {
                req["prop"] = json!(prop);
            }
            if let Some(payload) = flag("--payload").and_then(|i| rest.get(i + 1)) {
                req["payload"] = serde_json::from_str(payload).context("--payload must be JSON")?;
            }
            let v = ask(&sock, req)?;
            println!(
                "sent {} to #{} ({}) {}",
                v["sent"].as_str().unwrap_or(""),
                v["to"],
                v["kind"].as_str().unwrap_or(""),
                v["payload"]
            );
        }
        c @ ("console" | "calls" | "events" | "frames" | "deliveries") => {
            if flag("-f").is_some() {
                return watch(&sock, vec![c.to_string()]);
            }
            let v = ask(&sock, json!({ "op": c, "limit": num("-n").unwrap_or(30) }))?;
            for r in v.as_array().into_iter().flatten() {
                print_record(c, r);
            }
        }
        "watch" => watch(&sock, rest[1..].to_vec())?,
        "state" | "windows" => println!(
            "{}",
            serde_json::to_string_pretty(&ask(&sock, json!({ "op": cmd }))?)?
        ),
        "set" => {
            let (key, value) = (
                arg(1).context("set <key> <json>")?,
                arg(2).context("set <key> <json>")?,
            );
            let value: Value = serde_json::from_str(&value).with_context(|| {
                format!("not JSON: {value} (strings need quotes: '\"drawer\"')")
            })?;
            println!(
                "{}",
                ask(&sock, json!({ "op": "set", "key": key, "value": value }))?
            );
        }
        "shot" => shot(&sock, &arg(1).context("shot <surface>")?, arg(2))?,
        other => anyhow::bail!("unknown devtools command {other}\n{USAGE}"),
    }
    Ok(())
}

fn watch(sock: &Path, channels: Vec<String>) -> anyhow::Result<()> {
    let mut s = UnixStream::connect(sock)?;
    let channels = if channels.is_empty() {
        CHANNELS.iter().map(|c| c.to_string()).collect()
    } else {
        channels
    };
    writeln!(s, "{}", json!({ "op": "watch", "channels": channels }))?;
    for line in BufReader::new(s).lines() {
        let v: Value = serde_json::from_str(&line?)?;
        let channel = v["channel"].as_str().unwrap_or("");
        print!("{channel:<10} ");
        print_record(channel, &v["record"]);
    }
    Ok(())
}
