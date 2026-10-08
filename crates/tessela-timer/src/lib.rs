//! Module `timer`: `after(ms, fn)` and `every(ms, fn)` for extensions.
//!
//! Needs the capability `timer`. A timer belongs to the code that created it,
//! like everything else in the graph: one started inside a binding is
//! cancelled when the binding re-runs, and all of them stop when the
//! extension unloads. Both return a function that cancels the timer.
//!
//! ```lua
//! local flash = signal(false)
//! on_click = function()
//!   flash:set(true)
//!   after(1500, function() flash:set(false) end)
//! end
//! ```
//!
//! One thread serves every extension in the process.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};
use tessela_core::{FN, FnId, Inbound, Lang, Module, ModuleCx, ResourceRef, Sink};

/// Shortest repeating interval, so `every(0, ...)` cannot spin a core.
pub const MIN_INTERVAL: Duration = Duration::from_millis(16);
const MAX_DELAY: Duration = Duration::from_secs(24 * 3600);

const LUAU: &str = r#"
local rt = __rt
local function start(kind, ms, fn)
	local id = rt.invoke("timer", kind, { ms = ms, cb = fn }).id
	return function() rt.invoke("timer", "cancel", { id = id }) end
end
function after(ms, fn) return start("after", ms, fn) end
function every(ms, fn) return start("every", ms, fn) end
"#;

const JS: &str = r#"
"use strict";
(() => {
  const rt = globalThis.__rt;
  const start = (kind, ms, fn) => {
    const id = rt.invoke("timer", kind, { ms, cb: fn }).id;
    return () => { rt.invoke("timer", "cancel", { id }); };
  };
  rt.define("after", (ms, fn) => start("after", ms, fn));
  rt.define("every", (ms, fn) => start("every", ms, fn));
})();
"#;

enum Cmd {
    Add {
        key: u64,
        at: Instant,
        every: Option<Duration>,
        sink: Sink,
        f: FnId,
        resource: ResourceRef,
    },
    Cancel(u64),
}

struct Entry {
    every: Option<Duration>,
    sink: Sink,
    f: FnId,
    resource: ResourceRef,
}

pub struct Timers {
    tx: Sender<Cmd>,
    next_key: AtomicU64,
}

impl Default for Timers {
    fn default() -> Self {
        Self::new()
    }
}

impl Timers {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        thread::Builder::new()
            .name("timers".into())
            .spawn(move || run(rx))
            .expect("spawn timer thread");
        Self {
            tx,
            next_key: AtomicU64::new(1),
        }
    }
}

fn run(rx: Receiver<Cmd>) {
    let mut queue: BinaryHeap<Reverse<(Instant, u64)>> = BinaryHeap::new();
    let mut entries: HashMap<u64, Entry> = HashMap::new();
    loop {
        let cmd = match queue.peek() {
            Some(Reverse((at, _))) => match rx.recv_deadline(*at) {
                Ok(c) => Some(c),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            },
        };
        match cmd {
            Some(Cmd::Add {
                key,
                at,
                every,
                sink,
                f,
                resource,
            }) => {
                entries.insert(
                    key,
                    Entry {
                        every,
                        sink,
                        f,
                        resource,
                    },
                );
                queue.push(Reverse((at, key)));
            }
            Some(Cmd::Cancel(key)) => {
                entries.remove(&key); // its heap slot is skipped when it comes up
            }
            None => {}
        }
        let now = Instant::now();
        while let Some(Reverse((at, key))) = queue.peek().copied() {
            if at > now {
                break;
            }
            queue.pop();
            let Some(e) = entries.get(&key) else { continue };
            e.sink.send(Inbound::Callback {
                f: e.f,
                value: Value::Null,
            });
            match e.every {
                // Skip missed ticks instead of firing a burst after a stall.
                Some(d) => queue.push(Reverse(((at + d).max(now), key))),
                None => {
                    let e = entries.remove(&key).expect("present");
                    e.sink.send(Inbound::Finished(e.resource));
                }
            }
        }
    }
}

impl Module for Timers {
    fn name(&self) -> &str {
        "timer"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => LUAU,
            Lang::JavaScript => JS,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String> {
        cx.require("timer")?;
        match func {
            "after" | "every" => {
                let ms = args["ms"]
                    .as_f64()
                    .filter(|m| m.is_finite() && *m >= 0.0)
                    .ok_or("ms must be a number >= 0")?;
                let mut delay = Duration::from_secs_f64(ms / 1000.0).min(MAX_DELAY);
                if func == "every" {
                    delay = delay.max(MIN_INTERVAL);
                }
                let f = args["cb"]
                    .get(FN)
                    .and_then(Value::as_u64)
                    .ok_or("expected a function")?;
                let key = self.next_key.fetch_add(1, Ordering::Relaxed);
                let resource = cx.own(key)?;
                let every = (func == "every").then_some(delay);
                let _ = self.tx.send(Cmd::Add {
                    key,
                    at: Instant::now() + delay,
                    every,
                    sink: cx.sink(),
                    f,
                    resource,
                });
                Ok(json!({ "id": key }))
            }
            "cancel" => {
                let key = args["id"].as_u64().ok_or("missing id")?;
                let _ = self.tx.send(Cmd::Cancel(key));
                cx.finish(key);
                Ok(Value::Null)
            }
            other => Err(format!("timer: unknown function '{other}'")),
        }
    }

    fn release(&self, key: u64) {
        let _ = self.tx.send(Cmd::Cancel(key));
    }
}
