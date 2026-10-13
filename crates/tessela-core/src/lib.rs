//! Engine-agnostic core: the reactive graph, extension [`Instance`]s and the
//! two contracts everything else plugs into.
//!
//! - [`Engine`] / [`EngineFactory`]: a guest language (Luau, JS/TSX, later
//!   WASM). Engines only marshal values and call functions; reactivity,
//!   ownership and capability checks live here, so every engine behaves the
//!   same.
//! - [`Module`]: the *only* way an extension reaches the outside world (Unix
//!   sockets, timers, storage, network...). A module is invoked synchronously
//!   from guest code through [`ModuleCx`] and answers asynchronously through a
//!   [`Sink`]. Resources it creates are owned by the reactive graph and
//!   released through [`Module::release`] when their owner goes away.
//!
//! This crate performs no I/O, spawns no threads and owns no sockets. That is
//! what lets the same instance run on a desktop, behind a remote renderer or
//! inside a message viewer with zero capabilities.

pub mod reactive;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

pub use reactive::{
    Core, FnId, FnKind, Grants, HostError, LogLine, PersistStore, ResourceRef, SigId,
};
pub use tessela_view as view;
pub use tessela_view::{Action, Handler, Node, Vocabulary};

// ---------------------------------------------------------------- limits

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Allocator cap for the engine instance.
    pub memory_bytes: usize,
    /// CPU time allowed per entry into guest code (load, render, handler).
    pub budget: Duration,
    /// Cap on values in one converted guest result, so a guest cannot hand
    /// the renderer a million-node tree.
    pub max_values: usize,
    /// Cap on nesting of guest values.
    pub max_depth: usize,
    /// Module calls allowed per second (token bucket, burst = one second).
    pub calls_per_sec: u32,
    /// Live module resources (subscriptions, timers) at any time.
    pub max_resources: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 16 << 20,
            budget: Duration::from_millis(50),
            max_values: 20_000,
            max_depth: 64,
            calls_per_sec: 200,
            max_resources: 256,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct GuestError(pub String);

impl From<HostError> for GuestError {
    fn from(e: HostError) -> Self {
        GuestError(e.to_string())
    }
}

// ---------------------------------------------------------------- modules

/// Guest language, so modules can ship a prelude per language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Luau,
    JavaScript,
}

/// Asynchronous input for an instance, produced by modules.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// Set a signal (a subscription delivered a value; `null` = unavailable).
    Signal { sig: SigId, value: Value },
    /// Call a guest handler, untracked (call results, timers).
    Callback { f: FnId, value: Value },
    /// A resource finished on its own (a one-shot timer fired): stop counting
    /// it against the quota. The module is not asked to release it.
    Finished(ResourceRef),
}

/// Where modules send [`Inbound`]s for one instance. Cheap to clone; safe to
/// keep after the instance is gone (deliveries are then dropped).
#[derive(Clone)]
pub struct Sink(Arc<dyn Fn(Inbound) + Send + Sync>);

impl Sink {
    pub fn new(f: impl Fn(Inbound) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
    pub fn send(&self, inbound: Inbound) {
        (self.0)(inbound)
    }
    /// A sink that drops everything, for instances that must stay inert.
    pub fn none() -> Self {
        Self::new(|_| {})
    }
}

/// A host capability exposed to guests: everything that is not pure
/// computation goes through one of these.
pub trait Module: Send + Sync + 'static {
    /// Namespace guests (or this module's prelude) invoke: `__rt.invoke(name, func, args)`.
    fn name(&self) -> &str;

    /// Guest-side API for this module, run before the sandbox is sealed with
    /// access to the private `__rt` helpers. `None` = no guest API.
    fn prelude(&self, _lang: Lang) -> Option<&str> {
        None
    }

    /// Called synchronously from guest code, or by the host for a declarative
    /// action. Functions in `args` arrive as `{"$fn": id}` handler markers.
    /// Must not block.
    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String>;

    /// A resource this module created (via [`ModuleCx::signal`] or
    /// [`ModuleCx::own`]) was disposed by the graph.
    fn release(&self, _key: u64) {}
}

/// What a module may do while handling a call.
pub struct ModuleCx<'a> {
    core: &'a mut Core,
    index: usize,
    sink: &'a Sink,
}

impl ModuleCx<'_> {
    pub fn extension(&self) -> &str {
        &self.core.extension
    }

    /// Fails unless the extension's manifest granted `capability`.
    pub fn require(&self, capability: &str) -> Result<(), String> {
        self.core.require(capability).map_err(|e| e.to_string())
    }

    /// A signal owned by the calling code. With `resource = Some(key)`,
    /// disposing the signal calls `release(key)` on this module, and the
    /// signal counts against the extension's resource quota.
    pub fn signal(&mut self, initial: Value, resource: Option<u64>) -> Result<SigId, String> {
        let r = resource.map(|key| ResourceRef {
            module: self.index,
            key,
        });
        self.core
            .sig_new_with(initial, r)
            .map_err(|e| e.to_string())
    }

    /// Like [`signal`](Self::signal), but owned by the instance instead of the
    /// calling code: it lives until the extension unloads. For values every
    /// part of an extension shares (a clock, a shared key), which the
    /// module's prelude can then cache — calling again from inside a binding
    /// only reads it.
    pub fn root_signal(&mut self, initial: Value, resource: Option<u64>) -> Result<SigId, String> {
        let prev = self.core.enter(None);
        let r = self.signal(initial, resource);
        self.core.leave(prev);
        r
    }

    /// Tie a non-signal resource (a timer, an in-flight request) to the
    /// caller. Returns the reference to send in [`Inbound::Finished`].
    pub fn own(&mut self, key: u64) -> Result<ResourceRef, String> {
        let r = ResourceRef {
            module: self.index,
            key,
        };
        self.core.own_resource(r).map_err(|e| e.to_string())?;
        Ok(r)
    }

    /// The resource ended early (e.g. the guest cancelled it).
    pub fn finish(&mut self, key: u64) {
        self.core.finish_resource(ResourceRef {
            module: self.index,
            key,
        });
    }

    pub fn sink(&self) -> Sink {
        self.sink.clone()
    }
}

// ---------------------------------------------------------------- tracing

/// One module call, made by guest code or a declarative action.
#[derive(Debug, Clone)]
pub struct InvokeRecord {
    pub extension: String,
    pub module: String,
    pub func: String,
    pub args: Value,
    pub result: Result<Value, String>,
    pub took: Duration,
}

/// Receives an [`InvokeRecord`] per module call; for devtools.
pub type InvokeTrace = Arc<dyn Fn(InvokeRecord) + Send + Sync>;

// ---------------------------------------------------------------- sources

/// The files an entry may import, by path relative to its root (`src/a.tsx`).
/// The host implements this — reading files, remembering them for hot reload —
/// so neither the core nor the engines touch the filesystem.
pub trait Sources: Send + Sync {
    fn read(&self, path: &str) -> Option<String>;
}

/// File extensions tried, in order, for an import without one.
pub const IMPORT_EXTENSIONS: &[&str] = &["tsx", "ts", "jsx", "js", "luau", "lua"];

/// Resolve `spec`, imported from the file `base`, to a path for [`Sources`].
/// Two kinds of imports exist:
///
/// - relative (`./x`, `../x`): inside the importer's own root, the project or
///   the package the importer belongs to;
/// - packages (`@kit/x`): files of a root the project declares. The path
///   keeps the `@kit` prefix; the host maps it, and finds nothing for names
///   the project did not declare.
///
/// Nothing else: no `fs`, no absolute paths, no URLs.
pub fn resolve_import(base: &str, spec: &str) -> Result<String, String> {
    let (mut parts, rest): (Vec<&str>, &str) = if let Some(pkg) = spec.strip_prefix('@') {
        let (name, rest) = pkg.split_once('/').ok_or_else(|| {
            format!("cannot import '{spec}': name a file of the package (@name/file)")
        })?;
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!("cannot import '{spec}': bad package name"));
        }
        (vec![&spec[..name.len() + 1]], rest)
    } else if spec.starts_with("./") || spec.starts_with("../") {
        let mut parts: Vec<&str> = base.split('/').collect();
        parts.pop(); // the importing file itself
        (parts, spec)
    } else {
        return Err(format!(
            "cannot import '{spec}': only relative imports and project packages (@name/...) exist"
        ));
    };
    // A package's files cannot climb out of it either.
    let floor = usize::from(parts.first().is_some_and(|p| p.starts_with('@')));
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.len() <= floor {
                    let what = if floor == 1 {
                        "the package"
                    } else {
                        "the project"
                    };
                    return Err(format!(
                        "cannot import '{spec}' from '{base}': outside {what}"
                    ));
                }
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    if parts.len() <= floor {
        return Err(format!("cannot import '{spec}'"));
    }
    Ok(parts.join("/"))
}

// ---------------------------------------------------------------- engines

/// Shared between an [`Instance`] and its engine.
pub struct HostCtx {
    pub core: RefCell<Core>,
    deadline: Cell<Instant>,
    pub limits: Limits,
    vocabulary: Arc<Vocabulary>,
    modules: Vec<Arc<dyn Module>>,
    sink: Sink,
    bucket: Cell<(f64, Instant)>,
    sources: Option<Arc<dyn Sources>>,
    trace: Option<InvokeTrace>,
}

impl HostCtx {
    /// For the engine's interrupt hook.
    pub fn budget_exceeded(&self) -> bool {
        Instant::now() > self.deadline.get()
    }

    fn arm(&self) {
        self.deadline.set(Instant::now() + self.limits.budget);
    }

    /// Restart the CPU budget. For engines, after host work done on the
    /// guest's behalf (loading and transpiling an imported file): that time
    /// is not guest CPU.
    pub fn rearm(&self) {
        self.arm();
    }

    /// Project files this instance may import; `None` = imports are refused.
    pub fn sources(&self) -> Option<&Arc<dyn Sources>> {
        self.sources.as_ref()
    }

    /// Components guests may construct, for the engines' preludes.
    pub fn components(&self) -> impl Iterator<Item = (&'static str, Option<&'static str>)> + '_ {
        self.vocabulary
            .guest_components()
            .map(|c| (c.name, c.shorthand))
    }

    fn take_token(&self) -> Result<(), String> {
        let rate = self.limits.calls_per_sec as f64;
        let (tokens, at) = self.bucket.get();
        let now = Instant::now();
        let tokens = (tokens + now.duration_since(at).as_secs_f64() * rate).min(rate);
        if tokens < 1.0 {
            self.bucket.set((tokens, now));
            return Err(format!(
                "extension '{}' exceeded {} module calls per second",
                self.core.borrow().extension,
                self.limits.calls_per_sec
            ));
        }
        self.bucket.set((tokens - 1.0, now));
        Ok(())
    }

    pub fn invoke(&self, module: &str, func: &str, args: Value) -> Result<Value, String> {
        let Some(trace) = &self.trace else {
            return self.invoke_untraced(module, func, args);
        };
        let started = Instant::now();
        let result = self.invoke_untraced(module, func, args.clone());
        trace(InvokeRecord {
            extension: self.core.borrow().extension.clone(),
            module: module.to_string(),
            func: func.to_string(),
            args,
            result: result.clone(),
            took: started.elapsed(),
        });
        result
    }

    fn invoke_untraced(&self, module: &str, func: &str, args: Value) -> Result<Value, String> {
        let (index, m) = self
            .modules
            .iter()
            .enumerate()
            .find(|(_, m)| m.name() == module)
            .ok_or_else(|| format!("no module '{module}' in this host"))?;
        self.take_token()?;
        let mut core = self.core.borrow_mut();
        let mut cx = ModuleCx {
            core: &mut core,
            index,
            sink: &self.sink,
        };
        m.invoke(&mut cx, func, args)
    }

    /// Module preludes for `lang`, in registration order.
    pub fn module_preludes(&self, lang: Lang) -> Vec<(String, String)> {
        self.modules
            .iter()
            .filter_map(|m| {
                m.prelude(lang)
                    .map(|p| (m.name().to_string(), p.to_string()))
            })
            .collect()
    }
}

/// A guest language runtime bound to one instance.
///
/// Values crossing the boundary are JSON. Guest functions found in values are
/// registered with the core and replaced by markers: `{"$bind": id}` for
/// reactive props, `{"$fn": id}` for handlers (see [`fn_kind`]). A guest's
/// core prelude must offer `signal`, `persist`, `log`, the component
/// constructors from [`HostCtx::components`] and, privately to module
/// preludes, `__rt.invoke`, `__rt.wrap` and `__rt.action`.
pub trait Engine {
    /// Evaluate the entry file; returns the exported root (a node or a binding).
    fn load(&mut self, source: &str, file: &str) -> Result<Value, GuestError>;
    fn call(&mut self, f: FnId, args: Value) -> Result<Value, GuestError>;
    /// Forget guest functions the graph disposed.
    fn drop_fns(&mut self, ids: &[FnId]);
}

pub trait EngineFactory: Send + Sync + 'static {
    fn name(&self) -> &str;
    /// File extensions this engine loads, without the dot.
    fn extensions(&self) -> &[&str];
    fn create(&self, host: Rc<HostCtx>) -> Result<Box<dyn Engine>, GuestError>;
}

pub const BIND: &str = "$bind";
pub const FN: &str = "$fn";
pub const ACTION: &str = "$action";

/// Decides how a guest function found under `key` is registered. Props named
/// `on_*` (or `onX` in JS, normalized by [`prop_key`]) are handlers; any other
/// function prop is a reactive binding. Functions passed to a module are
/// always handlers.
pub fn fn_kind(key: Option<&str>, to_module: bool) -> FnKind {
    if to_module || key.is_some_and(|k| k.starts_with("on_")) {
        FnKind::Handler
    } else {
        FnKind::Binding
    }
}

pub fn marker(kind: FnKind, id: FnId) -> Value {
    let k = if kind == FnKind::Handler { FN } else { BIND };
    serde_json::json!({ k: id })
}

/// The value of an action prop: `{"$action": {module, func, args}}`.
pub fn action_marker(module: &str, func: &str, args: Value) -> Value {
    serde_json::json!({ ACTION: { "module": module, "func": func, "args": args } })
}

/// Normalize JS-style handler names (`onClick`) to the vocabulary's
/// snake_case (`on_click`).
pub fn prop_key(key: &str) -> String {
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

// ---------------------------------------------------------------- instances

pub struct InstanceSpec<'a> {
    pub name: &'a str,
    /// File name of the entry; its extension picks the engine.
    pub file: &'a str,
    pub source: &'a str,
    pub grants: Grants,
    pub limits: Limits,
    pub engines: &'a [Arc<dyn EngineFactory>],
    pub modules: Vec<Arc<dyn Module>>,
    pub vocabulary: Arc<Vocabulary>,
    pub sink: Sink,
    /// Backing store for `persist(...)`; `None` = values live with the instance.
    pub persist: Option<PersistStore>,
    /// Files the entry may import; `None` = a single-file extension.
    pub sources: Option<Arc<dyn Sources>>,
    /// Sees every module call (devtools); `None` costs nothing.
    pub trace: Option<InvokeTrace>,
}

/// One loaded extension: an engine, its graph and its exported root.
pub struct Instance {
    pub name: String,
    // Drop order matters: the engine (and its guest handles) goes first.
    engine: Box<dyn Engine>,
    ctx: Rc<HostCtx>,
    root: Value,
}

impl Instance {
    pub fn new(spec: InstanceSpec<'_>) -> Result<Self, GuestError> {
        let ext = Path::new(spec.file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let factory = spec
            .engines
            .iter()
            .find(|f| f.extensions().contains(&ext))
            .ok_or_else(|| {
                let known: Vec<&str> = spec.engines.iter().map(|f| f.name()).collect();
                GuestError(format!(
                    "no engine for '.{ext}' files (engines: {})",
                    known.join(", ")
                ))
            })?;
        let mut core =
            Core::new(spec.name, spec.grants).with_max_resources(spec.limits.max_resources);
        if let Some(p) = spec.persist {
            core = core.with_persist(p);
        }
        let ctx = Rc::new(HostCtx {
            core: RefCell::new(core),
            deadline: Cell::new(Instant::now() + spec.limits.budget),
            limits: spec.limits,
            vocabulary: spec.vocabulary,
            modules: spec.modules,
            sink: spec.sink,
            bucket: Cell::new((spec.limits.calls_per_sec as f64, Instant::now())),
            sources: spec.sources,
            trace: spec.trace,
        });
        let mut engine = factory.create(ctx.clone())?;
        ctx.arm();
        let root = engine.load(spec.source, spec.file)?;
        let mut me = Self {
            name: spec.name.into(),
            engine,
            ctx,
            root,
        };
        me.collect();
        Ok(me)
    }

    /// Resolve every binding and validate the result against the vocabulary.
    /// Only dirty bindings re-enter the guest; clean ones reuse their cache.
    pub fn render(&mut self) -> Node {
        self.ctx.arm();
        let mut result = Err(GuestError("not rendered".into()));
        // A binding may set a signal another binding read; settle in a few passes.
        for _ in 0..3 {
            self.refresh_memos();
            let root = self.root.clone();
            result = self.materialize(&root, 0);
            if !self.ctx.core.borrow().is_dirty() {
                break;
            }
        }
        self.collect();
        match result.and_then(|v| {
            self.ctx
                .vocabulary
                .parse(v)
                .map_err(|e| GuestError(e.to_string()))
        }) {
            Ok(node) => node,
            Err(e) => Node::error(&self.name, &e.0),
        }
    }

    /// Re-run memos whose inputs changed; each sets its signal only if the
    /// result differs, which is what spares the code that reads it.
    fn refresh_memos(&mut self) {
        for _ in 0..8 {
            let dirty = self.ctx.core.borrow().dirty_memos();
            if dirty.is_empty() {
                return;
            }
            for (f, sig) in dirty {
                // An earlier memo of this pass may have disposed this one.
                if !self.ctx.core.borrow().is_binding(f) {
                    continue;
                }
                let prev = self.ctx.core.borrow_mut().begin_binding(f);
                let r = self.engine.call(f, Value::Null);
                self.ctx
                    .core
                    .borrow_mut()
                    .end_binding(f, prev, r.as_ref().ok().cloned());
                match r {
                    Ok(v) => {
                        let _ = self.ctx.core.borrow_mut().sig_set(sig, v);
                    }
                    Err(e) => self.ctx.core.borrow_mut().log(format!("memo failed: {e}")),
                }
            }
        }
    }

    pub fn needs_render(&self) -> bool {
        self.ctx.core.borrow().is_dirty()
    }

    /// Apply something a module sent.
    pub fn deliver(&mut self, inbound: Inbound) -> Result<(), GuestError> {
        let r = match inbound {
            Inbound::Signal { sig, value } => {
                self.ctx.core.borrow_mut().deliver(sig, value);
                Ok(())
            }
            Inbound::Callback { f, value } => self.run_untracked(f, value),
            Inbound::Finished(r) => {
                self.ctx.core.borrow_mut().finish_resource(r);
                Ok(())
            }
        };
        self.collect();
        r
    }

    /// The user triggered an event prop. Guest functions run as untracked
    /// handlers; actions go straight to their module without running guest
    /// code. `payload` is the event data (`null` for clicks).
    pub fn trigger(&mut self, handler: &Handler, payload: Value) -> Result<(), GuestError> {
        let r = match handler {
            Handler::Fn(f) => self.run_untracked(*f, payload),
            Handler::Action { action } => self.run_action(action, payload),
        };
        self.collect();
        r
    }

    /// Shorthand for `trigger(&Handler::Fn(f), payload)`.
    pub fn ui(&mut self, handler: FnId, payload: Value) -> Result<(), GuestError> {
        self.trigger(&Handler::Fn(handler), payload)
    }

    pub fn take_logs(&mut self) -> Vec<String> {
        self.ctx.core.borrow_mut().take_logs()
    }

    /// Like [`take_logs`](Self::take_logs), with where each line came from.
    pub fn take_log_lines(&mut self) -> Vec<LogLine> {
        self.ctx.core.borrow_mut().take_log_lines()
    }

    /// (live bindings, live signals) — exposed for leak tests.
    pub fn live_counts(&self) -> (usize, usize) {
        self.ctx.core.borrow().live_counts()
    }

    pub fn live_resources(&self) -> usize {
        self.ctx.core.borrow().live_resources()
    }

    fn run_untracked(&mut self, f: FnId, args: Value) -> Result<(), GuestError> {
        self.ctx.arm();
        let prev = self.ctx.core.borrow_mut().enter(Some(f));
        let r = self.engine.call(f, args);
        self.ctx.core.borrow_mut().leave(prev);
        r.map(|_| ())
    }

    fn run_action(&mut self, action: &Action, payload: Value) -> Result<(), GuestError> {
        let mut args = action.args.clone();
        if !payload.is_null() {
            match &mut args {
                Value::Object(m) => {
                    m.insert("event".into(), payload);
                }
                Value::Null => args = serde_json::json!({ "event": payload }),
                _ => {}
            }
        }
        // Owned by the instance root: actions are fire-and-forget.
        let prev = self.ctx.core.borrow_mut().enter(None);
        let r = self.ctx.invoke(&action.module, &action.func, args);
        self.ctx.core.borrow_mut().leave(prev);
        r.map(|_| ()).map_err(GuestError)
    }

    fn collect(&mut self) {
        let fns = self.ctx.core.borrow_mut().take_released_fns();
        if !fns.is_empty() {
            self.engine.drop_fns(&fns);
        }
        self.release_resources();
    }

    fn release_resources(&self) {
        let resources = self.ctx.core.borrow_mut().take_released_resources();
        for r in resources {
            if let Some(m) = self.ctx.modules.get(r.module) {
                m.release(r.key);
            }
        }
    }

    fn eval_binding(&mut self, id: FnId) -> Result<Value, GuestError> {
        if let Some(v) = self.ctx.core.borrow().binding_cached(id) {
            return Ok(v.clone());
        }
        if !self.ctx.core.borrow().is_binding(id) {
            return Err(GuestError(format!("stale binding {id}")));
        }
        let prev = self.ctx.core.borrow_mut().begin_binding(id);
        let r = self.engine.call(id, Value::Null);
        self.ctx
            .core
            .borrow_mut()
            .end_binding(id, prev, r.as_ref().ok().cloned());
        r
    }

    fn materialize(&mut self, v: &Value, depth: usize) -> Result<Value, GuestError> {
        if depth > self.ctx.limits.max_depth {
            return Err(GuestError("view tree too deep".into()));
        }
        Ok(match v {
            Value::Object(m) if m.len() == 1 && m.contains_key(BIND) => {
                let id = m[BIND].as_u64().unwrap_or_default();
                let raw = self.eval_binding(id)?;
                self.materialize(&raw, depth + 1)?
            }
            Value::Object(m) if m.len() == 1 && m.contains_key(FN) => m[FN].clone(),
            Value::Object(m) if m.len() == 1 && m.contains_key(ACTION) => v.clone(),
            Value::Object(m) => {
                let mut out = Map::with_capacity(m.len());
                // A hidden surface or a closed popup shows nothing: leave its
                // children unevaluated (their bindings keep their caches).
                let gate = match m.get("type").and_then(Value::as_str) {
                    Some(view::styled::SURFACE) => Some("visible"),
                    Some(view::styled::POPUP) => Some("open"),
                    _ => None,
                };
                let hidden = match gate.and_then(|g| m.get(g)) {
                    Some(v) => self.materialize(v, depth + 1)? == Value::Bool(false),
                    None => false,
                };
                for (k, val) in m {
                    if hidden && k == "children" {
                        continue;
                    }
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

impl Drop for Instance {
    fn drop(&mut self) {
        // Unsubscribe, cancel timers... everything the extension still holds.
        self.ctx.core.borrow_mut().dispose_all();
        self.release_resources();
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_import;

    #[test]
    fn imports_resolve_inside_the_root() {
        assert_eq!(
            resolve_import("src/surfaces/bar.tsx", "../components/Tile").unwrap(),
            "src/components/Tile"
        );
        assert_eq!(resolve_import("src/a.ts", "./b").unwrap(), "src/b");
        assert_eq!(resolve_import("a.ts", "./lib/x.ts").unwrap(), "lib/x.ts");
        assert!(resolve_import("src/a.ts", "../../etc/passwd").is_err());
        assert!(resolve_import("a.ts", "../x").is_err());
        assert!(resolve_import("a.ts", "fs").is_err());
        assert!(resolve_import("a.ts", "/etc/passwd").is_err());
        // Packages: named roots, with the same confinement.
        assert_eq!(
            resolve_import("src/bar.tsx", "@kit/components/Icon").unwrap(),
            "@kit/components/Icon"
        );
        assert_eq!(
            resolve_import("@kit/components/Icon.tsx", "../lib/glyphs").unwrap(),
            "@kit/lib/glyphs"
        );
        assert!(
            resolve_import("@kit/Icon.tsx", "../../src/secret")
                .unwrap_err()
                .contains("outside the package")
        );
        assert!(resolve_import("a.ts", "@kit").is_err());
        assert!(resolve_import("a.ts", "@../x").is_err());
        assert!(resolve_import("a.ts", "@kit/../../x").is_err());
    }
}
