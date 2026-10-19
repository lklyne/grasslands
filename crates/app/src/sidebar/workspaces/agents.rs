//! The agents panel, after herdr's agent panel
//! (`client/shell/agent_sidebar.rs`): every agent on one line, its status
//! glyph with its workspace (and tab, when the workspace has more than one),
//! then its name. "grouped" keeps herdr's workspace order, blocked first
//! within each workspace; "priority" is herdr's attention queue. The sort
//! toggle sits in the panel's title bar.
//!
//! Agents on every connected machine share the list (Local's first in
//! "grouped"); with more than one machine each row ends in its machine's
//! label, and clicking an agent on another machine selects that machine.

use std::cmp::Reverse;

use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{AgentSort, ROW, dim_text, text};
use crate::actions::OpenContextMenu;
use crate::chrome::MenuTarget;
use crate::herdr::{Agent, Snapshot};
use crate::label;
use crate::state::{AppState, Machine, MachineId};
use crate::status::{self, priority};

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

/// Every connected machine's agents in `sort` order, each with its machine.
/// "grouped" keeps the machines' order; "priority" merges them.
fn ordered_everywhere(state: &AppState, sort: AgentSort) -> Vec<(&Machine, &Agent)> {
    let mut agents: Vec<(&Machine, &Agent)> = state
        .live_machines()
        .flat_map(|m| ordered(m.snapshot(), sort).into_iter().map(move |a| (m, a)))
        .collect();
    if sort == AgentSort::Priority {
        agents.sort_by_key(|(_, a)| {
            (
                Reverse(priority(a.agent_status)),
                Reverse(a.state_change_seq),
            )
        });
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

    /// Focuses the agent's pane; herdr's `pane.focus` counts its result as
    /// seen (`done` becomes `idle`).
    pub fn activate(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let machine = self.state.read(cx).selected().clone();
        self.activate_on(&machine, pane_id, window, cx);
    }

    /// [`Self::activate`] on `machine`, selecting it first.
    pub fn activate_on(
        &mut self,
        machine: &MachineId,
        pane_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.select_machine(machine, cx);
        });
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

    /// One agent's row; `show_machine` adds its machine's label.
    fn render_agent(
        &self,
        machine: &Machine,
        agent: &Agent,
        show_machine: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let snapshot = machine.snapshot();
        let selected = state.selected() == machine.id();
        let focused = selected && machine.focused_pane() == Some(agent.pane_id.as_str());
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
        let machine_id = machine.id().clone();
        let menu_machine = machine_id.clone();
        // Pane ids repeat across machines.
        let key = match &machine_id {
            MachineId::Local => agent.pane_id.clone(),
            MachineId::Remote(_) => format!("{machine_id}/{}", agent.pane_id),
        };
        let selector = format!("agent-{key}");
        h_flex()
            .id(SharedString::from(format!("agent-{key}")))
            .debug_selector(move || selector)
            .flex_shrink_0()
            .h(ROW)
            .pl(px(6.))
            .pr_2()
            .gap_1()
            .rounded_sm()
            .cursor_pointer()
            .when(focused, |this| this.bg(theme.sidebar_accent))
            .hover(|this| this.bg(theme.sidebar_accent.opacity(0.6)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.activate_on(&machine_id, &pane_id, window, cx);
            }))
            // A pane's menu acts on the selected machine.
            .when(selected, |this| {
                this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    window.dispatch_action(
                        Box::new(OpenContextMenu {
                            target: MenuTarget::Pane {
                                machine: menu_machine.clone(),
                                pane_id: menu_pane_id.clone(),
                            },
                            position: event.position,
                        }),
                        cx,
                    );
                })
            })
            .child(status::indicator(agent.agent_status, cx))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if focused {
                        theme.foreground
                    } else {
                        dim_text(cx)
                    })
                    .child(place),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(label::agent(snapshot, agent).to_owned()),
            )
            .when(show_machine, |this| {
                this.child(div().flex_1()).child(
                    text(machine.label().to_owned())
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
            })
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
        div().text_sm().child("Agents")
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

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        [
            ("Grouped", AgentSort::Grouped),
            ("Priority", AgentSort::Priority),
        ]
        .into_iter()
        .fold(menu.label("Sort"), |menu, (label, sort)| {
            let this = cx.entity().downgrade();
            menu.item(
                PopupMenuItem::new(label)
                    .checked(self.sort == sort)
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.sort = sort;
                            cx.notify();
                        });
                    }),
            )
        })
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
        let state = self.state.read(cx);
        let show_machine = state.machines().count() > 1;
        let rows: Vec<AnyElement> = ordered_everywhere(state, self.sort)
            .into_iter()
            .map(|(machine, agent)| self.render_agent(machine, agent, show_machine, cx))
            .collect();
        let theme = cx.theme();
        let empty = rows
            .is_empty()
            .then(|| text("No agents").px_2().text_color(theme.muted_foreground));
        v_flex()
            .id("agent-rows")
            .track_focus(&self.focus)
            .size_full()
            // Under the dock's title bar, like the main area's tab strip.
            .border_t_1()
            .border_color(theme.border)
            .p_1()
            .overflow_y_scroll()
            .text_sm()
            .children(rows)
            .children(empty)
    }
}
