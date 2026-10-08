//! marathon — a Linux desktop shell built on tessela: a bar of sandboxed
//! Luau/TSX widgets fed by services over Unix sockets.
//!
//! ```text
//! marathon [--config marathon.toml] [--headless] [--seconds N] [--click WIDGET@SECS]... [--gpui]
//! ```
//!
//! Everything shell-specific lives here (config format, CLI, logging, which
//! renderer). Loading, sandboxing and rendering come from the framework.

use std::collections::{HashMap, VecDeque};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use tessela::host::{App, Headless};
use tessela::unix::{Registry, UnixServices};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

const USAGE: &str = "marathon [--config marathon.toml] [--headless] [--seconds N] [--click WIDGET@SECS]... [--gpui]";

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
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument {other}\n{USAGE}"),
        }
    }
    Ok(a)
}

type LogBuffer = Arc<Mutex<VecDeque<String>>>;

/// Logs go to a ring buffer the TUI displays, and to stderr otherwise.
struct RingLogger {
    buf: LogBuffer,
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

    let headless = args.headless || (!args.gpui && !std::io::stdout().is_terminal());
    let logs: LogBuffer = Arc::default();
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
    let services = UnixServices::with_registry(Registry {
        dirs: vec![services_dir.clone()],
        explicit: config
            .services
            .iter()
            .map(|(k, v)| (k.clone(), resolve(&base, v)))
            .collect(),
    });
    log::info!(
        "services: {} (found: {})",
        services_dir.display(),
        services.known().join(", ")
    );

    let widgets_dir = resolve(&base, &config.widgets_dir);
    let app = config
        .bar
        .iter()
        .fold(
            App::builder().engines(tessela::engines()).module(services),
            |b, w| b.extension(widgets_dir.join(w)),
        )
        .build()?;

    if args.gpui {
        #[cfg(feature = "gpui")]
        return app.run(tessela::gpui::LayerBar::new("marathon").title("marathon"));
        #[cfg(not(feature = "gpui"))]
        anyhow::bail!(
            "built without the `gpui` feature: cargo run -p marathon --features gpui -- --gpui"
        );
    }
    if headless {
        let mut r = Headless::new();
        if let Some(s) = args.seconds {
            r = r.for_duration(Duration::from_secs_f64(s));
        }
        for (w, at) in args.clicks {
            r = r.click(w, Duration::from_secs_f64(at));
        }
        return app.run(r);
    }
    app.run(tessela::tui::Tui::new("marathon").with_logs(logs))
}
