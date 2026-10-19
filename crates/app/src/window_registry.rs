//! Per-main-window services (workspace, chrome, overlay) for code that only
//! has the `Window`. One global per handle type, keyed by window id; each
//! module wraps it in its own `get`.

use std::collections::HashMap;

use gpui_kit::*;

struct Registry<H>(HashMap<WindowId, H>);

impl<H> Default for Registry<H> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<H: 'static> Global for Registry<H> {}

/// Registers `handle` for `window`, replacing any earlier one.
pub fn insert<H: 'static>(window: &Window, handle: H, cx: &mut App) {
    let id = window.window_handle().window_id();
    cx.default_global::<Registry<H>>().0.insert(id, handle);
}

/// The handle registered for `window`, if any.
pub fn get<H: Clone + 'static>(window: &Window, cx: &App) -> Option<H> {
    let id = window.window_handle().window_id();
    cx.try_global::<Registry<H>>()?.0.get(&id).cloned()
}

/// The entity registered (weakly) for `window`, if it is still alive.
pub fn upgrade<T: 'static>(window: &Window, cx: &App) -> Option<Entity<T>> {
    get::<WeakEntity<T>>(window, cx)?.upgrade()
}
