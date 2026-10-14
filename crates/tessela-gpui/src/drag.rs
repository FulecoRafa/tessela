//! Dragging a `Box drag` out of a surface onto other applications
//! (docs/adr/0023). GPUI drags stay inside its own windows on Wayland, so
//! the renderer recognizes the gesture itself and starts the drag through
//! the vendored platform crate (`gpui_linux::start_drag`).

use gpui::{Pixels, Point, Window};
use tessela_host::Slot;
use tessela_view::Handler;

/// How far the pointer moves, pressed, before a press becomes a drag.
pub(crate) const THRESHOLD: f32 = 6.0;

/// A press on a `Box drag`, until the pointer moves past [`THRESHOLD`] (a
/// drag) or is released (a click).
pub(crate) struct Press {
    pub from: Point<Pixels>,
    pub paths: Vec<String>,
    pub ui: Slot,
    pub start: Option<Handler>,
    pub end: Option<Handler>,
}

/// Whether the pointer moved from `from` to `to` far enough to drag.
pub(crate) fn far(from: Point<Pixels>, to: Point<Pixels>) -> bool {
    let (dx, dy) = (f32::from(to.x - from.x), f32::from(to.y - from.y));
    dx * dx + dy * dy > THRESHOLD * THRESHOLD
}

/// `file://` URI of an absolute path, percent-encoding all but unreserved
/// characters and `/` (RFC 8089, as file managers write them).
pub(crate) fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// What a drag of `paths` offers: `text/uri-list` (CRLF-terminated file
/// URIs, for file managers, browsers, chat apps) and `text/plain` (the
/// paths, one per line, for terminals and text fields).
pub(crate) fn offers(paths: &[String]) -> Vec<(String, Vec<u8>)> {
    let uris: String = paths.iter().map(|p| file_uri(p) + "\r\n").collect();
    vec![
        ("text/uri-list".into(), uris.into_bytes()),
        (
            "text/plain;charset=utf-8".into(),
            paths.join("\n").into_bytes(),
        ),
        ("text/plain".into(), paths.join("\n").into_bytes()),
    ]
}

/// Starts the drag from `window` with the press still held; `on_end` gets
/// whether a target took the drop. False when no drag started.
pub(crate) fn start(window: &Window, paths: &[String], on_end: Box<dyn FnOnce(bool)>) -> bool {
    #[cfg(target_os = "linux")]
    {
        gpui_linux::start_drag(window, offers(paths), on_end)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, paths, on_end);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_are_percent_encoded() {
        assert_eq!(
            file_uri("/tmp/a b/ç#1.txt"),
            "file:///tmp/a%20b/%C3%A7%231.txt"
        );
        assert_eq!(file_uri("/home/u/x-y_z.~"), "file:///home/u/x-y_z.~");
    }

    #[test]
    fn a_drag_offers_uris_and_text() {
        let o = offers(&["/a b".into(), "/c".into()]);
        assert_eq!(
            o[0],
            (
                "text/uri-list".into(),
                b"file:///a%20b\r\nfile:///c\r\n".to_vec()
            )
        );
        assert_eq!(o[2], ("text/plain".into(), b"/a b\n/c".to_vec()));
    }

    #[test]
    fn a_press_drags_past_the_threshold() {
        let at = |x, y| gpui::point(gpui::px(x), gpui::px(y));
        assert!(!far(at(10.0, 10.0), at(14.0, 14.0)));
        assert!(far(at(10.0, 10.0), at(17.0, 10.0)));
    }
}
