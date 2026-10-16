//! The workspace list, modeled on herdr's own sidebar. One row per herdr
//! workspace: label, repo and worktree, tab and pane counts, aggregate agent
//! status, and badges for blocked agents and finished ones nobody has looked
//! at. Each row expands to its agents, blocked first.
//!
//! Click a workspace to focus it (`workspace.focus`; the terminal area
//! follows herdr's focused workspace). Click an agent to focus its pane.
//! Right-click or "…" opens the chrome's workspace menu (rename, new tab,
//! close, close group); × closes through the chrome (`chrome::get`), which
//! asks first when agents are running. The header's + creates a workspace or a worktree.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::Path;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{FocusPane, OpenContextMenu};
use crate::attention::{Attention, agent_name, status_label};
use crate::chrome::{self, HerdrCommand, MenuCommand, MenuTarget};
use crate::herdr::{Agent, AgentStatus, Snapshot, WorkspaceInfo, WorktreeParams};
use crate::state::AppState;

pub fn init(_cx: &mut App) {}

pub struct WorkspaceList {
    state: Entity<AppState>,
    /// Workspaces whose agent rows are folded away. Everything starts open.
    collapsed: HashSet<String>,
    _observe: Subscription,
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

/// A workspace's agents in attention order.
fn agents_of<'a>(snapshot: &'a Snapshot, workspace_id: &str) -> Vec<&'a Agent> {
    let mut agents: Vec<&Agent> = snapshot
        .agents
        .iter()
        .filter(|a| a.workspace_id == workspace_id)
        .collect();
    agents.sort_by(|a, b| by_attention(a, b));
    agents
}

/// Blocked and unseen-done counts over `agents`.
fn needs_you(agents: &[&Agent], attention: &Attention, snapshot: &Snapshot) -> (usize, usize) {
    let blocked = agents
        .iter()
        .filter(|a| a.agent_status == AgentStatus::Blocked)
        .count();
    let unseen = agents
        .iter()
        .filter(|a| attention.is_unseen_done(snapshot, &a.pane_id))
        .count();
    (blocked, unseen)
}

/// "repo" for a main checkout, "repo · checkout-folder" for a linked
/// worktree (with `true`), `None` outside git.
fn worktree_line(info: &WorkspaceInfo) -> Option<(String, bool)> {
    let worktree = info.worktree.as_ref()?;
    if !worktree.is_linked_worktree {
        return Some((worktree.repo_name.clone(), false));
    }
    let checkout = Path::new(&worktree.checkout_path).file_name().map_or_else(
        || worktree.checkout_path.clone(),
        |n| n.to_string_lossy().into_owned(),
    );
    Some((format!("{} · {checkout}", worktree.repo_name), true))
}

fn plural(n: u64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn counts(info: &WorkspaceInfo) -> String {
    format!(
        "{} · {}",
        plural(info.tab_count, "tab"),
        plural(info.pane_count, "pane")
    )
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

fn dot(color: Hsla) -> Div {
    div().flex_shrink_0().size_2().rounded_full().bg(color)
}

impl WorkspaceList {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        script::start(window, cx);
        Self {
            state,
            collapsed: HashSet::new(),
            _observe: observe,
        }
    }

    /// `workspace.focus`; the terminal area follows herdr's focused workspace.
    pub fn activate_workspace(&mut self, workspace_id: &str, cx: &mut Context<Self>) {
        self.state
            .read(cx)
            .focus_workspace(workspace_id, cx)
            .detach_and_log_err(cx);
    }

    /// Focuses the agent's pane and counts its result as seen.
    pub fn activate_agent(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.mark_seen(pane_id, cx));
        window.dispatch_action(
            Box::new(FocusPane {
                pane_id: pane_id.to_owned(),
            }),
            cx,
        );
    }

    pub fn toggle(&mut self, workspace_id: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(workspace_id) {
            self.collapsed.insert(workspace_id.to_owned());
        }
        cx.notify();
    }

    /// A new workspace in the active worktree's folder, focused. herdr names
    /// it; rename it from its menu.
    pub fn new_workspace(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let cwd = state.active_worktree().cloned();
        state.new_workspace(cwd, None, cx).detach_and_log_err(cx);
    }

    /// Asks for a branch name, then `worktree.create` from the focused
    /// workspace's repo. herdr opens the worktree as a new, focused workspace.
    pub fn new_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::prompt::ask_name(
            "New Worktree",
            "Create",
            "",
            move |branch, window, cx| {
                let _ = this.update(cx, |this, cx| this.create_worktree(branch, window, cx));
            },
            window,
            cx,
        );
    }

    /// `worktree.create` on a new `branch` from the focused workspace's repo;
    /// a failure becomes an error toast.
    pub fn create_worktree(&mut self, branch: &str, window: &mut Window, cx: &mut Context<Self>) {
        let branch = branch.trim();
        if branch.is_empty() {
            return;
        }
        let state = self.state.read(cx);
        let params = WorktreeParams {
            workspace_id: state.snapshot.focused_workspace_id.clone(),
            branch: Some(branch.to_owned()),
            focus: true,
            ..Default::default()
        };
        let task = state.create_worktree(params, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = task.await {
                cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(error.to_string()).title("New worktree"),
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_header(
        &self,
        blocked: usize,
        unseen: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let this = cx.entity().downgrade();
        h_flex()
            .id("workspaces-header")
            .flex_shrink_0()
            .h(px(28.))
            .pl_3()
            .pr_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().text_xs().font_semibold().child("WORKSPACES"))
            .when(blocked > 0, |this| {
                this.child(badge(format!("{blocked} blocked"), cx.theme().danger))
            })
            .when(unseen > 0, |this| {
                this.child(badge(format!("{unseen} done"), cx.theme().success))
            })
            .child(
                Button::new("workspaces-new")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New…")
                    .dropdown_menu(move |menu, _, _| {
                        let (a, b) = (this.clone(), this.clone());
                        menu.item(
                            PopupMenuItem::new("New Workspace").on_click(move |_, _, cx| {
                                let _ = a.update(cx, |this, cx| this.new_workspace(cx));
                            }),
                        )
                        .item(
                            PopupMenuItem::new("New Worktree…").on_click(move |_, window, cx| {
                                let _ = b.update(cx, |this, cx| this.new_worktree(window, cx));
                            }),
                        )
                    }),
            )
    }

    fn render_workspace(&self, info: &WorkspaceInfo, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let snapshot = &state.snapshot;
        let id = info.workspace_id.clone();
        let agents = agents_of(snapshot, &id);
        let (blocked, unseen) = needs_you(&agents, &state.attention, snapshot);
        let focused = snapshot.focused_workspace_id.as_deref() == Some(id.as_str());
        let open = !self.collapsed.contains(&id);
        let theme = cx.theme();
        let group = SharedString::from(format!("ws-row-{id}"));

        let chevron = div()
            .flex_shrink_0()
            .w_4()
            .when(!agents.is_empty(), |this| {
                let id = id.clone();
                this.child(
                    div()
                        .id(SharedString::from(format!("ws-toggle-{id}")))
                        .debug_selector(|| format!("ws-toggle-{id}"))
                        .cursor_pointer()
                        .child(
                            gpui_kit::component::Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall()
                            .text_color(theme.muted_foreground),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle(&id, cx);
                        })),
                )
            });

        let detail = worktree_line(info);
        let row_id = id.clone();
        let menu_id = id.clone();
        let more_id = id.clone();
        let close_id = id.clone();
        let row = h_flex()
            .id(SharedString::from(format!("ws-{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("ws-{id}")
            })
            .group(group.clone())
            .pl_1()
            .pr_1()
            .py_1()
            .gap_1()
            .items_start()
            .cursor_pointer()
            .when(focused, |this| this.bg(theme.accent))
            .hover(|this| this.bg(theme.accent.opacity(0.6)))
            .on_click(cx.listener(move |this, _, _, cx| this.activate_workspace(&row_id, cx)))
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                window.dispatch_action(
                    Box::new(OpenContextMenu {
                        target: MenuTarget::Workspace {
                            workspace_id: menu_id.clone(),
                        },
                        position: event.position,
                    }),
                    cx,
                );
            })
            .child(chevron)
            .child(
                div()
                    .pt_1p5()
                    .child(dot(status_color(info.agent_status, cx))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .when(focused, |this| this.font_semibold())
                                    .child(info.label.clone()),
                            )
                            .when(blocked > 0, |this| {
                                this.child(badge(blocked.to_string(), theme.danger))
                            })
                            .when(unseen > 0, |this| {
                                this.child(badge(unseen.to_string(), theme.success))
                            }),
                    )
                    .when_some(detail, |this, (line, linked)| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(div().truncate().child(line))
                                .when(linked, |this| {
                                    this.child(
                                        div()
                                            .flex_shrink_0()
                                            .px_1()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(theme.border)
                                            .child("worktree"),
                                    )
                                }),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(counts(info)),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .invisible()
                    .group_hover(group, |this| this.visible())
                    .child(
                        Button::new(SharedString::from(format!("ws-more-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Ellipsis)
                            .tooltip("Workspace menu")
                            .on_click(move |event, window, cx| {
                                cx.stop_propagation();
                                window.dispatch_action(
                                    Box::new(OpenContextMenu {
                                        target: MenuTarget::Workspace {
                                            workspace_id: more_id.clone(),
                                        },
                                        position: event.position(),
                                    }),
                                    cx,
                                );
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("ws-close-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Close workspace")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                close_workspace(&close_id, window, cx);
                            }),
                    ),
            );

        let agent_rows = open.then(|| {
            agents
                .iter()
                .map(|agent| self.render_agent(agent, cx))
                .collect::<Vec<_>>()
        });
        v_flex()
            .child(row)
            .children(agent_rows.into_iter().flatten())
            .into_any_element()
    }

    fn render_agent(&self, agent: &Agent, cx: &Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let focused = state.focused_pane.as_deref() == Some(agent.pane_id.as_str());
        let unseen = state
            .attention
            .is_unseen_done(&state.snapshot, &agent.pane_id);
        let color = status_color(agent.agent_status, cx);
        let pane_id = agent.pane_id.clone();
        let menu_pane_id = agent.pane_id.clone();
        h_flex()
            .id(SharedString::from(format!("agent-{}", agent.pane_id)))
            .debug_selector({
                let pane_id = agent.pane_id.clone();
                move || format!("agent-{pane_id}")
            })
            .pl(px(28.))
            .pr_3()
            .py_0p5()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .when(focused, |this| this.bg(theme.accent.opacity(0.6)))
            .hover(|this| this.bg(theme.accent.opacity(0.6)))
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
            .child(dot(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .child(agent_name(agent).to_owned()),
            )
            .child(match agent.agent_status {
                AgentStatus::Blocked => badge("blocked", color),
                AgentStatus::Done if unseen => badge("done", color),
                status => div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status_label(status)),
            })
            .into_any_element()
    }
}

/// Closes through the chrome, which asks first when agents are running there.
pub(super) fn close_workspace(workspace_id: &str, window: &mut Window, cx: &mut App) {
    let Some(chrome) = chrome::get(window, cx) else {
        return;
    };
    let command = HerdrCommand::CloseWorkspace {
        workspace_id: workspace_id.to_owned(),
        close_group: false,
    };
    chrome.update(cx, |chrome, cx| chrome.run(command, window, cx));
}

/// The chrome's rename prompt for a workspace.
pub(super) fn rename_workspace(workspace_id: &str, window: &mut Window, cx: &mut App) {
    let Some(chrome) = chrome::get(window, cx) else {
        return;
    };
    let command = MenuCommand::Rename(MenuTarget::Workspace {
        workspace_id: workspace_id.to_owned(),
    });
    chrome.update(cx, |chrome, cx| {
        chrome.run_menu_command(command, window, cx)
    });
}

impl Render for WorkspaceList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let snapshot = state.snapshot.clone();
        let all: Vec<&Agent> = snapshot.agents.iter().collect();
        let (blocked, unseen) = needs_you(&all, &state.attention, &snapshot);
        let rows: Vec<AnyElement> = snapshot
            .workspaces
            .iter()
            .map(|w| self.render_workspace(&w.info, cx))
            .collect();
        let empty = rows.is_empty();
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .child(self.render_header(blocked, unseen, cx))
            .child(
                v_flex()
                    .id("workspace-rows")
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
                                .text_color(muted)
                                .child("No workspaces"),
                        )
                    }),
            )
    }
}

mod script;
#[cfg(test)]
mod tests;
