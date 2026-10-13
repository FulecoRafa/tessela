//! A named value produced on a background thread and delivered to every
//! extension that subscribed to it. Subscriptions are root signals (owned by
//! the extension, not the calling code), so preludes cache them per topic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tessela_core::{Inbound, ModuleCx, SigId, Sink};

#[derive(Default)]
struct Topic {
    value: Value,
    subs: HashMap<u64, (Sink, SigId)>,
}

/// Subscription keys are unique in the process, so a module may subscribe an
/// extension to another module's topic and forward `release` safely.
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// Every topic of one module.
#[derive(Default)]
pub(crate) struct Topics {
    topics: Mutex<HashMap<String, Topic>>,
    /// Subscription key → topic name, for `release`.
    keys: Mutex<HashMap<u64, String>>,
    /// Host code following a topic (the renderer follows the color scheme).
    listeners: Mutex<Vec<(String, crossbeam_channel::Sender<Value>)>>,
}

impl Topics {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// A root signal holding the topic's current value (`null` until the
    /// first publish). Returns `{ "sig": id }` for the prelude.
    pub fn subscribe(&self, cx: &mut ModuleCx<'_>, name: &str) -> Result<Value, String> {
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        let mut topics = self.topics.lock().unwrap();
        let topic = topics.entry(name.to_string()).or_default();
        let sig = cx.root_signal(topic.value.clone(), Some(key))?;
        topic.subs.insert(key, (cx.sink(), sig));
        self.keys.lock().unwrap().insert(key, name.to_string());
        Ok(json!({ "sig": sig }))
    }

    /// Set the value; subscribers hear about it only if it changed.
    pub fn publish(&self, name: &str, value: Value) {
        let mut topics = self.topics.lock().unwrap();
        let topic = topics.entry(name.to_string()).or_default();
        if topic.value == value {
            return;
        }
        self.listeners
            .lock()
            .unwrap()
            .retain(|(n, tx)| n != name || tx.send(value.clone()).is_ok());
        for (sink, sig) in topic.subs.values() {
            sink.send(Inbound::Signal {
                sig: *sig,
                value: value.clone(),
            });
        }
        topic.value = value;
    }

    /// Every new value of `name`, starting with the current one if any.
    pub fn listen(&self, name: &str) -> crossbeam_channel::Receiver<Value> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let current = self.get(name);
        if !current.is_null() {
            let _ = tx.send(current);
        }
        self.listeners.lock().unwrap().push((name.to_string(), tx));
        rx
    }

    pub fn get(&self, name: &str) -> Value {
        self.topics
            .lock()
            .unwrap()
            .get(name)
            .map(|t| t.value.clone())
            .unwrap_or(Value::Null)
    }

    /// Names of topics someone is subscribed to.
    pub fn watched(&self) -> Vec<String> {
        self.topics
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| !t.subs.is_empty())
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn release(&self, key: u64) {
        if let Some(name) = self.keys.lock().unwrap().remove(&key)
            && let Some(t) = self.topics.lock().unwrap().get_mut(&name)
        {
            t.subs.remove(&key);
        }
    }
}

/// JS prelude helper shared by the modules: `topic(module, name)` returns a
/// cached read-only signal for the topic.
pub(crate) const JS_TOPIC: &str = r#"
  const topics = new Map();
  const topic = (module, name) => {
    const key = module + ":" + name;
    let s = topics.get(key);
    if (!s) {
      s = rt.wrap(rt.invoke(module, "subscribe", { topic: name }).sig);
      topics.set(key, s);
    }
    return s;
  };
"#;

/// Luau counterpart of [`JS_TOPIC`].
pub(crate) const LUAU_TOPIC: &str = r#"
local topics = {}
local function topic(module, name)
	local key = module .. ":" .. name
	if not topics[key] then
		topics[key] = rt.wrap(rt.invoke(module, "subscribe", { topic = name }).sig)
	end
	return topics[key]
end
"#;
