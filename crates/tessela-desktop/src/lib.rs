//! Modules for Linux desktops: everything a shell (or a launcher) reads from
//! the machine without writing a service.
//!
//! | module | global | capability |
//! |---|---|---|
//! | [`Context`] | `ctx` | `context` |
//! | [`Hyprland`] | `hyprland` | `hyprland`, `hyprland:dispatch` |
//! | [`System`] | `system` | `system` |
//! | [`Apps`] | `apps` | `apps`, `apps:launch` |
//! | [`Tray`] | `tray` | `tray` |
//!
//! [`Images`] is not a module: renderers use it to turn `<Image src>` into a
//! file, under an allow-list.
//!
//! Every reactive value is a *topic*: one background producer (started by
//! the first subscriber) feeding every extension that reads it.

mod apps;
mod context;
mod hypr;
mod icons;
mod images;
mod system;
mod topic;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;

pub use apps::{Apps, DesktopEntry, exec_argv, lookup, parse_entry, scan, spawn};
pub use context::Context;
pub use hypr::{Hypr, Hyprland};
pub use icons::{IconTheme, data_dirs};
pub use images::{Images, cache_dir};
pub use system::System;
pub use tray::Tray;

/// Every module of this crate, sharing one icon theme and one Hyprland
/// connection. Register them all (`modules()`), or pick.
pub struct Desktop {
    pub icons: Arc<IconTheme>,
    pub context: Arc<Context>,
    pub hyprland: Arc<Hyprland>,
    pub system: Arc<System>,
    pub apps: Arc<Apps>,
    pub tray: Arc<Tray>,
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}

impl Desktop {
    pub fn new() -> Self {
        let icons = Arc::new(IconTheme::system());
        // Index the theme in the background: the first lookup would walk
        // thousands of files on a worker thread.
        let warm = icons.clone();
        std::thread::spawn(move || {
            warm.lookup("application-x-executable");
        });
        let hyprland = Arc::new(Hyprland::new());
        Self {
            context: Arc::new(Context::new(Some(hyprland.clone()))),
            system: Arc::new(System::new()),
            apps: Arc::new(Apps::new(icons.clone())),
            tray: Arc::new(Tray::new(icons.clone())),
            hyprland,
            icons,
        }
    }

    pub fn modules(&self) -> Vec<Arc<dyn tessela_core::Module>> {
        vec![
            self.context.clone(),
            self.hyprland.clone(),
            self.system.clone(),
            self.apps.clone(),
            self.tray.clone(),
        ]
    }

    /// Image resolution for a project rooted at `project`.
    pub fn images(&self, project: Option<PathBuf>) -> Images {
        Images::new(project, self.icons.clone())
    }

    /// What every topic holds right now, for diagnostics.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "context": self.context.snapshot(),
            "hyprland": self.hyprland.snapshot(),
            "system": self.system.snapshot(),
            "tray": self.tray.snapshot(),
            "iconTheme": self.icons.theme(),
        })
    }
}
