//! **tessela** (provisional name) — a dynamic UI and extension framework.
//!
//! Applications are assembled from four kinds of parts, each behind a feature:
//!
//! | part | contract | shipped |
//! |---|---|---|
//! | engines | [`core::EngineFactory`] | `luau`, `quickjs` (JS/TSX) |
//! | modules | [`core::Module`] | `unix` (services over Unix sockets), `timer` |
//! | renderers | [`host::Renderer`] | [`host::Headless`], `tui`, `gpui` |
//! | extensions | `widget.toml` + entry file | written by users, sandboxed |
//!
//! ```no_run
//! # #[cfg(all(feature = "luau", feature = "unix"))]
//! # fn main() -> anyhow::Result<()> {
//! use tessela::prelude::*;
//!
//! App::builder()
//!     .engine(tessela::luau::Luau)
//!     .module(tessela::unix::UnixServices::new(["/run/user/1000/marathon/services"]))
//!     .extension("widgets/clock")
//!     .build()?
//!     .run(Headless::new())
//! # }
//! # #[cfg(not(all(feature = "luau", feature = "unix")))] fn main() {}
//! ```
//!
//! See `docs/ARCHITECTURE.md` for the layering and `docs/adr/` for decisions.

pub use tessela_core as core;
pub use tessela_host as host;
pub use tessela_proto as proto;
pub use tessela_view as view;

#[cfg(feature = "gpui")]
pub use tessela_gpui as gpui;
#[cfg(feature = "luau")]
pub use tessela_luau as luau;
#[cfg(feature = "quickjs")]
pub use tessela_quickjs as quickjs;
#[cfg(feature = "timer")]
pub use tessela_timer as timer;
#[cfg(feature = "tui")]
pub use tessela_tui as tui;
#[cfg(feature = "unix")]
pub use tessela_unix as unix;

pub mod prelude {
    pub use tessela_core::{
        EngineFactory, Grants, Inbound, Instance, InstanceSpec, Limits, Module, Node, Sink,
    };
    pub use tessela_host::{App, Frame, Headless, Renderer, Surface, UiEvent};
}

/// Every engine compiled in, for hosts that accept any extension language.
pub fn engines() -> Vec<std::sync::Arc<dyn tessela_core::EngineFactory>> {
    #[allow(unused_mut)]
    let mut v: Vec<std::sync::Arc<dyn tessela_core::EngineFactory>> = Vec::new();
    #[cfg(feature = "luau")]
    v.push(std::sync::Arc::new(tessela_luau::Luau));
    #[cfg(feature = "quickjs")]
    v.push(std::sync::Arc::new(tessela_quickjs::QuickJs));
    v
}
