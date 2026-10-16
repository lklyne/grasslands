//! AppKit glue for the overlay window.

use gpui_kit::{Bounds, Pixels, Window, point, px, size};
use objc2::rc::Retained;
use objc2::{ClassType as _, Message as _};
use objc2_app_kit::{
    NSColor, NSEvent, NSEventModifierFlags, NSEventType, NSView, NSWindow, NSWindowOrderingMode,
    NSWindowStyleMask,
};
use objc2_foundation::{NSNotFound, NSPoint, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The `NSWindow` behind a GPUI window.
pub fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    gpui_view(window)?.window()
}

/// GPUI's own `NSView` in a window (`GPUIView`): the view that turns key
/// events into GPUI input and text into the focused element's input handler.
pub fn gpui_view(window: &Window) -> Option<Retained<NSView>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live NSView, which lives as long as the
    // window; retaining it here keeps it valid; main thread.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    Some(view.retain())
}

/// Class name of `window`'s first responder (`GPUIView` when key events reach
/// GPUI), or `nil`.
pub fn first_responder_class(window: &NSWindow) -> String {
    window.firstResponder().map_or_else(
        || "nil".to_owned(),
        |r| r.class().name().to_string_lossy().into_owned(),
    )
}

/// Whether `view` is `window`'s first responder.
pub fn is_first_responder(window: &NSWindow, view: &NSView) -> bool {
    window
        .firstResponder()
        .is_some_and(|r| std::ptr::eq(&*r, view.as_super()))
}

/// Whether GPUI's `view` has a text input handler, i.e. a focused text field
/// was painted in its window. Asked the way AppKit's text system asks
/// (`selectedRange`, from `NSTextInputClient`); GPUI answers `NSNotFound`
/// when there is no handler, and AppKit then has nowhere to put typed text.
///
/// The handler reads the app, so call this outside any GPUI update.
pub fn has_input_handler(view: &NSView) -> bool {
    // SAFETY: `view` is GPUI's `GPUIView`, which implements `selectedRange`
    // returning an `NSRange`; main thread.
    let range: GpuiRange = unsafe { objc2::msg_send![view, selectedRange] };
    range.location != NSNotFound as usize
}

/// `NSRange` as GPUI's view registers it (`{NSRange=QQ}`); objc2's own
/// `NSRange` encodes as `_NSRange`, and objc2 refuses the mismatch.
#[repr(C)]
struct GpuiRange {
    location: usize,
    length: usize,
}

// SAFETY: two `NSUInteger`s, laid out as AppKit's `NSRange`.
unsafe impl objc2::encode::Encode for GpuiRange {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("NSRange", &[usize::ENCODING, usize::ENCODING]);
}

/// Hands `window` a key-down event the way AppKit's event loop does
/// (`-[NSWindow sendEvent:]`): it goes to the window's first responder,
/// through `keyDown:` and the text input system. This is a method call on
/// our own window, built in-process, not input posted through the OS; it
/// checks the path real typing takes, which GPUI-level dispatch skips.
///
/// GPUI handles it synchronously, so call this outside any GPUI update.
pub fn send_key_down(window: &NSWindow, characters: &str, key_code: u16, command: bool) -> bool {
    let chars = NSString::from_str(characters);
    let flags = if command {
        NSEventModifierFlags::Command
    } else {
        NSEventModifierFlags::empty()
    };
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        flags,
        0.0,
        window.windowNumber(),
        None,
        &chars,
        &chars,
        false,
        key_code,
    );
    let Some(event) = event else {
        return false;
    };
    window.sendEvent(&event);
    true
}

/// The characters and US-layout virtual key code AppKit reports for the
/// GPUI key name `key`: lowercase letters, space and the editing keys.
pub fn key_code(key: &str) -> Option<(String, u16)> {
    const LETTERS: [(char, u16); 26] = [
        ('a', 0),
        ('b', 11),
        ('c', 8),
        ('d', 2),
        ('e', 14),
        ('f', 3),
        ('g', 5),
        ('h', 4),
        ('i', 34),
        ('j', 38),
        ('k', 40),
        ('l', 37),
        ('m', 46),
        ('n', 45),
        ('o', 31),
        ('p', 35),
        ('q', 12),
        ('r', 15),
        ('s', 1),
        ('t', 17),
        ('u', 32),
        ('v', 9),
        ('w', 13),
        ('x', 7),
        ('y', 16),
        ('z', 6),
    ];
    let named = |ch: char, code: u16| Some((ch.to_string(), code));
    match key {
        "space" => named(' ', 49),
        "enter" => named('\r', 36),
        "backspace" => named('\u{7f}', 51),
        "escape" => named('\u{1b}', 53),
        "left" => named('\u{f702}', 123),
        "right" => named('\u{f703}', 124),
        "down" => named('\u{f701}', 125),
        "up" => named('\u{f700}', 126),
        _ => {
            let mut chars = key.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                return None;
            };
            LETTERS
                .iter()
                .find(|(letter, _)| *letter == ch)
                .and_then(|&(letter, code)| named(letter, code))
        }
    }
}

/// Makes `view` (GPUI's view) the first responder of `window` again, so
/// AppKit sends it keyDown, and through it text, rather than to the window,
/// which beeps. Returns whether AppKit accepted it.
pub fn make_first_responder(window: &NSWindow, view: &NSView) -> bool {
    window.makeFirstResponder(Some(view.as_super()))
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
///
/// Changing the style mask swaps the window's frame view, and AppKit makes
/// the window itself first responder. Key events then never reach GPUI's
/// view: `-[NSWindow keyDown:]` finds no responder and beeps, and typing into
/// a dialog does nothing (0008). So GPUI's `view` is made first responder
/// again here.
pub fn attach(parent: &NSWindow, child: &NSWindow, view: &NSView) {
    child.setStyleMask(NSWindowStyleMask::Borderless);
    if !make_first_responder(child, view) {
        tracing::warn!("overlay: GPUI's view refused first responder after attach");
    }
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
