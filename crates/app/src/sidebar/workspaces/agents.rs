//! The agents panel, after herdr's agent panel
//! (`client/shell/agent_sidebar.rs`): every agent as two lines, its status
//! glyph with its workspace (and tab, when the workspace has more than one),
//! then its name. "grouped" keeps herdr's workspace order, blocked first
//! within each workspace; "priority" is herdr's attention queue. The sort
//! toggle sits in the panel's title bar.

use std::cmp::Reverse;

use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{AgentSort, LINE, TEXT, dim_text, glyph, priority, status_color, text};
use crate::actions::OpenContextMenu;
use crate::chrome::MenuTarget;
use crate::herdr::{Agent, AgentStatus, Snapshot};
use crate::state::AppState;

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

pub struct AgentList {
    state: Entity<AppState>,
    pub(super) sort: AgentSort,
    focus: FocusHandle,
    _observe: Subscription,
}

impl AgentList {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self {
            _observe: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            sort: AgentSort::Grouped,
            focus: cx.focus_handle(),
        }
    }

    /// Focuses the agent's pane and counts its result as seen.
    pub fn activate(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.mark_seen(pane_id, cx));
        window.dispatch_action(
            Box::new(crate::actions::FocusPane {
                pane_id: pane_id.to_owned(),
            }),
            cx,
        );
    }

    pub fn toggle_sort(&mut self, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            AgentSort::Grouped => AgentSort::Priority,
            AgentSort::Priority => AgentSort::Grouped,
        };
        cx.notify();
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
                this.activate(&pane_id, window, cx);
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
                    .child(text(glyph(status)).text_color(status_color(status, cx)))
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
                        AgentStatus::Blocked => {
                            this.child(Tag::danger().outline().xsmall().child("blocked"))
                        }
                        AgentStatus::Done => {
                            this.child(Tag::info().outline().xsmall().child("done"))
                        }
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

impl EventEmitter<PanelEvent> for AgentList {}

impl Focusable for AgentList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for AgentList {
    fn panel_name(&self) -> &'static str {
        "AgentsPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for AgentList {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().text_xs().child("Agents")
    }

    fn toolbar_buttons(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<Vec<Button>> {
        let label = match self.sort {
            AgentSort::Grouped => "Grouped",
            AgentSort::Priority => "Priority",
        };
        Some(vec![
            Button::new("agents-sort")
                .label(label)
                .tooltip("Sort agents")
                .debug_selector(|| "agents-sort".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_sort(cx))),
        ])
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for AgentList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.state.read(cx).snapshot.clone();
        let rows: Vec<AnyElement> = ordered(&snapshot, self.sort)
            .into_iter()
            .map(|agent| self.render_agent(agent, &snapshot, cx))
            .collect();
        let theme = cx.theme();
        let empty = rows
            .is_empty()
            .then(|| text(" no agents").text_color(theme.muted_foreground));
        v_flex()
            .id("agent-rows")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .font_family(theme.mono_font_family.clone())
            .text_size(TEXT)
            .line_height(LINE)
            .children(rows)
            .children(empty)
    }
}
