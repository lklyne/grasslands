//! The workspace list, laid out like herdr's own TUI sidebar: a "spaces"
//! section (one compact row per workspace: status glyph and label, then the
//! branch and its ahead/behind counts), a footer with "new" and "menu", and an
//! "agents" section listing every agent, grouped by workspace or by priority.
//! It draws in the monospace font at terminal density. The rules (glyphs,
//! colors, grouping, ordering) follow herdr 0.9.5's `client/shell/sidebar.rs`
//! and `agent_sidebar.rs`; see docs/decisions/0009-workspace-list.md.
//!
//! Click a workspace to focus it (`workspace.focus`; the terminal area
//! follows herdr's focused workspace). Click an agent to focus its pane.
//! Right-click or "…" opens the chrome's workspace menu (rename, new tab,
//! close, close group); × closes through the chrome (`chrome::get`), which
//! asks first when agents are running. + on a repo's row makes a worktree of
//! that repo.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{CommandPalette, OpenContextMenu};
use crate::chrome::{self, HerdrCommand, MenuCommand, MenuEntry, MenuTarget};
use crate::herdr::{Agent, AgentStatus, Snapshot, WorkspaceInfo, WorktreeParams};
use crate::state::{AppEvent, AppState};
use git::{Checkout, HeadWatcher, Placement, Repo};
use groups::{Entry, Kind};

pub fn init(_cx: &mut App) {}

/// Text size and line height: one herdr cell row per line.
const TEXT: Pixels = px(13.);
const LINE: Pixels = px(17.);
/// How often every cached checkout is re-read, for what the `HEAD` watcher
/// can't see (commits, fetches and pushes move ahead/behind).
const GIT_REFRESH: Duration = Duration::from_secs(5);

/// The agents section's order, herdr's `agent_panel_sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSort {
    /// By workspace, in herdr's order ("grouped").
    Grouped,
    /// The attention queue: blocked, done, working, idle, newest change first.
    Priority,
}

pub struct WorkspaceList {
    state: Entity<AppState>,
    /// Repo groups (by key) folded to their parent rows.
    folded: HashSet<String>,
    agents_open: bool,
    agent_sort: AgentSort,
    /// git2's answer per workspace folder (`None`: not in git).
    checkouts: HashMap<PathBuf, Option<Checkout>>,
    /// Folders being read for the first time.
    reading: HashSet<PathBuf>,
    heads: Option<HeadWatcher>,
    _observe: Subscription,
    _subscribe: Subscription,
    _tasks: Vec<Task<()>>,
}

/// herdr's `status_priority`: what needs you most ranks highest.
fn priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

/// The status that needs you most among `statuses`.
fn rolled_up(statuses: impl IntoIterator<Item = AgentStatus>) -> AgentStatus {
    statuses
        .into_iter()
        .max_by_key(|&s| priority(s))
        .unwrap_or(AgentStatus::Unknown)
}

/// herdr's dot glyphs: filled when something happened, a ring when idle.
fn glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done => "●",
        AgentStatus::Idle => "○",
        AgentStatus::Unknown => "·",
    }
}

/// herdr's `status_color`, mapped onto the theme's palette.
fn status_color(status: AgentStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        AgentStatus::Blocked => theme.red,
        AgentStatus::Working => theme.yellow,
        AgentStatus::Done => theme.cyan,
        AgentStatus::Idle => theme.green,
        AgentStatus::Unknown => theme.muted_foreground,
    }
}

/// A label that isn't the focused one: herdr's `subtext0`.
fn dim_text(cx: &App) -> Hsla {
    cx.theme().foreground.opacity(0.78)
}

fn text(s: impl Into<SharedString>) -> Div {
    div().flex_shrink_0().whitespace_nowrap().child(s.into())
}

/// A pill counting blocked agents or unseen results.
fn badge(label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .px_1()
        .rounded_sm()
        .text_size(px(11.))
        .text_color(color)
        .bg(color.opacity(0.15))
        .child(label.into())
}

fn badges(blocked: usize, unseen: usize, cx: &App) -> Div {
    h_flex()
        .flex_shrink_0()
        .gap_1()
        .when(blocked > 0, |this| {
            this.child(badge(blocked.to_string(), cx.theme().red))
        })
        .when(unseen > 0, |this| {
            this.child(badge(unseen.to_string(), cx.theme().cyan))
        })
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

impl WorkspaceList {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let subscribe = cx.subscribe(&state, |this, _, event, cx| match event {
            AppEvent::SnapshotReplaced => this.sync_checkouts(cx),
            // The focused pane moved to another checkout; catch up on any
            // branch change the watcher could have missed.
            AppEvent::ActiveWorktreeChanged(_) => this.refresh_checkouts(cx),
            _ => {}
        });
        // Tests stay off notify's thread and timers, like `AppState` stays
        // off herdr's.
        let (heads, tasks) = if cfg!(test) {
            (None, Vec::new())
        } else {
            let (tx, rx) = async_channel::unbounded::<PathBuf>();
            let heads_task = cx.spawn(async move |this, cx| {
                while let Ok(git_dir) = rx.recv().await {
                    if this
                        .update(cx, |this, cx| this.head_changed(&git_dir, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let refresh_task = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(GIT_REFRESH).await;
                    if this
                        .update(cx, |this, cx| this.refresh_checkouts(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let watcher = HeadWatcher::new(tx)
                .inspect_err(|error| tracing::warn!("workspaces: no HEAD watcher: {error}"))
                .ok();
            (watcher, vec![heads_task, refresh_task])
        };
        script::start(window, cx);
        let mut this = Self {
            state,
            folded: HashSet::new(),
            agents_open: true,
            agent_sort: AgentSort::Grouped,
            checkouts: HashMap::new(),
            reading: HashSet::new(),
            heads,
            _observe: observe,
            _subscribe: subscribe,
            _tasks: tasks,
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

    fn refresh_checkouts(&mut self, cx: &mut Context<Self>) {
        let dirs = self.checkouts.keys().cloned().collect();
        self.read_checkouts(dirs, cx);
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
                let mut changed = false;
                for (dir, checkout) in results {
                    this.reading.remove(&dir);
                    changed |= this.checkouts.get(&dir) != Some(&checkout);
                    this.checkouts.insert(dir, checkout);
                }
                if changed {
                    this.watch_heads();
                    cx.notify();
                }
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

    fn entries<'a>(&self, snapshot: &'a Snapshot) -> Vec<Entry<'a>> {
        let infos: Vec<&WorkspaceInfo> = snapshot.workspaces.iter().map(|w| &w.info).collect();
        groups::entries(&infos, |info| self.placement(snapshot, info), &self.folded)
    }

    /// Every repo some workspace is in, in herdr's order.
    fn repos(&self, snapshot: &Snapshot) -> Vec<Repo> {
        let mut repos: Vec<Repo> = Vec::new();
        for w in &snapshot.workspaces {
            if let Some(p) = self.placement(snapshot, &w.info)
                && !repos.iter().any(|r| r.key == p.repo.key)
            {
                repos.push(p.repo);
            }
        }
        repos
    }

    /// The workspaces a row speaks for: itself, or for the parent row of a
    /// folded group, the whole group (herdr's `displayed_workspace_status`).
    fn members<'a>(&self, snapshot: &'a Snapshot, entry: &Entry<'a>) -> Vec<&'a WorkspaceInfo> {
        let folded_key = match (&entry.kind, &entry.repo) {
            (Kind::Parent, Some(repo)) if self.folded.contains(&repo.key) => &repo.key,
            _ => return vec![entry.info],
        };
        snapshot
            .workspaces
            .iter()
            .map(|w| &w.info)
            .filter(|info| {
                info.workspace_id == entry.info.workspace_id
                    || self
                        .placement(snapshot, info)
                        .is_some_and(|p| p.linked && &p.repo.key == folded_key)
            })
            .collect()
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
            Box::new(crate::actions::FocusPane {
                pane_id: pane_id.to_owned(),
            }),
            cx,
        );
    }

    /// Folds or unfolds a repo group, by repo key.
    pub fn toggle_group(&mut self, repo_key: &str, cx: &mut Context<Self>) {
        if !self.folded.remove(repo_key) {
            self.folded.insert(repo_key.to_owned());
        }
        cx.notify();
    }

    pub fn toggle_agents(&mut self, cx: &mut Context<Self>) {
        self.agents_open = !self.agents_open;
        cx.notify();
    }

    pub fn toggle_agent_sort(&mut self, cx: &mut Context<Self>) {
        self.agent_sort = match self.agent_sort {
            AgentSort::Grouped => AgentSort::Priority,
            AgentSort::Priority => AgentSort::Grouped,
        };
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

    /// The footer's "menu": the chrome's menu for the focused workspace plus
    /// the list's own entries, or the command palette when nothing is
    /// focused. herdr's global menu (settings, keybinds, …) has no
    /// counterpart here; the palette holds the app's commands.
    pub fn open_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.state.read(cx).snapshot.focused_workspace_id.clone();
        let Some(chrome) = chrome::get(window, cx) else {
            return;
        };
        let Some(workspace_id) = focused else {
            chrome.update(cx, |chrome, cx| chrome.open_command_palette(window, cx));
            return;
        };
        let (a, b) = (cx.entity().downgrade(), cx.entity().downgrade());
        let extra = vec![
            MenuEntry::item(
                "New Workspace",
                MenuCommand::Callback(Rc::new(move |_, cx| {
                    let _ = a.update(cx, |this, cx| this.new_workspace(cx));
                })),
            ),
            MenuEntry::item(
                "New Worktree…",
                MenuCommand::Callback(Rc::new(move |window, cx| {
                    let _ = b.update(cx, |this, cx| this.new_worktree(None, window, cx));
                })),
            ),
            MenuEntry::item(
                "Command Palette…",
                MenuCommand::Action(Rc::new(CommandPalette)),
            ),
        ];
        chrome.update(cx, |chrome, cx| {
            chrome.context_menu(
                MenuTarget::Workspace { workspace_id },
                position,
                extra,
                window,
                cx,
            )
        });
    }

    fn section_label(label: &'static str, cx: &App) -> Div {
        text(label)
            .font_bold()
            .text_color(cx.theme().muted_foreground)
    }

    fn render_spaces_header(&self, snapshot: &Snapshot, cx: &App) -> Div {
        let attention = &self.state.read(cx).attention;
        let blocked = snapshot
            .agents
            .iter()
            .filter(|a| a.agent_status == AgentStatus::Blocked)
            .count();
        let unseen = snapshot
            .agents
            .iter()
            .filter(|a| attention.is_unseen_done(snapshot, &a.pane_id))
            .count();
        h_flex()
            .flex_shrink_0()
            .h(LINE * 2.)
            .items_start()
            .pr_1()
            .child(Self::section_label(" spaces", cx))
            .child(div().flex_1())
            .child(badges(blocked, unseen, cx))
    }

    /// One workspace: status glyph and label, then (except for a group's
    /// linked worktrees, as in herdr) its branch and ahead/behind counts.
    fn render_space(
        &self,
        entry: &Entry,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let info = entry.info;
        let id = info.workspace_id.clone();
        let focused = snapshot.focused_workspace_id.as_deref() == Some(id.as_str());
        let members = self.members(snapshot, entry);
        let agents: Vec<&Agent> = snapshot
            .agents
            .iter()
            .filter(|a| members.iter().any(|m| m.workspace_id == a.workspace_id))
            .collect();
        let blocked = agents
            .iter()
            .filter(|a| a.agent_status == AgentStatus::Blocked)
            .count();
        let unseen = agents
            .iter()
            .filter(|a| state.attention.is_unseen_done(snapshot, &a.pane_id))
            .count();
        let mut status = rolled_up(members.iter().map(|m| m.agent_status));
        // herdr's `done` means unseen; this app also counts looks herdr
        // can't see, and a seen result is idle again.
        if status == AgentStatus::Done && unseen == 0 {
            status = AgentStatus::Idle;
        }
        let checkout = self.checkout(snapshot, info);
        let branch_line = (entry.kind != Kind::Child)
            .then(|| checkout.and_then(|c| c.branch.clone()))
            .flatten()
            .map(|branch| (branch, checkout.and_then(|c| c.ahead_behind)));
        let theme = cx.theme();
        let row_group = SharedString::from(format!("ws-row-{id}"));

        let lead = match (&entry.kind, &entry.repo) {
            (Kind::Parent, Some(repo)) => {
                let key = repo.key.clone();
                let open = !self.folded.contains(&key);
                div()
                    .id(SharedString::from(format!("fold-{key}")))
                    .debug_selector({
                        let name = repo.name.clone();
                        move || format!("fold-{name}")
                    })
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_color(theme.blue)
                    .cursor_pointer()
                    .child(if open { "▾ " } else { "▸ " })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_group(&key, cx);
                    }))
                    .into_any_element()
            }
            (Kind::Child, _) => text("   ").into_any_element(),
            _ => text(" ").into_any_element(),
        };
        let first = h_flex()
            .h(LINE)
            .child(lead)
            .child(text(glyph(status)).text_color(status_color(status, cx)))
            .child(text(" "))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(focused, |this| {
                        this.font_bold().text_color(theme.foreground)
                    })
                    .when(!focused, |this| this.text_color(dim_text(cx)))
                    .child(info.label.clone()),
            )
            .child(badges(blocked, unseen, cx).pr_1());
        let second = branch_line.map(|(branch, ahead_behind)| {
            let (ahead, behind) = ahead_behind.unwrap_or_default();
            h_flex()
                .h(LINE)
                .pr_1()
                .child(text("   "))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(if focused {
                            theme.magenta
                        } else {
                            theme.muted_foreground
                        })
                        .child(branch),
                )
                .when(ahead > 0, |this| {
                    this.child(text(format!(" ↑{ahead}")).text_color(theme.green))
                })
                .when(behind > 0, |this| {
                    this.child(text(format!(" ↓{behind}")).text_color(theme.red))
                })
        });

        // Hover actions sit over the row's right end, so they never move it.
        let new_in = entry
            .repo
            .as_ref()
            .filter(|_| entry.kind != Kind::Child)
            .map(|r| r.root.clone());
        let (more_id, close_id) = (id.clone(), id.clone());
        let action = |label: &'static str, name: String| {
            div()
                .id(SharedString::from(name))
                .px_1()
                .text_color(theme.muted_foreground)
                .hover(|this| this.text_color(theme.foreground))
                .cursor_pointer()
                .child(label)
        };
        let actions = h_flex()
            .absolute()
            .top_0()
            .right_0()
            .h(LINE)
            .bg(theme.sidebar_accent)
            .invisible()
            .group_hover(row_group.clone(), |this| this.visible())
            .when_some(new_in, |this, root| {
                this.child(
                    action("+", format!("ws-new-{id}"))
                        .tooltip(|window, cx| Tooltip::new("New worktree").build(window, cx))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.new_worktree(Some(root.clone()), window, cx);
                        })),
                )
            })
            .child(
                action("…", format!("ws-more-{id}")).on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    open_workspace_menu(&more_id, event.position(), window, cx);
                }),
            )
            .child(
                action("×", format!("ws-close-{id}")).on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    close_workspace(&close_id, window, cx);
                }),
            );

        let (row_id, menu_id) = (id.clone(), id.clone());
        v_flex()
            .id(SharedString::from(format!("ws-{id}")))
            .debug_selector(move || format!("ws-{id}"))
            .group(row_group)
            .relative()
            .cursor_pointer()
            .when(focused, |this| this.bg(theme.sidebar_accent))
            .hover(|this| this.bg(theme.sidebar_accent.opacity(0.6)))
            .on_click(cx.listener(move |this, _, _, cx| this.activate_workspace(&row_id, cx)))
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                open_workspace_menu(&menu_id, event.position, window, cx);
            })
            .child(first)
            .children(second)
            .child(actions)
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let link = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .whitespace_nowrap()
                .text_color(theme.muted_foreground)
                .hover(|this| this.text_color(theme.foreground))
                .cursor_pointer()
                .child(label)
        };
        h_flex()
            .flex_shrink_0()
            .h(LINE)
            .pr_1()
            .child(
                link("spaces-new", " new")
                    .on_click(cx.listener(|this, _, _, cx| this.new_workspace(cx))),
            )
            .child(div().flex_1())
            .child(link("spaces-menu", "menu").on_click(cx.listener(
                |this, event: &ClickEvent, window, cx| this.open_menu(event.position(), window, cx),
            )))
    }
}

fn open_workspace_menu(
    workspace_id: &str,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    window.dispatch_action(
        Box::new(OpenContextMenu {
            target: MenuTarget::Workspace {
                workspace_id: workspace_id.to_owned(),
            },
            position,
        }),
        cx,
    );
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
        let snapshot = self.state.read(cx).snapshot.clone();
        let entries = self.entries(&snapshot);
        let rows: Vec<AnyElement> = entries
            .iter()
            .map(|entry| self.render_space(entry, &snapshot, cx))
            .collect();
        let theme = cx.theme();
        let empty = rows
            .is_empty()
            .then(|| text(" no workspaces").text_color(theme.muted_foreground));
        v_flex()
            .size_full()
            .font_family(theme.mono_font_family.clone())
            .text_size(TEXT)
            .line_height(LINE)
            .child(
                v_flex()
                    .flex_1()
                    .min_h(LINE * 4.)
                    .child(self.render_spaces_header(&snapshot, cx))
                    .child(
                        v_flex()
                            .id("workspace-rows")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(rows)
                            .children(empty),
                    )
                    .child(self.render_footer(cx)),
            )
            .child(self.render_agents(&snapshot, cx))
    }
}

mod agents;
mod git;
mod groups;
mod script;
#[cfg(test)]
mod tests;
