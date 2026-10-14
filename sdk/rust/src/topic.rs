use std::sync::{Arc, Mutex, MutexGuard};

use async_channel::Sender;
use serde::Serialize;
use serde_json::Value;

/// One subscription on the wire: the client it belongs to and the request id
/// its events carry.
struct Sub {
    client: u64,
    id: Value,
    tx: Sender<String>,
}

struct Inner<T> {
    value: T,
    json: Value,
    subs: Vec<Sub>,
}

/// A subscribable path with its current value.
///
/// Cheap to clone and `Send + Sync`: publish from any task or thread.
/// `T` is anything `Serialize` (a `serde_json::Value`, a struct, an
/// `Option<..>` for "null until known").
pub struct Topic<T> {
    path: Arc<str>,
    inner: Arc<Mutex<Inner<T>>>,
}

impl<T> Clone for Topic<T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            inner: self.inner.clone(),
        }
    }
}

pub(crate) fn event_line(id: &Value, value: &str) -> String {
    format!("{{\"id\":{id},\"event\":{value}}}\n")
}

fn to_json<T: Serialize>(path: &str, v: &T) -> Option<Value> {
    match serde_json::to_value(v) {
        Ok(j) => Some(j),
        Err(e) => {
            eprintln!("topic {path}: value does not serialize: {e}");
            None
        }
    }
}

impl<T: Serialize + Clone + Send + 'static> Topic<T> {
    pub(crate) fn new(path: &str, initial: T) -> Self {
        let json = to_json(path, &initial).unwrap_or(Value::Null);
        Self {
            path: path.into(),
            inner: Arc::new(Mutex::new(Inner {
                value: initial,
                json,
                subs: Vec::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner<T>> {
        // A panic while holding the lock leaves plain data behind: keep going.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Set the value and notify subscribers, unless it serializes to the same
    /// JSON as the current one. Returns whether it changed.
    pub fn publish(&self, value: T) -> bool {
        let Some(json) = to_json(&self.path, &value) else {
            return false;
        };
        let mut inner = self.lock();
        if inner.json == json {
            return false;
        }
        let text = json.to_string();
        inner.value = value;
        inner.json = json;
        inner
            .subs
            .retain(|s| s.tx.try_send(event_line(&s.id, &text)).is_ok());
        true
    }

    /// Change the value in place and publish it (if it changed).
    pub fn update(&self, f: impl FnOnce(&mut T)) -> bool {
        let mut v = self.get();
        f(&mut v);
        self.publish(v)
    }

    /// The current value.
    pub fn get(&self) -> T {
        self.lock().value.clone()
    }

    /// The current value as JSON.
    pub fn json(&self) -> Value {
        self.lock().json.clone()
    }
}

/// What the protocol needs from a topic, whatever its `T`.
pub(crate) trait AnyTopic {
    fn current(&self) -> Value;
    /// Queue the current value as the first event and register, atomically,
    /// so no change can slip in between.
    fn subscribe(&self, client: u64, id: Value, tx: &Sender<String>);
    fn unsubscribe(&self, client: u64, id: &Value);
    fn drop_client(&self, client: u64);
}

impl<T: Serialize + Clone + Send + 'static> AnyTopic for Topic<T> {
    fn current(&self) -> Value {
        self.json()
    }

    fn subscribe(&self, client: u64, id: Value, tx: &Sender<String>) {
        let mut inner = self.lock();
        let _ = tx.try_send(event_line(&id, &inner.json.to_string()));
        inner.subs.push(Sub {
            client,
            id,
            tx: tx.clone(),
        });
    }

    fn unsubscribe(&self, client: u64, id: &Value) {
        self.lock()
            .subs
            .retain(|s| !(s.client == client && &s.id == id));
    }

    fn drop_client(&self, client: u64) {
        self.lock().subs.retain(|s| s.client != client);
    }
}
