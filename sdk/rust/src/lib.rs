//! Write tessela (and Marathon) services in Rust.
//!
//! A service is a Unix socket `<dir>/<name>.sock` speaking one JSON object per
//! line. It declares **topics** (state that widgets subscribe to: they get the
//! current value first, then every change) and **methods** (actions they
//! call). This crate speaks the protocol, keeps any number of clients, and
//! cleans the socket up; you write the data.
//!
//! ```no_run
//! use std::time::Duration;
//! use tessela_service::{Service, Timer, json, Value};
//!
//! let mut svc = Service::new("counter");
//! let count = svc.topic("count", 0u64);
//! let c = count.clone();
//! svc.method("reset", move |_: Value| {
//!     let c = c.clone();
//!     async move { c.publish(0); Ok(json!("done")) }
//! });
//! svc.run(async move {
//!     loop {
//!         Timer::after(Duration::from_secs(1)).await;
//!         count.update(|n| *n += 1);
//!     }
//! })
//! .unwrap();
//! ```
//!
//! **Several services, one process.** [`Host`] serves many [`Service`]s
//! (one socket each) on a single thread; that is how a project ships all its
//! plugins as one binary:
//!
//! ```no_run
//! # use tessela_service::{Host, Service};
//! let audio = Service::new("audio");
//! let power = Service::new("power");
//! Host::new().add(audio).add(power).run(async { /* start background work */ }).unwrap();
//! ```
//!
//! **Runtime.** Everything runs on one thread, on the small `async-io` /
//! `async-executor` stack (the one zbus uses by default), so handlers and
//! background tasks may hold `Rc`/`RefCell` state. Use [`spawn`] for more
//! tasks, [`unblock`] for blocking work (file or HTTP I/O), [`Timer`] to
//! wait, and the `process` and `dbus` helpers for the usual data sources.
//!
//! **Where the socket goes.** `$TESSELA_SERVICES_DIR`, else
//! `$MARATHON_SERVICES_DIR` (both set by `tessela run` for a project's
//! plugins), else `$XDG_RUNTIME_DIR/marathon/services`. See [`services_dir`].
//!
//! **Shutdown.** SIGTERM or SIGINT, the parent dying (`PR_SET_PDEATHSIG`) or
//! [`Shutdown::stop`] end [`Host::run`]: every task is dropped (children
//! started with [`process`] are killed) and the sockets are removed.

mod error;
mod host;
mod service;
mod topic;
mod wake;

#[cfg(feature = "dbus")]
pub mod dbus;
#[cfg(feature = "process")]
pub mod process;

use std::path::PathBuf;

pub use async_executor::Task;
pub use async_io::Timer;
pub use blocking::unblock;
pub use error::{Error, Result};
pub use futures_lite;
pub use host::{Host, Shutdown, spawn};
pub use serde_json::{self, Value, json};
pub use service::{Description, Service};
pub use topic::Topic;
pub use wake::{Wake, poll};

/// Where services put their sockets: `$TESSELA_SERVICES_DIR`, else
/// `$MARATHON_SERVICES_DIR`, else `$XDG_RUNTIME_DIR/marathon/services`
/// (`/tmp/marathon/services` without a runtime dir).
pub fn services_dir() -> PathBuf {
    for var in ["TESSELA_SERVICES_DIR", "MARATHON_SERVICES_DIR"] {
        if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
            return dir.into();
        }
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    runtime.join("marathon").join("services")
}

/// `<services_dir>/<name>.sock`.
pub fn socket_path(name: &str) -> PathBuf {
    services_dir().join(format!("{name}.sock"))
}
