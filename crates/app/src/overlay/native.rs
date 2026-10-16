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

/// Makes `child` a borderless, see-through window glued above `parent`. As a
/// child window it moves, hides and orders with the parent; its level is reset
/// to the parent's because GPUI opened it as a pop-up, which would float above
/// every other app. Its frame is set separately, with [`match_content_frame`],
/// outside any GPUI update.
///
/// Mouse capture is left alone: this runs a tick after the overlay opens, and
/// by then the overlay may already have turned capture on for the menu or
/// dialog it opened for. Setting click-through here is what left real clicks
/// falling through to the terminals (0008).
///
/// GPUI opened it as a non-activating `NSPanel`. The style mask below drops
/// the non-activating bit, so a click on the overlay activates the app like a
/// click on the main window. Panels also hide whenever the app deactivates;
/// that is turned off so the child never leaves its parent's stack.
pub fn attach(parent: &NSWindow, child: &NSWindow) {
    child.setStyleMask(NSWindowStyleMask::Borderless);
    child.setOpaque(false);
    child.setBackgroundColor(Some(&NSColor::clearColor()));
    child.setHasShadow(false);
    // SAFETY: `child` is GPUI's `NSPanel` subclass (WindowKind::PopUp), which
    // has this setter; main thread.
    let _: () = unsafe { objc2::msg_send![child, setHidesOnDeactivate: false] };
    // SAFETY: both windows are live and this runs on the main thread.
    unsafe { parent.addChildWindow_ordered(child, NSWindowOrderingMode::Above) };
    child.setLevel(parent.level());
}

/// Sizes `child` to `parent`'s content rect. AppKit calls GPUI's resize
/// callback for `child` synchronously from here, and that callback borrows
/// the app; call this only when no GPUI update is running (see
/// `Overlay::match_parent_frame`), or the resize is dropped with
/// "RefCell already borrowed".
pub fn match_content_frame(parent: &NSWindow, child: &NSWindow) {
    let content = parent.contentRectForFrameRect(parent.frame());
    child.setFrame_display(content, true);
}

/// Orders the attached overlay in above its parent without making it key.
pub fn show(child: &NSWindow) {
    child.orderFront(None);
}

pub fn set_ignores_mouse(window: &NSWindow, ignores: bool) {
    window.setIgnoresMouseEvents(ignores);
}

pub fn ignores_mouse(window: &NSWindow) -> bool {
    window.ignoresMouseEvents()
}

/// Makes `window` key (and orders it front). Call outside GPUI updates: AppKit
/// calls GPUI's activation callbacks synchronously.
pub fn make_key(window: &NSWindow) {
    window.makeKeyAndOrderFront(None);
}

pub fn is_key(window: &NSWindow) -> bool {
    window.isKeyWindow()
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
