//! AppKit glue for the overlay window.

use gpui_kit::{Bounds, Pixels, Window, point, px, size};
use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The `NSWindow` behind a GPUI window.
pub fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live content NSView; we only read its window,
    // on the main thread.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    view.window()
}

/// Makes `child` a borderless, see-through, click-through window glued above
/// `parent`'s content area. As a child window it moves, hides and orders with
/// the parent; its level is reset to the parent's because GPUI opened it as a
/// pop-up, which would float above every other app.
pub fn attach(parent: &NSWindow, child: &NSWindow) {
    child.setStyleMask(NSWindowStyleMask::Borderless);
    child.setOpaque(false);
    child.setBackgroundColor(Some(&NSColor::clearColor()));
    child.setHasShadow(false);
    child.setIgnoresMouseEvents(true);
    // SAFETY: both windows are live and this runs on the main thread.
    unsafe { parent.addChildWindow_ordered(child, NSWindowOrderingMode::Above) };
    child.setLevel(parent.level());
    match_content_frame(parent, child);
}

pub fn match_content_frame(parent: &NSWindow, child: &NSWindow) {
    let content = parent.contentRectForFrameRect(parent.frame());
    child.setFrame_display(content, true);
}

pub fn set_ignores_mouse(window: &NSWindow, ignores: bool) {
    window.setIgnoresMouseEvents(ignores);
}

/// Frames (window coordinates, top-left origin) and hidden flags of the
/// native child views of a GPUI window's content view: the Ghostty surfaces.
pub fn subviews(window: &Window) -> Vec<(Bounds<Pixels>, bool)> {
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
            let top = height - frame.origin.y - frame.size.height;
            (
                Bounds::new(
                    point(px(frame.origin.x as f32), px(top as f32)),
                    size(px(frame.size.width as f32), px(frame.size.height as f32)),
                ),
                child.isHidden(),
            )
        })
        .collect()
}

/// Whether this app is the active (frontmost) app. A window can only be key
/// while it is.
pub fn app_is_active() -> bool {
    objc2::MainThreadMarker::new()
        .is_some_and(|mtm| objc2_app_kit::NSApplication::sharedApplication(mtm).isActive())
}
