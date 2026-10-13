//! Icon theme lookup (freedesktop icon themes, simplified): the configured
//! GTK theme, the themes it inherits, hicolor, then the pixmaps directories.
//!
//! Each theme is indexed once by walking its directories, keeping for every
//! icon name the best file: scalable SVG first, then the PNG closest to 64 px.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const EXTENSIONS: &[&str] = &["svg", "png", "xpm"];

/// `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, in priority order.
pub fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs = vec![
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join(".local/share")),
    ];
    let system =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(
        system
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    );
    dirs.dedup();
    dirs
}

/// The GTK icon theme name from settings.ini, "Adwaita" otherwise.
fn configured_theme() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    for file in [
        ".config/gtk-4.0/settings.ini",
        ".config/gtk-3.0/settings.ini",
    ] {
        let Ok(text) = std::fs::read_to_string(Path::new(&home).join(file)) else {
            continue;
        };
        if let Some(v) = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("gtk-icon-theme-name"))
            .and_then(|r| r.trim_start().strip_prefix('='))
        {
            return v.trim().trim_matches('"').to_string();
        }
    }
    "Adwaita".into()
}

/// Bigger is better.
fn score(path: &Path) -> i32 {
    let mut size = 0;
    for part in path.components().filter_map(|c| c.as_os_str().to_str()) {
        if part == "scalable" {
            return 1000;
        }
        let n = part.split('x').next().and_then(|n| n.parse::<i32>().ok());
        if let Some(n) = n.filter(|n| (8..=1024).contains(n)) {
            size = n;
        }
    }
    let svg = path.extension().is_some_and(|e| e == "svg");
    match size {
        0 if svg => 900,
        0 => 1,
        s if s >= 48 => 500 - (s - 64).abs(),
        s => s,
    }
}

fn walk(dir: &Path, depth: usize, out: &mut HashMap<String, (i32, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let is_dir = std::fs::metadata(&path).is_ok_and(|m| m.is_dir());
        if is_dir {
            if depth < 4 {
                walk(&path, depth + 1, out);
            }
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !EXTENSIONS.contains(&ext) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let s = score(&path);
        let better = out.get(name).is_none_or(|(old, _)| s > *old);
        if better {
            out.insert(name.to_string(), (s, path));
        }
    }
}

type Index = Arc<HashMap<String, PathBuf>>;

pub struct IconTheme {
    theme: String,
    bases: Vec<PathBuf>,
    pixmaps: Vec<PathBuf>,
    indexes: Mutex<HashMap<String, Index>>,
    found: Mutex<HashMap<String, Option<PathBuf>>>,
}

impl Default for IconTheme {
    fn default() -> Self {
        Self::system()
    }
}

impl IconTheme {
    pub fn system() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let data = data_dirs();
        let mut bases = vec![PathBuf::from(&home).join(".icons")];
        bases.extend(data.iter().map(|d| d.join("icons")));
        let mut pixmaps: Vec<PathBuf> = data.iter().map(|d| d.join("pixmaps")).collect();
        pixmaps.push("/usr/share/pixmaps".into());
        Self {
            theme: configured_theme(),
            bases,
            pixmaps,
            indexes: Mutex::default(),
            found: Mutex::default(),
        }
    }

    pub fn theme(&self) -> &str {
        &self.theme
    }

    /// Directories icons may come from, for an image allow-list.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.bases.iter().chain(&self.pixmaps).cloned().collect()
    }

    fn inherits(&self, theme: &str) -> Vec<String> {
        for base in &self.bases {
            if let Ok(text) = std::fs::read_to_string(base.join(theme).join("index.theme")) {
                return text
                    .lines()
                    .find_map(|l| l.strip_prefix("Inherits="))
                    .map(|v| {
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
        Vec::new()
    }

    /// The theme, what it inherits (breadth first), then hicolor.
    fn chain(&self) -> Vec<String> {
        let mut chain = vec![self.theme.clone()];
        let mut i = 0;
        while i < chain.len() && chain.len() < 8 {
            for parent in self.inherits(&chain[i].clone()) {
                if !chain.contains(&parent) {
                    chain.push(parent);
                }
            }
            i += 1;
        }
        chain.retain(|t| t != "hicolor");
        chain.push("hicolor".into());
        chain
    }

    fn index(&self, theme: &str) -> Index {
        if let Some(i) = self.indexes.lock().unwrap().get(theme) {
            return i.clone();
        }
        let mut found = HashMap::new();
        for base in &self.bases {
            walk(&base.join(theme), 0, &mut found);
        }
        let index: Index = Arc::new(found.into_iter().map(|(k, (_, p))| (k, p)).collect());
        self.indexes
            .lock()
            .unwrap()
            .insert(theme.to_string(), index.clone());
        index
    }

    /// File for an icon name (or an absolute path to an icon), if any.
    pub fn lookup(&self, name: &str) -> Option<PathBuf> {
        self.lookup_in(name, &[])
    }

    /// Like [`lookup`](Self::lookup), trying `extra` theme directories first
    /// (a tray item's IconThemePath).
    pub fn lookup_in(&self, name: &str, extra: &[PathBuf]) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        if name.starts_with('/') {
            return Path::new(name).is_file().then(|| PathBuf::from(name));
        }
        if !extra.is_empty() {
            let mut found = HashMap::new();
            for dir in extra {
                walk(dir, 0, &mut found);
            }
            if let Some((_, p)) = found.remove(name) {
                return Some(p);
            }
        }
        if let Some(hit) = self.found.lock().unwrap().get(name) {
            return hit.clone();
        }
        let hit = self
            .chain()
            .iter()
            .find_map(|t| self.index(t).get(name).cloned())
            .or_else(|| {
                self.pixmaps.iter().find_map(|d| {
                    EXTENSIONS
                        .iter()
                        .map(|e| d.join(format!("{name}.{e}")))
                        .find(|p| p.is_file())
                })
            });
        self.found
            .lock()
            .unwrap()
            .insert(name.to_string(), hit.clone());
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_scalable_then_near_64() {
        assert!(
            score(Path::new("/i/hicolor/scalable/apps/a.svg"))
                > score(Path::new("/i/hicolor/64x64/apps/a.png"))
        );
        assert!(
            score(Path::new("/i/hicolor/64x64/apps/a.png"))
                > score(Path::new("/i/hicolor/256x256/apps/a.png"))
        );
        assert!(
            score(Path::new("/i/hicolor/256x256/apps/a.png"))
                > score(Path::new("/i/hicolor/16x16/apps/a.png"))
        );
        assert!(score(Path::new("/i/hicolor/48x48@2/apps/a.png")) > 0);
    }
}
