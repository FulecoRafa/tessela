//! Keeps a project's plugins (its services) running.
//!
//! Each plugin is a command run from the project directory with
//! `TESSELA_SERVICES_DIR` (and `MARATHON_SERVICES_DIR`, which the SDKs read)
//! pointing at the directory where it must create `<name>.sock`. Output goes
//! to the log, prefixed with the plugin's name. A plugin that exits is
//! restarted, waiting longer each time it dies quickly; all of them are
//! killed (with their children) when the runner exits.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::project::PluginConfig;

const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Running at least this long resets the backoff.
const HEALTHY: Duration = Duration::from_secs(10);

pub struct Plugins {
    children: Arc<Mutex<Vec<(String, u32)>>>,
    stopping: Arc<AtomicBool>,
}

fn spawn(name: &str, cfg: &PluginConfig, root: &Path, services: &Path) -> std::io::Result<Child> {
    let (program, args) = cfg
        .command
        .split_first()
        .ok_or_else(|| std::io::Error::other("empty command"))?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("TESSELA_SERVICES_DIR", services)
        .env("MARATHON_SERVICES_DIR", services)
        .env("TESSELA_PLUGIN", name)
        .env("PYTHONUNBUFFERED", "1")
        .envs(&cfg.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as _),
    ]
    .into_iter()
    .flatten()
    {
        let name = name.to_string();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                log::info!(target: "tessela::plugin", "[{name}] {line}");
            }
        });
    }
    Ok(child)
}

impl Plugins {
    pub fn start(
        plugins: &std::collections::BTreeMap<String, PluginConfig>,
        root: &Path,
        services: &Path,
    ) -> Self {
        let children: Arc<Mutex<Vec<(String, u32)>>> = Arc::default();
        let stopping = Arc::new(AtomicBool::new(false));
        for (name, cfg) in plugins {
            let (name, cfg) = (name.clone(), cfg.clone());
            let (root, services): (PathBuf, PathBuf) = (root.to_path_buf(), services.to_path_buf());
            let (children, stopping) = (children.clone(), stopping.clone());
            thread::Builder::new()
                .name(format!("plugin-{name}"))
                .spawn(move || {
                    let mut backoff = Duration::from_secs(1);
                    while !stopping.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        match spawn(&name, &cfg, &root, &services) {
                            Ok(mut child) => {
                                log::info!("plugin {name}: started (pid {})", child.id());
                                children.lock().unwrap().push((name.clone(), child.id()));
                                let status = child.wait();
                                let pid = child.id();
                                children.lock().unwrap().retain(|(_, p)| *p != pid);
                                if stopping.load(Ordering::Relaxed) {
                                    break;
                                }
                                log::warn!("plugin {name}: exited ({status:?})");
                            }
                            Err(e) => {
                                log::warn!("plugin {name}: cannot start {:?}: {e}", cfg.command)
                            }
                        }
                        if started.elapsed() > HEALTHY {
                            backoff = Duration::from_secs(1);
                        }
                        thread::sleep(backoff);
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                })
                .expect("spawn plugin supervisor");
        }
        Self { children, stopping }
    }

    /// Running plugins: (name, pid).
    pub fn running(&self) -> Vec<(String, u32)> {
        self.children.lock().unwrap().clone()
    }

    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        for (name, pid) in self.children.lock().unwrap().drain(..) {
            // The whole group: plugins may have children of their own.
            // SAFETY: plain syscall; a stale pid only fails with ESRCH.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            log::info!("plugin {name}: stopped");
        }
    }
}

impl Drop for Plugins {
    fn drop(&mut self) {
        self.stop();
    }
}
