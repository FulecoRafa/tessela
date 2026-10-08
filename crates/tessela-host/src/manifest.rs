use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tessela_core::{Grants, Limits};

/// `widget.toml`, the extension manifest.
///
/// ```toml
/// name = "battery"
/// entry = "battery.luau"          # .luau/.lua or .tsx/.ts/.jsx/.js
/// capabilities = ["service:power"]
/// memory_mb = 16                  # default 16
/// budget_ms = 50                  # CPU per entry into guest code; default 50
/// ```
///
/// `uses = ["power"]` is accepted as shorthand for `service:power`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub entry: PathBuf,
    /// Extension point this extension plugs into. Only `view` exists today
    /// (docs/adr/0006).
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: usize,
    #[serde(default = "default_budget_ms")]
    pub budget_ms: u64,
}

fn default_kind() -> String {
    "view".into()
}
fn default_memory_mb() -> usize {
    16
}
fn default_budget_ms() -> u64 {
    50
}

impl Manifest {
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(dir.join("widget.toml"))?;
        let mut m: Manifest = toml::from_str(&text)?;
        anyhow::ensure!(
            m.kind == "view",
            "unknown extension kind '{}' (only 'view' exists yet)",
            m.kind
        );
        if m.entry.is_relative() {
            m.entry = dir.join(&m.entry);
        }
        Ok(m)
    }

    pub fn grants(&self) -> Grants {
        Grants::new(
            self.capabilities
                .iter()
                .cloned()
                .chain(self.uses.iter().map(|s| format!("service:{s}"))),
        )
    }

    pub fn limits(&self) -> Limits {
        Limits {
            memory_bytes: self.memory_mb * 1024 * 1024,
            budget: Duration::from_millis(self.budget_ms),
            ..Limits::default()
        }
    }
}
