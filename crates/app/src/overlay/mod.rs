//! A transparent, borderless child window over the main window, for anything
//! that must float above the Ghostty terminals (their native views sit above
//! everything GPUI draws in the main window).
//!
//! The overlay window's root is a GPUI Kit `Root`, so inside it GPUI Kit's
//! window-level UI works as usual: `window.push_notification`,
//! `window.open_dialog`, `window.open_sheet`. Floating pieces of your own
//! (context menus, the palette) go in as layers. Its coordinates are the main
//! window's content coordinates, so anything anchored to an element's bounds
//! in the main window lines up as-is.
//!
//! ```ignore
//! let overlay = overlay::get_or_open(window, cx)?;          // window: the main window
//! overlay.update(cx, |overlay, cx| overlay.update_window(cx, |window, cx| {
//!     window.push_notification("Saved", cx);
//! }));
//! let id = overlay.update(cx, |overlay, cx| overlay.push_layer(menu_view, Capture::Modal, cx));
//! overlay.update(cx, |overlay, cx| overlay.remove_layer(id, cx));
//! ```
//!
//! Mouse: the overlay takes the pointer only where it is over something the
//! overlay drew that occludes (toasts, menus, dialogs: GPUI Kit's floating
//! surfaces all `occlude()`); everywhere else clicks fall through to the main
//! window. While a `Capture::Modal` layer, a dialog or a sheet is open it takes
//! every click and becomes the key window, so menus and the palette get the
//! keyboard; focus goes back to the main window when the last one closes.
//! See docs/decisions/0008-terminal-area-and-overlay.md.

mod native;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::base::Root;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::*;

pub use native::ns_window;
#[cfg(debug_assertions)]
pub use native::{app_is_active, subviews as native_subviews};

/// How a layer treats the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// Takes every click and the keyboard while open: menus, palette, prompts.
    Modal,
    /// Takes the pointer only over what it draws (and only where that
    /// occludes): toasts, badges, hovering previews.
    Passive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

struct Layer {
    id: LayerId,
    view: AnyView,
    capture: Capture,
}

/// One overlay per main window. Get it with [`get_or_open`].
pub struct Overlay {
    /// The main window's `NSWindow`, which gets key status back.
    parent_ns: objc2::rc::Retained<objc2_app_kit::NSWindow>,
    window: WindowHandle<Root>,
    /// The overlay's own `NSWindow`, kept so capture changes never need an
    /// update of the overlay window (which fails while it is mid-update).
    ns: objc2::rc::Retained<objc2_app_kit::NSWindow>,
    /// GPUI's view in the overlay (`GPUIView`). It must be the overlay's first
    /// responder, or AppKit never hands it key events (0008).
    view: objc2::rc::Retained<objc2_app_kit::NSView>,
    content: Entity<OverlayContent>,
    next_layer: u64,
    /// Whether a GPUI Kit dialog or sheet is open in the overlay. Recorded
    /// wherever the overlay window is at hand (after each `update_window`,
    /// on every overlay frame), so deciding capture never has to reach into
    /// the window.
    kit_modal: Rc<Cell<bool>>,
    /// Whether the pointer was over something the overlay drew, last time
    /// that could be checked.
    over_content: bool,
    /// Whether the overlay window currently takes mouse events.
    capturing: bool,
    /// Whether the overlay should be the key window (and owes key status back
    /// to the parent once nothing is modal).
    keyed: bool,
    /// A key hand-off task is queued; it applies whatever `keyed` is when it
    /// runs, so a menu closing and a dialog opening in one go make a single
    /// request instead of two racing ones.
    key_request_pending: bool,
    last_pointer: Option<Point<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct Overlays(HashMap<WindowId, Entity<Overlay>>);

impl Global for Overlays {}

/// The overlay for the main window `window`, opening it on first use.
pub fn get_or_open(window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Overlay>> {
    let id = window.window_handle().window_id();
    if let Some(overlay) = cx.try_global::<Overlays>().and_then(|o| o.0.get(&id)) {
        return Ok(overlay.clone());
    }
    let overlay = Overlay::open(window, cx)?;
    cx.default_global::<Overlays>()
        .0
        .insert(id, overlay.clone());
    Ok(overlay)
}

/// The overlay for `window` if one is open.
pub fn get(window: &Window, cx: &App) -> Option<Entity<Overlay>> {
    let id = window.window_handle().window_id();
    cx.try_global::<Overlays>()?.0.get(&id).cloned()
}

/// Feeds the main window's pointer position to its overlay, so the overlay
/// starts taking the mouse when the pointer moves onto one of its toasts.
/// Called from a main-window mouse-move listener (the terminal area installs
/// one; the pointer over a terminal still reaches GPUI, as Ghostty's views
/// don't hit-test).
pub fn track_pointer(position: Point<Pixels>, window: &Window, cx: &mut App) {
    if let Some(overlay) = get(window, cx) {
        overlay.update(cx, |overlay, cx| overlay.sync_capture(Some(position), cx));
    }
}

/// The window that owns keyboard input for the main window `window` right
/// now: its overlay while a modal layer, dialog or sheet is open there (that
/// is the window AppKit makes key), otherwise the main window itself.
pub fn key_target(window: &Window, cx: &mut App) -> AnyWindowHandle {
    match get(window, cx) {
        Some(overlay) if overlay.read(cx).is_modal(cx) => overlay.read(cx).window_handle(),
        _ => window.window_handle(),
    }
}

/// Sends `keystroke` to [`key_target`], as AppKit would deliver a key press
/// to the key window. `window` is the main window. For scripts and tests:
/// nothing goes through the OS. Returns whether something handled it.
pub fn dispatch_keystroke(keystroke: Keystroke, window: &mut Window, cx: &mut App) -> bool {
    let target = key_target(window, cx);
    if target == window.window_handle() {
        return window.dispatch_keystroke(keystroke, cx);
    }
    cx.update_window(target, |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx)
    })
    .unwrap_or_else(|error| {
        tracing::warn!("overlay: key target window gone: {error:#}");
        false
    })
}

/// Dispatches `action` from the focus of [`key_target`]. `window` is the
/// main window.
pub fn dispatch_action(action: Box<dyn Action>, window: &mut Window, cx: &mut App) {
    let target = key_target(window, cx);
    if target == window.window_handle() {
        window.dispatch_action(action, cx);
        return;
    }
    if let Err(error) = cx.update_window(target, |_, window, cx| window.dispatch_action(action, cx))
    {
        tracing::warn!("overlay: key target window gone: {error:#}");
    }
}

/// One key press per character of `text`, as a keyboard sends them (for
/// scripts typing through [`dispatch_keystroke`]).
pub fn keystrokes(text: &str) -> Vec<Keystroke> {
    text.chars()
        .map(|ch| {
            let (key, key_char) = match ch {
                ' ' => ("space".to_owned(), Some(" ".to_owned())),
                '\n' => ("enter".to_owned(), None),
                ch => (ch.to_lowercase().to_string(), Some(ch.to_string())),
            };
            Keystroke {
                modifiers: Modifiers {
                    shift: ch.is_uppercase(),
                    ..Modifiers::default()
                },
                key,
                key_char,
            }
        })
        .collect()
}

impl Overlay {
    fn open(parent: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let parent_ns = ns_window(parent).ok_or_else(|| anyhow::anyhow!("no parent NSWindow"))?;
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(parent.bounds())),
            // The bounds are relative to this display, the main one if unset.
            display_id: parent.display(cx).map(|display| display.id()),
            titlebar: None,
            focus: false,
            // Shown by `attach_later`, once it is borderless and click-through.
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..WindowOptions::default()
        };
        let mut content = None;
        let kit_modal = Rc::new(Cell::new(false));
        let window = cx.open_window(options, |window, cx| {
            let view = cx.new(|_| OverlayContent {
                kit_modal: kit_modal.clone(),
                ..OverlayContent::default()
            });
            content = Some(view.clone());
            // Root paints the theme background over the whole window; the
            // overlay must stay see-through.
            cx.new(|cx| Root::new(view, window, cx).bg(transparent_black()))
        })?;
        let content = content.ok_or_else(|| anyhow::anyhow!("overlay built no content"))?;
        let (child, view) = cx.update_window(window.into(), |_, window, _| {
            (ns_window(window), native::gpui_view(window))
        })?;
        let child = child.ok_or_else(|| anyhow::anyhow!("no overlay NSWindow"))?;
        let view = view.ok_or_else(|| anyhow::anyhow!("no overlay NSView"))?;
        // Click-through from the start. Only `sync_capture` changes it after
        // this; the deferred `attach` must not (it used to, and undid the
        // capture of the menu or dialog the overlay was opened for).
        native::set_ignores_mouse(&child, true);
        Self::attach_later(parent_ns.clone(), child.clone(), view.clone(), cx);

        let overlay = cx.new(|cx| {
            let subscriptions =
                vec![
                    cx.observe_window_bounds(parent, |this: &mut Self, parent, cx| {
                        this.match_parent_frame(parent, cx);
                    }),
                ];
            Self {
                parent_ns,
                window,
                ns: child,
                view,
                content: content.clone(),
                next_layer: 0,
                kit_modal,
                over_content: false,
                capturing: false,
                keyed: false,
                key_request_pending: false,
                last_pointer: None,
                _subscriptions: subscriptions,
            }
        });
        let weak = overlay.downgrade();
        content.update(cx, |content, _| content.overlay = Some(weak));
        // Tooltips over a terminal would be hidden under it, so the main
        // window's all show here (the coordinates match).
        let tooltips = cx.update_window(window.into(), |_, window, cx| {
            window.own_tooltip_overlay(cx)
        })?;
        parent.host_tooltips_in(Some(tooltips), cx);
        // GPUI Kit dialogs and sheets open and close inside the overlay's Root
        // (including on their own, e.g. on Escape), which changes whether the
        // overlay is modal.
        if let Ok(root) = window.entity(cx) {
            overlay.update(cx, |overlay, cx| {
                let observe = cx.observe(&root, |this, _, cx| this.sync_capture(None, cx));
                overlay._subscriptions.push(observe);
            });
        }
        Ok(overlay)
    }

    pub fn window_handle(&self) -> AnyWindowHandle {
        self.window.into()
    }

    /// Runs `f` inside the overlay window, e.g. to push a GPUI Kit notification
    /// or open a dialog there. Returns `None` if the overlay window is gone.
    pub fn update_window<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Window, &mut App) -> R,
    ) -> Option<R> {
        let kit_modal = self.kit_modal.clone();
        let result = cx
            .update_window(self.window.into(), |_, window, cx| {
                let result = f(window, cx);
                kit_modal.set(window.has_active_dialog(cx) || window.has_active_sheet(cx));
                result
            })
            .ok();
        // A dialog or sheet may have opened or closed.
        self.sync_capture(None, cx);
        result
    }

    /// Adds a layer drawn over the whole overlay, above earlier layers. The
    /// view positions itself (e.g. `anchored()` at a main-window point).
    pub fn push_layer(
        &mut self,
        view: impl Into<AnyView>,
        capture: Capture,
        cx: &mut Context<Self>,
    ) -> LayerId {
        let id = LayerId(self.next_layer);
        self.next_layer += 1;
        let view = view.into();
        self.content.update(cx, |content, cx| {
            content.layers.push(Layer { id, view, capture });
            cx.notify();
        });
        self.sync_capture(None, cx);
        id
    }

    pub fn remove_layer(&mut self, id: LayerId, cx: &mut Context<Self>) {
        self.content.update(cx, |content, cx| {
            content.layers.retain(|layer| layer.id != id);
            cx.notify();
        });
        self.sync_capture(None, cx);
    }

    pub fn has_layer(&self, id: LayerId, cx: &App) -> bool {
        self.content
            .read(cx)
            .layers
            .iter()
            .any(|layer| layer.id == id)
    }

    /// Whether the overlay's last frame was drawn during a drag, so it shows
    /// the drag's preview until it draws again.
    pub fn drew_drag(&self, cx: &App) -> bool {
        self.content.read(cx).drew_drag
    }

    /// Whether something modal is open in the overlay: a `Capture::Modal`
    /// layer, or a Kit dialog or sheet. Reads only plain state, so it gives
    /// the same answer whatever window is being updated.
    pub fn is_modal(&self, cx: &App) -> bool {
        self.kit_modal.get()
            || self
                .content
                .read(cx)
                .layers
                .iter()
                .any(|layer| layer.capture == Capture::Modal)
    }

    /// Whether the overlay's `NSWindow` takes mouse events right now, read
    /// from AppKit rather than from our own flag (for logs and tests).
    pub fn is_capturing(&self) -> bool {
        !native::ignores_mouse(&self.ns)
    }

    /// Whether the overlay's `NSWindow` is the key window.
    pub fn is_key(&self) -> bool {
        native::is_key(&self.ns)
    }

    /// The overlay's AppKit side, for checking real typing. Its methods
    /// must run outside GPUI updates, so get it in one and use it after.
    pub fn native_keyboard(&self) -> NativeKeyboard {
        NativeKeyboard {
            ns: self.ns.clone(),
            view: self.view.clone(),
        }
    }

    fn match_parent_frame(&mut self, parent: &mut Window, cx: &mut Context<Self>) {
        let Some(parent_ns) = ns_window(parent) else {
            return;
        };
        let child = match cx.update_window(self.window.into(), |_, window, _| ns_window(window)) {
            Ok(Some(child)) => child,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!("overlay: window gone while following the parent: {error:#}");
                return;
            }
        };
        Self::set_frame_later(parent_ns, child, cx);
    }

    /// Makes the new overlay a borderless, click-through child of the parent,
    /// sizes it and shows it, from a foreground task. Like resizing, changing
    /// its style mask resizes its content view, and AppKit calls GPUI's resize
    /// callback synchronously; inside a GPUI update (the overlay often opens
    /// lazily, mid-event) that callback finds the app borrowed.
    fn attach_later(
        parent: objc2::rc::Retained<objc2_app_kit::NSWindow>,
        child: objc2::rc::Retained<objc2_app_kit::NSWindow>,
        view: objc2::rc::Retained<objc2_app_kit::NSView>,
        cx: &mut App,
    ) {
        cx.spawn(async move |_| {
            let before = native::ignores_mouse(&child);
            native::attach(&parent, &child, &view);
            let attached = native::ignores_mouse(&child);
            native::match_content_frame(&parent, &child);
            native::show(&child);
            tracing::debug!(
                "overlay: attached; click-through before={before} after-attach={attached} after-show={}; first responder {}",
                native::ignores_mouse(&child),
                native::first_responder_class(&child)
            );
        })
        .detach();
    }

    /// Resizes the overlay to the parent's content rect from a foreground
    /// task, i.e. after the current GPUI update has ended. Doing it inline
    /// (from `observe_window_bounds`, mid-update) makes AppKit re-enter GPUI's
    /// resize callback for the overlay while the app is borrowed, and that
    /// resize is lost.
    fn set_frame_later(
        parent: objc2::rc::Retained<objc2_app_kit::NSWindow>,
        child: objc2::rc::Retained<objc2_app_kit::NSWindow>,
        cx: &mut App,
    ) {
        cx.spawn(async move |_| native::match_content_frame(&parent, &child))
            .detach();
    }

    /// Hands key status to the overlay (`keyed`) or back to the main window,
    /// from a foreground task, as GPUI's own `activate_window` does, but
    /// straight on the `NSWindow`s: going through `update_window` fails while
    /// that window is mid-update (a sync from the main window's mouse-move
    /// handler), and the hand-off was then lost.
    ///
    /// One task at a time, reading `keyed` when it runs. A menu closing and
    /// a dialog opening used to queue "main window" then "overlay" as two
    /// tasks, flicking key status (and Ghostty's focus) for nothing.
    fn request_key(&mut self, cx: &mut Context<Self>) {
        if self.key_request_pending {
            tracing::debug!(
                "overlay: key request already queued; it will apply keyed={}",
                self.keyed
            );
            return;
        }
        self.key_request_pending = true;
        cx.spawn(async move |this, cx| {
            let Ok((keyed, ns, parent_ns, view, window)) = this.update(cx, |this, _| {
                this.key_request_pending = false;
                (
                    this.keyed,
                    this.ns.clone(),
                    this.parent_ns.clone(),
                    this.view.clone(),
                    this.window,
                )
            }) else {
                return;
            };
            let (target, what) = if keyed {
                (&ns, "overlay")
            } else {
                (&parent_ns, "main window")
            };
            if !native::is_key(target) {
                native::make_key(target);
            }
            if keyed && !native::is_first_responder(&ns, &view) {
                tracing::warn!(
                    "overlay: first responder was {}, not GPUI's view; restoring it",
                    native::first_responder_class(&ns)
                );
                native::make_first_responder(&ns, &view);
            }
            let focused = cx
                .update_window(window.into(), |_, window, cx| window.focused(cx))
                .ok()
                .flatten();
            tracing::debug!(
                "overlay: asked {what} to become key; key now: {}, app active: {}; overlay {:?}, focused element {focused:?}",
                native::is_key(target),
                native::app_is_active(),
                KeyboardPath::read(&ns, &view),
            );
        })
        .detach();
    }

    /// Recomputes whether the overlay takes the mouse and the keyboard.
    /// `pointer` is the latest pointer position in window coordinates, if known.
    fn sync_capture(&mut self, pointer: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        if pointer.is_some() {
            self.last_pointer = pointer;
        }
        // Kit dialogs close on their own (Escape, Cancel, ×) without telling
        // us, so refresh the recorded state whenever the overlay window can be
        // read. While it is mid-update it can't; then the last recorded state
        // stands, rather than a failed read turning into "not modal".
        match cx.update_window(self.window.into(), |_, window, cx| {
            window.has_active_dialog(cx) || window.has_active_sheet(cx)
        }) {
            Ok(open) => self.kit_modal.set(open),
            Err(error) => tracing::debug!(
                "overlay: dialog state unreadable, keeping kit_modal={}: {error:#}",
                self.kit_modal.get()
            ),
        }
        let modal = self.is_modal(cx);
        if let Some(pointer) = self.last_pointer {
            let background = self.content.read(cx).background.clone();
            // The hit test needs the overlay window; while it is mid-update
            // keep the last answer rather than guessing "not over content".
            match cx.update_window(self.window.into(), |_, window, _| {
                background.is_some_and(|hitbox| !hitbox.is_hovered_at(pointer, window))
            }) {
                Ok(over) => self.over_content = over,
                Err(error) => tracing::debug!(
                    "overlay: hit test skipped, keeping over_content={}: {error:#}",
                    self.over_content
                ),
            }
        }
        let capture = modal || self.over_content;
        // Compare with AppKit's actual state, not only our flag, so anything
        // else flipping it is corrected on the next sync.
        let actual = !native::ignores_mouse(&self.ns);
        if capture != self.capturing || capture != actual {
            if capture == self.capturing {
                tracing::warn!(
                    "overlay: NSWindow capture was {actual}, expected {capture}; correcting"
                );
            }
            tracing::debug!(
                "overlay: mouse capture {} (modal={modal}, over_content={}, pointer={:?})",
                if capture { "on" } else { "off" },
                self.over_content,
                self.last_pointer,
            );
            self.capturing = capture;
            native::set_ignores_mouse(&self.ns, !capture);
        }
        if modal != self.keyed {
            tracing::debug!(
                "overlay: {}",
                if modal {
                    "modal, making the overlay key"
                } else {
                    "nothing modal, handing key status back to the main window"
                }
            );
            self.keyed = modal;
            self.request_key(cx);
        }
    }
}

/// The overlay's `NSWindow` and GPUI view. Every method calls into GPUI
/// synchronously, so use them outside GPUI updates.
pub struct NativeKeyboard {
    ns: objc2::rc::Retained<objc2_app_kit::NSWindow>,
    view: objc2::rc::Retained<objc2_app_kit::NSView>,
}

impl NativeKeyboard {
    /// What real typing in the overlay meets right now.
    pub fn path(&self) -> KeyboardPath {
        KeyboardPath::read(&self.ns, &self.view)
    }

    /// Sends a key-down to the overlay's `NSWindow` as AppKit's event loop
    /// would (`native::send_key_down`): first responder, `keyDown:`, the
    /// text input system. `key` is a GPUI key name (`a`, `space`,
    /// `backspace`, `left`, `enter`, `escape`, ...), optionally with `cmd-`.
    /// Returns false for a key it has no key code for.
    pub fn send_key(&self, key: &str) -> bool {
        let (command, key) = match key.strip_prefix("cmd-") {
            Some(key) => (true, key),
            None => (false, key),
        };
        let Some((characters, key_code)) = native::key_code(key) else {
            return false;
        };
        native::send_key_down(&self.ns, &characters, key_code, command)
    }
}

/// What a real key press in the overlay meets, read from AppKit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardPath {
    pub key: bool,
    /// Class of the overlay's first responder; `GPUIView` when key events
    /// reach GPUI.
    pub first_responder: String,
    /// GPUI's view is first responder.
    pub view_first: bool,
    /// A focused text field registered an input handler for typed text.
    pub input_handler: bool,
}

impl KeyboardPath {
    fn read(ns: &objc2_app_kit::NSWindow, view: &objc2_app_kit::NSView) -> Self {
        Self {
            key: native::is_key(ns),
            first_responder: native::first_responder_class(ns),
            view_first: native::is_first_responder(ns, view),
            input_handler: native::has_input_handler(view),
        }
    }
}

/// The overlay window's content: a see-through background plus the layers.
#[derive(Default)]
struct OverlayContent {
    overlay: Option<WeakEntity<Overlay>>,
    layers: Vec<Layer>,
    /// The bottom-most hitbox, covering the whole window. Where it is hovered
    /// nothing the overlay drew is under the pointer.
    background: Option<Hitbox>,
    /// Shared with `Overlay::kit_modal`.
    kit_modal: Rc<Cell<bool>>,
    /// Whether the last frame was drawn during a drag. GPUI paints the drag
    /// preview into every window that draws then, this one included.
    drew_drag: bool,
}

impl Render for OverlayContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // base's Root doesn't size rems from the theme the way the main
        // window's Kit root does; without this, menus and dialogs here draw
        // `text_sm` from a 16px rem and come out larger than the app's.
        window.set_rem_size(cx.theme().font_size);
        self.drew_drag = cx.has_active_drag();
        let this = cx.entity().downgrade();
        let overlay = self.overlay.clone();
        let kit_modal = self.kit_modal.clone();
        div()
            .size_full()
            .relative()
            .child(
                canvas(
                    move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |_, hitbox, window, cx| {
                        this.update(cx, |this, _| this.background = Some(hitbox))
                            .ok();
                        // A Kit dialog may have closed on its own (Escape,
                        // Cancel, ×); this window is at hand now, so record it.
                        kit_modal.set(window.has_active_dialog(cx) || window.has_active_sheet(cx));
                        // Layers just changed what is under the pointer;
                        // re-decide once this frame is the one hit-tested.
                        if let Some(overlay) = overlay.as_ref().and_then(WeakEntity::upgrade) {
                            cx.defer(move |cx| {
                                overlay.update(cx, |overlay, cx| overlay.sync_capture(None, cx));
                            });
                        }
                        let overlay = overlay.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            if let Some(overlay) = overlay.as_ref().and_then(WeakEntity::upgrade) {
                                // Defer: the overlay reads this window, which is
                                // being updated right now.
                                let position = event.position;
                                cx.defer(move |cx| {
                                    overlay.update(cx, |overlay, cx| {
                                        overlay.sync_capture(Some(position), cx)
                                    });
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(
                self.layers
                    .iter()
                    .map(|layer| div().absolute().size_full().child(layer.view.clone())),
            )
    }
}
