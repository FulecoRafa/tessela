//! Where an extension's code comes from: a widget directory with
//! `widget.toml`, or an entry of a project (docs/adr/0015). Both read their
//! files through [`FileSources`], confined to their root.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tessela_core::{Grants, Limits, Sources};

use crate::Manifest;

/// One entry of a project: a file inside `root` run as its own extension,
/// with its own capabilities and limits.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Extension name: logs, persistence, `ctx.surface`.
    pub name: String,
    /// Imports cannot leave this directory.
    pub root: PathBuf,
    /// Relative to `root`, with `/` separators: `src/surfaces/bar.tsx`.
    pub file: String,
    pub capabilities: Vec<String>,
    pub limits: Limits,
    /// Packages it may import as `@name/...`: name → directory.
    pub packages: Vec<(String, PathBuf)>,
}

#[derive(Debug, Clone)]
pub(crate) enum Origin {
    Widget(PathBuf),
    Entry(Entry),
}

/// What a load needs, resolved from an [`Origin`] (manifests are re-read on
/// every reload, so editing `widget.toml` applies too).
pub(crate) struct Resolved {
    pub name: String,
    pub root: PathBuf,
    pub file: String,
    pub grants: Grants,
    pub limits: Limits,
    pub packages: Vec<(String, PathBuf)>,
    /// Files whose change reloads the extension besides the ones it reads.
    pub config: Vec<PathBuf>,
}

impl Origin {
    pub fn name(&self) -> String {
        match self {
            Origin::Widget(dir) => dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            Origin::Entry(e) => e.name.clone(),
        }
    }

    pub fn resolve(&self) -> Result<Resolved, String> {
        match self {
            Origin::Widget(dir) => {
                let manifest = Manifest::load(dir).map_err(|e| format!("manifest: {e}"))?;
                let file = manifest
                    .entry
                    .strip_prefix(dir)
                    .unwrap_or(&manifest.entry)
                    .to_string_lossy()
                    .replace('\\', "/");
                Ok(Resolved {
                    name: manifest.name.clone(),
                    root: dir.clone(),
                    file,
                    grants: manifest.grants(),
                    limits: manifest.limits(),
                    packages: Vec::new(),
                    config: vec![dir.join("widget.toml")],
                })
            }
            Origin::Entry(e) => Ok(Resolved {
                name: e.name.clone(),
                root: e.root.clone(),
                file: e.file.clone(),
                grants: Grants::new(e.capabilities.iter().cloned()),
                limits: e.limits,
                packages: e.packages.clone(),
                config: Vec::new(),
            }),
        }
    }
}

/// Reads files under a root (and under declared package roots, for `@name/...`
/// paths) for the engines, refusing anything that resolves outside them
/// (`..` is already refused by the core; this also stops symlinks), and
/// remembers what was read for hot reload.
pub(crate) struct FileSources {
    root: PathBuf,
    packages: Vec<(String, PathBuf)>,
    read: Mutex<Vec<PathBuf>>,
}

impl FileSources {
    pub fn with_packages(root: &Path, packages: &[(String, PathBuf)]) -> Self {
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        Self {
            root: canonical(root),
            packages: packages
                .iter()
                .map(|(n, p)| (n.clone(), canonical(p)))
                .collect(),
            read: Mutex::default(),
        }
    }

    /// Every file read so far, in order.
    pub fn read_files(&self) -> Vec<PathBuf> {
        self.read.lock().unwrap().clone()
    }
}

impl Sources for FileSources {
    fn read(&self, path: &str) -> Option<String> {
        let (root, rel) = match path.strip_prefix('@') {
            Some(pkg) => {
                let (name, rel) = pkg.split_once('/')?;
                let root = self
                    .packages
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, r)| r)?;
                (root, rel)
            }
            None => (&self.root, path),
        };
        let full = root.join(rel);
        let canonical = full.canonicalize().ok()?;
        if !canonical.starts_with(root) {
            log::warn!(
                "refused to read {} (outside {})",
                canonical.display(),
                root.display()
            );
            return None;
        }
        let text = std::fs::read_to_string(&canonical).ok()?;
        self.read.lock().unwrap().push(full);
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stay_inside_the_root() {
        let base = std::env::temp_dir().join(format!("tessela-sources-{}", std::process::id()));
        let root = base.join("project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "export const a = 1;").unwrap();
        std::fs::write(base.join("secret.ts"), "nope").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(base.join("secret.ts"), root.join("src/link.ts")).unwrap();

        let s = FileSources::with_packages(&root, &[]);
        assert_eq!(s.read("src/a.ts").as_deref(), Some("export const a = 1;"));
        assert_eq!(s.read("../secret.ts"), None);
        #[cfg(unix)]
        assert_eq!(
            s.read("src/link.ts"),
            None,
            "symlinks out of the root are refused"
        );
        assert_eq!(s.read_files(), vec![root.join("src/a.ts")]);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn packages_read_under_their_own_root() {
        let base = std::env::temp_dir().join(format!("tessela-packages-{}", std::process::id()));
        let (app, kit) = (base.join("app"), base.join("kit"));
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::create_dir_all(kit.join("ui")).unwrap();
        std::fs::write(kit.join("ui/Icon.tsx"), "icon").unwrap();
        std::fs::write(app.join("src/secret.ts"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(app.join("src/secret.ts"), kit.join("ui/link.ts")).unwrap();

        let s = FileSources::with_packages(&app, &[("kit".into(), kit.clone())]);
        assert_eq!(s.read("@kit/ui/Icon.tsx").as_deref(), Some("icon"));
        assert_eq!(
            s.read("@other/ui/Icon.tsx"),
            None,
            "undeclared packages do not exist"
        );
        #[cfg(unix)]
        assert_eq!(
            s.read("@kit/ui/link.ts"),
            None,
            "a package cannot reach into the app"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
