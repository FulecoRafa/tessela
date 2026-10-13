//! What only the renderer knows, for devtools: which windows are open where,
//! and how long drawing each one took. Everything else (trees, patches,
//! module traffic) is observed in the host, independently of the renderer.
//!
//! It also takes two requests back, through an [`InspectControl`] the
//! renderer hands to the probe: outline a node (highlight) and pick mode,
//! where the next click in a window selects the node under the pointer
//! instead of reaching its handlers.

use std::sync::Arc;
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
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

/// A request from devtools to the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InspectCommand {
    /// Outline `(slot, node)` in whatever window draws it; `None` clears.
    Highlight(Option<(usize, u64)>),
    /// Pick mode: pointer moves outline the node under them, a left click
    /// reports it through [`RenderProbe::picked`] and ends the mode, a right
    /// click cancels. Handlers see none of it.
    Pick(bool),
}

/// Sends [`InspectCommand`]s to a running renderer.
#[derive(Clone)]
pub struct InspectControl(UnboundedSender<InspectCommand>);

impl InspectControl {
    /// A control and the receiving end a renderer reads.
    pub fn channel() -> (Self, UnboundedReceiver<InspectCommand>) {
        let (tx, rx) = unbounded();
        (Self(tx), rx)
    }

    /// `false` once the renderer is gone.
    pub fn send(&self, command: InspectCommand) -> bool {
        self.0.unbounded_send(command).is_ok()
    }
}

#[allow(unused_variables)]
pub trait RenderProbe: Send + Sync {
    /// One window finished building its element tree.
    fn rendered(&self, slot: usize, popup: bool, took: Duration);
    /// The set of open windows changed.
    fn windows(&self, windows: Vec<WindowInfo>);
    /// The renderer started and accepts inspect commands.
    fn attach(&self, control: InspectControl) {}
    /// Pick mode: the pointer is over `node` (`done == false`), or the user
    /// clicked it (`done == true`: pick mode is over; `node == None` when
    /// cancelled).
    fn picked(&self, slot: usize, node: Option<u64>, done: bool) {}
    /// Where the highlighted node was drawn, in its window: `[x, y, w, h]`.
    fn measured(&self, slot: usize, node: u64, bounds: [f32; 4]) {}
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
