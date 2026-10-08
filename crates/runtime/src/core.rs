//! Engine-independent reactive core.
//!
//! Signals live here, in Rust, not in the guest. A guest reading `vol()` calls
//! [`Core::sig_get`], which records the read against whichever binding is being
//! evaluated. That is all "tracking" means, and it is why Luau and TSX widgets
//! get identical semantics: neither engine implements reactivity itself.
//!
//! Ownership follows Solid: everything a binding creates while it runs (nested
//! bindings, handlers, local signals, service subscriptions) is owned by that
//! binding and disposed when it re-runs, so a widget that rebuilds a list on
//! every change does not leak callbacks or subscriptions.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

pub type FnId = u64;
pub type SigId = u64;
pub type Token = u64;

/// Requests the widget makes of the outside world. The runtime never performs
/// I/O itself; the shell drains these and talks to the connector.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Subscribe {
        service: String,
        path: String,
        token: SigId,
    },
    Unsubscribe {
        service: String,
        path: String,
        token: SigId,
    },
    Call {
        service: String,
        method: String,
        args: Value,
        token: Token,
    },
    Log {
        widget: String,
        message: String,
    },
}

#[derive(Debug)]
struct Signal {
    value: Value,
    dependents: HashSet<FnId>,
    source: Option<(String, String)>,
}

#[derive(Debug, Default)]
struct Binding {
    deps: HashSet<SigId>,
    cached: Option<Value>,
    dirty: bool,
}

#[derive(Debug, Clone, Copy)]
enum Owned {
    Fn(FnId),
    Sig(SigId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKind {
    Binding,
    Handler,
}

#[derive(Debug)]
pub struct Core {
    pub widget: String,
    uses: HashSet<String>,
    next_id: u64,
    signals: HashMap<SigId, Signal>,
    bindings: HashMap<FnId, Binding>,
    owned: HashMap<Option<FnId>, Vec<Owned>>,
    owner: Option<FnId>,
    tracking: Vec<HashSet<SigId>>,
    effects: Vec<Effect>,
    pending_calls: HashMap<Token, Option<FnId>>,
    /// Functions disposed since the engine last collected them.
    released: Vec<FnId>,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("widget '{widget}' did not declare service '{service}' in its manifest (uses = [...])")]
    Undeclared { widget: String, service: String },
    #[error("unknown signal {0}")]
    UnknownSignal(SigId),
}

impl Core {
    pub fn new(widget: impl Into<String>, uses: impl IntoIterator<Item = String>) -> Self {
        Self {
            widget: widget.into(),
            uses: uses.into_iter().collect(),
            next_id: 0,
            signals: HashMap::new(),
            bindings: HashMap::new(),
            owned: HashMap::new(),
            owner: None,
            tracking: Vec::new(),
            effects: Vec::new(),
            pending_calls: HashMap::new(),
            released: Vec::new(),
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn own(&mut self, item: Owned) {
        self.owned.entry(self.owner).or_default().push(item);
    }

    // ---- called from guest host functions -------------------------------

    pub fn register_fn(&mut self, kind: FnKind) -> FnId {
        let id = self.id();
        if kind == FnKind::Binding {
            self.bindings.insert(
                id,
                Binding {
                    dirty: true,
                    ..Default::default()
                },
            );
        }
        self.own(Owned::Fn(id));
        id
    }

    pub fn sig_new(&mut self, value: Value) -> SigId {
        let id = self.id();
        self.signals.insert(
            id,
            Signal {
                value,
                dependents: HashSet::new(),
                source: None,
            },
        );
        self.own(Owned::Sig(id));
        id
    }

    /// Tracked read: inside a binding, records the dependency.
    pub fn sig_get(&mut self, id: SigId) -> Result<Value, HostError> {
        let sig = self.signals.get(&id).ok_or(HostError::UnknownSignal(id))?;
        let v = sig.value.clone();
        if let Some(frame) = self.tracking.last_mut() {
            frame.insert(id);
        }
        Ok(v)
    }

    /// Untracked read, for `update(fn)` and handlers.
    pub fn sig_peek(&self, id: SigId) -> Result<Value, HostError> {
        self.signals
            .get(&id)
            .map(|s| s.value.clone())
            .ok_or(HostError::UnknownSignal(id))
    }

    pub fn sig_set(&mut self, id: SigId, value: Value) -> Result<(), HostError> {
        let sig = self
            .signals
            .get_mut(&id)
            .ok_or(HostError::UnknownSignal(id))?;
        if sig.value == value {
            return Ok(());
        }
        sig.value = value;
        let deps: Vec<FnId> = sig.dependents.iter().copied().collect();
        for b in deps {
            if let Some(binding) = self.bindings.get_mut(&b) {
                binding.dirty = true;
            }
        }
        Ok(())
    }

    pub fn check_use(&self, service: &str) -> Result<(), HostError> {
        if self.uses.contains(service) {
            Ok(())
        } else {
            Err(HostError::Undeclared {
                widget: self.widget.clone(),
                service: service.into(),
            })
        }
    }

    pub fn subscribe(&mut self, service: &str, path: &str) -> Result<SigId, HostError> {
        self.check_use(service)?;
        let id = self.sig_new(Value::Null);
        self.signals.get_mut(&id).unwrap().source = Some((service.into(), path.into()));
        self.effects.push(Effect::Subscribe {
            service: service.into(),
            path: path.into(),
            token: id,
        });
        Ok(id)
    }

    pub fn call(
        &mut self,
        service: &str,
        method: &str,
        args: Value,
        callback: Option<FnId>,
    ) -> Result<Token, HostError> {
        self.check_use(service)?;
        let token = self.id();
        self.pending_calls.insert(token, callback);
        self.effects.push(Effect::Call {
            service: service.into(),
            method: method.into(),
            args,
            token,
        });
        Ok(token)
    }

    pub fn log(&mut self, message: String) {
        let widget = self.widget.clone();
        self.effects.push(Effect::Log { widget, message });
    }

    // ---- called by the widget driver -------------------------------------

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    pub fn take_released(&mut self) -> Vec<FnId> {
        std::mem::take(&mut self.released)
    }

    pub fn is_dirty(&self) -> bool {
        self.bindings.values().any(|b| b.dirty)
    }

    /// Value pushed by a service. Unknown tokens (already disposed) are ignored.
    pub fn deliver(&mut self, token: SigId, value: Value) {
        let _ = self.sig_set(token, value);
    }

    pub fn take_call(&mut self, token: Token) -> Option<Option<FnId>> {
        self.pending_calls.remove(&token)
    }

    pub fn binding_cached(&self, id: FnId) -> Option<&Value> {
        self.bindings
            .get(&id)
            .filter(|b| !b.dirty)
            .and_then(|b| b.cached.as_ref())
    }

    pub fn is_binding(&self, id: FnId) -> bool {
        self.bindings.contains_key(&id)
    }

    /// Prepare to (re)evaluate a binding: dispose what its last run created,
    /// make it the owner and open a tracking frame. Returns the previous owner.
    pub fn begin_binding(&mut self, id: FnId) -> Option<FnId> {
        self.dispose_children(Some(id));
        self.tracking.push(HashSet::new());
        std::mem::replace(&mut self.owner, Some(id))
    }

    pub fn end_binding(&mut self, id: FnId, prev_owner: Option<FnId>, result: Option<Value>) {
        self.owner = prev_owner;
        let deps = self.tracking.pop().unwrap_or_default();
        let Some(binding) = self.bindings.get_mut(&id) else {
            return;
        };
        let old = std::mem::replace(&mut binding.deps, deps.clone());
        binding.dirty = false;
        binding.cached = result;
        for s in old.difference(&deps) {
            if let Some(sig) = self.signals.get_mut(s) {
                sig.dependents.remove(&id);
            }
        }
        for s in &deps {
            if let Some(sig) = self.signals.get_mut(s) {
                sig.dependents.insert(id);
            }
        }
    }

    /// Run untracked guest code (handlers, callbacks) owned by `owner`.
    pub fn enter(&mut self, owner: Option<FnId>) -> Option<FnId> {
        std::mem::replace(&mut self.owner, owner)
    }

    pub fn leave(&mut self, prev: Option<FnId>) {
        self.owner = prev;
    }

    fn dispose_children(&mut self, owner: Option<FnId>) {
        let Some(items) = self.owned.remove(&owner) else {
            return;
        };
        for item in items {
            match item {
                Owned::Fn(f) => {
                    if let Some(b) = self.bindings.remove(&f) {
                        for s in b.deps {
                            if let Some(sig) = self.signals.get_mut(&s) {
                                sig.dependents.remove(&f);
                            }
                        }
                    }
                    self.dispose_children(Some(f));
                    self.released.push(f);
                }
                Owned::Sig(s) => {
                    if let Some(sig) = self.signals.remove(&s) {
                        if let Some((service, path)) = sig.source {
                            self.effects.push(Effect::Unsubscribe {
                                service,
                                path,
                                token: s,
                            });
                        }
                    }
                }
            }
        }
    }

    pub fn live_counts(&self) -> (usize, usize) {
        (self.bindings.len(), self.signals.len())
    }
}
