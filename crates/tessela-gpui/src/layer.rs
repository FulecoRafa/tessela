//! Surfaces where there is no layer-shell (macOS): a borderless panel above
//! other windows, on every space, placed on the display the way layer-shell
//! places a surface on its output. The space a surface reserves (exclusive
//! zone) and the layer it asks for are not honored: every surface floats at
//! the same level.

use gpui::{Bounds, Pixels, Size, point, px};

/// The layers a surface may ask for, as `gpui::layer_shell::Layer` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layer {
    Background,
    Bottom,
    Top,
    Overlay,
}

/// Where a surface anchored to `[top, right, bottom, left]` sits on a
/// display of `size`, in the display's coordinates (what window bounds are
/// on macOS): stretched along an axis anchored on both ends, centered along
/// one anchored on neither, `margin` (`[top, right, bottom, left]`) from the
/// edges it is anchored to.
pub(crate) fn frame(
    display: Size<Pixels>,
    anchors: [bool; 4],
    width: f32,
    height: f32,
    margin: [f32; 4],
) -> Bounds<Pixels> {
    let [top, right, bottom, left] = anchors;
    let [mt, mr, mb, ml] = margin;
    let (dw, dh) = (f32::from(display.width), f32::from(display.height));
    let axis = |start: bool, end: bool, size: f32, total: f32, m0: f32, m1: f32| {
        if start && end {
            (m0, (total - m0 - m1).max(1.0))
        } else if start {
            (m0, size)
        } else if end {
            (total - size - m1, size)
        } else {
            ((total - size) / 2.0, size)
        }
    };
    let (x, w) = axis(left, right, width, dw, ml, mr);
    let (y, h) = axis(top, bottom, height, dh, mt, mb);
    Bounds {
        origin: point(px(x), px(y)),
        size: Size::new(px(w), px(h)),
    }
}
