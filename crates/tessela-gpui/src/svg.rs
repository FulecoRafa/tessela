//! SVGs rasterized at the size they are drawn.
//!
//! GPUI's `img(path)` renders an SVG once at its intrinsic size and then
//! scales the bitmap: a 535 px logo drawn at 28 px comes out jagged. Here
//! the drawing is rendered straight to the device pixels it will cover.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::RenderImage;
use resvg::{tiny_skia, usvg};

thread_local! {
    /// By file and side in device pixels; `None` remembers a failure.
    static CACHE: RefCell<HashMap<(PathBuf, u32), Option<Arc<RenderImage>>>> =
        RefCell::default();
}

pub fn is_svg(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// `path` rendered into a `side`×`side` square (aspect kept, centered).
pub fn render(path: &Path, side: u32) -> Option<Arc<RenderImage>> {
    let side = side.clamp(1, 2048);
    let key = (path.to_path_buf(), side);
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let image = rasterize(path, side)
        .inspect_err(|e| log::warn!("svg {}: {e}", path.display()))
        .ok()
        .map(Arc::new);
    CACHE.with(|c| c.borrow_mut().insert(key, image.clone()));
    image
}

fn rasterize(path: &Path, side: u32) -> anyhow::Result<RenderImage> {
    let data = std::fs::read(path)?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default())?;
    let size = tree.size();
    let scale = side as f32 / size.width().max(size.height());
    let (dx, dy) = (
        (side as f32 - size.width() * scale) / 2.0,
        (side as f32 - size.height() * scale) / 2.0,
    );
    let mut pixmap = tiny_skia::Pixmap::new(side, side).ok_or(usvg::Error::InvalidSize)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy),
        &mut pixmap.as_mut(),
    );
    // Premultiplied RGBA → the straight BGRA GPUI uploads.
    let mut bytes = pixmap.take();
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
        if px[3] > 0 && px[3] < 255 {
            let a = px[3] as f32 / 255.0;
            for c in &mut px[..3] {
                *c = (*c as f32 / a).round().min(255.0) as u8;
            }
        }
    }
    let buffer = image::RgbaImage::from_raw(side, side, bytes)
        .ok_or_else(|| anyhow::anyhow!("pixmap size"))?;
    Ok(RenderImage::new([image::Frame::new(buffer)]))
}
