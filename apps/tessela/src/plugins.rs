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

/// What devtools shows per plugin.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PluginStatus {
    pub pid: Option<u32>,
    pub starts: u32,
    /// How the last run ended (exit status or a spawn error).
    pub last_exit: Option<String>,
}

pub struct Plugins {
    children: Arc<Mutex<Vec<(String, u32)>>>,
    status: Arc<Mutex<std::collections::BTreeMap<String, PluginStatus>>>,
    stopping: Arc<AtomicBool>,
}

fn spawn(name: &str, cfg: &PluginConfig, root: &Path, services: &Path) -> std::io::Result<Child> {
    let (program, args) = cfg
        .command
        .split_first()
        .ok_or_else(|| std::io::Error::other("empty command"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(root)
        .env("TESSELA_SERVICES_DIR", services)
        .env("MARATHON_SERVICES_DIR", services)
        .env("TESSELA_PLUGIN", name)
        .env("PYTHONUNBUFFERED", "1")
        .envs(&cfg.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // If the runner dies, even by SIGKILL, the kernel stops the plugin too.
    // SAFETY: prctl is async-signal-safe; nothing else runs before exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
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
        let status: Arc<Mutex<std::collections::BTreeMap<String, PluginStatus>>> =
            Arc::new(Mutex::new(
                plugins
                    .keys()
                    .map(|n| (n.clone(), PluginStatus::default()))
                    .collect(),
            ));
        for (name, cfg) in plugins {
            let (name, cfg) = (name.clone(), cfg.clone());
            let (root, services): (PathBuf, PathBuf) = (root.to_path_buf(), services.to_path_buf());
            let (children, stopping, status) = (children.clone(), stopping.clone(), status.clone());
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
                                if let Some(s) = status.lock().unwrap().get_mut(&name) {
                                    s.pid = Some(child.id());
                                    s.starts += 1;
                                }
                                let exit = child.wait();
                                let pid = child.id();
                                children.lock().unwrap().retain(|(_, p)| *p != pid);
                                if let Some(s) = status.lock().unwrap().get_mut(&name) {
                                    s.pid = None;
                                    s.last_exit = Some(format!("{exit:?}"));
                                }
                                if stopping.load(Ordering::Relaxed) {
                                    break;
                                }
                                log::warn!("plugin {name}: exited ({exit:?})");
                            }
                            Err(e) => {
                                if let Some(s) = status.lock().unwrap().get_mut(&name) {
                                    s.last_exit = Some(e.to_string());
                                }
                                log::warn!("plugin {name}: cannot start {:?}: {e}", cfg.command);
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
        Self {
            children,
            status,
            stopping,
        }
    }

    /// Every configured plugin, running or not.
    pub fn status(&self) -> std::collections::BTreeMap<String, PluginStatus> {
        self.status.lock().unwrap().clone()
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
