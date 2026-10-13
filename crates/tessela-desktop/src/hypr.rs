//! Module `hyprland`: workspaces, windows and the keyboard layout, from
//! Hyprland's IPC sockets; `dispatch` for actions.
//!
//! Needs the capability `hyprland`; dispatching needs `hyprland:dispatch`.
//! `switchLayout(index)` switches every keyboard's layout (same capability).
//! The `exec` dispatchers are refused: launching programs is the `apps`
//! module's job (`apps:launch`), so a dispatch grant is not an exec grant.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::thread;
use std::time::Duration;

use crossbeam_channel::unbounded;
use serde_json::{Value, json};
use tessela_core::{Lang, Module, ModuleCx};

use crate::topic::{JS_TOPIC, LUAU_TOPIC, Topics};

/// Hyprland's IPC: one request socket, one event socket.
#[derive(Clone)]
pub struct Hypr {
    dir: PathBuf,
}

impl Hypr {
    /// The running instance, from `$HYPRLAND_INSTANCE_SIGNATURE`.
    pub fn detect() -> Option<Self> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        let dir = PathBuf::from(runtime).join("hypr").join(sig);
        dir.join(".socket.sock").exists().then_some(Self { dir })
    }

    pub fn request(&self, command: &str) -> std::io::Result<String> {
        let mut s = UnixStream::connect(self.dir.join(".socket.sock"))?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        s.write_all(command.as_bytes())?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        Ok(out)
    }

    /// `j/<what>` parsed; `null` on any failure.
    pub fn json(&self, what: &str) -> Value {
        self.request(&format!("j/{what}"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)
    }

    /// Lines `event>>data` as they happen.
    pub fn events(&self) -> std::io::Result<impl Iterator<Item = (String, String)>> {
        let s = UnixStream::connect(self.dir.join(".socket2.sock"))?;
        Ok(BufReader::new(s)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| {
                let (name, data) = l.split_once(">>")?;
                Some((name.to_string(), data.to_string()))
            }))
    }
}

/// Snapshot of what extensions see, computed from the IPC replies.
pub(crate) fn workspaces(monitors: &Value, workspaces: &Value) -> Value {
    let empty = vec![];
    let monitors = monitors.as_array().unwrap_or(&empty);
    let focused_monitor = monitors.iter().find(|m| m["focused"] == true);
    let mut out: Vec<Value> = workspaces
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|w| w["id"].as_i64().is_some_and(|id| id > 0)) // skip special workspaces
        .map(|w| {
            let id = w["id"].as_i64().unwrap_or(0);
            let monitor = w["monitor"].as_str().unwrap_or("");
            let active = monitors
                .iter()
                .any(|m| m["name"] == monitor && m["activeWorkspace"]["id"].as_i64() == Some(id));
            let focused =
                focused_monitor.is_some_and(|m| m["activeWorkspace"]["id"].as_i64() == Some(id));
            json!({
                "id": id,
                "name": w["name"],
                "monitor": monitor,
                "windows": w["windows"].as_u64().unwrap_or(0),
                "active": active,
                "focused": focused,
            })
        })
        .collect();
    out.sort_by_key(|w| w["id"].as_i64());
    Value::Array(out)
}

pub(crate) fn clients(clients: &Value, active: &Value) -> Value {
    let focused = active["address"].as_str().unwrap_or("");
    let empty = vec![];
    Value::Array(
        clients
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter(|c| {
                c["mapped"] != false && c["workspace"]["id"].as_i64().is_some_and(|id| id > 0)
            })
            .map(|c| {
                let address = c["address"].as_str().unwrap_or("");
                json!({
                    "address": address,
                    "class": c["class"],
                    "title": c["title"],
                    "workspace": c["workspace"]["id"],
                    "focused": !address.is_empty() && address == focused,
                })
            })
            .collect(),
    )
}

pub(crate) fn screen(monitors: &Value) -> Value {
    let Some(m) = monitors
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["focused"] == true).or(ms.first()))
    else {
        return Value::Null;
    };
    let scale = m["scale"].as_f64().unwrap_or(1.0).max(0.1);
    json!({
        "name": m["name"],
        "width": (m["width"].as_f64().unwrap_or(0.0) / scale).round(),
        "height": (m["height"].as_f64().unwrap_or(0.0) / scale).round(),
        "scale": scale,
    })
}

fn main_layout(devices: &Value) -> Value {
    let keyboards = devices["keyboards"].as_array();
    let main = keyboards.and_then(|ks| ks.iter().find(|k| k["main"] == true).or(ks.first()));
    main.map(|k| k["active_keymap"].clone())
        .unwrap_or(Value::Null)
}

/// Events that change what the topics show.
const REFRESH_ON: &[&str] = &[
    "workspace",
    "workspacev2",
    "focusedmon",
    "focusedmonv2",
    "activewindow",
    "activewindowv2",
    "createworkspace",
    "createworkspacev2",
    "destroyworkspace",
    "destroyworkspacev2",
    "moveworkspace",
    "moveworkspacev2",
    "renameworkspace",
    "openwindow",
    "closewindow",
    "movewindow",
    "movewindowv2",
    "windowtitle",
    "windowtitlev2",
    "monitoradded",
    "monitoraddedv2",
    "monitorremoved",
    "monitorremovedv2",
    "changefloatingmode",
];

pub struct Hyprland {
    hypr: Option<Hypr>,
    topics: Arc<Topics>,
    started: Once,
    js: String,
    luau: String,
}

impl Default for Hyprland {
    fn default() -> Self {
        Self::new()
    }
}

impl Hyprland {
    pub fn new() -> Self {
        let js = format!(
            r#""use strict";
(() => {{
  const rt = globalThis.__rt;
{JS_TOPIC}
  rt.define("hyprland", Object.freeze({{
    workspaces: () => topic("hyprland", "workspaces")() ?? [],
    clients: () => topic("hyprland", "clients")() ?? [],
    keyboardLayout: () => topic("hyprland", "keyboardLayout")() ?? "",
    dispatch: (command) => {{ rt.invoke("hyprland", "dispatch", {{ command: String(command) }}); }},
    dispatchAction: (command) => rt.action("hyprland", "dispatch", {{ command: String(command) }}),
    switchLayout: (index) => {{ rt.invoke("hyprland", "switchLayout", {{ index }}); }},
  }}));
}})();
"#
        );
        let luau = format!(
            r#"local rt = __rt
{LUAU_TOPIC}
hyprland = table.freeze({{
	workspaces = function() return topic("hyprland", "workspaces")() or {{}} end,
	clients = function() return topic("hyprland", "clients")() or {{}} end,
	keyboardLayout = function() return topic("hyprland", "keyboardLayout")() or "" end,
	dispatch = function(command) rt.invoke("hyprland", "dispatch", {{ command = tostring(command) }}) end,
	dispatchAction = function(command) return rt.action("hyprland", "dispatch", {{ command = tostring(command) }}) end,
	switchLayout = function(index) rt.invoke("hyprland", "switchLayout", {{ index = index }}) end,
}})
"#
        );
        Self {
            hypr: Hypr::detect(),
            topics: Topics::new(),
            started: Once::new(),
            js,
            luau,
        }
    }

    fn start(&self) {
        let Some(hypr) = self.hypr.clone() else {
            log::warn!("hyprland: not running under Hyprland; workspaces and windows stay empty");
            for t in ["workspaces", "clients"] {
                self.topics.publish(t, json!([]));
            }
            return;
        };
        let topics = self.topics.clone();
        let refresh = move |hypr: &Hypr| {
            let monitors = hypr.json("monitors");
            topics.publish(
                "workspaces",
                workspaces(&monitors, &hypr.json("workspaces")),
            );
            topics.publish(
                "clients",
                clients(&hypr.json("clients"), &hypr.json("activewindow")),
            );
            topics.publish("screen", screen(&monitors));
        };
        // Bursts of events (a window opening fires several) cost one refresh.
        // The first one runs here too, not on the worker that subscribed.
        let (tx, rx) = unbounded::<()>();
        let h = hypr.clone();
        let layout = self.topics.clone();
        thread::Builder::new()
            .name("hyprland-refresh".into())
            .spawn(move || {
                refresh(&h);
                layout.publish("keyboardLayout", main_layout(&h.json("devices")));
                while rx.recv().is_ok() {
                    thread::sleep(Duration::from_millis(25));
                    while rx.try_recv().is_ok() {}
                    refresh(&h);
                }
            })
            .expect("spawn hyprland refresher");
        let topics = self.topics.clone();
        thread::Builder::new()
            .name("hyprland-events".into())
            .spawn(move || {
                loop {
                    match hypr.events() {
                        Ok(events) => {
                            for (name, data) in events {
                                if name == "activelayout" {
                                    // "<keyboard>,<layout name>"; the name may contain commas.
                                    if let Some((_, layout)) = data.split_once(',') {
                                        topics.publish("keyboardLayout", json!(layout));
                                    }
                                } else if REFRESH_ON.contains(&name.as_str()) {
                                    let _ = tx.send(());
                                }
                            }
                        }
                        Err(e) => log::warn!("hyprland: event socket: {e}"),
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            })
            .expect("spawn hyprland events");
    }

    /// Topic values for diagnostics.
    pub fn snapshot(&self) -> Value {
        json!({
            "workspaces": self.topics.get("workspaces"),
            "clients": self.topics.get("clients"),
            "keyboardLayout": self.topics.get("keyboardLayout"),
            "screen": self.topics.get("screen"),
        })
    }

    /// Release a subscription another module made on our topics.
    pub(crate) fn release_topic(&self, key: u64) {
        self.topics.release(key);
    }

    /// Focused monitor in logical pixels, or `null`; used by `ctx.screen()`.
    pub(crate) fn screen_topic(&self) -> (Arc<Topics>, &'static str) {
        self.started.call_once(|| self.start());
        (self.topics.clone(), "screen")
    }
}

impl Module for Hyprland {
    fn name(&self) -> &str {
        "hyprland"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => &self.luau,
            Lang::JavaScript => &self.js,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String> {
        cx.require("hyprland")?;
        match func {
            "subscribe" => {
                let topic = args["topic"].as_str().ok_or("missing topic")?;
                if !matches!(
                    topic,
                    "workspaces" | "clients" | "keyboardLayout" | "screen"
                ) {
                    return Err(format!("hyprland: unknown topic '{topic}'"));
                }
                self.started.call_once(|| self.start());
                self.topics.subscribe(cx, topic)
            }
            "dispatch" => {
                cx.require("hyprland:dispatch")?;
                let command = args["command"].as_str().ok_or("missing command")?.trim();
                let dispatcher = command.split_whitespace().next().unwrap_or("");
                if command.is_empty() || command.contains(['\n', '\r', ';']) {
                    return Err("hyprland: invalid dispatch".into());
                }
                if matches!(dispatcher, "exec" | "execr") {
                    return Err("hyprland: exec is not dispatchable; use apps.launch".into());
                }
                let hypr = self
                    .hypr
                    .as_ref()
                    .ok_or("hyprland: not running under Hyprland")?;
                let reply = hypr
                    .request(&format!("dispatch {command}"))
                    .map_err(|e| format!("hyprland: {e}"))?;
                if reply.trim() != "ok" {
                    log::warn!("hyprland: dispatch {command}: {}", reply.trim());
                }
                Ok(Value::Null)
            }
            // `hyprctl switchxkblayout` is a command, not a dispatcher; it is the
            // only command offered (others, like `keyword`, rewrite the config).
            "switchLayout" => {
                cx.require("hyprland:dispatch")?;
                let index = args["index"]
                    .as_u64()
                    .ok_or("index must be a number >= 0")?;
                let hypr = self
                    .hypr
                    .as_ref()
                    .ok_or("hyprland: not running under Hyprland")?;
                hypr.request(&format!("switchxkblayout all {index}"))
                    .map_err(|e| format!("hyprland: {e}"))?;
                Ok(Value::Null)
            }
            other => Err(format!("hyprland: unknown function '{other}'")),
        }
    }

    fn release(&self, key: u64) {
        self.topics.release(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_mark_active_and_focused() {
        let monitors = json!([
            {"name": "A", "focused": true, "activeWorkspace": {"id": 3}, "width": 1920, "height": 1080, "scale": 1.25},
            {"name": "B", "focused": false, "activeWorkspace": {"id": 5}, "width": 1000, "height": 500, "scale": 1.0},
        ]);
        let ws = json!([
            {"id": 5, "name": "5", "monitor": "B", "windows": 1},
            {"id": 3, "name": "3", "monitor": "A", "windows": 2},
            {"id": -98, "name": "special:x", "monitor": "A", "windows": 1},
        ]);
        let out = workspaces(&monitors, &ws);
        assert_eq!(out[0]["id"], 3);
        assert_eq!(out[0]["focused"], true);
        assert_eq!(out[1]["active"], true);
        assert_eq!(out[1]["focused"], false);
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert_eq!(screen(&monitors)["width"], 1536.0);

        let cs = json!([{"address": "0x1", "class": "a", "title": "t", "workspace": {"id": 3}, "mapped": true}]);
        assert_eq!(clients(&cs, &json!({"address": "0x1"}))[0]["focused"], true);
        assert_eq!(
            main_layout(&json!({"keyboards": [{"main": true, "active_keymap": "English (US)"}]})),
            "English (US)"
        );
    }
}
