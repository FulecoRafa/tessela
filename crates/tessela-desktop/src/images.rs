//! Image sources for renderers: what `<Image src=...>` may show.
//!
//! - `asset:<path>` — a file of the project (inside its root);
//! - `icon:<name>` — an icon of the desktop's theme;
//! - `file:<absolute path>` — what modules hand out (app icons, tray icons,
//!   album covers): only under the icon/pixmap directories, the image cache
//!   and the project.
//!
//! `file:` paths are checked lexically (absolute, no `..`, under an allowed
//! root, an image extension) rather than canonically: on NixOS every icon is a
//! symlink into some package in /nix/store, so canonical paths never stay
//! under the theme directory. A guest cannot read pixels back, so the worst a
//! forged path does is show another image file on screen.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::icons::IconTheme;

const IMAGE_EXTENSIONS: &[&str] = &["png", "svg", "jpg", "jpeg", "gif", "webp", "bmp", "xpm"];

/// Where modules write images they produce (tray pixmaps, menu icons).
pub fn cache_dir() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(runtime).join("tessela").join("images")
}

fn clean(p: &Path) -> bool {
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

pub struct Images {
    project: Option<PathBuf>,
    icons: Arc<IconTheme>,
    roots: Vec<PathBuf>,
    cache: Mutex<HashMap<String, Option<PathBuf>>>,
}

impl Images {
    pub fn new(project: Option<PathBuf>, icons: Arc<IconTheme>) -> Self {
        let mut roots = icons.roots();
        roots.push(cache_dir());
        roots.push(std::env::temp_dir());
        if let Some(p) = &project {
            roots.push(p.clone());
        }
        // MPRIS covers usually come from the browser's or player's cache.
        if let Ok(home) = std::env::var("HOME") {
            roots.push(PathBuf::from(home).join(".cache"));
        }
        Self {
            project: project.map(|p| p.canonicalize().unwrap_or(p)),
            icons,
            roots,
            cache: Mutex::default(),
        }
    }

    fn file_allowed(&self, p: &Path) -> bool {
        clean(p)
            && p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
            && self.roots.iter().any(|r| p.starts_with(r))
    }

    /// The file to draw for `src`, or `None` (the renderer shows nothing).
    pub fn resolve(&self, src: &str) -> Option<PathBuf> {
        if let Some(hit) = self.cache.lock().unwrap().get(src) {
            return hit.clone();
        }
        let hit = if let Some(rel) = src.strip_prefix("asset:") {
            self.project.as_ref().and_then(|root| {
                let p = root.join(rel).canonicalize().ok()?;
                p.starts_with(root).then_some(p)
            })
        } else if let Some(name) = src.strip_prefix("icon:") {
            self.icons.lookup(name)
        } else {
            let path = Path::new(src.strip_prefix("file:").unwrap_or(src));
            (self.file_allowed(path) && path.is_file()).then(|| path.to_path_buf())
        };
        if hit.is_none() {
            log::debug!("images: nothing to show for {src}");
        }
        // A missing file may appear soon (a player writes the cover after
        // announcing it), so `file:` misses are not cached.
        if hit.is_some() || !src.starts_with("file:") {
            self.cache
                .lock()
                .unwrap()
                .insert(src.to_string(), hit.clone());
        }
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_sources_stay_under_allowed_roots() {
        let icons = Arc::new(IconTheme::system());
        let images = Images::new(None, icons);
        let cache = cache_dir();
        assert!(images.file_allowed(&cache.join("a.png")));
        assert!(!images.file_allowed(&cache.join("../../etc/a.png")));
        assert!(!images.file_allowed(Path::new("/etc/shadow")));
        assert!(!images.file_allowed(&cache.join("notes.txt")));
        assert!(!images.file_allowed(Path::new("relative.png")));
    }
}
