//! The "agents" section under the spaces, after herdr's agent panel
//! (`client/shell/agent_sidebar.rs`): every agent as two lines, its status
//! glyph with its workspace (and tab, when the workspace has more than one),
//! then its name. "grouped" keeps herdr's workspace order, blocked first
//! within each workspace; "priority" is herdr's attention queue. Clicking the
//! section label folds it.

use std::cmp::Reverse;

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{AgentSort, LINE, WorkspaceList, badge, dim_text, glyph, priority, status_color, text};
use crate::actions::OpenContextMenu;
use crate::chrome::MenuTarget;
use crate::herdr::{Agent, AgentStatus, Snapshot};

/// What herdr calls an agent in its panel.
fn agent_label(agent: &Agent) -> &str {
    agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref())
        .unwrap_or("agent")
}

/// The agents in `sort` order.
pub(super) fn ordered(snapshot: &Snapshot, sort: AgentSort) -> Vec<&Agent> {
    let mut agents: Vec<&Agent> = snapshot.agents.iter().collect();
    match sort {
        AgentSort::Grouped => {
            let position = |a: &Agent| {
                snapshot
                    .workspaces
                    .iter()
                    .position(|w| w.info.workspace_id == a.workspace_id)
            };
            agents.sort_by_key(|a| (position(a), Reverse(priority(a.agent_status))));
        }
        AgentSort::Priority => {
            agents.sort_by_key(|a| {
                (
                    Reverse(priority(a.agent_status)),
                    Reverse(a.state_change_seq),
                )
            });
        }
    }
    agents
}

impl WorkspaceList {
    pub(super) fn render_agents(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let open = self.agents_open;
        let header = h_flex()
            .flex_shrink_0()
            .h(LINE)
            .pr_1()
            .child(
                h_flex()
                    .id("agents-toggle")
                    .debug_selector(|| "agents-toggle".into())
                    .cursor_pointer()
                    .child(Self::section_label(" agents", cx))
                    .child(text(if open { " ▾" } else { " ▸" }).text_color(theme.muted_foreground))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_agents(cx))),
            )
            .child(div().flex_1())
            .when(open, |this| {
                this.child(
                    div()
                        .id("agents-sort")
                        .debug_selector(|| "agents-sort".into())
                        .whitespace_nowrap()
                        .font_bold()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.text_color(theme.foreground))
                        .cursor_pointer()
                        .child(match self.agent_sort {
                            AgentSort::Grouped => "grouped",
                            AgentSort::Priority => "priority",
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_agent_sort(cx))),
                )
            });
        let section = v_flex()
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border)
            .child(header);
        if !open {
            return section;
        }
        let rows: Vec<AnyElement> = ordered(snapshot, self.agent_sort)
            .into_iter()
            .map(|agent| self.render_agent(agent, snapshot, cx))
            .collect();
        let empty = rows
            .is_empty()
            .then(|| text(" no agents").text_color(cx.theme().muted_foreground));
        section.flex_1().min_h(LINE * 3.).child(
            v_flex()
                .id("agent-rows")
                .flex_1()
                .min_h_0()
                .pt(LINE)
                .overflow_y_scroll()
                .children(rows)
                .children(empty),
        )
    }

    fn render_agent(
        &self,
        agent: &Agent,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let focused = state.focused_pane.as_deref() == Some(agent.pane_id.as_str());
        let unseen = state.attention.is_unseen_done(snapshot, &agent.pane_id);
        let status = match agent.agent_status {
            AgentStatus::Done if !unseen => AgentStatus::Idle,
            status => status,
        };
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|w| w.info.workspace_id == agent.workspace_id);
        let mut place = workspace.map_or(agent.workspace_id.clone(), |w| w.info.label.clone());
        if let Some(tab) = workspace
            .filter(|w| w.tabs.len() > 1)
            .and_then(|w| w.tabs.iter().find(|t| t.info.tab_id == agent.tab_id))
        {
            place = format!("{place} · {}", tab.info.label);
        }
        let color = status_color(status, cx);
        let (pane_id, menu_pane_id) = (agent.pane_id.clone(), agent.pane_id.clone());
        v_flex()
            .id(SharedString::from(format!("agent-{}", agent.pane_id)))
            .debug_selector({
                let pane_id = agent.pane_id.clone();
                move || format!("agent-{pane_id}")
            })
            .cursor_pointer()
            .when(focused, |this| this.bg(theme.sidebar_accent))
            .hover(|this| this.bg(theme.sidebar_accent.opacity(0.6)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.activate_agent(&pane_id, window, cx);
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
            .child(
                h_flex()
                    .h(LINE)
                    .pr_1()
                    .child(text(" "))
                    .child(text(glyph(status)).text_color(color))
                    .child(text(" "))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_bold()
                            .text_color(if focused {
                                theme.foreground
                            } else {
                                dim_text(cx)
                            })
                            .child(place),
                    )
                    .map(|this| match status {
                        AgentStatus::Blocked => this.child(badge("blocked", color)),
                        AgentStatus::Done => this.child(badge("done", color)),
                        _ => this,
                    }),
            )
            .child(
                h_flex().h(LINE).pr_1().child(text("   ")).child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(agent_label(agent).to_owned()),
                ),
            )
            .into_any_element()
    }
}
