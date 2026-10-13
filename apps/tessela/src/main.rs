//! `tessela` — runs a project (a directory with `tessela.toml`).
//!
//! ```text
//! tessela run <project> [--dev | --nested[=WxH@SCALE]] [--devtools] [--headless [--seconds N]]
//! tessela devtools [--project NAME] <command>     (see devtools.rs)
//! ```
//!
//! `--dev` places surfaces next to the desktop's own bar instead of over it:
//! nothing reserves space and everything sits below (or beside) that bar.
//! `--nested` renders into a headless sway of its own (see nested.rs), for
//! previews and screenshots without a screen.
//!
//! Logs go to stderr; `TESSELA_LOG=debug` for more, `TESSELA_LOG=warn` for less.
//! Other crates (GPUI, zbus...) only log warnings.

mod devtools;
mod nested;
mod plugins;
mod project;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use tessela::desktop::Desktop;
use tessela::host::{App, Headless};
use tessela::state::SharedState;
use tessela::timer::Timers;
use tessela::unix::UnixServices;

use crate::devtools::Devtools;
use crate::plugins::Plugins;
use crate::project::Project;

const USAGE: &str = "\
tessela run <project> [--dev | --nested[=WxH@SCALE]] [--devtools] [--headless [--seconds N]]
tessela devtools [--project NAME] <command>   (tessela devtools --help)";

/// Set when running with --devtools: log lines also go to its console.
static DEVTOOLS: OnceLock<Arc<Devtools>> = OnceLock::new();

struct Logger {
    level: log::LevelFilter,
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        let target = m.target();
        let ours = target.starts_with("tessela") || target.starts_with("marathon");
        // zbus warns whenever a name is owned without serving objects, which
        // is what a StatusNotifierHost does by spec; its errors still show.
        let others = if target.starts_with("zbus") {
            log::LevelFilter::Error
        } else {
            log::LevelFilter::Warn
        };
        m.level() <= if ours { self.level } else { others }
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("{:<5} {}", r.level(), r.args());
            if let Some(d) = DEVTOOLS.get() {
                d.host_log(r.level(), &r.args().to_string());
            }
        }
    }
    fn flush(&self) {}
}

fn init_log() {
    let level = match std::env::var("TESSELA_LOG").as_deref() {
        Ok("debug") => log::LevelFilter::Debug,
        Ok("warn") => log::LevelFilter::Warn,
        Ok("error") => log::LevelFilter::Error,
        _ => log::LevelFilter::Info,
    };
    let _ = log::set_boxed_logger(Box::new(Logger { level }));
    log::set_max_level(log::LevelFilter::Debug);
}

struct RunArgs {
    project: PathBuf,
    dev: bool,
    /// Render into a headless sway: `WxH@SCALE`.
    nested: Option<String>,
    devtools: bool,
    headless: bool,
    seconds: Option<f64>,
}

fn run_args(mut it: impl Iterator<Item = String>) -> anyhow::Result<RunArgs> {
    let mut a = RunArgs {
        project: PathBuf::new(),
        dev: false,
        nested: None,
        devtools: false,
        headless: false,
        seconds: None,
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dev" => a.dev = true,
            "--devtools" => a.devtools = true,
            "--nested" => a.nested = Some("1920x1080@1.25".into()),
            s if s.starts_with("--nested=") => a.nested = Some(s["--nested=".len()..].to_string()),
            "--headless" => a.headless = true,
            "--seconds" => {
                a.seconds = Some(it.next().context("--seconds needs a number")?.parse()?)
            }
            s if !s.starts_with('-') && a.project.as_os_str().is_empty() => a.project = s.into(),
            other => anyhow::bail!("unknown argument {other}\n{USAGE}"),
        }
    }
    anyhow::ensure!(!a.project.as_os_str().is_empty(), "which project?\n{USAGE}");
    Ok(a)
}

fn theme_tokens(tokens: &HashMap<String, String>) -> HashMap<String, u32> {
    tokens
        .iter()
        .filter_map(|(k, v)| {
            let [r, g, b, _] = tessela::view::styled::parse_hex(v)?;
            Some((k.clone(), (r as u32) << 16 | (g as u32) << 8 | b as u32))
        })
        .collect()
}

fn run(args: RunArgs) -> anyhow::Result<()> {
    let project = Project::load(&args.project)?;
    let runtime = project.runtime_dir();
    let services = runtime.join("services");
    std::fs::create_dir_all(&services)?;
    // Before any thread starts: it changes this process's environment.
    let _nested = match &args.nested {
        Some(spec) => {
            let n = nested::start(&runtime, spec)?;
            // SAFETY: no other thread exists yet.
            unsafe {
                std::env::set_var("WAYLAND_DISPLAY", &n.display);
                std::env::remove_var("DISPLAY");
            }
            Some(n)
        }
        None => None,
    };
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&services, std::fs::Permissions::from_mode(0o700))?;
    }
    log::info!(
        "{}: {} surfaces ({}), {} plugins, services in {}",
        project.name,
        project.surfaces.len(),
        project
            .surfaces
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        project.plugins.len(),
        services.display()
    );

    let plugins = Arc::new(Plugins::start(&project.plugins, &project.root, &services));
    // Give plugins a moment to create their sockets before the first render.
    if !project.plugins.is_empty() {
        std::thread::sleep(Duration::from_millis(300));
    }

    let desktop = Arc::new(Desktop::new());
    let state = Arc::new(SharedState::with_store(project.state_file()));
    let mut builder = App::builder()
        .engines(tessela::engines())
        .module(UnixServices::new([services.clone()]))
        .module(Timers::new())
        .shared_module(state.clone());
    let devtools = args.devtools.then(|| {
        let d = Devtools::new();
        d.attach(devtools::Sources {
            project: project.name.clone(),
            state: state.clone(),
            desktop: desktop.clone(),
            plugins: plugins.clone(),
            screen: args.nested.as_deref().and_then(nested::logical_size),
        });
        let _ = DEVTOOLS.set(d.clone());
        d
    });
    if let Some(d) = &devtools {
        d.serve(runtime.join("devtools.sock"))?;
        builder = builder.probe(d.clone());
    }
    for m in desktop.modules() {
        builder = builder.shared_module(m);
    }
    for entry in project.entries() {
        builder = builder.entry(entry);
    }
    let app = builder.build()?;

    let result = if args.headless {
        let mut r = Headless::new();
        if let Some(s) = args.seconds {
            r = r.for_duration(Duration::from_secs_f64(s));
        }
        app.run(r)
    } else {
        run_gpui(app, &project, &desktop, args.dev, devtools)
    };
    plugins.stop();
    result
}

#[cfg(feature = "gpui")]
fn run_gpui(
    app: tessela::host::App,
    project: &Project,
    desktop: &Arc<Desktop>,
    dev: bool,
    devtools: Option<Arc<Devtools>>,
) -> anyhow::Result<()> {
    use tessela::gpui::{Shell, Theme};
    let images = Arc::new(desktop.images(Some(project.root.clone())));
    let mut shell = Shell::new(project.name.clone());
    shell.font = project.font.clone();
    shell.theme = Theme {
        dark: theme_tokens(&project.theme.dark),
        light: theme_tokens(&project.theme.light),
    };
    shell.images = Arc::new(move |src| images.resolve(src));
    shell.dev_placement = dev;
    shell.scheme = Some(desktop.context.color_scheme());
    shell.inspector = devtools.map(|d| tessela::gpui::Inspector(d));
    app.run(shell)
}

#[cfg(not(feature = "gpui"))]
fn run_gpui(
    _: tessela::host::App,
    _: &Project,
    _: &Arc<Desktop>,
    _: bool,
    _: Option<Arc<Devtools>>,
) -> anyhow::Result<()> {
    anyhow::bail!("built without the `gpui` feature; use --headless")
}

fn main() -> anyhow::Result<()> {
    init_log();
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("run") => run(run_args(args)?),
        Some("devtools") => {
            // Piped into `head`: exit quietly instead of panicking. (Only
            // here: the runner must survive writes to closed sockets.)
            // SAFETY: restoring the default disposition of one signal.
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            }
            devtools::client(args)
        }
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other}\n{USAGE}"),
    }
}
