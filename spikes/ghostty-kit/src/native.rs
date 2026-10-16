//! AppKit glue for the overlay window.

use gpui_kit::Window;
use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

pub fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live content NSView; we only read its window
    // on the main thread.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    view.window()
}

/// Makes `child` a borderless, transparent, click-through window glued above
/// `parent`'s content area.
pub fn attach_overlay(parent: &NSWindow, child: &NSWindow) {
    child.setStyleMask(NSWindowStyleMask::Borderless);
    child.setOpaque(false);
    child.setBackgroundColor(Some(&NSColor::clearColor()));
    child.setHasShadow(false);
    child.setIgnoresMouseEvents(true);
    // SAFETY: both windows are live and we are on the main thread.
    unsafe { parent.addChildWindow_ordered(child, NSWindowOrderingMode::Above) };
    child.setLevel(parent.level());
    match_content_frame(parent, child);
}

pub fn match_content_frame(parent: &NSWindow, child: &NSWindow) {
    let content = parent.contentRectForFrameRect(parent.frame());
    child.setFrame_display(content, true);
}

pub fn window_number(window: &Window) -> Option<isize> {
    ns_window(window).map(|window| window.windowNumber())
}

/// Frames (top-left origin, points) and hidden flags of the native child
/// views GPUI's content view holds, i.e. the Ghostty surfaces.
pub fn child_view_frames(window: &Window) -> Vec<(f64, f64, f64, f64, bool)> {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return Vec::new();
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Vec::new();
    };
    // SAFETY: as in `ns_window`.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    let height = view.bounds().size.height;
    view.subviews()
        .iter()
        .map(|child| {
            let frame = child.frame();
            (
                frame.origin.x,
                height - frame.origin.y - frame.size.height,
                frame.size.width,
                frame.size.height,
                child.isHidden(),
            )
        })
        .collect()
}
