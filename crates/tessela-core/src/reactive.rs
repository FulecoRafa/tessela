//! Engine-independent reactive graph.
//!
//! Signals live here, in Rust, not in the guest. A guest reading `vol()` calls
//! [`Core::sig_get`], which records the read against whichever binding is
//! being evaluated. That is all "tracking" means, and it is why every engine
//! gets identical semantics: none of them implements reactivity itself.
//!
//! Ownership follows Solid: everything a binding creates while it runs
//! (nested bindings, handlers, local signals, module resources) is owned by
//! that binding and disposed when it re-runs. Disposing a resource reports it
//! back so the module that created it can release it (unsubscribe, cancel a
//! timer, ...). The core itself never performs I/O.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

pub type FnId = u64;
pub type SigId = u64;

/// A module-owned resource tied to the lifetime of its owner in the graph.
/// `module` is the index of the module in the instance's module list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceRef {
    pub module: usize,
    pub key: u64,
}

#[derive(Debug)]
struct Signal {
    value: Value,
    dependents: HashSet<FnId>,
    resource: Option<ResourceRef>,
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
    Resource(ResourceRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKind {
    /// Re-evaluated when the signals it read change (reactive props).
    Binding,
    /// Called on demand (event handlers, callbacks); never tracked.
    Handler,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("extension '{extension}' did not declare capability '{capability}' in its manifest")]
    Undeclared {
        extension: String,
        capability: String,
    },
    #[error("unknown signal {0}")]
    UnknownSignal(SigId),
}

/// Capabilities granted to one extension. `service:*` grants every `service:`.
#[derive(Debug, Clone, Default)]
pub struct Grants(HashSet<String>);

impl Grants {
    pub fn new(caps: impl IntoIterator<Item = String>) -> Self {
        Self(caps.into_iter().collect())
    }

    pub fn allows(&self, cap: &str) -> bool {
        if self.0.contains(cap) {
            return true;
        }
        match cap.split_once(':') {
            Some((kind, _)) => self.0.contains(&format!("{kind}:*")),
            None => false,
        }
    }
}

#[derive(Debug)]
pub struct Core {
    pub extension: String,
    grants: Grants,
    next_id: u64,
    signals: HashMap<SigId, Signal>,
    bindings: HashMap<FnId, Binding>,
    owned: HashMap<Option<FnId>, Vec<Owned>>,
    owner: Option<FnId>,
    tracking: Vec<HashSet<SigId>>,
    logs: Vec<String>,
    /// Guest functions disposed since the engine last collected them.
    released_fns: Vec<FnId>,
    /// Module resources disposed since the instance last released them.
    released_resources: Vec<ResourceRef>,
}

impl Core {
    pub fn new(extension: impl Into<String>, grants: Grants) -> Self {
        Self {
            extension: extension.into(),
            grants,
            next_id: 0,
            signals: HashMap::new(),
            bindings: HashMap::new(),
            owned: HashMap::new(),
            owner: None,
            tracking: Vec::new(),
            logs: Vec::new(),
            released_fns: Vec::new(),
            released_resources: Vec::new(),
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn own(&mut self, item: Owned) {
        self.owned.entry(self.owner).or_default().push(item);
    }

    // ---- guest-facing -----------------------------------------------------

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
        self.sig_new_with(value, None)
    }

    /// A signal backed by a module resource: disposing the signal releases it.
    pub fn sig_new_with(&mut self, value: Value, resource: Option<ResourceRef>) -> SigId {
        let id = self.id();
        self.signals.insert(
            id,
            Signal {
                value,
                dependents: HashSet::new(),
                resource,
            },
        );
        self.own(Owned::Sig(id));
        id
    }

    /// Tie a resource that is not a signal (a timer, a pending call) to the
    /// current owner.
    pub fn own_resource(&mut self, resource: ResourceRef) {
        self.own(Owned::Resource(resource));
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

    pub fn require(&self, capability: &str) -> Result<(), HostError> {
        if self.grants.allows(capability) {
            Ok(())
        } else {
            Err(HostError::Undeclared {
                extension: self.extension.clone(),
                capability: capability.into(),
            })
        }
    }

    pub fn log(&mut self, message: String) {
        self.logs.push(message);
    }

    pub fn current_owner(&self) -> Option<FnId> {
        self.owner
    }

    // ---- driver-facing ----------------------------------------------------

    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    pub fn take_released_fns(&mut self) -> Vec<FnId> {
        std::mem::take(&mut self.released_fns)
    }

    pub fn take_released_resources(&mut self) -> Vec<ResourceRef> {
        std::mem::take(&mut self.released_resources)
    }

    pub fn is_dirty(&self) -> bool {
        self.bindings.values().any(|b| b.dirty)
    }

    /// Value pushed from outside. Unknown ids (already disposed) are ignored.
    pub fn deliver(&mut self, id: SigId, value: Value) {
        let _ = self.sig_set(id, value);
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

    /// Dispose the whole graph, releasing every module resource.
    pub fn dispose_all(&mut self) {
        let owners: Vec<Option<FnId>> = self.owned.keys().copied().collect();
        for o in owners {
            self.dispose_children(o);
        }
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
                    self.released_fns.push(f);
                }
                Owned::Sig(s) => {
                    if let Some(r) = self.signals.remove(&s).and_then(|sig| sig.resource) {
                        self.released_resources.push(r);
                    }
                }
                Owned::Resource(r) => self.released_resources.push(r),
            }
        }
    }

    /// (live bindings, live signals) — exposed for leak tests.
    pub fn live_counts(&self) -> (usize, usize) {
        (self.bindings.len(), self.signals.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_grants() {
        let g = Grants::new(["service:power".to_string(), "timer:*".to_string()]);
        assert!(g.allows("service:power"));
        assert!(!g.allows("service:audio"));
        assert!(g.allows("timer:after"));
        assert!(!g.allows("timer"));
    }

    #[test]
    fn rerunning_a_binding_releases_its_resources() {
        let mut c = Core::new("t", Grants::default());
        let b = c.register_fn(FnKind::Binding);
        let prev = c.begin_binding(b);
        let r = ResourceRef { module: 0, key: 7 };
        c.sig_new_with(Value::Null, Some(r));
        c.end_binding(b, prev, None);
        assert!(c.take_released_resources().is_empty());
        let prev = c.begin_binding(b);
        c.end_binding(b, prev, None);
        assert_eq!(c.take_released_resources(), [r]);
    }
}
