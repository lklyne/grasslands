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
//! See docs/decisions/0006-terminal-area-and-overlay.md.

mod native;

use std::collections::HashMap;

use gpui_kit::base::Root;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

pub use native::{ns_window, subviews as native_subviews};

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
    parent: AnyWindowHandle,
    window: WindowHandle<Root>,
    content: Entity<OverlayContent>,
    next_layer: u64,
    /// Whether the overlay window currently takes mouse events.
    capturing: bool,
    /// Whether we made the overlay the key window (and owe it back to the parent).
    keyed: bool,
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

impl Overlay {
    fn open(parent: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let parent_ns = ns_window(parent).ok_or_else(|| anyhow::anyhow!("no parent NSWindow"))?;
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(parent.bounds())),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..WindowOptions::default()
        };
        let mut content = None;
        let window = cx.open_window(options, |window, cx| {
            let view = cx.new(|_| OverlayContent::default());
            content = Some(view.clone());
            // Root paints the theme background over the whole window; the
            // overlay must stay see-through.
            cx.new(|cx| Root::new(view, window, cx).bg(transparent_black()))
        })?;
        let content = content.ok_or_else(|| anyhow::anyhow!("overlay built no content"))?;
        window.update(cx, |_, window, _| -> anyhow::Result<()> {
            let child = ns_window(window).ok_or_else(|| anyhow::anyhow!("no overlay NSWindow"))?;
            native::attach(&parent_ns, &child);
            Ok(())
        })??;

        let parent_handle = parent.window_handle();
        let overlay = cx.new(|cx| {
            let subscriptions =
                vec![
                    cx.observe_window_bounds(parent, |this: &mut Self, parent, cx| {
                        this.match_parent_frame(parent, cx);
                    }),
                ];
            Self {
                parent: parent_handle,
                window,
                content: content.clone(),
                next_layer: 0,
                capturing: false,
                keyed: false,
                last_pointer: None,
                _subscriptions: subscriptions,
            }
        });
        let weak = overlay.downgrade();
        content.update(cx, |content, _| content.overlay = Some(weak));
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
        let result = cx
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
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

    /// Whether something modal is open in the overlay.
    pub fn is_modal(&self, cx: &mut App) -> bool {
        let layers = self
            .content
            .read(cx)
            .layers
            .iter()
            .any(|layer| layer.capture == Capture::Modal);
        // Through the untyped handle: a typed `update` would lease the Root
        // entity that `has_active_dialog` reads.
        layers
            || cx
                .update_window(self.window.into(), |_, window, cx| {
                    window.has_active_dialog(cx) || window.has_active_sheet(cx)
                })
                .unwrap_or(false)
    }

    /// Whether the overlay currently takes mouse events (for logs and tests).
    pub fn is_capturing(&self) -> bool {
        self.capturing
    }

    fn match_parent_frame(&mut self, parent: &mut Window, cx: &mut Context<Self>) {
        let Some(parent_ns) = ns_window(parent) else {
            return;
        };
        cx.update_window(self.window.into(), |_, window, _| {
            if let Some(child) = ns_window(window) {
                native::match_content_frame(&parent_ns, &child);
            }
        })
        .ok();
    }

    /// Recomputes whether the overlay takes the mouse and the keyboard.
    /// `pointer` is the latest pointer position in window coordinates, if known.
    fn sync_capture(&mut self, pointer: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        if pointer.is_some() {
            self.last_pointer = pointer;
        }
        let modal = self.is_modal(cx);
        let over_content = self.last_pointer.is_some_and(|pointer| {
            let background = self.content.read(cx).background.clone();
            cx.update_window(self.window.into(), |_, window, _| {
                background.is_some_and(|hitbox| !hitbox.is_hovered_at(pointer, window))
            })
            .unwrap_or(false)
        });
        let capture = modal || over_content;
        if capture != self.capturing {
            self.capturing = capture;
            cx.update_window(self.window.into(), |_, window, _| {
                if let Some(ns) = ns_window(window) {
                    native::set_ignores_mouse(&ns, !capture);
                }
            })
            .ok();
        }
        if modal && !self.keyed {
            self.keyed = true;
            cx.update_window(self.window.into(), |_, window, _| window.activate_window())
                .ok();
        } else if !modal && self.keyed {
            self.keyed = false;
            self.parent
                .update(cx, |_, window, _| window.activate_window())
                .ok();
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
}

impl Render for OverlayContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let overlay = self.overlay.clone();
        div()
            .size_full()
            .relative()
            .child(
                canvas(
                    move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |_, hitbox, window, cx| {
                        this.update(cx, |this, _| this.background = Some(hitbox))
                            .ok();
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
