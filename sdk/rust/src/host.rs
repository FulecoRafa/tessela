use std::cell::RefCell;
use std::future::Future;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener as StdListener, UnixStream as StdStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use async_executor::{LocalExecutor, Task};
use async_net::unix::UnixListener;
use async_signal::{Signal, Signals};
use futures_lite::{StreamExt, future};

use crate::service::Service;

thread_local! {
    static CURRENT: RefCell<Option<Rc<LocalExecutor<'static>>>> = const { RefCell::new(None) };
}

/// Run a task on the current [`Host`]'s thread. It may hold `!Send` state.
/// Detach it (`.detach()`) to let it run on its own; dropping the returned
/// [`Task`] cancels it.
///
/// # Panics
/// Outside [`Host::run`] (or [`Service::run`]).
pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
    CURRENT.with(|c| {
        c.borrow()
            .as_ref()
            .expect("tessela_service::spawn called outside Host::run")
            .spawn(future)
    })
}

/// Stops a running [`Host`] from anywhere (another thread, a test).
#[derive(Clone)]
pub struct Shutdown(async_channel::Sender<()>);

impl Shutdown {
    pub fn stop(&self) {
        let _ = self.0.try_send(());
    }
}

/// Serves several [`Service`]s, one socket each, on one thread.
pub struct Host {
    services: Vec<Service>,
    dir: Option<PathBuf>,
    exit_with_parent: bool,
    stop: (async_channel::Sender<()>, async_channel::Receiver<()>),
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

/// A bound socket, removed on drop if it is still ours.
struct Bound {
    path: PathBuf,
    ino: u64,
}

impl Drop for Bound {
    fn drop(&mut self) {
        // A newer instance may have replaced it; leave that one alone.
        if std::fs::metadata(&self.path).is_ok_and(|m| m.ino() == self.ino) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn bind(path: &Path) -> io::Result<(StdListener, Bound)> {
    if let Some(dir) = path.parent()
        && !dir.exists()
    {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        // Live: someone else serves it. Dead (a crash left it behind): clean up.
        if StdStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} is served by another process", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = StdListener::bind(path)?;
    // Only this user may connect.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let ino = std::fs::metadata(path)?.ino();
    Ok((
        listener,
        Bound {
            path: path.to_path_buf(),
            ino,
        },
    ))
}

impl Host {
    pub fn new() -> Self {
        Self {
            services: Vec::new(),
            dir: None,
            exit_with_parent: true,
            stop: async_channel::bounded(1),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, service: Service) -> Self {
        self.services.push(service);
        self
    }

    /// Put the sockets here instead of [`crate::services_dir`].
    pub fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Do not stop when the parent process dies (by default the service
    /// asks the kernel for SIGTERM then, like the runner does).
    pub fn keep_running_without_parent(mut self) -> Self {
        self.exit_with_parent = false;
        self
    }

    pub fn shutdown_handle(&self) -> Shutdown {
        Shutdown(self.stop.0.clone())
    }

    /// Bind every socket, run `main` beside them and serve until SIGTERM,
    /// SIGINT, the parent's death or [`Shutdown::stop`]. Then every task is
    /// dropped and the sockets are removed. Fails before serving anything if
    /// a socket cannot be bound (a live one belongs to another process).
    pub fn run(self, main: impl Future<Output = ()> + 'static) -> io::Result<()> {
        let dir = self.dir.clone().unwrap_or_else(crate::services_dir);
        let mut names: Vec<&str> = self.services.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("service {} added twice", w[0]),
            ));
        }
        let mut bound = Vec::new();
        let mut listeners = Vec::new();
        for svc in &self.services {
            let path = dir.join(format!("{}.sock", svc.name));
            let (l, b) = bind(&path)?;
            bound.push(b);
            listeners.push((path, UnixListener::try_from(l)?));
        }
        if self.exit_with_parent {
            // SAFETY: plain prctl; no memory is touched.
            unsafe {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            }
        }
        let mut signals = Signals::new([Signal::Term, Signal::Int])?;

        let ex = Rc::new(LocalExecutor::new());
        CURRENT.with(|c| *c.borrow_mut() = Some(ex.clone()));
        for (svc, (path, listener)) in self.services.into_iter().zip(listeners) {
            eprintln!("{}: listening on {}", svc.name, path.display());
            let svc = Rc::new(svc);
            ex.spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => spawn(svc.clone().serve_client(stream)).detach(),
                        Err(e) => {
                            eprintln!("{}: accept: {e}", svc.name);
                            async_io::Timer::after(Duration::from_millis(100)).await;
                        }
                    }
                }
            })
            .detach();
        }
        ex.spawn(main).detach();

        let stop = self.stop.1;
        async_io::block_on(ex.run(future::or(
            async {
                signals.next().await;
            },
            async {
                let _ = stop.recv().await;
            },
        )));
        CURRENT.with(|c| *c.borrow_mut() = None);
        // Dropping the executor drops every task: children die, sockets close.
        drop(ex);
        drop(bound);
        Ok(())
    }
}
