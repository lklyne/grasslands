//! App chrome: command palette, context menus, toasts and confirmations.
//!
//! Ghostty's native views sit above GPUI's layer, so anything that must float
//! over a terminal is drawn in a transparent overlay window (being proven by
//! the ghostty-terminals agent). Callers go through `Chrome`'s methods so the
//! choice of in-window vs overlay rendering stays in this module.

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use crate::state::AppState;

pub fn init(_cx: &mut App) {}

pub struct Chrome {
    _state: Entity<AppState>,
}

impl Chrome {
    pub fn new(state: Entity<AppState>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { _state: state }
    }

    pub fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toast("Command palette: not built yet", window, cx);
    }

    pub fn toast(
        &mut self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(message.into(), cx);
    }
}

impl Render for Chrome {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The in-window part of the chrome is empty; overlays live elsewhere.
        div()
    }
}
