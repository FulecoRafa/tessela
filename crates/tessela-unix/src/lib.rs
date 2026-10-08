//! Module `service`: services over Unix sockets.
//!
//! A service is any process listening on `<dir>/<name>.sock` and speaking
//! [`tessela_proto`]. Extensions reach one with `use("name")`, which requires
//! the capability `service:name` in their manifest; they never open a socket
//! themselves. The [`connector`] multiplexes identical subscriptions across
//! extensions and reconnects when a service restarts.
//!
//! Guest API (Luau shown; TSX is the same with `.` instead of `:`):
//!
//! ```lua
//! local power = use("power")                  -- needs "service:power"
//! local bat = power:subscribe("battery")      -- signal; nil while unavailable
//! power:call("set_profile", { profile = "performance" }, function(r)
//!   if r.ok then log(r.value) else log(r.error) end
//! end)
//! ```

pub mod connector;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tessela_core::{FN, Inbound, Lang, Module, ModuleCx};

pub use connector::{Connector, Delivery, DeliverySink, Registry, valid_service_name};

const LUAU: &str = r#"
local rt = __rt
function use(name)
	rt.invoke("service", "open", { service = name })
	local svc = {}
	function svc:subscribe(path)
		return rt.wrap(rt.invoke("service", "subscribe", { service = name, path = path }).sig)
	end
	function svc:call(method, args, cb)
		rt.invoke("service", "call", { service = name, method = method, args = args, cb = cb })
	end
	return table.freeze(svc)
end
"#;

const JS: &str = r#"
"use strict";
(() => {
  const rt = globalThis.__rt;
  rt.define("use", (name) => {
    rt.invoke("service", "open", { service: name });
    return Object.freeze({
      subscribe: (path) => rt.wrap(rt.invoke("service", "subscribe", { service: name, path }).sig),
      call: (method, args, cb) => {
        rt.invoke("service", "call", { service: name, method, args: args ?? null, cb: cb ?? null });
      },
    });
  });
})();
"#;

pub struct UnixServices {
    connector: Connector,
    registry: Registry,
    next_key: AtomicU64,
    subscriptions: Mutex<HashMap<u64, (String, String)>>,
}

impl UnixServices {
    /// Look for `<name>.sock` in each directory, in order.
    pub fn new<P: Into<PathBuf>>(dirs: impl IntoIterator<Item = P>) -> Self {
        Self::with_registry(Registry {
            dirs: dirs.into_iter().map(Into::into).collect(),
            ..Default::default()
        })
    }

    pub fn with_registry(mut registry: Registry) -> Self {
        // `a/b/../c` counts against the 108-byte limit of socket paths;
        // canonical paths are shorter and make log messages clearer.
        for d in registry
            .dirs
            .iter_mut()
            .chain(registry.explicit.values_mut())
        {
            if let Ok(c) = std::fs::canonicalize(&*d) {
                *d = c;
            }
        }
        Self {
            connector: Connector::start(registry.clone()),
            registry,
            next_key: AtomicU64::new(1),
            subscriptions: Mutex::default(),
        }
    }

    /// Services currently discoverable, for diagnostics.
    pub fn known(&self) -> Vec<String> {
        self.registry.known()
    }

    fn service<'a>(cx: &ModuleCx<'_>, args: &'a Value) -> Result<&'a str, String> {
        let name = args["service"].as_str().ok_or("missing service name")?;
        if !valid_service_name(name) {
            return Err(format!("invalid service name '{name}'"));
        }
        cx.require(&format!("service:{name}"))?;
        Ok(name)
    }
}

fn contains_function(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.contains_key(FN) || m.values().any(contains_function),
        Value::Array(a) => a.iter().any(contains_function),
        _ => false,
    }
}

impl Module for UnixServices {
    fn name(&self) -> &str {
        "service"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => LUAU,
            Lang::JavaScript => JS,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String> {
        let service = Self::service(cx, &args)?.to_string();
        match func {
            "open" => Ok(Value::Null),
            "subscribe" => {
                let path = args["path"].as_str().ok_or("missing path")?.to_string();
                let key = self.next_key.fetch_add(1, Ordering::Relaxed);
                let sig = cx.signal(Value::Null, Some(key));
                let sink = cx.sink();
                self.connector.subscribe(
                    &service,
                    &path,
                    DeliverySink::new(move |d| {
                        match d {
                            Delivery::Event { value, .. } => {
                                sink.send(Inbound::Signal { sig, value })
                            }
                            Delivery::Unavailable { .. } => sink.send(Inbound::Signal {
                                sig,
                                value: Value::Null,
                            }),
                            Delivery::CallResult { .. } => {}
                        }
                        true
                    }),
                    key,
                );
                self.subscriptions
                    .lock()
                    .unwrap()
                    .insert(key, (service, path));
                Ok(json!({ "sig": sig }))
            }
            "call" => {
                let method = args["method"].as_str().ok_or("missing method")?.to_string();
                let call_args = args.get("args").cloned().unwrap_or(Value::Null);
                if contains_function(&call_args) {
                    return Err("functions cannot be sent to services".into());
                }
                let callback = args["cb"].get(FN).and_then(Value::as_u64);
                let key = self.next_key.fetch_add(1, Ordering::Relaxed);
                let sink = cx.sink();
                self.connector.call(
                    &service,
                    &method,
                    call_args,
                    DeliverySink::new(move |d| {
                        if let (Delivery::CallResult { result, .. }, Some(f)) = (d, callback) {
                            let value = match result {
                                Ok(v) => json!({ "ok": true, "value": v }),
                                Err(e) => json!({ "ok": false, "error": e }),
                            };
                            sink.send(Inbound::Callback { f, value });
                        }
                        true
                    }),
                    key,
                );
                Ok(Value::Null)
            }
            other => Err(format!("service: unknown function '{other}'")),
        }
    }

    fn release(&self, key: u64) {
        if let Some((service, path)) = self.subscriptions.lock().unwrap().remove(&key) {
            self.connector.unsubscribe(&service, &path, key);
        }
    }
}
