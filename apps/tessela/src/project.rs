//! `tessela.toml`: a project's surfaces, plugins and theme.
//!
//! ```toml
//! name = "my-shell"
//! font = "Inter"
//! themes = ["../my-kit/theme.toml"]   # [dark] and [light] tokens, merged in order
//!
//! [surfaces.bar]                       # one extension per surface
//! entry = "src/surfaces/bar.tsx"       # relative to the project
//! capabilities = ["context", "state"]
//! memory_mb = 32                       # optional limits, as in widget.toml
//!
//! [plugins.audio]                      # a service the runner keeps alive
//! command = ["python3", "plugins/audio.py"]
//!
//! [imports]                            # packages: import { Icon } from "@kit/Icon"
//! kit = "../my-kit"                     # a directory, relative to the project
//!
//! [theme.dark]                         # tokens for "$name" colors (override the files)
//! surface = "#232834"
//! [theme.light]
//! surface = "#FCFCFC"
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use tessela::core::Limits;
use tessela::host::Entry;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub font: Option<String>,
    pub surfaces: BTreeMap<String, SurfaceConfig>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginConfig>,
    #[serde(default)]
    pub imports: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub themes: Vec<PathBuf>,
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(skip)]
    pub root: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceConfig {
    pub entry: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub memory_mb: Option<usize>,
    pub budget_ms: Option<u64>,
    pub calls_per_sec: Option<u32>,
    pub max_resources: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    #[serde(default)]
    pub dark: HashMap<String, String>,
    #[serde(default)]
    pub light: HashMap<String, String>,
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Project {
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let root = dir
            .canonicalize()
            .with_context(|| format!("no project at {}", dir.display()))?;
        let file = root.join("tessela.toml");
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let mut p: Project =
            toml::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
        anyhow::ensure!(
            valid_name(&p.name),
            "project name '{}' must be [a-zA-Z0-9_-]",
            p.name
        );
        for name in p.surfaces.keys().chain(p.plugins.keys()) {
            anyhow::ensure!(valid_name(name), "name '{name}' must be [a-zA-Z0-9_-]");
        }
        for (name, s) in &p.surfaces {
            anyhow::ensure!(
                !s.entry.starts_with('/') && !s.entry.split('/').any(|c| c == ".."),
                "surface {name}: entry must be a path inside the project"
            );
        }
        for (name, dir) in p.imports.iter_mut() {
            anyhow::ensure!(
                valid_name(name),
                "import name '{name}' must be [a-zA-Z0-9_-]"
            );
            let full = root.join(&*dir);
            *dir = full
                .canonicalize()
                .with_context(|| format!("import '{name}': no directory {}", full.display()))?;
        }
        // Theme files first, then the manifest's own tokens on top.
        let mut theme = ThemeConfig::default();
        for file in &p.themes {
            let path = root.join(file);
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("theme {}", path.display()))?;
            let t: ThemeConfig =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            theme.dark.extend(t.dark);
            theme.light.extend(t.light);
        }
        theme.dark.extend(std::mem::take(&mut p.theme.dark));
        theme.light.extend(std::mem::take(&mut p.theme.light));
        p.theme = theme;
        for (scheme, tokens) in [("dark", &p.theme.dark), ("light", &p.theme.light)] {
            for (k, v) in tokens {
                anyhow::ensure!(
                    tessela::view::styled::parse_hex(v).is_some(),
                    "theme.{scheme}.{k}: '{v}' is not a #rrggbb color"
                );
            }
        }
        p.root = root;
        Ok(p)
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.surfaces
            .iter()
            .map(|(name, s)| {
                let d = Limits::default();
                Entry {
                    name: name.clone(),
                    root: self.root.clone(),
                    file: s.entry.clone(),
                    capabilities: s.capabilities.clone(),
                    packages: self
                        .imports
                        .iter()
                        .map(|(n, d)| (n.clone(), d.clone()))
                        .collect(),
                    limits: Limits {
                        memory_bytes: s.memory_mb.map(|m| m << 20).unwrap_or(32 << 20),
                        budget: s.budget_ms.map(Duration::from_millis).unwrap_or(d.budget),
                        calls_per_sec: s.calls_per_sec.unwrap_or(d.calls_per_sec),
                        max_resources: s.max_resources.unwrap_or(d.max_resources),
                        ..d
                    },
                }
            })
            .collect()
    }

    /// `$XDG_RUNTIME_DIR/tessela/<name>`: sockets of this project's services.
    pub fn runtime_dir(&self) -> PathBuf {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(runtime).join("tessela").join(&self.name)
    }

    /// `~/.local/state/tessela/<name>/state.json`: values saved with `shared(..., { store: true })`.
    pub fn state_file(&self) -> PathBuf {
        let base = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")
            });
        base.join("tessela").join(&self.name).join("state.json")
    }
}
