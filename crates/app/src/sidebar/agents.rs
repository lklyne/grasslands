//! The agent list: name, workspace and status, blocked first. Click focuses
//! the agent's pane via `FocusPane`; right-click opens the pane's menu.
//! Badges mark blocked agents and finished ones nobody has looked at yet.

use std::cmp::Ordering;

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{FocusPane, OpenContextMenu};
use crate::attention::{agent_name, status_label};
use crate::chrome::MenuTarget;
use crate::herdr::{Agent, AgentStatus};
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

/// Blocked agents need you, so they come first; then running, finished, idle.
fn rank(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 0,
        AgentStatus::Working => 1,
        AgentStatus::Done => 2,
        AgentStatus::Idle => 3,
        AgentStatus::Unknown => 4,
    }
}

fn by_attention(a: &Agent, b: &Agent) -> Ordering {
    rank(a.agent_status)
        .cmp(&rank(b.agent_status))
        .then_with(|| a.workspace_id.cmp(&b.workspace_id))
        .then_with(|| agent_name(a).cmp(agent_name(b)))
        .then_with(|| a.pane_id.cmp(&b.pane_id))
}

fn status_color(status: AgentStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        AgentStatus::Blocked => theme.danger,
        AgentStatus::Working => theme.warning,
        AgentStatus::Done => theme.success,
        AgentStatus::Idle | AgentStatus::Unknown => theme.muted_foreground,
    }
}

fn badge(label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .px_1p5()
        .rounded_full()
        .text_xs()
        .text_color(color)
        .bg(color.opacity(0.15))
        .child(label.into())
}

impl Render for AgentList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let mut agents: Vec<&Agent> = state.snapshot.agents.iter().collect();
        agents.sort_by(|a, b| by_attention(a, b));

        let blocked = agents
            .iter()
            .filter(|a| a.agent_status == AgentStatus::Blocked)
            .count();
        let unseen = agents
            .iter()
            .filter(|a| state.attention.is_unseen_done(&state.snapshot, &a.pane_id))
            .count();
        let header = h_flex()
            .flex_shrink_0()
            .h(px(28.))
            .px_3()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().text_xs().font_semibold().child("AGENTS"))
            .when(blocked > 0, |this| {
                this.child(badge(format!("{blocked} blocked"), cx.theme().danger))
            })
            .when(unseen > 0, |this| {
                this.child(badge(format!("{unseen} done"), cx.theme().success))
            });

        let rows = agents.iter().map(|agent| {
            let workspace = state
                .snapshot
                .workspace(&agent.workspace_id)
                .map_or_else(|| agent.workspace_id.clone(), |w| w.info.label.clone());
            let focused = state.focused_pane.as_deref() == Some(agent.pane_id.as_str());
            let unseen = state
                .attention
                .is_unseen_done(&state.snapshot, &agent.pane_id);
            let color = status_color(agent.agent_status, cx);
            let pane_id = agent.pane_id.clone();
            let menu_pane_id = agent.pane_id.clone();
            h_flex()
                .id(SharedString::from(format!("agent-{}", agent.pane_id)))
                .px_3()
                .py_1()
                .gap_2()
                .items_center()
                .cursor_pointer()
                .when(focused, |this| this.bg(cx.theme().accent))
                .hover(|this| this.bg(cx.theme().accent.opacity(0.6)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.state
                        .update(cx, |state, cx| state.mark_seen(&pane_id, cx));
                    window.dispatch_action(
                        Box::new(FocusPane {
                            pane_id: pane_id.clone(),
                        }),
                        cx,
                    );
                }))
                .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    window.dispatch_action(
                        Box::new(OpenContextMenu {
                            target: MenuTarget::Pane {
                                pane_id: menu_pane_id.clone(),
                            },
                            position: event.position,
                        }),
                        cx,
                    );
                })
                .child(div().flex_shrink_0().size_2().rounded_full().bg(color))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .child(agent_name(agent).to_owned()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(workspace),
                        ),
                )
                .child(match agent.agent_status {
                    AgentStatus::Blocked => badge("blocked", color),
                    AgentStatus::Done if unseen => badge("done", color),
                    status => div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(status_label(status)),
                })
        });

        let empty = agents.is_empty();
        v_flex().size_full().child(header).child(
            v_flex()
                .id("agent-rows")
                .flex_1()
                .overflow_y_scroll()
                .py_1()
                .children(rows)
                .when(empty, |this| {
                    this.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("No agents running"),
                    )
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::by_attention;
    use crate::herdr::{Agent, AgentStatus};

    fn agent(pane: &str, status: AgentStatus) -> Agent {
        Agent {
            pane_id: pane.into(),
            terminal_id: String::new(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            focused: false,
            agent_status: status,
            revision: 0,
            name: Some(pane.into()),
            agent: None,
            display_agent: None,
            cwd: None,
            foreground_cwd: None,
            title: None,
            completion_seq: None,
            state_change_seq: 0,
            interactive_ready: false,
            launch_pending: false,
            state_labels: Default::default(),
        }
    }

    #[test]
    fn blocked_agents_sort_first() {
        let mut agents = [
            agent("a", AgentStatus::Idle),
            agent("b", AgentStatus::Done),
            agent("c", AgentStatus::Blocked),
            agent("d", AgentStatus::Working),
        ];
        agents.sort_by(by_attention);
        let order: Vec<&str> = agents.iter().map(|a| a.pane_id.as_str()).collect();
        assert_eq!(order, ["c", "d", "b", "a"]);
    }
}
