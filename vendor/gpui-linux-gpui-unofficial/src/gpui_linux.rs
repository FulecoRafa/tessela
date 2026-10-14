#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;
// tessela patch: outgoing drags a caller starts itself; see `start_drag`.
pub use linux::start_drag;
