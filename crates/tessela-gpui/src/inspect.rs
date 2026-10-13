//! What only the renderer knows, for devtools: which windows are open where,
//! and how long drawing each one took. Everything else (trees, patches,
//! module traffic) is observed in the host, independently of the renderer.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WindowInfo {
    pub slot: usize,
    /// Node id of the `Popup` this window shows, for popup windows.
    pub popup: Option<u64>,
    pub layer: String,
    /// `[top, right, bottom, left]`.
    pub anchors: [bool; 4],
    pub size: [f32; 2],
    pub margin: [f32; 4],
    /// Covers the output; an outside press dismisses.
    pub overlay: bool,
    pub keyboard: bool,
}

pub trait RenderProbe: Send + Sync {
    /// One window finished building its element tree.
    fn rendered(&self, slot: usize, popup: bool, took: Duration);
    /// The set of open windows changed.
    fn windows(&self, windows: Vec<WindowInfo>);
}

#[derive(Clone)]
pub struct Inspector(pub Arc<dyn RenderProbe>);

impl Inspector {
    pub(crate) fn rendered(&self, slot: usize, popup: bool, took: Duration) {
        self.0.rendered(slot, popup, took);
    }

    pub(crate) fn set_windows(&self, windows: Vec<WindowInfo>) {
        self.0.windows(windows);
    }
}
