//! Watching the host work, for devtools: what loaded, what each module call
//! did, what arrived from modules, what the user triggered, how long renders
//! took and the patches they produced. Install one with
//! [`AppBuilder::probe`](crate::AppBuilder::probe); without it nothing is
//! measured.
//!
//! Every method runs on a worker thread, in the middle of its loop: keep
//! them cheap (record and return).

use std::path::PathBuf;
use std::time::Duration;

use tessela_core::{Inbound, InvokeRecord};

use crate::{Frame, Slot, UiEvent};

/// Live objects in an instance's reactive graph after a render.
#[derive(Debug, Clone, Copy, Default)]
pub struct GraphStats {
    pub bindings: usize,
    pub signals: usize,
    /// Module resources: subscriptions, timers...
    pub resources: usize,
}

#[allow(unused_variables)]
pub trait Probe: Send + Sync + 'static {
    /// The slots exist; keep them to send events of your own.
    fn started(&self, slots: &[Slot]) {}
    /// An extension (re)loaded; `files` are the ones it read.
    fn loaded(&self, slot: usize, name: &str, files: &[PathBuf], took: Duration) {}
    fn failed(&self, slot: usize, name: &str, error: &str) {}
    /// A render ran; `frame` is what went to the renderer (`None`: no change).
    fn rendered(&self, slot: usize, took: Duration, frame: Option<&Frame>, stats: GraphStats) {}
    /// A module delivered something (a signal value, a callback, a finished resource).
    fn delivered(&self, slot: usize, inbound: &Inbound, took: Duration, error: Option<&str>) {}
    /// The user (or a probe) triggered an event prop.
    fn event(&self, slot: usize, event: &UiEvent, took: Duration, error: Option<&str>) {}
    /// A line from the guest's `log(...)`.
    fn log(&self, slot: usize, line: &str) {}
    /// The same, with where the guest called it (`file:line`), when the
    /// engine knows. Defaults to [`log`](Self::log).
    fn log_at(&self, slot: usize, line: &str, at: Option<&str>) {
        self.log(slot, line);
    }
    /// A module call, from guest code or a declarative action.
    fn invoke(&self, slot: usize, record: InvokeRecord) {}
}
