//! The spaces panel, laid out like herdr's own TUI sidebar: one compact row
//! per workspace (status glyph, label, then the branch and its ahead/behind
//! counts). The agents panel ([`AgentList`]) lists every agent,
//! grouped by workspace or by priority. Both draw one-line rows in the file
//! tree's type and row height and sit in the sidebar's dock, which draws their
//! title bars. The rules (glyphs, colors, grouping, ordering) follow herdr
//! 0.9.5's `client/shell/sidebar.rs` and `agent_sidebar.rs`; see
//! docs/decisions/0009-workspace-list.md.
//!
//! Click a workspace to focus it (`workspace.focus`; the terminal area
//! follows herdr's focused workspace). Click an agent to focus its pane.
//! Right-click or "…" opens the chrome's workspace menu (rename, new tab,
//! close, close group); × closes through the chrome (`chrome::get`), which
//! asks first when agents are running. + on a repo's row makes a worktree of
//! that repo. Drag a row to reorder (`workspace.move_block`): a repo group
//! moves as one block, a linked worktree only within its group.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::OpenContextMenu;
use crate::chrome::{self, HerdrCommand, MenuCommand, MenuTarget};
use crate::herdr::{Agent, AgentStatus, Snapshot, WorkspaceInfo, WorktreeParams};
use crate::state::{AppEvent, AppState};
use git::{Checkout, HeadWatcher, Placement, Repo};
use groups::{Entry, Kind, Slot};

pub use agents::AgentList;

pub fn init(_cx: &mut App) {}

/// Row height: every row is one line of `text_sm`.
const ROW: Pixels = px(20.);
/// One hover action (+, …, ×) on a workspace row.
const ACTION: Pixels = px(14.);
/// The fold chevron's column; rows without one leave it empty.
const CHEVRON: Pixels = px(12.);
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
    /// git2's answer per workspace folder (`None`: not in git).
    checkouts: HashMap<PathBuf, Option<Checkout>>,
    /// Folders being read for the first time.
    reading: HashSet<PathBuf>,
    heads: Option<HeadWatcher>,
    /// The dragged row and the row the drag last ticked over, so each new
    /// row ticks once.
    drag_tick: Option<(usize, usize)>,
    /// Where each workspace's row was drawn last frame, for the script's
    /// in-app pointer input.
    drawn_rows: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    focus: FocusHandle,
    _observe: Subscription,
    _subscribe: Subscription,
    _tasks: Vec<Task<()>>,
}

/// A workspace row being dragged to a new place in the list.
#[derive(Clone)]
struct DraggedWorkspace {
    slot: Slot,
    /// Its row in the list, to show which side of a target it will land on.
    row: usize,
    label: SharedString,
}

impl Render for DraggedWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.label.clone())
    }
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

/// herdr's dots, with icons for the states that want the user: a question
/// for blocked (herdr's "waiting for input"), a solid badge with a check for
/// done. Every status gets the same slot so labels line up.
fn status_icon(status: AgentStatus, cx: &App) -> Div {
    let color = status_color(status, cx);
    let slot = div()
        .flex_shrink_0()
        .size(px(12.))
        .flex()
        .items_center()
        .justify_center();
    let dot = || div().size(px(7.)).rounded_full();
    match status {
        AgentStatus::Blocked => slot.child(
            Icon::new(assets::IconName::CircleQuestionMark)
                .xsmall()
                .text_color(color),
        ),
        AgentStatus::Done => slot.child(
            div()
                .size(px(10.))
                .rounded_full()
                .bg(color)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::default()
                        .data(BOLD_CHECK)
                        .with_size(px(7.))
                        .text_color(cx.theme().background),
                ),
        ),
        AgentStatus::Working => slot.child(dot().bg(color)),
        AgentStatus::Idle => slot.child(dot().border_1().border_color(color)),
        AgentStatus::Unknown => slot.child(div().size(px(3.)).rounded_full().bg(color)),
    }
}

/// Lucide's check at a heavier stroke, legible at badge size.
const BOLD_CHECK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>"#;

/// herdr's `status_color`, mapped onto the theme's status colors.
fn status_color(status: AgentStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        AgentStatus::Blocked => theme.danger,
        AgentStatus::Working => theme.warning,
        AgentStatus::Done => theme.info,
        AgentStatus::Idle => theme.success,
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
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
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
        let mut this = Self {
            state,
            folded: HashSet::new(),
            checkouts: HashMap::new(),
            reading: HashSet::new(),
            heads,
            drag_tick: None,
            drawn_rows: Rc::default(),
            focus: cx.focus_handle(),
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

    /// Folds or unfolds a repo group, by repo key.
    pub fn toggle_group(&mut self, repo_key: &str, cx: &mut Context<Self>) {
        let opened = self.folded.remove(repo_key);
        if !opened {
            self.folded.insert(repo_key.to_owned());
        }
        let cue = if opened {
            cues::Cue::Open
        } else {
            cues::Cue::Close
        };
        crate::sounds::play(cue, crate::sounds::subtle());
        cx.notify();
    }

    /// Ticks as a drag from row `from` enters row `over`, a semitone lower
    /// for each row further down the list than where it started (higher going
    /// up). The first move only notes where the drag is.
    fn drag_tick_over(&mut self, from: usize, over: usize) {
        let last = match self.drag_tick {
            Some((origin, last)) if origin == from => last,
            _ => {
                self.drag_tick = Some((from, over));
                return;
            }
        };
        if last == over {
            return;
        }
        self.drag_tick = Some((from, over));
        let rows = from as f64 - over as f64;
        let options = cues::PlayOptions::default().pitch(2f64.powf(rows / 12.0));
        crate::sounds::play(cues::Cue::Select, options);
    }

    /// Drops `dragged` where `target` is; see [`groups::reordered`].
    fn reorder(&mut self, dragged: &Slot, target: &Slot, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let snapshot = &state.snapshot;
        let infos: Vec<&WorkspaceInfo> = snapshot.workspaces.iter().map(|w| &w.info).collect();
        let entries = groups::entries(
            &infos,
            |info| self.placement(snapshot, info),
            &HashSet::new(),
        );
        if let Some(order) = groups::reordered(&entries, dragged, target) {
            state.reorder_workspaces(order, cx).detach_and_log_err(cx);
        }
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
            let result = task.await;
            let cue = if result.is_ok() {
                cues::Cue::Success
            } else {
                cues::Cue::Error
            };
            crate::sounds::play(cue, cues::PlayOptions::default());
            if let Err(error) = result {
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

    /// One workspace on one line: status glyph and label, then (except for a
    /// group's linked worktrees, as in herdr) its branch and ahead/behind.
    fn render_space(
        &self,
        row: usize,
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
        let unseen = agents
            .iter()
            .any(|a| state.attention.is_unseen_done(snapshot, &a.pane_id));
        let mut status = rolled_up(members.iter().map(|m| m.agent_status));
        // herdr's `done` means unseen; this app also counts looks herdr
        // can't see, and a seen result is idle again.
        if status == AgentStatus::Done && !unseen {
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
                    .cursor_pointer()
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_group(&key, cx);
                    }))
                    .into_any_element()
            }
            _ => div().into_any_element(),
        };
        let indent = if entry.kind == Kind::Child {
            CHEVRON
        } else {
            px(0.)
        };
        let label = div()
            .min_w_0()
            .truncate()
            .when(focused, |this| this.text_color(theme.foreground))
            .when(!focused, |this| this.text_color(dim_text(cx)))
            .child(info.label.clone());
        let branch = branch_line.map(|(branch, ahead_behind)| {
            let (ahead, behind) = ahead_behind.unwrap_or_default();
            h_flex()
                .min_w_0()
                .gap_1()
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
                    this.child(text(format!("↑{ahead}")).text_color(theme.success))
                })
                .when(behind > 0, |this| {
                    this.child(text(format!("↓{behind}")).text_color(theme.danger))
                })
        });

        // Hover actions sit over the row's right end; while hovered the row
        // pads its text clear of them, so labels truncate instead of running
        // underneath.
        let new_in = entry
            .repo
            .as_ref()
            .filter(|_| entry.kind != Kind::Child)
            .map(|r| r.root.clone());
        let actions_width = ACTION * if new_in.is_some() { 3. } else { 2. } + px(4.);
        let (more_id, close_id) = (id.clone(), id.clone());
        let action = |label: &'static str, name: String| {
            div()
                .id(SharedString::from(name))
                .w(ACTION)
                .flex()
                .justify_center()
                .text_color(theme.muted_foreground)
                .hover(|this| this.text_color(theme.foreground))
                .cursor_pointer()
                .child(label)
        };
        let actions = h_flex()
            .absolute()
            .top_0()
            .right_0()
            .h(ROW)
            .pr_1()
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
        let slot = Slot::of(entry);
        let dragged = DraggedWorkspace {
            slot: slot.clone(),
            row,
            label: info.label.clone().into(),
        };
        let (accept_slot, drop_slot) = (slot.clone(), slot);
        let drag_border = theme.drag_border;
        h_flex()
            .id(SharedString::from(format!("ws-{id}")))
            .debug_selector(move || format!("ws-{id}"))
            .group(row_group)
            .relative()
            .flex_shrink_0()
            .h(ROW)
            .pl(px(6.) + indent)
            .pr_2()
            .gap_1()
            .rounded_sm()
            .cursor_pointer()
            .when(focused, |this| this.bg(theme.sidebar_accent))
            .hover(|this| {
                this.bg(theme.sidebar_accent.opacity(0.6))
                    .pr(actions_width)
            })
            .on_click(cx.listener(move |this, _, _, cx| this.activate_workspace(&row_id, cx)))
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                open_workspace_menu(&menu_id, event.position, window, cx);
            })
            .on_drag(dragged, |dragged, _, _, cx| {
                crate::sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                crate::workspace::keep_terminals_during_drag(cx);
                cx.new(|_| dragged.clone())
            })
            .can_drop(move |dragged, _, _| {
                dragged
                    .downcast_ref::<DraggedWorkspace>()
                    .is_some_and(|d| groups::accepts(&d.slot, &accept_slot))
            })
            // The line marks the side the row will land on.
            .drag_over::<DraggedWorkspace>(move |style, dragged, _, _| {
                let style = style.border_color(drag_border);
                if dragged.row < row {
                    style.border_b_2()
                } else {
                    style.border_t_2()
                }
            })
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<DraggedWorkspace>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        this.drag_tick_over(event.drag(cx).row, row);
                    }
                },
            ))
            .on_drop(cx.listener(move |this, dragged: &DraggedWorkspace, _, cx| {
                this.drag_tick = None;
                this.reorder(&dragged.slot, &drop_slot, cx);
            }))
            .child(div().flex_shrink_0().w(CHEVRON).child(lead))
            .child(status_icon(status, cx))
            .child(label)
            .children(branch)
            .child(actions)
            .child({
                let drawn = self.drawn_rows.clone();
                let id = info.workspace_id.clone();
                canvas(
                    move |bounds, _, _| {
                        drawn.borrow_mut().insert(id, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .into_any_element()
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

impl EventEmitter<PanelEvent> for WorkspaceList {}

impl Focusable for WorkspaceList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for WorkspaceList {
    fn panel_name(&self) -> &'static str {
        "SpacesPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for WorkspaceList {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().text_sm().child("Workspaces")
    }

    fn toolbar_buttons(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<Vec<Button>> {
        Some(vec![
            Button::new("spaces-new")
                .icon(IconName::Plus)
                .tooltip("New Workspace")
                .on_click(cx.listener(|this, _, _, cx| this.new_workspace(cx))),
        ])
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let (a, b) = (cx.entity().downgrade(), cx.entity().downgrade());
        menu.item(
            PopupMenuItem::new("New Workspace").on_click(move |_, _, cx| {
                let _ = a.update(cx, |this, cx| this.new_workspace(cx));
            }),
        )
        .item(
            PopupMenuItem::new("New Worktree…").on_click(move |_, window, cx| {
                let _ = b.update(cx, |this, cx| this.new_worktree(None, window, cx));
            }),
        )
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for WorkspaceList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.state.read(cx).snapshot.clone();
        self.drawn_rows.borrow_mut().clear();
        let entries = self.entries(&snapshot);
        let rows: Vec<AnyElement> = entries
            .iter()
            .enumerate()
            .map(|(row, entry)| self.render_space(row, entry, &snapshot, cx))
            .collect();
        let theme = cx.theme();
        let empty = rows.is_empty().then(|| {
            text("No workspaces")
                .px_2()
                .text_color(theme.muted_foreground)
        });
        v_flex()
            .id("workspace-rows")
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

/// Debug builds only: drives both panels from `RT_SIDEBAR_SCRIPT`.
pub fn start_script(
    spaces: WeakEntity<WorkspaceList>,
    agents: WeakEntity<AgentList>,
    window: &mut Window,
    cx: &mut App,
) {
    script::start(spaces, agents, window, cx);
}

mod agents;
mod git;
mod groups;
mod script;
#[cfg(test)]
mod tests;
