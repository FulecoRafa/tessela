//! Module `apps`: desktop entries, their icons and launching.
//!
//! Needs the capability `apps`; `launch` needs `apps:launch`.
//!
//! ```tsx
//! apps.lookup("com.mitchellh.ghostty")  // { id, name, icon } for a window class
//! apps.icon("discord")                   // image source, "" when the theme has none
//! apps.launch("org.gnome.Nautilus")      // a desktop entry id...
//! apps.launch(["ghostty", "-e", "btop"]) // ...or a command (no shell involved)
//! ```
//!
//! Launched programs run in their own process group, detached from ours,
//! without the dev shell's `LD_LIBRARY_PATH` (it would load our libraries
//! into them).

use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tessela_core::{Lang, Module, ModuleCx};

use crate::icons::{IconTheme, data_dirs};

const RESCAN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct DesktopEntry {
    /// File name without `.desktop`: `org.gnome.Nautilus`.
    pub id: String,
    pub name: String,
    pub icon: String,
    pub exec: String,
    pub wm_class: String,
    pub hidden: bool,
}

/// The `[Desktop Entry]` group of a .desktop file.
pub fn parse_entry(id: &str, text: &str) -> Option<DesktopEntry> {
    let mut in_group = false;
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            fields.entry(k.trim()).or_insert(v.trim());
        }
    }
    if fields.get("Type").is_some_and(|t| *t != "Application") {
        return None;
    }
    let flag = |k: &str| fields.get(k).is_some_and(|v| *v == "true");
    Some(DesktopEntry {
        id: id.to_string(),
        name: fields.get("Name").unwrap_or(&id).to_string(),
        icon: fields.get("Icon").unwrap_or(&"").to_string(),
        exec: fields.get("Exec").unwrap_or(&"").to_string(),
        wm_class: fields.get("StartupWMClass").unwrap_or(&"").to_string(),
        hidden: flag("NoDisplay") || flag("Hidden"),
    })
}

/// Split an `Exec` line into argv, dropping field codes (`%u`, `%F`...).
pub fn exec_argv(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        args.push(cur);
    }
    args.into_iter()
        .filter(|a| !(a.len() == 2 && a.starts_with('%')))
        .map(|a| a.replace("%%", "%"))
        .collect()
}

/// Every desktop entry, earlier data dirs shadowing later ones.
pub fn scan() -> Vec<DesktopEntry> {
    let mut seen = HashMap::new();
    for dir in data_dirs() {
        scan_dir(&dir.join("applications"), "", &mut seen);
    }
    let mut out: Vec<DesktopEntry> = seen.into_values().collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn scan_dir(dir: &Path, prefix: &str, seen: &mut HashMap<String, DesktopEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // Subdirectories become part of the id: kde/foo.desktop → kde-foo.
            scan_dir(&path, &format!("{prefix}{name}-"), seen);
        } else if let Some(stem) = name.strip_suffix(".desktop") {
            let id = format!("{prefix}{stem}");
            if seen.contains_key(&id) {
                continue;
            }
            if let Some(entry) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| parse_entry(&id, &t))
            {
                seen.insert(id, entry);
            }
        }
    }
}

/// The entry a window class most likely belongs to.
pub fn lookup<'a>(entries: &'a [DesktopEntry], app_id: &str) -> Option<&'a DesktopEntry> {
    if app_id.is_empty() {
        return None;
    }
    let lower = app_id.to_lowercase();
    let last = lower.rsplit('.').next().unwrap_or(&lower);
    let id_last = |e: &DesktopEntry| {
        e.id.to_lowercase()
            .rsplit('.')
            .next()
            .map(str::to_string)
            .unwrap_or_default()
    };
    entries
        .iter()
        .find(|e| e.id == app_id)
        .or_else(|| entries.iter().find(|e| e.id.to_lowercase() == lower))
        .or_else(|| {
            entries
                .iter()
                .find(|e| !e.wm_class.is_empty() && e.wm_class.to_lowercase() == lower)
        })
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.id.to_lowercase() == last || id_last(e) == last)
        })
        .or_else(|| entries.iter().find(|e| e.name.to_lowercase() == lower))
}

pub struct Apps {
    icons: Arc<IconTheme>,
    entries: Mutex<Option<(Instant, Arc<Vec<DesktopEntry>>)>>,
}

const JS: &str = r#"
"use strict";
(() => {
  const rt = globalThis.__rt;
  rt.define("apps", Object.freeze({
    lookup: (appId) => rt.invoke("apps", "lookup", { appId: String(appId) }),
    icon: (name) => rt.invoke("apps", "icon", { name: String(name) }),
    list: () => rt.invoke("apps", "list", null),
    launch: (target) => { rt.invoke("apps", "launch", { target }); },
    launchAction: (target) => rt.action("apps", "launch", { target }),
  }));
})();
"#;

const LUAU: &str = r#"
local rt = __rt
apps = table.freeze({
	lookup = function(appId) return rt.invoke("apps", "lookup", { appId = tostring(appId) }) end,
	icon = function(name) return rt.invoke("apps", "icon", { name = tostring(name) }) end,
	list = function() return rt.invoke("apps", "list", nil) end,
	launch = function(target) rt.invoke("apps", "launch", { target = target }) end,
	launchAction = function(target) return rt.action("apps", "launch", { target = target }) end,
})
"#;

/// An image source for a found icon.
pub(crate) fn icon_source(path: Option<PathBuf>) -> Value {
    json!(
        path.map(|p| format!("file:{}", p.display()))
            .unwrap_or_default()
    )
}

impl Apps {
    pub fn new(icons: Arc<IconTheme>) -> Self {
        Self {
            icons,
            entries: Mutex::default(),
        }
    }

    pub fn entries(&self) -> Arc<Vec<DesktopEntry>> {
        let mut cache = self.entries.lock().unwrap();
        match &*cache {
            Some((at, e)) if at.elapsed() < RESCAN => e.clone(),
            _ => {
                let e = Arc::new(scan());
                *cache = Some((Instant::now(), e.clone()));
                e
            }
        }
    }

    fn info(&self, e: &DesktopEntry, fallback_icon: &str) -> Value {
        let icon = self
            .icons
            .lookup(&e.icon)
            .or_else(|| self.icons.lookup(fallback_icon));
        json!({ "id": e.id, "name": e.name, "icon": icon_source(icon) })
    }
}

/// Start a program detached: own process group, no stdio, reaped by a thread.
pub fn spawn(argv: &[String]) -> Result<(), String> {
    let (program, args) = argv.split_first().ok_or("nothing to launch")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("LD_LIBRARY_PATH")
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot launch {program}: {e}"))?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

impl Module for Apps {
    fn name(&self) -> &str {
        "apps"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => LUAU,
            Lang::JavaScript => JS,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String> {
        match func {
            "lookup" => {
                cx.require("apps")?;
                let app_id = args["appId"].as_str().ok_or("missing appId")?;
                let entries = self.entries();
                Ok(lookup(&entries, app_id)
                    .map(|e| self.info(e, app_id))
                    .unwrap_or(Value::Null))
            }
            "icon" => {
                cx.require("apps")?;
                let name = args["name"].as_str().ok_or("missing name")?;
                Ok(icon_source(self.icons.lookup(name)))
            }
            "list" => {
                cx.require("apps")?;
                let entries = self.entries();
                Ok(Value::Array(
                    entries
                        .iter()
                        .filter(|e| !e.hidden)
                        .map(|e| self.info(e, ""))
                        .collect(),
                ))
            }
            "launch" => {
                cx.require("apps:launch")?;
                let argv: Vec<String> = match &args["target"] {
                    Value::String(id) => {
                        let id = id.strip_suffix(".desktop").unwrap_or(id);
                        let entries = self.entries();
                        match entries.iter().find(|e| e.id == id) {
                            Some(e) => exec_argv(&e.exec),
                            None => vec![id.to_string()],
                        }
                    }
                    Value::Array(parts) => parts
                        .iter()
                        .map(|p| {
                            p.as_str()
                                .map(String::from)
                                .ok_or("command parts must be strings")
                        })
                        .collect::<Result<_, _>>()?,
                    _ => return Err("launch needs a desktop entry id or a command array".into()),
                };
                log::info!("apps: {} launches {:?}", cx.extension(), argv);
                spawn(&argv)?;
                Ok(Value::Null)
            }
            other => Err(format!("apps: unknown function '{other}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries_and_exec() {
        let e = parse_entry(
            "org.x.Files",
            "[Desktop Entry]\nType=Application\nName=Files\nIcon=folder\nExec=files --new %U\nStartupWMClass=Nautilus\n[Desktop Action new]\nName=Other\n",
        )
        .unwrap();
        assert_eq!(e.name, "Files");
        assert_eq!(exec_argv(&e.exec), vec!["files", "--new"]);
        assert_eq!(
            exec_argv(r#"sh -c "echo \"a b\"" 100%%"#),
            vec!["sh", "-c", "echo \"a b\"", "100%"]
        );
        assert!(parse_entry("l", "[Desktop Entry]\nType=Link\n").is_none());

        let entries = vec![
            e.clone(),
            DesktopEntry {
                id: "com.mitchellh.ghostty".into(),
                name: "Ghostty".into(),
                ..e.clone()
            },
            DesktopEntry {
                id: "discord".into(),
                name: "Discord".into(),
                wm_class: String::new(),
                ..e.clone()
            },
        ];
        assert_eq!(lookup(&entries, "nautilus").unwrap().id, "org.x.Files");
        assert_eq!(
            lookup(&entries, "com.mitchellh.ghostty").unwrap().id,
            "com.mitchellh.ghostty"
        );
        assert_eq!(
            lookup(&entries, "ghostty").unwrap().id,
            "com.mitchellh.ghostty"
        );
        assert_eq!(lookup(&entries, "Discord").unwrap().id, "discord");
        assert!(lookup(&entries, "nothing").is_none());
    }
}
