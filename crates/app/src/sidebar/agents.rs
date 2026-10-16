//! The agent list: name, workspace and status, blocked first. Click focuses
//! the agent's pane via `FocusPane`. Placeholder until the agents agent fills it.

use gpui_kit::*;

use crate::placeholder::placeholder;
use crate::state::AppState;

pub fn init(_cx: &mut App) {}

pub struct AgentList {
    state: Entity<AppState>,
    _observe: Subscription,
}

impl AgentList {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _observe: observe,
        }
    }
}

impl Render for AgentList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.state.read(cx).snapshot.agents.len();
        placeholder("Agents", format!("{count} agents"), cx)
    }
}
