//! marathon — spike of a GPUI-style shell with sandboxed Luau/TSX widgets and
//! services behind a universal Unix-socket connector.
//!
//! ```text
//! marathon [--config marathon.toml] [--headless] [--seconds N] [--click WIDGET@SECS]... [--gpui]
//! ```

mod flatten;
#[cfg(feature = "gpui")]
mod gpui_bar;
mod host;
mod tui;

use std::collections::{HashMap, VecDeque};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use connector::{Connector, Registry};
use crossbeam_channel::unbounded;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_widgets_dir")]
    widgets_dir: PathBuf,
    /// Defaults to `$XDG_RUNTIME_DIR/marathon/services`.
    services_dir: Option<PathBuf>,
    /// Widget directory names, left to right.
    bar: Vec<String>,
    /// Sockets that live outside `services_dir`: `name = "/path/to.sock"`.
    #[serde(default)]
    services: HashMap<String, PathBuf>,
}

fn default_widgets_dir() -> PathBuf {
    "widgets".into()
}

struct Args {
    config: PathBuf,
    headless: bool,
    gpui: bool,
    seconds: Option<f64>,
    clicks: Vec<(String, f64)>,
}

fn args() -> anyhow::Result<Args> {
    let mut a = Args {
        config: "marathon.toml".into(),
        headless: false,
        gpui: false,
        seconds: None,
        clicks: vec![],
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => a.config = it.next().context("--config needs a path")?.into(),
            "--headless" => a.headless = true,
            "--gpui" => a.gpui = true,
            "--seconds" => {
                a.seconds = Some(it.next().context("--seconds needs a number")?.parse()?)
            }
            "--click" => {
                let spec = it.next().context("--click needs WIDGET@SECS")?;
                let (w, s) = spec.split_once('@').context("--click needs WIDGET@SECS")?;
                a.clicks.push((w.to_string(), s.parse()?));
            }
            "-h" | "--help" => {
                println!(
                    "marathon [--config marathon.toml] [--headless] [--seconds N] [--click WIDGET@SECS]... [--gpui]"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    Ok(a)
}

/// Logs go to a ring buffer the TUI displays, and to stderr when headless.
struct RingLogger {
    buf: tui::LogBuffer,
    echo: bool,
}

impl log::Log for RingLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!("{:<5} {}", r.level(), r.args());
        if self.echo {
            eprintln!("{line}");
        }
        let mut b = self.buf.lock().unwrap();
        b.push_back(line);
        while b.len() > 500 {
            b.pop_front();
        }
    }
    fn flush(&self) {}
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn main() -> anyhow::Result<()> {
    let args = args()?;
    let text = std::fs::read_to_string(&args.config)
        .with_context(|| format!("reading {}", args.config.display()))?;
    let config: Config = toml::from_str(&text)?;
    let base = args
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let headless = args.headless || !std::io::stdout().is_terminal();
    let logs: tui::LogBuffer = Arc::new(Mutex::new(VecDeque::new()));
    log::set_boxed_logger(Box::new(RingLogger {
        buf: logs.clone(),
        echo: headless || args.gpui,
    }))?;
    log::set_max_level(log::LevelFilter::Info);

    let services_dir = match &config.services_dir {
        Some(d) => resolve(&base, d),
        None => PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into()))
            .join("marathon/services"),
    };
    let registry = Registry {
        dirs: vec![services_dir.clone()],
        explicit: config
            .services
            .iter()
            .map(|(k, v)| (k.clone(), resolve(&base, v)))
            .collect(),
    };
    log::info!(
        "services: {} (found: {})",
        services_dir.display(),
        registry.known().join(", ")
    );
    let connector = Connector::start(registry);

    let (frames_tx, frames_rx) = unbounded();
    let widgets_dir = resolve(&base, &config.widgets_dir);
    let widgets: Vec<_> = config
        .bar
        .iter()
        .enumerate()
        .map(|(slot, name)| {
            host::spawn(
                slot,
                widgets_dir.join(name),
                connector.clone(),
                frames_tx.clone(),
            )
        })
        .collect();

    if args.gpui {
        #[cfg(feature = "gpui")]
        return gpui_bar::run(widgets, frames_rx);
        #[cfg(not(feature = "gpui"))]
        anyhow::bail!("built without the `gpui` feature: cargo run --features gpui -- --gpui");
    }
    if headless {
        return run_headless(widgets, frames_rx, args.seconds, args.clicks);
    }
    tui::run(widgets, frames_rx, logs)
}

/// Prints the bar every time it changes. `--click battery@3` clicks the first
/// clickable element of the `battery` widget three seconds in.
fn run_headless(
    widgets: Vec<host::WidgetHandle>,
    frames: crossbeam_channel::Receiver<host::Frame>,
    seconds: Option<f64>,
    mut clicks: Vec<(String, f64)>,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let mut tree = vec![None; widgets.len()];
    let mut last = String::new();
    loop {
        let elapsed = start.elapsed().as_secs_f64();
        if seconds.is_some_and(|s| elapsed >= s) {
            return Ok(());
        }
        clicks.retain(|(name, at)| {
            if elapsed < *at {
                return true;
            }
            let Some(slot) = widgets.iter().position(|w| &w.name == name) else {
                return false;
            };
            let mut segs = Vec::new();
            if let Some(node) = &tree[slot] {
                flatten::flatten(node, &mut segs);
            }
            match segs.iter().find_map(|s| s.click) {
                Some(h) => {
                    log::info!("(headless) click {name}");
                    let _ = widgets[slot].ui.send(host::UiEvent::Click(h));
                }
                None => log::warn!("(headless) {name} has nothing clickable"),
            }
            false
        });
        if let Ok(f) = frames.recv_timeout(Duration::from_millis(100)) {
            tree[f.slot] = Some(f.node);
            while let Ok(f) = frames.try_recv() {
                tree[f.slot] = Some(f.node);
            }
            let line = flatten::to_plain(&tree);
            if line != last {
                println!("{:>6.2}s  {line}", start.elapsed().as_secs_f64());
                last = line;
            }
        }
    }
}
