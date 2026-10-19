//! Keeps a project's plugins (its services) running.
//!
//! Each plugin is a command run from the project directory with
//! `TESSELA_SERVICES_DIR` (and `MARATHON_SERVICES_DIR`, which the SDKs read)
//! pointing at the directory where it must create `<name>.sock`. Output goes
//! to the log, prefixed with the plugin's name. A plugin that exits is
//! restarted, waiting longer each time it dies quickly; all of them are
//! killed (with their children) when the runner exits.
//!
//! A plugin may serve several services (one socket each) from one process;
//! `services = [...]` names them, so the runner can wait for their sockets
//! and devtools can tell which are up. A relative program path with a `/`
//! (`services/target/release/x`) is inside the project.

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
    /// The sockets it serves (declared, or the one named like it) and
    /// whether each exists.
    pub services: Vec<(String, bool)>,
}

pub struct Plugins {
    /// Declared services per plugin (or its own name), and where they live.
    serves: std::collections::BTreeMap<String, Vec<String>>,
    dir: PathBuf,
    children: Arc<Mutex<Vec<(String, u32)>>>,
    status: Arc<Mutex<std::collections::BTreeMap<String, PluginStatus>>>,
    stopping: Arc<AtomicBool>,
}

fn spawn(name: &str, cfg: &PluginConfig, root: &Path, services: &Path) -> std::io::Result<Child> {
    let (program, args) = cfg
        .command
        .split_first()
        .ok_or_else(|| std::io::Error::other("empty command"))?;
    let mut cmd = Command::new(resolve(root, program));
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
    // Elsewhere a plugin on the Rust SDK watches its parent itself.
    // SAFETY: prctl is async-signal-safe; nothing else runs before exec.
    #[cfg(target_os = "linux")]
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
        let serves = plugins
            .iter()
            .map(|(n, c)| {
                let s = if c.services.is_empty() {
                    vec![n.clone()]
                } else {
                    c.services.clone()
                };
                (n.clone(), s)
            })
            .collect();
        Self {
            children,
            status,
            stopping,
            serves,
            dir: services.to_path_buf(),
        }
    }

    /// Every configured plugin, running or not.
    pub fn status(&self) -> std::collections::BTreeMap<String, PluginStatus> {
        let mut all = self.status.lock().unwrap().clone();
        for (name, s) in all.iter_mut() {
            s.services = services_up(&self.serves[name], &self.dir);
        }
        all
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

/// A relative program path with a `/` is relative to the project, whatever
/// the runner's own directory; a bare name is looked up in `PATH`.
fn resolve(root: &Path, program: &str) -> PathBuf {
    let p = Path::new(program);
    if program.contains('/') && p.is_relative() {
        root.join(p)
    } else {
        p.to_path_buf()
    }
}

/// Wait until every named service accepts connections on `<dir>/<name>.sock`,
/// at most `timeout`. Returns the ones still missing.
pub fn wait_for_services(names: &[String], dir: &Path, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut missing: Vec<String> = names.to_vec();
    loop {
        missing.retain(|n| {
            std::os::unix::net::UnixStream::connect(dir.join(format!("{n}.sock"))).is_err()
        });
        if missing.is_empty() || Instant::now() >= deadline {
            return missing;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Which of `names` have a socket in `dir` right now.
pub fn services_up(names: &[String], dir: &Path) -> Vec<(String, bool)> {
    names
        .iter()
        .map(|n| (n.clone(), dir.join(format!("{n}.sock")).exists()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tessela-plugins-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn relative_programs_live_in_the_project() {
        let root = Path::new("/p/shell");
        assert_eq!(
            resolve(root, "services/target/release/x"),
            Path::new("/p/shell/services/target/release/x")
        );
        assert_eq!(resolve(root, "./bin/x"), Path::new("/p/shell/./bin/x"));
        assert_eq!(resolve(root, "python3"), Path::new("python3"));
        assert_eq!(resolve(root, "/usr/bin/x"), Path::new("/usr/bin/x"));
    }

    #[test]
    fn a_relative_program_starts_from_the_project() {
        let root = temp("prog");
        let services = root.join("run");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let script = root.join("bin/hello");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"$TESSELA_PLUGIN\" > \"$TESSELA_SERVICES_DIR/../out\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(&services).unwrap();
        let cfg = PluginConfig {
            command: vec!["bin/hello".into()],
            env: Default::default(),
            services: vec![],
        };
        // The runner's own directory is elsewhere; the path still resolves.
        let mut child = spawn("hello", &cfg, &root, &services).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(root.join("out")).unwrap().trim(),
            "hello"
        );
    }

    #[test]
    fn waits_for_every_declared_socket() {
        let dir = temp("wait");
        let names = vec!["audio".to_string(), "power".to_string()];
        let d = dir.clone();
        let t = thread::spawn(move || {
            let a = UnixListener::bind(d.join("audio.sock")).unwrap();
            thread::sleep(Duration::from_millis(150));
            let p = UnixListener::bind(d.join("power.sock")).unwrap();
            thread::sleep(Duration::from_millis(500));
            drop((a, p));
        });
        let start = Instant::now();
        assert!(wait_for_services(&names, &dir, Duration::from_secs(3)).is_empty());
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert_eq!(
            services_up(&names, &dir),
            vec![("audio".into(), true), ("power".into(), true)]
        );
        t.join().unwrap();
        // Never comes up: reported after the timeout.
        let missing = wait_for_services(&["nope".to_string()], &dir, Duration::from_millis(100));
        assert_eq!(missing, vec!["nope".to_string()]);
    }
}
