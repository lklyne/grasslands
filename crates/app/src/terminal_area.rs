//! herdr's tab bar and the active tab's split tree of Ghostty terminals.
//! Placeholder until the ghostty-terminals agent fills it.

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

use crate::placeholder::placeholder;
use crate::state::AppState;

pub fn init(_cx: &mut App) {}

pub struct TerminalArea {
    state: Entity<AppState>,
    _observe: Subscription,
}

impl TerminalArea {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _observe: observe,
        }
    }

    /// Ask herdr to focus `pane_id`; the focus ring follows herdr's
    /// `pane.focused` event, not this call.
    pub fn focus_pane(&mut self, pane_id: &str, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.mark_seen(pane_id, cx);
            state.focus_pane(pane_id, cx).detach_and_log_err(cx);
        });
    }
}

impl Render for TerminalArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let note = format!(
            "herdr: {:?} ({})",
            state.connection,
            state
                .herdr_config
                .as_ref()
                .and_then(|config| config.session.as_deref())
                .unwrap_or("default session")
        );
        v_flex()
            .size_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .h(px(32.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().tab_bar),
            )
            .child(placeholder("Terminals", note, cx))
    }
}
