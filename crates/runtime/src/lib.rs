//! Sandboxed widget runtimes.
//!
//! A [`Widget`] is one guest program (Luau or TSX) with its own engine
//! instance, memory cap and CPU budget. It owns no sockets and performs no
//! I/O: the guest can only reach what the host functions expose, and those
//! only emit [`Effect`]s that the shell decides how to carry out.
//!
//! Sandbox layers, in order:
//! 1. **Capabilities**: guests see a handful of host functions (`signal`,
//!    `use`, component constructors, `log`). `use("x")` fails unless the
//!    manifest lists `x`. No `io`, `os.execute`, `require`, `import`, `fetch`.
//! 2. **Resources**: per-widget memory limit, per-entry CPU deadline enforced
//!    by the engine's interrupt hook, bounded output tree size.
//! 3. **Faults**: each widget runs on its own thread with its own engine, so a
//!    crash or stall is contained to that widget. (Engine memory-safety bugs
//!    are *not* contained; that is what a WASM boundary would add.)

mod core;
mod js;
mod lua;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::core::Core;
pub use crate::core::{Effect, FnId, SigId, Token};
pub use view::Node;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub entry: PathBuf,
    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: usize,
    #[serde(default = "default_budget_ms")]
    pub budget_ms: u64,
}

fn default_memory_mb() -> usize {
    16
}
fn default_budget_ms() -> u64 {
    50
}

impl Manifest {
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(dir.join("widget.toml"))?;
        let mut m: Manifest = toml::from_str(&text)?;
        if m.entry.is_relative() {
            m.entry = dir.join(&m.entry);
        }
        Ok(m)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A service pushed a new value for a subscription.
    Event { token: SigId, value: Value },
    /// The service behind a subscription is down; the signal becomes `nil`/`null`.
    Unavailable { token: SigId },
    CallResult {
        token: Token,
        result: Result<Value, String>,
    },
    /// The user interacted with a component carrying this handler id.
    Ui { handler: FnId, args: Value },
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct GuestError(pub String);

/// What every engine adapter must provide. Functions found in guest values
/// are registered with the [`Core`] and replaced by markers:
/// `{"$bind": id}` for reactive props, `{"$fn": id}` for event handlers.
pub(crate) trait Engine {
    fn load(&mut self, source: &str, file: &str) -> Result<Value, GuestError>;
    fn call(&mut self, f: FnId, args: Value) -> Result<Value, GuestError>;
    fn drop_fns(&mut self, ids: &[FnId]);
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub memory_bytes: usize,
    pub budget: Duration,
    /// Cap on values in one converted guest result, to stop a widget from
    /// handing the renderer a million-node tree.
    pub max_values: usize,
}

impl Limits {
    pub fn from_manifest(m: &Manifest) -> Self {
        Self {
            memory_bytes: m.memory_mb * 1024 * 1024,
            budget: Duration::from_millis(m.budget_ms),
            max_values: 20_000,
        }
    }
}

pub(crate) struct Shared {
    pub core: RefCell<Core>,
    pub deadline: Cell<Instant>,
    pub limits: Limits,
}

pub struct Widget {
    pub name: String,
    shared: Rc<Shared>,
    engine: Box<dyn Engine>,
    root: Value,
}

const MAX_DEPTH: usize = 64;

impl Widget {
    pub fn from_manifest(m: &Manifest) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(&m.entry)?;
        let file = m
            .entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        Ok(Self::new(
            &m.name,
            &file,
            &source,
            m.uses.clone(),
            Limits::from_manifest(m),
        )?)
    }

    pub fn new(
        name: &str,
        file: &str,
        source: &str,
        uses: Vec<String>,
        limits: Limits,
    ) -> Result<Self, GuestError> {
        let shared = Rc::new(Shared {
            core: RefCell::new(Core::new(name, uses)),
            deadline: Cell::new(Instant::now() + limits.budget),
            limits,
        });
        let mut engine: Box<dyn Engine> = match Path::new(file).extension().and_then(|e| e.to_str())
        {
            Some("lua" | "luau") => Box::new(lua::LuaEngine::new(shared.clone())?),
            Some("tsx" | "jsx" | "ts" | "js") => Box::new(js::JsEngine::new(shared.clone())?),
            other => {
                return Err(GuestError(format!(
                    "unsupported widget file type: {other:?}"
                )));
            }
        };
        shared.deadline.set(Instant::now() + limits.budget);
        let root = engine.load(source, file)?;
        Ok(Self {
            name: name.into(),
            shared,
            engine,
            root,
        })
    }

    fn arm(&self) {
        self.shared
            .deadline
            .set(Instant::now() + self.shared.limits.budget);
    }

    /// Resolve every binding and parse the result into the view vocabulary.
    /// Only dirty bindings re-enter the guest; clean ones reuse their cache.
    pub fn render(&mut self) -> Node {
        self.arm();
        let mut result = Err(GuestError("not rendered".into()));
        // A binding may set a signal another binding read; settle in a few passes.
        for _ in 0..3 {
            let root = self.root.clone();
            result = self.materialize(&root, 0);
            if !self.shared.core.borrow().is_dirty() {
                break;
            }
        }
        self.collect();
        match result.and_then(|v| view::parse(v).map_err(|e| GuestError(e.to_string()))) {
            Ok(node) => node,
            Err(e) => Node::Error {
                widget: self.name.clone(),
                message: e.0,
            },
        }
    }

    pub fn needs_render(&self) -> bool {
        self.shared.core.borrow().is_dirty()
    }

    pub fn input(&mut self, input: Input) -> Result<(), GuestError> {
        match input {
            Input::Event { token, value } => self.shared.core.borrow_mut().deliver(token, value),
            Input::Unavailable { token } => {
                self.shared.core.borrow_mut().deliver(token, Value::Null)
            }
            Input::CallResult { token, result } => {
                let cb = self.shared.core.borrow_mut().take_call(token).flatten();
                if let Some(cb) = cb {
                    let arg = match result {
                        Ok(v) => serde_json::json!({ "ok": true, "value": v }),
                        Err(e) => serde_json::json!({ "ok": false, "error": e }),
                    };
                    self.run_untracked(cb, arg)?;
                }
            }
            Input::Ui { handler, args } => self.run_untracked(handler, args)?,
        }
        self.collect();
        Ok(())
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        self.shared.core.borrow_mut().take_effects()
    }

    /// (live bindings, live signals) — exposed for leak tests.
    pub fn live_counts(&self) -> (usize, usize) {
        self.shared.core.borrow().live_counts()
    }

    fn run_untracked(&mut self, f: FnId, args: Value) -> Result<(), GuestError> {
        self.arm();
        let prev = self.shared.core.borrow_mut().enter(Some(f));
        let r = self.engine.call(f, args);
        self.shared.core.borrow_mut().leave(prev);
        r.map(|_| ())
    }

    fn collect(&mut self) {
        let released = self.shared.core.borrow_mut().take_released();
        if !released.is_empty() {
            self.engine.drop_fns(&released);
        }
    }

    fn eval_binding(&mut self, id: FnId) -> Result<Value, GuestError> {
        if let Some(v) = self.shared.core.borrow().binding_cached(id) {
            return Ok(v.clone());
        }
        if !self.shared.core.borrow().is_binding(id) {
            return Err(GuestError(format!("stale binding {id}")));
        }
        let prev = self.shared.core.borrow_mut().begin_binding(id);
        let r = self.engine.call(id, Value::Null);
        self.shared
            .core
            .borrow_mut()
            .end_binding(id, prev, r.as_ref().ok().cloned());
        r
    }

    fn materialize(&mut self, v: &Value, depth: usize) -> Result<Value, GuestError> {
        if depth > MAX_DEPTH {
            return Err(GuestError("view tree too deep".into()));
        }
        Ok(match v {
            Value::Object(m) if m.len() == 1 && m.contains_key("$bind") => {
                let id = m["$bind"].as_u64().unwrap_or_default();
                let raw = self.eval_binding(id)?;
                self.materialize(&raw, depth + 1)?
            }
            Value::Object(m) if m.len() == 1 && m.contains_key("$fn") => m["$fn"].clone(),
            Value::Object(m) => {
                let mut out = Map::with_capacity(m.len());
                for (k, val) in m {
                    out.insert(k.clone(), self.materialize(val, depth + 1)?);
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|i| self.materialize(i, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            other => other.clone(),
        })
    }
}

/// Normalize JS-style handler names (`onClick`) to the vocabulary's
/// snake_case (`on_click`). Lua widgets already use snake_case.
pub(crate) fn prop_key(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("on") {
        if rest.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            let mut out = String::from("on");
            for c in rest.chars() {
                if c.is_ascii_uppercase() {
                    out.push('_');
                    out.push(c.to_ascii_lowercase());
                } else {
                    out.push(c);
                }
            }
            return out;
        }
    }
    key.to_string()
}

pub(crate) fn is_handler_key(key: &str) -> bool {
    key.starts_with("on_")
}

#[cfg(test)]
mod tests;
