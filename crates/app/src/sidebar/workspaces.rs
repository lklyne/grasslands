//! The workspace list, modeled on herdr's own sidebar. Workspaces are grouped
//! under their repo (main checkout first, linked worktrees nested after it;
//! see `groups`), with repo-less ones in a last section. A workspace row shows
//! its label, git branch (`git`), tab and pane counts, aggregate agent status,
//! and badges for blocked agents and finished ones nobody has looked at. Each
//! row expands to its agents, blocked first; a collapsed repo heading rolls up
//! its workspaces' status and badges.
//!
//! Click a workspace to focus it (`workspace.focus`; the terminal area
//! follows herdr's focused workspace). Click an agent to focus its pane.
//! Right-click or "…" opens the chrome's workspace menu (rename, new tab,
//! close, close group); × closes through the chrome (`chrome::get`), which
//! asks first when agents are running. The header's + creates a workspace or a
//! worktree from the focused workspace; a repo heading's + makes a worktree of
//! that repo.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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
use crate::state::{AppEvent, AppState};
use git::{Checkout, HeadWatcher, Placement};
use groups::{Groups, RepoGroup};

pub fn init(_cx: &mut App) {}

pub struct WorkspaceList {
    state: Entity<AppState>,
    /// Workspaces whose agent rows are folded away. Everything starts open.
    collapsed: HashSet<String>,
    /// Repos (by key) whose workspaces are folded away.
    collapsed_repos: HashSet<String>,
    /// git2's answer per workspace folder (`None`: not in git).
    checkouts: HashMap<PathBuf, Option<Checkout>>,
    /// Folders being read for the first time.
    reading: HashSet<PathBuf>,
    heads: Option<HeadWatcher>,
    _observe: Subscription,
    _subscribe: Subscription,
    _heads_task: Option<Task<()>>,
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

/// The folder a workspace's branch is read from: herdr's checkout path, or
/// the folder of the focused pane in its active tab.
fn checkout_dir(snapshot: &Snapshot, info: &WorkspaceInfo) -> Option<PathBuf> {
    if let Some(worktree) = &info.worktree {
        return Some(PathBuf::from(&worktree.checkout_path));
    }
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|w| w.info.workspace_id == info.workspace_id)?;
    let tab = workspace
        .tabs
        .iter()
        .find(|t| t.info.tab_id == info.active_tab_id)?;
    let pane = snapshot.panes.get(tab.focused_pane_id.as_ref()?)?;
    pane.working_dir().map(PathBuf::from)
}

/// The status that needs you most among `statuses` (blocked over working
/// over done over idle).
fn rolled_up(statuses: impl IntoIterator<Item = AgentStatus>) -> AgentStatus {
    statuses
        .into_iter()
        .min_by_key(|&s| rank(s))
        .unwrap_or(AgentStatus::Unknown)
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
        let subscribe = cx.subscribe(&state, |this, _, event, cx| match event {
            AppEvent::SnapshotReplaced => this.sync_checkouts(cx),
            // The focused pane moved to another checkout; catch up on any
            // branch change the watcher could have missed.
            AppEvent::ActiveWorktreeChanged(_) => {
                let dirs = this.checkouts.keys().cloned().collect();
                this.read_checkouts(dirs, cx);
            }
            _ => {}
        });
        // Tests stay off notify's thread, like `AppState` stays off herdr's.
        let (heads, heads_task) = if cfg!(test) {
            (None, None)
        } else {
            let (tx, rx) = async_channel::unbounded::<PathBuf>();
            let task = cx.spawn(async move |this, cx| {
                while let Ok(git_dir) = rx.recv().await {
                    if this
                        .update(cx, |this, cx| this.head_changed(&git_dir, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let watcher = HeadWatcher::new(tx)
                .inspect_err(|error| tracing::warn!("workspaces: no HEAD watcher: {error}"))
                .ok();
            (watcher, Some(task))
        };
        script::start(window, cx);
        let mut this = Self {
            state,
            collapsed: HashSet::new(),
            collapsed_repos: HashSet::new(),
            checkouts: HashMap::new(),
            reading: HashSet::new(),
            heads,
            _observe: observe,
            _subscribe: subscribe,
            _heads_task: heads_task,
        };
        this.sync_checkouts(cx);
        this
    }

    /// Reads folders of workspaces seen for the first time and forgets ones
    /// no workspace uses any more.
    fn sync_checkouts(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.state.read(cx).snapshot.clone();
        let wanted: HashSet<PathBuf> = snapshot
            .workspaces
            .iter()
            .filter_map(|w| checkout_dir(&snapshot, &w.info))
            .collect();
        let before = self.checkouts.len();
        self.checkouts.retain(|dir, _| wanted.contains(dir));
        if self.checkouts.len() != before {
            self.watch_heads();
        }
        let new: Vec<PathBuf> = wanted
            .into_iter()
            .filter(|d| !self.checkouts.contains_key(d) && !self.reading.contains(d))
            .collect();
        if !new.is_empty() {
            self.reading.extend(new.iter().cloned());
            self.read_checkouts(new, cx);
        }
    }

    /// Re-reads `dirs` with git2 off the main thread.
    fn read_checkouts(&mut self, dirs: Vec<PathBuf>, cx: &mut Context<Self>) {
        if dirs.is_empty() {
            return;
        }
        let read = cx.background_executor().spawn(async move {
            dirs.into_iter()
                .map(|dir| {
                    let checkout = git::read(&dir);
                    (dir, checkout)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = read.await;
            this.update(cx, |this, cx| {
                for (dir, checkout) in results {
                    this.reading.remove(&dir);
                    this.checkouts.insert(dir, checkout);
                }
                this.watch_heads();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn head_changed(&mut self, git_dir: &std::path::Path, cx: &mut Context<Self>) {
        let dirs = self
            .checkouts
            .iter()
            .filter(|(_, c)| c.as_ref().is_some_and(|c| c.git_dir == git_dir))
            .map(|(dir, _)| dir.clone())
            .collect();
        self.read_checkouts(dirs, cx);
    }

    fn watch_heads(&mut self) {
        if let Some(heads) = &mut self.heads {
            heads.set(
                self.checkouts
                    .values()
                    .flatten()
                    .map(|c| c.git_dir.clone())
                    .collect(),
            );
        }
    }

    fn checkout(&self, snapshot: &Snapshot, info: &WorkspaceInfo) -> Option<&Checkout> {
        self.checkouts.get(&checkout_dir(snapshot, info)?)?.as_ref()
    }

    /// herdr's repo info, else what git2 found in the workspace's folder.
    fn placement(&self, snapshot: &Snapshot, info: &WorkspaceInfo) -> Option<Placement> {
        info.worktree
            .as_ref()
            .map(Placement::from_herdr)
            .or_else(|| {
                self.checkout(snapshot, info)
                    .and_then(|c| c.placement.clone())
            })
    }

    fn groups<'a>(&self, snapshot: &'a Snapshot) -> Groups<'a> {
        groups::group(snapshot.workspaces.iter().map(|w| &w.info), |info| {
            self.placement(snapshot, info)
        })
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

    /// Folds or unfolds a repo group, by repo key.
    pub fn toggle_repo(&mut self, repo_key: &str, cx: &mut Context<Self>) {
        if !self.collapsed_repos.remove(repo_key) {
            self.collapsed_repos.insert(repo_key.to_owned());
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

    /// Asks for a branch name, then `worktree.create` in the repo at
    /// `repo_root` (the focused workspace's repo when `None`). herdr opens the
    /// worktree as a new, focused workspace.
    pub fn new_worktree(
        &mut self,
        repo_root: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        crate::prompt::ask_name(
            "New Worktree",
            "Create",
            "",
            move |branch, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.create_worktree(branch, repo_root.as_deref(), window, cx);
                });
            },
            window,
            cx,
        );
    }

    /// `worktree.create` on a new `branch` in the repo at `repo_root`, or the
    /// focused workspace's repo; a failure becomes an error toast.
    pub fn create_worktree(
        &mut self,
        branch: &str,
        repo_root: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let branch = branch.trim();
        if branch.is_empty() {
            return;
        }
        let state = self.state.read(cx);
        let params = WorktreeParams {
            workspace_id: match repo_root {
                Some(_) => None,
                None => state.snapshot.focused_workspace_id.clone(),
            },
            cwd: repo_root.map(str::to_owned),
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
                                let _ =
                                    b.update(cx, |this, cx| this.new_worktree(None, window, cx));
                            }),
                        )
                    }),
            )
    }

    /// A workspace row, `indent` from the left, and its agents when open.
    fn render_workspace(
        &self,
        info: &WorkspaceInfo,
        indent: Pixels,
        branch: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            .pl(indent)
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
                    .when_some(branch, |this, branch| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(branch),
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
                .map(|agent| self.render_agent(agent, indent, cx))
                .collect::<Vec<_>>()
        });
        v_flex()
            .child(row)
            .children(agent_rows.into_iter().flatten())
            .into_any_element()
    }

    /// A repo's heading: chevron, name, a + for a worktree of this repo,
    /// and, when folded, its workspaces' status and badges rolled up.
    fn render_repo(&self, group: &RepoGroup, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let snapshot = &state.snapshot;
        let repo = &group.repo;
        let open = !self.collapsed_repos.contains(&repo.key);
        let (mut blocked, mut unseen) = (0, 0);
        for (info, _) in &group.workspaces {
            let (b, u) = needs_you(
                &agents_of(snapshot, &info.workspace_id),
                &state.attention,
                snapshot,
            );
            blocked += b;
            unseen += u;
        }
        let status = rolled_up(group.workspaces.iter().map(|(i, _)| i.agent_status));
        let theme = cx.theme();
        let name = repo.name.clone();
        let key = repo.key.clone();
        let root = repo.root.clone();
        let hover = SharedString::from(format!("repo-row-{key}"));
        h_flex()
            .id(SharedString::from(format!("repo-{key}")))
            .debug_selector({
                let name = name.clone();
                move || format!("repo-{name}")
            })
            .group(hover.clone())
            .pl_1()
            .pr_1()
            .pt_1p5()
            .pb_0p5()
            .gap_1()
            .items_center()
            .cursor_pointer()
            .hover(|this| this.bg(theme.accent.opacity(0.6)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_repo(&key, cx)))
            .child(
                gpui_kit::component::Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(name.clone()),
            )
            .when(!open, |this| {
                this.child(dot(status_color(status, cx)))
                    .when(blocked > 0, |this| {
                        this.child(badge(blocked.to_string(), theme.danger))
                    })
                    .when(unseen > 0, |this| {
                        this.child(badge(unseen.to_string(), theme.success))
                    })
            })
            .child(
                div()
                    .flex_shrink_0()
                    .invisible()
                    .group_hover(hover, |this| this.visible())
                    .child(
                        Button::new(SharedString::from(format!("repo-new-{}", repo.key)))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip(format!("New worktree of {name}"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.new_worktree(Some(root.clone()), window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_section(label: &'static str, cx: &App) -> AnyElement {
        div()
            .pl(px(24.))
            .pt_1p5()
            .pb_0p5()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(label)
            .into_any_element()
    }

    fn render_agent(&self, agent: &Agent, indent: Pixels, cx: &Context<Self>) -> AnyElement {
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
            .pl(indent + px(24.))
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
        let groups = self.groups(&snapshot);
        let branch = |info: &WorkspaceInfo| {
            self.checkout(&snapshot, info)
                .and_then(|c| c.branch.clone())
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for group in &groups.repos {
            rows.push(self.render_repo(group, cx));
            if self.collapsed_repos.contains(&group.repo.key) {
                continue;
            }
            for &(info, linked) in &group.workspaces {
                let indent = if linked { px(20.) } else { px(8.) };
                rows.push(self.render_workspace(info, indent, branch(info), cx));
            }
        }
        if !groups.repos.is_empty() && !groups.ungrouped.is_empty() {
            rows.push(Self::render_section("No repo", cx));
        }
        for &info in &groups.ungrouped {
            rows.push(self.render_workspace(info, px(4.), None, cx));
        }
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

mod git;
mod groups;
mod script;
#[cfg(test)]
mod tests;
