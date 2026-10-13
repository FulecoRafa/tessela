//! Module `state`: values shared by every extension of an app.
//!
//! Needs the capability `state`. `shared(key, initial)` returns a writable
//! signal; every extension that opens the same key sees the same value, and a
//! `set` anywhere reaches all of them. With `{ store: true }` the value is
//! also saved to the app's state file and read back on the next start.
//!
//! ```tsx
//! const panel = shared("panel", "");                // bar and drawer agree
//! const pins = shared("pinned", [], { store: true }); // survives restarts
//! panel.set("drawer");
//! ```
//!
//! The signal belongs to the extension, not to the code that opened it, so
//! calling `shared("panel")` again (even inside a binding) returns the same
//! signal. The first `initial` wins; later ones are ignored.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Sender, unbounded};
use serde_json::{Map, Value, json};
use tessela_core::{Inbound, Lang, Module, ModuleCx, SigId, Sink};

const MAX_KEY: usize = 128;
/// Writes are coalesced: one save at most this often.
const SAVE_DELAY: Duration = Duration::from_millis(300);

const JS: &str = r#"
"use strict";
(() => {
  const rt = globalThis.__rt;
  const cache = new Map();
  rt.define("shared", (key, initial, opts) => {
    key = String(key);
    let s = cache.get(key);
    if (s) return s;
    const store = !!(opts && opts.store);
    const local = rt.wrap(rt.invoke("state", "open", { key, initial: initial === undefined ? null : initial, store }).sig);
    const read = () => local();
    read.peek = () => local.peek();
    read.set = (v) => {
      local.set(v);
      rt.invoke("state", "set", { key, value: v === undefined ? null : v });
    };
    read.update = (f) => read.set(f(local.peek()));
    s = Object.freeze(read);
    cache.set(key, s);
    return s;
  });
})();
"#;

const LUAU: &str = r#"
local rt = __rt
local cache = {}
local Shared = {}
Shared.__index = Shared
Shared.__metatable = "locked"
function Shared.__call(self) return self._local() end
function Shared:get() return self._local() end
function Shared:peek() return self._local:peek() end
function Shared:set(v)
	self._local:set(v)
	rt.invoke("state", "set", { key = self._key, value = v })
end
function Shared:update(f) self:set(f(self._local:peek())) end
function shared(key, initial, opts)
	key = tostring(key)
	if cache[key] then return cache[key] end
	local sig = rt.invoke("state", "open", { key = key, initial = initial, store = opts ~= nil and opts.store == true }).sig
	local s = setmetatable({ _key = key, _local = rt.wrap(sig) }, Shared)
	table.freeze(s)
	cache[key] = s
	return s
end
"#;

struct Sub {
    key: String,
    sink: Sink,
    sig: SigId,
}

#[derive(Default)]
struct Inner {
    values: HashMap<String, Value>,
    subs: HashMap<u64, Sub>,
    stored: HashSet<String>,
}

/// One per app: every extension sees the same values.
pub struct SharedState {
    inner: Mutex<Inner>,
    next: AtomicU64,
    saver: Option<Sender<Map<String, Value>>>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    /// Values live as long as the process.
    pub fn new() -> Self {
        Self {
            inner: Mutex::default(),
            next: AtomicU64::new(1),
            saver: None,
        }
    }

    /// Keys opened with `{ store: true }` are saved to `file` (a JSON object)
    /// and read back from it.
    pub fn with_store(file: impl Into<PathBuf>) -> Self {
        let file = file.into();
        let mut inner = Inner::default();
        match std::fs::read_to_string(&file) {
            Ok(text) => match serde_json::from_str::<Map<String, Value>>(&text) {
                Ok(saved) => {
                    inner.stored = saved.keys().cloned().collect();
                    inner.values = saved.into_iter().collect();
                }
                Err(e) => log::warn!("state: ignoring {}: {e}", file.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("state: cannot read {}: {e}", file.display()),
        }
        let (tx, rx) = unbounded::<Map<String, Value>>();
        thread::Builder::new()
            .name("state-saver".into())
            .spawn(move || {
                while let Ok(mut latest) = rx.recv() {
                    thread::sleep(SAVE_DELAY);
                    while let Ok(newer) = rx.try_recv() {
                        latest = newer;
                    }
                    if let Err(e) = save(&file, &latest) {
                        log::warn!("state: cannot save {}: {e}", file.display());
                    }
                }
            })
            .expect("spawn state saver");
        Self {
            inner: Mutex::new(inner),
            next: AtomicU64::new(1),
            saver: Some(tx),
        }
    }

    /// Current value of every key, for diagnostics.
    pub fn snapshot(&self) -> Map<String, Value> {
        let inner = self.inner.lock().unwrap();
        inner
            .values
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn key(args: &Value) -> Result<String, String> {
        let key = args["key"].as_str().ok_or("missing key")?;
        if key.is_empty() || key.len() > MAX_KEY {
            return Err(format!("state keys are 1..{MAX_KEY} characters"));
        }
        Ok(key.to_string())
    }
}

fn save(file: &std::path::Path, values: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(values)?)?;
    std::fs::rename(tmp, file)
}

impl Module for SharedState {
    fn name(&self) -> &str {
        "state"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => LUAU,
            Lang::JavaScript => JS,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String> {
        cx.require("state")?;
        match func {
            "open" => {
                let key = Self::key(&args)?;
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                let mut inner = self.inner.lock().unwrap();
                if args["store"].as_bool() == Some(true) {
                    inner.stored.insert(key.clone());
                }
                let value = inner
                    .values
                    .entry(key.clone())
                    .or_insert_with(|| args["initial"].clone())
                    .clone();
                let sig = cx.root_signal(value, Some(id))?;
                inner.subs.insert(
                    id,
                    Sub {
                        key,
                        sink: cx.sink(),
                        sig,
                    },
                );
                Ok(json!({ "sig": sig }))
            }
            "set" => {
                let key = Self::key(&args)?;
                let value = args["value"].clone();
                let mut inner = self.inner.lock().unwrap();
                if inner.values.get(&key) == Some(&value) {
                    return Ok(Value::Null);
                }
                inner.values.insert(key.clone(), value.clone());
                for sub in inner.subs.values().filter(|s| s.key == key) {
                    sub.sink.send(Inbound::Signal {
                        sig: sub.sig,
                        value: value.clone(),
                    });
                }
                if inner.stored.contains(&key)
                    && let Some(saver) = &self.saver
                {
                    let stored: Map<String, Value> = inner
                        .stored
                        .iter()
                        .filter_map(|k| inner.values.get(k).map(|v| (k.clone(), v.clone())))
                        .collect();
                    let _ = saver.send(stored);
                }
                Ok(Value::Null)
            }
            other => Err(format!("state: unknown function '{other}'")),
        }
    }

    fn release(&self, key: u64) {
        self.inner.lock().unwrap().subs.remove(&key);
    }
}
