//! AppKit glue: moves the Ghostty views from inside GPUI's view to below it,
//! and reads the view tree and key-window state back for the script.
//!
//! GPUI's macOS window is `contentView` → `GPUIView` (layer-backed, one
//! `CAMetalLayer`). gpui-libghostty adds each `GpuiGhosttyView` as a subview of
//! `GPUIView`, so its layer composites above everything GPUI draws. Moving it
//! to `contentView`, ordered below `GPUIView`, puts it behind GPUI's layer;
//! with a transparent window GPUI's layer is non-opaque and cleared to alpha 0,
//! so the terminal shows wherever GPUI paints nothing.

use gpui_kit::Window;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSColor, NSView, NSWindow, NSWindowOrderingMode};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const TERMINAL_VIEW_CLASS: &str = "GpuiGhosttyView";

/// GPUI's own view (`GPUIView`), the one its Metal layer belongs to.
pub fn gpui_view(window: &Window) -> Option<Retained<NSView>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live `GPUIView`; we only use it on the main
    // thread while the window is open.
    unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
}

pub fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    gpui_view(window)?.window()
}

pub fn window_number(window: &Window) -> Option<isize> {
    ns_window(window).map(|window| window.windowNumber())
}

fn is_terminal_view(view: &NSView) -> bool {
    view.class().name().to_str() == Ok(TERMINAL_VIEW_CLASS)
}

/// Moves every Ghostty view out of `GPUIView` into its superview, ordered
/// directly below `GPUIView`. Returns how many moved. Call after each
/// `Terminal::spawn` (the shim adds the view synchronously).
///
/// The shim keeps `GPUIView` as the surface's "parent" and flips y against
/// its bounds. `GPUIView` fills `contentView` (autoresizing width and height),
/// so the same frame is correct in `contentView`'s coordinates.
pub fn lower_terminal_views(window: &Window) -> usize {
    let Some(gpui) = gpui_view(window) else {
        return 0;
    };
    // SAFETY: AppKit view-tree reads on the main thread.
    let Some(content) = (unsafe { gpui.superview() }) else {
        return 0;
    };
    let terminals: Vec<Retained<NSView>> = gpui
        .subviews()
        .iter()
        .filter(|view| is_terminal_view(view))
        .collect();
    for view in &terminals {
        view.removeFromSuperview();
        content.addSubview_positioned_relativeTo(view, NSWindowOrderingMode::Below, Some(&gpui));
    }
    terminals.len()
}

/// Paints the window's own background with an opaque color. A transparent
/// GPUI window shows the desktop wherever neither GPUI nor a Ghostty view has
/// drawn, e.g. a strip a resizing terminal hasn't redrawn yet. This makes that
/// strip the terminal background instead. GPUI only sets the background color
/// in `set_background_appearance`, so this sticks.
pub fn set_backdrop(window: &Window, red: f64, green: f64, blue: f64) {
    if let Some(window) = ns_window(window) {
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, 1.0);
        window.setBackgroundColor(Some(&color));
    }
}

/// One terminal view's frame in top-left window coordinates, points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub hidden: bool,
}

/// Where the Ghostty views are now: under `contentView` (`below`) and still
/// inside `GPUIView` (`above`).
pub struct Placement {
    pub below: Vec<ViewFrame>,
    pub above: usize,
    /// `contentView`'s subviews, bottom to top, by class name.
    pub order: Vec<String>,
}

pub fn placement(window: &Window) -> Option<Placement> {
    let gpui = gpui_view(window)?;
    // SAFETY: AppKit view-tree reads on the main thread.
    let content = unsafe { gpui.superview() }?;
    let height = content.bounds().size.height;
    let subviews = content.subviews();
    let below = subviews
        .iter()
        .filter(|view| is_terminal_view(view))
        .map(|view| {
            let frame = view.frame();
            ViewFrame {
                x: frame.origin.x,
                y: height - frame.origin.y - frame.size.height,
                width: frame.size.width,
                height: frame.size.height,
                hidden: view.isHidden(),
            }
        })
        .collect();
    let above = gpui
        .subviews()
        .iter()
        .filter(|view| is_terminal_view(view))
        .count();
    let order = subviews
        .iter()
        .map(|view| view.class().name().to_string_lossy().into_owned())
        .collect();
    Some(Placement {
        below,
        above,
        order,
    })
}

/// How many windows the app has, and whether `window` is the key window.
pub fn window_state(window: &Window) -> (usize, bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return (0, false);
    };
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    let count = windows.iter().filter(|w| w.isVisible()).count();
    for w in windows.iter() {
        eprintln!(
            "window: {} visible={} title={:?}",
            w.class().name().to_string_lossy(),
            w.isVisible(),
            w.title().to_string()
        );
    }
    let key = ns_window(window).is_some_and(|window| window.isKeyWindow());
    (count, key)
}
