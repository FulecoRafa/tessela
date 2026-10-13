//! Module `tray`: StatusNotifierItems (the icons apps such as Discord, Steam
//! or nm-applet publish) and their dbusmenu menus. Capability `tray`.
//!
//! We are a StatusNotifierHost: we register with the session's watcher
//! (the desktop's bar usually owns `org.kde.StatusNotifierWatcher`) and
//! never take its name, so we run next to that bar without disturbing it.
//!
//! ```tsx
//! tray.items()               // [{ id, title, tooltip, status, icon, onlyMenu, hasMenu }]
//! tray.menu(id)()            // menu tree while read; null before it loads
//! tray.activate(id) · tray.secondaryActivate(id) · tray.scroll(id, delta)
//! tray.trigger(id, entryId)  // click a menu entry
//! ```
//!
//! Guests only ever name items this module discovered: an id is looked up
//! in our own table before any D-Bus call, so a forged id reaches nothing.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};
use std::thread;

use serde_json::{Value as Json, json};
use tessela_core::{Lang, Module, ModuleCx};
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{OwnedValue, Value};

use crate::icons::IconTheme;
use crate::images::cache_dir;
use crate::topic::{JS_TOPIC, LUAU_TOPIC, Topics};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";

// ---------------------------------------------------------------- values

fn inner<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(b) => inner(b),
        v => v,
    }
}

fn string(v: &Value<'_>) -> Option<String> {
    match inner(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

fn boolean(v: &Value<'_>) -> Option<bool> {
    match inner(v) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn int(v: &Value<'_>) -> Option<i64> {
    match inner(v) {
        Value::I32(i) => Some(*i as i64),
        Value::U32(i) => Some(*i as i64),
        Value::I64(i) => Some(*i),
        _ => None,
    }
}

fn bytes(v: &Value<'_>) -> Vec<u8> {
    match inner(v) {
        Value::Array(a) => a
            .inner()
            .iter()
            .filter_map(|b| match b {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `a(iiay)`: icon pixmaps, ARGB32 in network byte order.
fn pixmaps(v: &Value<'_>) -> Vec<(u32, u32, Vec<u8>)> {
    let Value::Array(a) = inner(v) else {
        return Vec::new();
    };
    a.inner()
        .iter()
        .filter_map(|p| match p {
            Value::Structure(s) => match s.fields() {
                [w, h, data] => {
                    let (w, h) = (int(w)? as u32, int(h)? as u32);
                    let data = bytes(data);
                    (w > 0 && h > 0 && data.len() == (w * h * 4) as usize).then_some((w, h, data))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Write the best pixmap (the largest up to 64 px, else the smallest) as a
/// PNG in the image cache; returns an image source.
fn pixmap_source(maps: &[(u32, u32, Vec<u8>)]) -> Option<String> {
    let (w, h, argb) = maps
        .iter()
        .filter(|(w, _, _)| *w <= 64)
        .max_by_key(|(w, _, _)| *w)
        .or_else(|| maps.iter().min_by_key(|(w, _, _)| *w))?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    argb.hash(&mut hasher);
    let dir = cache_dir();
    let path = dir.join(format!("tray-{:016x}.png", hasher.finish()));
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        let rgba: Vec<u8> = argb
            .chunks_exact(4)
            .flat_map(|p| [p[1], p[2], p[3], p[0]])
            .collect();
        let file = std::fs::File::create(&path).ok()?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), *w, *h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&rgba).ok()?;
    }
    Some(format!("file:{}", path.display()))
}

/// A PNG blob (dbusmenu `icon-data`) into the image cache.
fn png_source(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    let dir = cache_dir();
    let path = dir.join(format!("menu-{:016x}.png", hasher.finish()));
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, data).ok()?;
    }
    Some(format!("file:{}", path.display()))
}

// ---------------------------------------------------------------- items

#[derive(Debug, Clone)]
struct Item {
    service: String,
    path: String,
    menu: Option<String>,
    info: Json,
}

/// "service/object/path" or just "service" (path /StatusNotifierItem).
fn split_entry(entry: &str) -> (String, String) {
    match entry.find('/') {
        Some(i) => (entry[..i].to_string(), entry[i..].to_string()),
        None => (entry.to_string(), "/StatusNotifierItem".to_string()),
    }
}

fn item_id(service: &str, path: &str) -> String {
    format!("{service}{path}")
}

fn read_item(
    conn: &Connection,
    icons: &IconTheme,
    service: &str,
    path: &str,
) -> zbus::Result<Item> {
    let props = Proxy::new(conn, service, path, "org.freedesktop.DBus.Properties")?;
    let all: HashMap<String, OwnedValue> = props.call("GetAll", &(ITEM_IFACE,))?;
    let get = |k: &str| all.get(k).map(|v| &**v);
    let s = |k: &str| get(k).and_then(string).unwrap_or_default();
    let theme_path = s("IconThemePath");
    let extra: Vec<PathBuf> = if theme_path.is_empty() {
        vec![]
    } else {
        vec![PathBuf::from(&theme_path)]
    };
    let status = s("Status");
    let icon_name = if status == "NeedsAttention" && !s("AttentionIconName").is_empty() {
        s("AttentionIconName")
    } else {
        s("IconName")
    };
    let icon = icons
        .lookup_in(&icon_name, &extra)
        .map(|p| format!("file:{}", p.display()))
        .or_else(|| get("IconPixmap").and_then(|v| pixmap_source(&pixmaps(v))))
        .unwrap_or_default();
    let tooltip = match get("ToolTip").map(inner) {
        Some(Value::Structure(st)) => st.fields().get(2).and_then(string).unwrap_or_default(),
        _ => String::new(),
    };
    let menu = get("Menu")
        .and_then(string)
        .filter(|m| !m.is_empty() && m != "/");
    let only_menu = get("ItemIsMenu").and_then(boolean).unwrap_or(false);
    let id = item_id(service, path);
    Ok(Item {
        service: service.to_string(),
        path: path.to_string(),
        info: json!({
            "id": id,
            "appId": s("Id"),
            "title": s("Title"),
            "tooltip": tooltip,
            "status": match status.as_str() { "Passive" => "passive", "NeedsAttention" => "attention", _ => "active" },
            "icon": icon,
            "onlyMenu": only_menu,
            "hasMenu": menu.is_some(),
        }),
        menu,
    })
}

// ---------------------------------------------------------------- menus

fn menu_entry(v: &Value<'_>) -> Option<Json> {
    let Value::Structure(s) = inner(v) else {
        return None;
    };
    let [id, props, children] = s.fields() else {
        return None;
    };
    let id = int(id)?;
    let mut p: HashMap<String, &Value<'_>> = HashMap::new();
    if let Value::Dict(d) = inner(props) {
        for (k, v) in d.iter() {
            if let Some(k) = string(k) {
                p.insert(k, v);
            }
        }
    }
    let s = |k: &str| p.get(k).and_then(|v| string(v)).unwrap_or_default();
    let b = |k: &str, d: bool| p.get(k).and_then(|v| boolean(v)).unwrap_or(d);
    if !b("visible", true) {
        return None;
    }
    let children: Vec<Json> = match inner(children) {
        Value::Array(a) => a.inner().iter().filter_map(menu_entry).collect(),
        _ => Vec::new(),
    };
    let toggle = match s("toggle-type").as_str() {
        "checkmark" => json!("checkmark"),
        "radio" => json!("radio"),
        _ => Json::Null,
    };
    let icon = p
        .get("icon-data")
        .and_then(|v| png_source(&bytes(v)))
        .unwrap_or_default();
    Some(json!({
        "id": id,
        "label": s("label"),
        "enabled": b("enabled", true),
        "separator": s("type") == "separator",
        "toggle": toggle,
        "checked": p.get("toggle-state").and_then(|v| int(v)) == Some(1),
        "icon": icon,
        "iconName": s("icon-name"),
        "children": children,
    }))
}

fn read_menu(conn: &Connection, icons: &IconTheme, item: &Item) -> Json {
    let Some(path) = &item.menu else {
        return Json::Null;
    };
    let fetch = || -> zbus::Result<Json> {
        let menu = Proxy::new(conn, item.service.as_str(), path.as_str(), MENU_IFACE)?;
        // Apps may build the menu lazily; failing here is fine.
        let _ = menu.call::<_, _, bool>("AboutToShow", &(0i32,));
        let reply = menu.call_method("GetLayout", &(0i32, -1i32, Vec::<String>::new()))?;
        // (u(ia{sv}av)): revision, then the root entry; we only show its children.
        type Layout = (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>));
        let (_revision, (_root, _props, children)): Layout = reply.body().deserialize()?;
        let mut entries = Json::Array(children.iter().filter_map(|c| menu_entry(c)).collect());
        // Entries with a theme icon name but no icon data: resolve it here.
        resolve_icons(&mut entries, icons);
        Ok(entries)
    };
    fetch().unwrap_or_else(|e| {
        log::warn!("tray: menu of {}: {e}", item.info["title"]);
        Json::Null
    })
}

fn resolve_icons(entries: &mut Json, icons: &IconTheme) {
    let Some(list) = entries.as_array_mut() else {
        return;
    };
    for e in list {
        if e["icon"] == ""
            && let Some(name) = e["iconName"].as_str().filter(|n| !n.is_empty())
        {
            if let Some(p) = icons.lookup(name) {
                e["icon"] = json!(format!("file:{}", p.display()));
            }
        }
        resolve_icons(&mut e["children"], icons);
    }
}

// ---------------------------------------------------------------- module

#[derive(Default)]
struct State {
    items: Vec<Item>,
}

pub struct Tray {
    topics: Arc<Topics>,
    state: Arc<Mutex<State>>,
    conn: Arc<Mutex<Option<Connection>>>,
    icons: Arc<IconTheme>,
    started: Once,
    js: String,
    luau: String,
}

impl Tray {
    pub fn new(icons: Arc<IconTheme>) -> Self {
        let js = format!(
            r#""use strict";
(() => {{
  const rt = globalThis.__rt;
{JS_TOPIC}
  const call = (func, args) => {{ rt.invoke("tray", func, args); }};
  rt.define("tray", Object.freeze({{
    items: () => topic("tray", "items")() ?? [],
    menu: (id) => topic("tray", "menu:" + String(id)),
    activate: (id) => call("activate", {{ id }}),
    secondaryActivate: (id) => call("secondaryActivate", {{ id }}),
    scroll: (id, delta) => call("scroll", {{ id, delta }}),
    trigger: (id, entry) => call("trigger", {{ id, entry }}),
  }}));
}})();
"#
        );
        let luau = format!(
            r#"local rt = __rt
{LUAU_TOPIC}
tray = table.freeze({{
	items = function() return topic("tray", "items")() or {{}} end,
	menu = function(id) return topic("tray", "menu:" .. tostring(id)) end,
	activate = function(id) rt.invoke("tray", "activate", {{ id = id }}) end,
	secondaryActivate = function(id) rt.invoke("tray", "secondaryActivate", {{ id = id }}) end,
	scroll = function(id, delta) rt.invoke("tray", "scroll", {{ id = id, delta = delta }}) end,
	trigger = function(id, entry) rt.invoke("tray", "trigger", {{ id = id, entry = entry }}) end,
}})
"#
        );
        Self {
            topics: Topics::new(),
            state: Arc::default(),
            conn: Arc::default(),
            icons,
            started: Once::new(),
            js,
            luau,
        }
    }

    fn publish_items(topics: &Topics, state: &State) {
        topics.publish(
            "items",
            Json::Array(state.items.iter().map(|i| i.info.clone()).collect()),
        );
    }

    fn upsert(
        conn: &Connection,
        icons: &IconTheme,
        state: &Mutex<State>,
        topics: &Topics,
        service: &str,
        path: &str,
    ) {
        match read_item(conn, icons, service, path) {
            Ok(item) => {
                let mut st = state.lock().unwrap();
                match st
                    .items
                    .iter_mut()
                    .find(|i| i.service == service && i.path == path)
                {
                    Some(old) => *old = item,
                    None => st.items.push(item),
                }
                Self::publish_items(topics, &st);
            }
            Err(e) => log::warn!("tray: cannot read {service}{path}: {e}"),
        }
    }

    fn refresh_menus(
        conn: &Connection,
        icons: &IconTheme,
        state: &Mutex<State>,
        topics: &Topics,
        service: &str,
    ) {
        let items: Vec<Item> = state
            .lock()
            .unwrap()
            .items
            .iter()
            .filter(|i| i.service == service)
            .cloned()
            .collect();
        let watched = topics.watched();
        for item in items {
            let name = format!("menu:{}", item_id(&item.service, &item.path));
            if watched.contains(&name) {
                topics.publish(&name, read_menu(conn, icons, &item));
            }
        }
    }

    /// Connecting and registering take hundreds of milliseconds: never on
    /// the worker thread that asked.
    fn start(&self) {
        let (topics, state, conn, icons) = (
            self.topics.clone(),
            self.state.clone(),
            self.conn.clone(),
            self.icons.clone(),
        );
        thread::Builder::new()
            .name("tray".into())
            .spawn(move || Self::connect(topics, state, conn, icons))
            .expect("spawn tray");
    }

    fn connect(
        all_topics: Arc<Topics>,
        all_state: Arc<Mutex<State>>,
        slot: Arc<Mutex<Option<Connection>>>,
        all_icons: Arc<IconTheme>,
    ) {
        let host = format!("org.kde.StatusNotifierHost-{}-tessela", std::process::id());
        let conn = match zbus::blocking::connection::Builder::session()
            .and_then(|b| b.name(host.as_str()))
            .and_then(|b| b.build())
        {
            Ok(c) => c,
            Err(e) => {
                log::warn!("tray: no session bus ({e}); the tray stays empty");
                all_topics.publish("items", json!([]));
                return;
            }
        };
        *slot.lock().unwrap() = Some(conn.clone());
        let registered = (|| -> zbus::Result<Vec<String>> {
            let watcher = Proxy::new(&conn, WATCHER, WATCHER_PATH, WATCHER)?;
            watcher.call::<_, _, ()>("RegisterStatusNotifierHost", &(host.as_str(),))?;
            watcher.get_property("RegisteredStatusNotifierItems")
        })();
        let entries = match registered {
            Ok(e) => e,
            Err(e) => {
                log::warn!(
                    "tray: no StatusNotifierWatcher on the session bus ({e}); the tray stays empty"
                );
                all_topics.publish("items", json!([]));
                return;
            }
        };
        all_topics.publish("items", json!([]));
        for entry in entries {
            let (service, path) = split_entry(&entry);
            Self::upsert(&conn, &all_icons, &all_state, &all_topics, &service, &path);
        }

        let spawn = |name: &str, rule: String, handle: Box<dyn Fn(&zbus::Message) + Send>| {
            let conn = conn.clone();
            thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    match MessageIterator::for_match_rule(rule.as_str(), &conn, Some(64)) {
                        Ok(it) => {
                            for msg in it.flatten() {
                                handle(&msg);
                            }
                        }
                        Err(e) => log::warn!("tray: cannot listen ({rule}): {e}"),
                    }
                })
                .expect("spawn tray listener");
        };

        let (c, icons, state, topics) = (
            conn.clone(),
            all_icons.clone(),
            all_state.clone(),
            all_topics.clone(),
        );
        spawn(
            "tray-watcher",
            format!("type='signal',interface='{WATCHER}'"),
            Box::new(move |msg| {
                let member = msg
                    .header()
                    .member()
                    .map(|m| m.to_string())
                    .unwrap_or_default();
                let Ok(entry) = msg.body().deserialize::<String>() else {
                    return;
                };
                let (service, path) = split_entry(&entry);
                match member.as_str() {
                    "StatusNotifierItemRegistered" => {
                        Self::upsert(&c, &icons, &state, &topics, &service, &path)
                    }
                    "StatusNotifierItemUnregistered" => {
                        let mut st = state.lock().unwrap();
                        // Some watchers announce just the service name.
                        st.items.retain(|i| {
                            !(i.service == service
                                && (entry.contains('/') && i.path == path || !entry.contains('/')))
                        });
                        Self::publish_items(&topics, &st);
                    }
                    _ => {}
                }
            }),
        );

        let (c, icons, state, topics) = (
            conn.clone(),
            all_icons.clone(),
            all_state.clone(),
            all_topics.clone(),
        );
        spawn(
            "tray-items",
            format!("type='signal',interface='{ITEM_IFACE}'"),
            Box::new(move |msg| {
                let header = msg.header();
                let (Some(sender), Some(path)) = (header.sender(), header.path()) else {
                    return;
                };
                let (sender, path) = (sender.to_string(), path.to_string());
                // Items registered by well-known name send signals from their
                // unique name; match by path among the services we know.
                let known: Vec<(String, String)> = state
                    .lock()
                    .unwrap()
                    .items
                    .iter()
                    .filter(|i| i.path == path)
                    .map(|i| (i.service.clone(), i.path.clone()))
                    .collect();
                for (service, path) in known {
                    if service == sender || !service.starts_with(':') {
                        Self::upsert(&c, &icons, &state, &topics, &service, &path);
                    }
                }
            }),
        );

        let (c, icons, state, topics) = (
            conn.clone(),
            all_icons.clone(),
            all_state.clone(),
            all_topics.clone(),
        );
        spawn(
            "tray-menus",
            format!("type='signal',interface='{MENU_IFACE}'"),
            Box::new(move |msg| {
                let header = msg.header();
                let Some(path) = header.path().map(|p| p.to_string()) else {
                    return;
                };
                let services: Vec<String> = state
                    .lock()
                    .unwrap()
                    .items
                    .iter()
                    .filter(|i| i.menu.as_deref() == Some(path.as_str()))
                    .map(|i| i.service.clone())
                    .collect();
                for s in services {
                    Self::refresh_menus(&c, &icons, &state, &topics, &s);
                }
            }),
        );
    }

    fn item(&self, args: &Json) -> Result<Item, String> {
        let id = args["id"].as_str().ok_or("missing item id")?;
        self.state
            .lock()
            .unwrap()
            .items
            .iter()
            .find(|i| item_id(&i.service, &i.path) == id)
            .cloned()
            .ok_or_else(|| format!("tray: no item '{id}'"))
    }

    /// Item method calls run off the worker thread: an app that hangs on
    /// D-Bus must not freeze the extensions.
    fn call_item(
        &self,
        item: Item,
        method: &'static str,
        args: impl serde::Serialize + zbus::zvariant::DynamicType + Send + 'static,
        iface: &'static str,
        path: Option<String>,
    ) {
        let Some(conn) = self.conn.lock().unwrap().clone() else {
            return;
        };
        thread::spawn(move || {
            let path = path.unwrap_or(item.path.clone());
            let r = Proxy::new(&conn, item.service.as_str(), path.as_str(), iface)
                .and_then(|p| p.call_method(method, &args).map(|_| ()));
            if let Err(e) = r {
                log::info!("tray: {method} on {}{}: {e}", item.service, item.path);
            }
        });
    }

    /// Items and loaded menus, for diagnostics.
    pub fn snapshot(&self) -> Json {
        json!({ "items": self.topics.get("items"), "watchedMenus": self.topics.watched() })
    }
}

impl Module for Tray {
    fn name(&self) -> &str {
        "tray"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => &self.luau,
            Lang::JavaScript => &self.js,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Json) -> Result<Json, String> {
        cx.require("tray")?;
        match func {
            "subscribe" => {
                let topic = args["topic"].as_str().ok_or("missing topic")?;
                self.started.call_once(|| self.start());
                if topic == "items" {
                    return self.topics.subscribe(cx, "items");
                }
                let Some(id) = topic.strip_prefix("menu:") else {
                    return Err(format!("tray: unknown topic '{topic}'"));
                };
                let item = self.item(&json!({ "id": id }))?;
                let r = self.topics.subscribe(cx, topic);
                // Load (or reload) the menu in the background: it is read
                // when it opens, and apps change menus between openings.
                let (conn, icons, topics, name) = (
                    self.conn.clone(),
                    self.icons.clone(),
                    self.topics.clone(),
                    topic.to_string(),
                );
                thread::spawn(move || {
                    if let Some(conn) = conn.lock().unwrap().clone() {
                        topics.publish(&name, read_menu(&conn, &icons, &item));
                    }
                });
                r
            }
            "activate" | "secondaryActivate" => {
                let item = self.item(&args)?;
                let method = if func == "activate" {
                    "Activate"
                } else {
                    "SecondaryActivate"
                };
                self.call_item(item, method, (0i32, 0i32), ITEM_IFACE, None);
                Ok(Json::Null)
            }
            "scroll" => {
                let item = self.item(&args)?;
                let delta = args["delta"].as_f64().unwrap_or(0.0).clamp(-10.0, 10.0) as i32 * 120;
                self.call_item(
                    item,
                    "Scroll",
                    (delta, "vertical".to_string()),
                    ITEM_IFACE,
                    None,
                );
                Ok(Json::Null)
            }
            "trigger" => {
                let item = self.item(&args)?;
                let entry = args["entry"].as_i64().ok_or("missing menu entry id")? as i32;
                let path = item.menu.clone().ok_or("tray: item has no menu")?;
                let timestamp = 0u32;
                self.call_item(
                    item,
                    "Event",
                    (
                        entry,
                        "clicked".to_string(),
                        OwnedValue::from(0i32),
                        timestamp,
                    ),
                    MENU_IFACE,
                    Some(path),
                );
                Ok(Json::Null)
            }
            other => Err(format!("tray: unknown function '{other}'")),
        }
    }

    fn release(&self, key: u64) {
        self.topics.release(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{Array, Dict, Signature, StructureBuilder};

    #[test]
    fn splits_watcher_entries() {
        assert_eq!(
            split_entry(":1.45/org/ayatana/NotificationItem/x"),
            (":1.45".into(), "/org/ayatana/NotificationItem/x".into())
        );
        assert_eq!(
            split_entry("org.kde.StatusNotifierItem-12-1"),
            (
                "org.kde.StatusNotifierItem-12-1".into(),
                "/StatusNotifierItem".into()
            )
        );
    }

    fn entry(
        id: i32,
        props: &[(&str, Value<'static>)],
        children: Vec<Value<'static>>,
    ) -> Value<'static> {
        let mut d = Dict::new(&Signature::Str, &Signature::Variant);
        for (k, v) in props {
            d.append(
                Value::from(k.to_string()),
                Value::Value(Box::new(v.clone())),
            )
            .unwrap();
        }
        let mut arr = Array::new(&Signature::Variant);
        for c in children {
            arr.append(Value::Value(Box::new(c))).unwrap();
        }
        Value::Structure(
            StructureBuilder::new()
                .add_field(id)
                .append_field(Value::Dict(d))
                .append_field(Value::Array(arr))
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn parses_dbusmenu_layouts() {
        let layout = entry(
            0,
            &[],
            vec![
                entry(1, &[("label", Value::from("_Abrir"))], vec![]),
                entry(2, &[("type", Value::from("separator"))], vec![]),
                entry(
                    3,
                    &[
                        ("label", Value::from("Hidden")),
                        ("visible", Value::from(false)),
                    ],
                    vec![],
                ),
                entry(
                    4,
                    &[
                        ("label", Value::from("Status")),
                        ("children-display", Value::from("submenu")),
                    ],
                    vec![entry(
                        41,
                        &[
                            ("label", Value::from("On")),
                            ("toggle-type", Value::from("radio")),
                            ("toggle-state", Value::from(1i32)),
                        ],
                        vec![],
                    )],
                ),
            ],
        );
        let root = menu_entry(&layout).unwrap();
        let children = root["children"].as_array().unwrap();
        assert_eq!(children.len(), 3, "invisible entries are dropped");
        assert_eq!(children[0]["label"], "_Abrir");
        assert_eq!(children[1]["separator"], true);
        assert_eq!(children[2]["children"][0]["toggle"], "radio");
        assert_eq!(children[2]["children"][0]["checked"], true);
    }

    #[test]
    fn pixmaps_become_pngs() {
        let maps = vec![(2u32, 2u32, vec![255u8, 255, 0, 0].repeat(4))];
        let src = pixmap_source(&maps).unwrap();
        let path = src.strip_prefix("file:").unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, 2);
    }
}
