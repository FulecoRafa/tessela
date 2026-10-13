//! Module `context`: quick facts about the machine and session, as the
//! global `ctx` (capability `context`).
//!
//! ```tsx
//! ctx.user, ctx.hostname, ctx.home, ctx.locale, ctx.desktop, ctx.surface
//! ctx.colorScheme()        // "dark" | "light", follows the desktop portal
//! ctx.screen()             // focused monitor, logical pixels (Hyprland)
//! ctx.now("minute")        // a Date that ticks every minute (or "second")
//! ```
//!
//! Static facts are read once, on first access. The reactive ones are
//! topics: one background producer each, started by the first subscriber.

use std::sync::{Arc, Once};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tessela_core::{Lang, Module, ModuleCx};

use crate::hypr::Hyprland;
use crate::topic::{JS_TOPIC, LUAU_TOPIC, Topics};

pub struct Context {
    topics: Arc<Topics>,
    hyprland: Option<Arc<Hyprland>>,
    clock: Once,
    scheme: Once,
    js: String,
    luau: String,
}

/// "pt_BR.UTF-8" → "pt-BR".
fn locale() -> String {
    let raw = ["LC_ALL", "LC_TIME", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "en_US".into());
    let base = raw.split(['.', '@']).next().unwrap_or("en_US");
    if base == "C" || base == "POSIX" {
        return "en-US".into();
    }
    base.replace('_', "-")
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

/// The portal's color-scheme: 1 prefers dark, 2 prefers light, 0 = no
/// preference (our default: dark).
fn scheme_name(v: u32) -> &'static str {
    if v == 2 { "light" } else { "dark" }
}

fn read_scheme(proxy: &zbus::blocking::Proxy<'_>) -> Option<u32> {
    use zbus::zvariant::OwnedValue;
    // ReadOne (Settings v2) returns the value; Read (v1) wraps it once more.
    if let Ok(v) =
        proxy.call::<_, _, OwnedValue>("ReadOne", &("org.freedesktop.appearance", "color-scheme"))
    {
        return u32::try_from(v).ok();
    }
    let v: OwnedValue = proxy
        .call("Read", &("org.freedesktop.appearance", "color-scheme"))
        .ok()?;
    let inner: OwnedValue = OwnedValue::try_from(v).ok()?;
    u32::try_from(inner).ok()
}

fn watch_scheme(topics: Arc<Topics>) {
    let run = || -> zbus::Result<()> {
        let conn = zbus::blocking::Connection::session()?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )?;
        let initial = read_scheme(&proxy).unwrap_or(0);
        topics.publish("colorScheme", json!(scheme_name(initial)));
        for msg in proxy.receive_signal("SettingChanged")? {
            let Ok((ns, key, value)) = msg
                .body()
                .deserialize::<(String, String, zbus::zvariant::OwnedValue)>()
            else {
                continue;
            };
            if ns == "org.freedesktop.appearance" && key == "color-scheme" {
                let v = u32::try_from(value).unwrap_or(0);
                topics.publish("colorScheme", json!(scheme_name(v)));
            }
        }
        Ok(())
    };
    if let Err(e) = run() {
        log::warn!("context: no desktop portal ({e}); colorScheme stays dark");
        topics.publish("colorScheme", json!("dark"));
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Publishes `clock:second` every second and `clock:minute` when the minute
/// turns, both as epoch milliseconds floored to their precision.
fn run_clock(topics: Arc<Topics>) {
    loop {
        let now = now_ms();
        topics.publish("clock:second", json!(now - now % 1000));
        topics.publish("clock:minute", json!(now - now % 60_000));
        thread::sleep(Duration::from_millis(1000 - now % 1000 + 2));
    }
}

impl Context {
    pub fn new(hyprland: Option<Arc<Hyprland>>) -> Self {
        let js = format!(
            r#""use strict";
(() => {{
  const rt = globalThis.__rt;
{JS_TOPIC}
  let facts = null;
  const fact = (k) => (facts ??= rt.invoke("context", "facts", null))[k];
  const ctx = {{
    colorScheme: () => topic("context", "colorScheme")() ?? "dark",
    screen: () => topic("context", "screen")(),
    now: (precision) => new Date(topic("context", precision === "second" ? "clock:second" : "clock:minute")() ?? Date.now()),
  }};
  for (const k of ["user", "hostname", "home", "locale", "desktop", "surface"]) {{
    Object.defineProperty(ctx, k, {{ get: () => fact(k), enumerable: true }});
  }}
  rt.define("ctx", Object.freeze(ctx));
}})();
"#
        );
        let luau = format!(
            r#"local rt = __rt
{LUAU_TOPIC}
local facts = nil
local function fact(k)
	if facts == nil then facts = rt.invoke("context", "facts", nil) end
	return facts[k]
end
local methods = {{
	colorScheme = function() return topic("context", "colorScheme")() or "dark" end,
	screen = function() return topic("context", "screen")() end,
	-- Epoch milliseconds; os.date is not available in the sandbox.
	now = function(precision)
		return topic("context", if precision == "second" then "clock:second" else "clock:minute")()
	end,
}}
ctx = setmetatable({{}}, {{
	__index = function(_, k) return methods[k] or fact(k) end,
	__newindex = function() error("ctx is read-only", 2) end,
	__metatable = "locked",
}})
"#
        );
        Self {
            topics: Topics::new(),
            hyprland,
            clock: Once::new(),
            scheme: Once::new(),
            js,
            luau,
        }
    }

    /// "dark" or "light" now and on every change, for host code (the
    /// renderer's theme). Starts the portal watcher if needed.
    pub fn color_scheme(&self) -> crossbeam_channel::Receiver<String> {
        let topics = self.topics.clone();
        self.scheme.call_once(|| {
            thread::Builder::new()
                .name("context-scheme".into())
                .spawn(move || watch_scheme(topics))
                .expect("spawn scheme watcher");
        });
        let values = self.topics.listen("colorScheme");
        let (tx, rx) = crossbeam_channel::unbounded();
        thread::spawn(move || {
            for v in values {
                if tx.send(v.as_str().unwrap_or("dark").to_string()).is_err() {
                    break;
                }
            }
        });
        rx
    }

    /// Topic values for diagnostics.
    pub fn snapshot(&self) -> Value {
        json!({
            "colorScheme": self.topics.get("colorScheme"),
            "clock:minute": self.topics.get("clock:minute"),
        })
    }
}

impl Module for Context {
    fn name(&self) -> &str {
        "context"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => &self.luau,
            Lang::JavaScript => &self.js,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String> {
        cx.require("context")?;
        match func {
            "facts" => Ok(json!({
                "user": env("USER"),
                "hostname": hostname(),
                "home": env("HOME"),
                "locale": locale(),
                "desktop": env("XDG_CURRENT_DESKTOP"),
                "surface": cx.extension(),
            })),
            "subscribe" => match args["topic"].as_str().ok_or("missing topic")? {
                t @ ("clock:minute" | "clock:second") => {
                    let topics = self.topics.clone();
                    self.clock.call_once(|| {
                        let now = now_ms();
                        topics.publish("clock:second", json!(now - now % 1000));
                        topics.publish("clock:minute", json!(now - now % 60_000));
                        let t = topics.clone();
                        thread::Builder::new()
                            .name("context-clock".into())
                            .spawn(move || run_clock(t))
                            .expect("spawn clock");
                    });
                    self.topics.subscribe(cx, t)
                }
                "colorScheme" => {
                    let topics = self.topics.clone();
                    self.scheme.call_once(|| {
                        thread::Builder::new()
                            .name("context-scheme".into())
                            .spawn(move || watch_scheme(topics))
                            .expect("spawn scheme watcher");
                    });
                    self.topics.subscribe(cx, "colorScheme")
                }
                "screen" => match &self.hyprland {
                    Some(h) => {
                        let (topics, name) = h.screen_topic();
                        topics.subscribe(cx, name)
                    }
                    None => self.topics.subscribe(cx, "screen"),
                },
                other => Err(format!("context: unknown topic '{other}'")),
            },
            other => Err(format!("context: unknown function '{other}'")),
        }
    }

    fn release(&self, key: u64) {
        self.topics.release(key);
        if let Some(h) = &self.hyprland {
            h.release_topic(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_become_bcp47() {
        // SAFETY: tests in this module run on one thread and restore nothing
        // other tests read.
        unsafe {
            std::env::set_var("LC_ALL", "pt_BR.UTF-8");
        }
        assert_eq!(locale(), "pt-BR");
        unsafe {
            std::env::set_var("LC_ALL", "C");
        }
        assert_eq!(locale(), "en-US");
        unsafe {
            std::env::remove_var("LC_ALL");
        }
        assert_eq!(scheme_name(1), "dark");
        assert_eq!(scheme_name(2), "light");
        assert_eq!(scheme_name(0), "dark");
    }
}
