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
//!
//! With saved SSH machines the list groups workspaces by machine, as herdr's
//! own sidebar does (docs/decisions/0019-remote-machines.md): a "Local"
//! header, then one collapsible header per machine with its status and a ···
//! menu. A machine that isn't connected keeps its last workspaces, dimmed
//! and inert. Every row's commands go to its own machine; a remote
//! workspace's repo grouping comes from herdr alone (its git read gives
//! the branch only).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::OpenContextMenu;
use crate::chrome::{
    self, Callback, Chrome, HerdrCommand, MenuCommand, MenuEntry, MenuTarget, WorktreeCommand,
};
use crate::herdr::config::AgentPanelSort;
use crate::herdr::{Snapshot, WorkspaceId, WorkspaceInfo, WorktreeParams};
use crate::machines::{self, MachineActions, MachineState, machine_header, ssh_config};
use crate::prompt::NewWorktree;
use crate::state::{AppEvent, AppState, Machine, MachineId, MachineTask, On};
use crate::status;
use git::{Checkout, HeadWatcher, Placement, RemoteCheckouts, Repo};
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

impl From<AgentPanelSort> for AgentSort {
    fn from(sort: AgentPanelSort) -> Self {
        match sort {
            AgentPanelSort::Spaces => Self::Grouped,
            AgentPanelSort::Priority => Self::Priority,
        }
    }
}

impl From<AgentSort> for AgentPanelSort {
    fn from(sort: AgentSort) -> Self {
        match sort {
            AgentSort::Grouped => Self::Spaces,
            AgentSort::Priority => Self::Priority,
        }
    }
}

pub struct WorkspaceList {
    state: Entity<AppState>,
    /// Each machine's repo groups (by key) folded to their parent rows.
    folded: HashMap<MachineId, HashSet<String>>,
    /// Machines whose groups are collapsed to their header.
    collapsed: HashSet<MachineId>,
    /// git's answer per Local workspace folder (`None`: not in git).
    checkouts: HashMap<PathBuf, Option<Checkout>>,
    /// Folders being read for the first time.
    reading: HashSet<PathBuf>,
    /// Saved machines' workspace branches.
    remote_checkouts: RemoteCheckouts,
    /// A refresh of every saved machine's branches is out.
    remote_refreshing: bool,
    heads: Option<HeadWatcher>,
    /// The dragged row and the row the drag last ticked over, so each new
    /// row ticks once.
    drag_tick: Option<(usize, usize)>,
    /// Where each workspace's row was drawn last frame (by [`row_key`]), for
    /// the script's in-app pointer input.
    drawn_rows: Rc<RefCell<HashMap<On<WorkspaceId>, Bounds<Pixels>>>>,
    /// Where each machine's header was drawn last frame, by machine.
    drawn_headers: Rc<RefCell<HashMap<MachineId, Bounds<Pixels>>>>,
    focus: FocusHandle,
    _observe: Subscription,
    _subscribe: Subscription,
    _tasks: Vec<Task<()>>,
}

/// A workspace row being dragged to a new place in the list.
#[derive(Clone)]
struct DraggedWorkspace {
    machine: MachineId,
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

/// A label that isn't the focused one: herdr's `subtext0`.
fn dim_text(cx: &App) -> Hsla {
    cx.theme().foreground.opacity(0.78)
}

fn text(s: impl Into<SharedString>) -> Div {
    div().flex_shrink_0().whitespace_nowrap().child(s.into())
}

/// What [`WorkspaceList::drawn_rows`] calls a workspace's row (ids repeat
/// across machines).
pub(super) fn row_key(machine: &MachineId, workspace_id: &WorkspaceId) -> On<WorkspaceId> {
    On::new(machine.clone(), workspace_id.clone())
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
            // Local's cache can change while another machine is selected.
            AppEvent::SnapshotReplaced | AppEvent::MachinesChanged => this.sync_checkouts(cx),
            // The focused pane moved to another checkout, or another machine
            // was selected; catch up on any branch change the watcher could
            // have missed (saved machines have no watcher).
            AppEvent::ActiveRootChanged(_) => this.refresh_checkouts(cx),
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
            folded: HashMap::new(),
            collapsed: HashSet::new(),
            checkouts: HashMap::new(),
            reading: HashSet::new(),
            remote_checkouts: RemoteCheckouts::default(),
            remote_refreshing: false,
            heads,
            drag_tick: None,
            drawn_rows: Rc::default(),
            drawn_headers: Rc::default(),
            focus: cx.focus_handle(),
            _observe: observe,
            _subscribe: subscribe,
            _tasks: tasks,
        };
        this.sync_checkouts(cx);
        this
    }

    /// Local's cache: the only machine whose checkouts give placement and
    /// upstream counts.
    fn local_snapshot(&self, cx: &App) -> std::sync::Arc<Snapshot> {
        self.state
            .read(cx)
            .machine(&MachineId::Local)
            .map(|m| m.snapshot().clone())
            .unwrap_or_default()
    }

    /// Reads folders of Local's workspaces seen for the first time and
    /// forgets ones no workspace uses any more.
    fn sync_checkouts(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.local_snapshot(cx);
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
        self.sync_remote_checkouts(cx);
    }

    fn refresh_checkouts(&mut self, cx: &mut Context<Self>) {
        let dirs = self.checkouts.keys().cloned().collect();
        self.read_checkouts(dirs, cx);
        // Saved machines are asked only while the app is in front (0020),
        // and not again while the last round is still out on a slow link.
        if self.remote_refreshing || cx.active_window().is_none() {
            return;
        }
        let remote = self.remote_checkouts.dirs();
        self.remote_refreshing = self.read_remote_checkouts(remote, true, cx);
    }

    /// Reads folders of connected saved machines' workspaces seen for the
    /// first time and forgets ones no workspace uses any more.
    fn sync_remote_checkouts(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let wanted = state
            .machines()
            .filter(|m| !m.id().is_local() && state.fs_for(m.id()).is_some())
            .flat_map(|m| {
                let snapshot = m.snapshot();
                snapshot
                    .workspaces
                    .iter()
                    .filter_map(|w| checkout_dir(snapshot, &w.info))
                    .map(|dir| (m.id().clone(), dir))
                    .collect::<Vec<_>>()
            })
            .collect();
        let new = self.remote_checkouts.sync(wanted);
        self.read_remote_checkouts(new, false, cx);
    }

    /// Asks each folder's machine for its branch, off the main thread;
    /// whether any was asked. A `refresh` clears `remote_refreshing` when done.
    fn read_remote_checkouts(
        &mut self,
        dirs: Vec<git::RemoteDir>,
        refresh: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.state.read(cx);
        let reads: Vec<_> = dirs
            .into_iter()
            .filter_map(|dir| Some((state.fs_for(&dir.0)?, dir)))
            .collect();
        if reads.is_empty() {
            return false;
        }
        let read = cx.background_executor().spawn(async move {
            reads
                .into_iter()
                .map(|(fs, dir)| {
                    let checkout = git::read_on(&fs, &dir.1);
                    (dir, checkout)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = read.await;
            this.update(cx, |this, cx| {
                if refresh {
                    this.remote_refreshing = false;
                }
                if this.remote_checkouts.store(results) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        true
    }

    /// Re-reads `dirs` with git off the main thread.
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

    /// The checkout for a Local workspace, the machine's branch for a
    /// saved machine's.
    fn checkout(
        &self,
        machine: &MachineId,
        snapshot: &Snapshot,
        info: &WorkspaceInfo,
    ) -> Option<&Checkout> {
        let dir = checkout_dir(snapshot, info)?;
        if !machine.is_local() {
            return self.remote_checkouts.get(machine, &dir);
        }
        self.checkouts.get(&dir)?.as_ref()
    }

    /// herdr's repo info, else what git found in the workspace's folder.
    fn placement(
        &self,
        machine: &MachineId,
        snapshot: &Snapshot,
        info: &WorkspaceInfo,
    ) -> Option<Placement> {
        info.worktree
            .as_ref()
            .map(Placement::from_herdr)
            .or_else(|| {
                self.checkout(machine, snapshot, info)
                    .and_then(|c| c.placement.clone())
            })
    }

    /// [`Self::placement`] for any machine's workspace, for the chrome's
    /// worktree menus and group closes (`chrome::set_placements`).
    pub fn placement_on(
        &self,
        machine: &MachineId,
        info: &WorkspaceInfo,
        cx: &App,
    ) -> Option<Placement> {
        let snapshot = self.state.read(cx).machine(machine)?.snapshot();
        self.placement(machine, snapshot, info)
    }

    /// Open Worktree… for the selected machine's focused workspace: a
    /// picker of its repo's worktrees (the chrome's).
    pub fn open_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let machine = state.selected().clone();
        let Some(workspace_id) = state.snapshot().focused_workspace_id.clone() else {
            return;
        };
        let Some(chrome) = chrome::get(window, cx) else {
            return;
        };
        let command = MenuCommand::Worktree(WorktreeCommand::Open {
            machine,
            workspace_id,
        });
        chrome.update(cx, |chrome, cx| {
            chrome.run_menu_command(command, window, cx)
        });
    }

    fn is_folded(&self, machine: &MachineId, repo_key: &str) -> bool {
        self.folded
            .get(machine)
            .is_some_and(|keys| keys.contains(repo_key))
    }

    fn entries<'a>(&self, machine: &MachineId, snapshot: &'a Snapshot) -> Vec<Entry<'a>> {
        let infos: Vec<&WorkspaceInfo> = snapshot.workspaces.iter().map(|w| &w.info).collect();
        groups::entries(
            &infos,
            |info| self.placement(machine, snapshot, info),
            self.folded.get(machine).unwrap_or(&HashSet::new()),
        )
    }

    /// Every repo some workspace is in, in herdr's order.
    fn repos(&self, machine: &MachineId, snapshot: &Snapshot) -> Vec<Repo> {
        let mut repos: Vec<Repo> = Vec::new();
        for w in &snapshot.workspaces {
            if let Some(p) = self.placement(machine, snapshot, &w.info)
                && !repos.iter().any(|r| r.key == p.repo.key)
            {
                repos.push(p.repo);
            }
        }
        repos
    }

    /// The workspaces a row speaks for: itself, or for the parent row of a
    /// folded group, the whole group (herdr's `displayed_workspace_status`).
    fn members<'a>(
        &self,
        machine: &MachineId,
        snapshot: &'a Snapshot,
        entry: &Entry<'a>,
    ) -> Vec<&'a WorkspaceInfo> {
        let folded_key = match (&entry.kind, &entry.repo) {
            (Kind::Parent, Some(repo)) if self.is_folded(machine, &repo.key) => &repo.key,
            _ => return vec![entry.info],
        };
        snapshot
            .workspaces
            .iter()
            .map(|w| &w.info)
            .filter(|info| {
                info.workspace_id == entry.info.workspace_id
                    || self
                        .placement(machine, snapshot, info)
                        .is_some_and(|p| p.linked && &p.repo.key == folded_key)
            })
            .collect()
    }

    /// `workspace.focus` on the selected machine; the terminal area follows
    /// herdr's focused workspace.
    pub fn activate_workspace(&mut self, workspace_id: &WorkspaceId, cx: &mut Context<Self>) {
        let machine = self.state.read(cx).selected().clone();
        self.activate_workspace_on(&machine, workspace_id, cx);
    }

    /// `workspace.focus` on `machine`, selecting it first.
    pub fn activate_workspace_on(
        &mut self,
        machine: &MachineId,
        workspace_id: &WorkspaceId,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state
                .focus_workspace(machine, workspace_id, cx)
                .detach_and_log_err(cx);
        });
    }

    /// Folds or unfolds one of the selected machine's repo groups, by key.
    pub fn toggle_group(&mut self, repo_key: &str, cx: &mut Context<Self>) {
        let machine = self.state.read(cx).selected().clone();
        self.toggle_group_on(&machine, repo_key, cx);
    }

    fn toggle_group_on(&mut self, machine: &MachineId, repo_key: &str, cx: &mut Context<Self>) {
        let folded = self.folded.entry(machine.clone()).or_default();
        let opened = folded.remove(repo_key);
        if !opened {
            folded.insert(repo_key.to_owned());
        }
        fold_cue(opened);
        cx.notify();
    }

    /// Collapses a machine's group to its header, or opens it.
    pub fn toggle_machine(&mut self, machine: &MachineId, cx: &mut Context<Self>) {
        let opened = self.collapsed.remove(machine);
        if !opened {
            self.collapsed.insert(machine.clone());
        }
        fold_cue(opened);
        cx.notify();
    }

    pub fn is_collapsed(&self, machine: &MachineId) -> bool {
        self.collapsed.contains(machine)
    }

    /// Where a workspace's row was drawn last frame, if it was.
    pub fn drawn_row(
        &self,
        machine: &MachineId,
        workspace_id: &WorkspaceId,
    ) -> Option<Bounds<Pixels>> {
        self.drawn_rows
            .borrow()
            .get(&row_key(machine, workspace_id))
            .copied()
    }

    /// The branch `workspace_id`'s row shows, for script hooks.
    pub fn branch_of(
        &self,
        machine: &MachineId,
        workspace_id: &WorkspaceId,
        cx: &App,
    ) -> Option<String> {
        let snapshot = self.state.read(cx).machine(machine)?.snapshot().clone();
        let info = &snapshot
            .workspaces
            .iter()
            .find(|w| &w.info.workspace_id == workspace_id)?
            .info;
        self.checkout(machine, &snapshot, info)?.branch.clone()
    }

    /// Where a machine's header was drawn last frame, if it was.
    pub fn drawn_header(&self, machine: &MachineId) -> Option<Bounds<Pixels>> {
        self.drawn_headers.borrow().get(machine).copied()
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

    /// Drops `dragged` where `target` is, both on `machine`; see
    /// [`groups::reordered`].
    fn reorder(
        &mut self,
        machine: &MachineId,
        dragged: &Slot,
        target: &Slot,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let Some(snapshot) = state.machine(machine).map(Machine::snapshot) else {
            return;
        };
        let infos: Vec<&WorkspaceInfo> = snapshot.workspaces.iter().map(|w| &w.info).collect();
        let entries = groups::entries(
            &infos,
            |info| self.placement(machine, snapshot, info),
            &HashSet::new(),
        );
        if let Some(order) = groups::reordered(&entries, dragged, target) {
            state
                .reorder_workspaces(machine, order, cx)
                .detach_and_log_err(cx);
        }
    }

    /// A new workspace on the selected machine, focused. herdr names it
    /// unless its `ui.prompt_new_workspace_name` asks for a name first.
    pub fn new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let machine = self.state.read(cx).selected().clone();
        self.new_workspace_on(&machine, window, cx);
    }

    /// A new workspace on `machine`, selected and focused: on Local in the
    /// active worktree's folder, elsewhere in herdr's default folder. With
    /// herdr's `ui.prompt_new_workspace_name` it asks for the name first; an
    /// empty name lets herdr pick one.
    pub fn new_workspace_on(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !crate::herdr_prefs::get(cx).prompt_new_workspace_name {
            self.create_workspace_on(machine, None, cx);
            return;
        }
        let (this, machine) = (cx.entity().downgrade(), machine.clone());
        crate::prompt::ask_name(
            "New Workspace",
            "Create",
            "",
            move |name, _, cx| {
                let name = name.trim();
                let label = (!name.is_empty()).then(|| name.to_owned());
                let _ = this.update(cx, |this, cx| this.create_workspace_on(&machine, label, cx));
            },
            window,
            cx,
        );
    }

    fn create_workspace_on(
        &mut self,
        machine: &MachineId,
        label: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.select_machine(machine, cx);
            let cwd = state.active_worktree().cloned();
            state
                .new_workspace(machine, cwd, label, cx)
                .detach_and_log_err(cx);
        });
    }

    /// Asks for a branch (and optionally a base ref and label), then
    /// `worktree.create` in the repo at `repo_root` (the focused workspace's
    /// repo when `None`). herdr opens the worktree as a new, focused
    /// workspace.
    pub fn new_worktree(
        &mut self,
        repo_root: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let machine = self.state.read(cx).selected().clone();
        self.new_worktree_on(machine, repo_root, window, cx);
    }

    /// [`Self::new_worktree`] in a repo on `machine`.
    fn new_worktree_on(
        &mut self,
        machine: MachineId,
        repo_root: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        crate::prompt::ask_new_worktree(
            move |worktree, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.create_worktree_on(&machine, worktree, repo_root.as_deref(), window, cx);
                });
            },
            window,
            cx,
        );
    }

    /// `worktree.create` of `worktree` in the repo at `repo_root`, or the
    /// focused workspace's repo; a failure becomes an error toast.
    pub fn create_worktree(
        &mut self,
        worktree: NewWorktree,
        repo_root: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let machine = self.state.read(cx).selected().clone();
        self.create_worktree_on(&machine, worktree, repo_root, window, cx);
    }

    /// [`Self::create_worktree`] on `machine`, which herdr then shows the
    /// new workspace on; it is selected.
    fn create_worktree_on(
        &mut self,
        machine: &MachineId,
        worktree: NewWorktree,
        repo_root: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if worktree.branch.is_empty() {
            return;
        }
        let task = self.state.update(cx, |state, cx| {
            state.select_machine(machine, cx);
            let params = WorktreeParams {
                workspace_id: match repo_root {
                    Some(_) => None,
                    None => state.snapshot().focused_workspace_id.clone(),
                },
                cwd: repo_root.map(str::to_owned),
                branch: Some(worktree.branch),
                base: worktree.base,
                label: worktree.label,
                focus: true,
                ..Default::default()
            };
            state.create_worktree(machine, params, cx)
        });
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
    /// An `inert` row (its machine isn't connected) draws dimmed and takes
    /// no input.
    fn render_space(
        &self,
        row: usize,
        machine: &Machine,
        entry: &Entry,
        cx: &Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let snapshot = machine.snapshot();
        let machine_id = machine.id().clone();
        let inert = !machine_id.is_local() && MachineState::of(machine.status()).0.is_dimmed();
        let info = entry.info;
        let id = info.workspace_id.clone();
        let key = row_key(&machine_id, &id);
        // Each machine has a focused workspace; the selected machine's is
        // the one on screen.
        let focused = state.selected() == &machine_id
            && snapshot.focused_workspace_id.as_deref() == Some(id.as_str());
        // herdr rolls a workspace's agents up itself; a folded group has no
        // herdr counterpart, so its members roll up here.
        let members = self.members(&machine_id, snapshot, entry);
        let status = match members.as_slice() {
            [_] => info.agent_status,
            many => status::rolled_up(many.iter().map(|m| m.agent_status)),
        };
        let checkout = self.checkout(&machine_id, snapshot, info);
        let branch_line = (entry.kind != Kind::Child)
            .then(|| checkout.and_then(|c| c.branch.clone()))
            .flatten()
            .map(|branch| (branch, checkout.and_then(|c| c.ahead_behind)));
        let theme = cx.theme();
        let row_group = SharedString::from(format!("ws-row-{key}"));

        let lead = match (&entry.kind, &entry.repo) {
            (Kind::Parent, Some(repo)) => {
                let repo_key = repo.key.clone();
                let open = !self.is_folded(&machine_id, &repo_key);
                let fold_machine = machine_id.clone();
                div()
                    .id(SharedString::from(format!("fold-{machine_id}-{repo_key}")))
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
                        this.toggle_group_on(&fold_machine, &repo_key, cx);
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
        let (more_machine, close_machine, new_machine) =
            (machine_id.clone(), machine_id.clone(), machine_id.clone());
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
                    action("+", format!("ws-new-{key}"))
                        .tooltip(|window, cx| Tooltip::new("New worktree").build(window, cx))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.new_worktree_on(
                                new_machine.clone(),
                                Some(root.clone()),
                                window,
                                cx,
                            );
                        })),
                )
            })
            .child(
                action("…", format!("ws-more-{key}")).on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    open_workspace_menu(&more_machine, &more_id, event.position(), window, cx);
                }),
            )
            .child(
                action("×", format!("ws-close-{key}")).on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    close_workspace(&close_machine, &close_id, window, cx);
                }),
            );

        let (row_id, menu_id) = (id.clone(), id.clone());
        let (row_machine, menu_machine, drop_machine) =
            (machine_id.clone(), machine_id.clone(), machine_id.clone());
        let slot = Slot::of(entry);
        let dragged = DraggedWorkspace {
            machine: machine_id.clone(),
            slot: slot.clone(),
            row,
            label: info.label.clone().into(),
        };
        let (accept_slot, drop_slot) = (slot.clone(), slot);
        let accept_machine = machine_id.clone();
        let drag_border = theme.drag_border;
        let selector = format!("ws-{key}");
        h_flex()
            .id(SharedString::from(format!("ws-{key}")))
            .debug_selector(move || selector)
            .group(row_group)
            .relative()
            .flex_shrink_0()
            .h(ROW)
            .pl(px(6.) + indent)
            .pr_2()
            .gap_1()
            .rounded_sm()
            .when(inert, |this| this.opacity(0.45))
            .when(focused, |this| this.bg(theme.sidebar_accent))
            .when(!inert, |this| {
                this.cursor_pointer()
                    .hover(|this| this.bg(theme.sidebar_accent.opacity(0.6)).pr(actions_width))
            })
            .when(!inert, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.activate_workspace_on(&row_machine, &row_id, cx)
                }))
                .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    open_workspace_menu(&menu_machine, &menu_id, event.position, window, cx);
                })
                .on_drag(dragged, |dragged, _, _, cx| {
                    crate::sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                    crate::workspace::keep_terminals_during_drag(cx);
                    cx.new(|_| dragged.clone())
                })
            })
            .can_drop(move |dragged, _, _| {
                dragged.downcast_ref::<DraggedWorkspace>().is_some_and(|d| {
                    d.machine == accept_machine && groups::accepts(&d.slot, &accept_slot)
                })
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
                this.reorder(&drop_machine, &dragged.slot, &drop_slot, cx);
            }))
            .child(div().flex_shrink_0().w(CHEVRON).child(lead))
            .child(status::indicator(status, cx))
            .child(label)
            .children(branch)
            .when(!inert, |this| this.child(actions))
            .child({
                let drawn = self.drawn_rows.clone();
                canvas(
                    move |bounds, _, _| {
                        drawn.borrow_mut().insert(key, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .into_any_element()
    }
}

/// Folding or unfolding a group sounds the same everywhere.
fn fold_cue(opened: bool) {
    let cue = if opened {
        cues::Cue::Open
    } else {
        cues::Cue::Close
    };
    crate::sounds::play(cue, crate::sounds::subtle());
}

impl WorkspaceList {
    /// A machine's group header. Local's has no menu.
    fn render_header(&self, machine: &Machine, cx: &Context<Self>) -> AnyElement {
        let id = machine.id().clone();
        let (state, _) = MachineState::of(machine.status());
        let this = cx.entity().downgrade();
        let toggle_id = id.clone();
        let mut header = machine_header(id.to_string(), machine.label().to_owned(), state)
            .collapsed(self.collapsed.contains(&id))
            .on_toggle({
                let this = this.clone();
                move |_, cx| {
                    this.update(cx, |this, cx| this.toggle_machine(&toggle_id, cx))
                        .ok();
                }
            });
        if !id.is_local() {
            let menu_id = id.clone();
            header = header.on_menu(move |position, window, cx| {
                this.update(cx, |this, cx| {
                    this.open_machine_menu(&menu_id, position, window, cx)
                })
                .ok();
            });
        }
        let drawn = self.drawn_headers.clone();
        div()
            .relative()
            .child(header)
            .child(
                canvas(
                    move |bounds, _, _| {
                        drawn.borrow_mut().insert(id, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    /// The ··· menu of a saved machine, at `position` in the main window.
    pub fn open_machine_menu(
        &mut self,
        id: &MachineId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(machine) = self.state.read(cx).machine(id) else {
            return;
        };
        let (status, _) = MachineState::of(machine.status());
        let label = machine.label().to_owned();
        let entries = machines::machine_menu(&label, status, self.machine_actions(id, window, cx));
        machines::open_machine_menu(entries, position, window, cx);
    }

    /// What a machine's menu does. Each callback talks to `AppState` only
    /// (a confirmation calls back from the overlay window); a failed herdr
    /// command becomes an error toast.
    fn machine_actions(
        &self,
        id: &MachineId,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> MachineActions {
        let state = self.state.clone();
        let chrome = chrome::get(window, cx).map(|c| c.downgrade());
        let report = move |title: &'static str, task: Task<anyhow::Result<()>>, cx: &mut App| {
            let chrome = chrome.clone();
            cx.spawn(async move |cx| {
                let Err(error) = task.await else {
                    return;
                };
                crate::sounds::play(cues::Cue::Error, crate::sounds::subtle());
                if let Some(chrome) = chrome {
                    toast_error(&chrome, title, error.to_string(), cx);
                }
            })
            .detach();
        };
        let with = |f: fn(&mut AppState, &MachineId, &mut Context<AppState>)| -> Callback {
            let (state, id) = (state.clone(), id.clone());
            Rc::new(move |_, cx| state.update(cx, |state, cx| f(state, &id, cx)))
        };
        let (rename_state, rename_id, rename_report) = (state.clone(), id.clone(), report.clone());
        let (enable_state, enable_id, enable_report) = (state.clone(), id.clone(), report.clone());
        let (remove_state, remove_id) = (state.clone(), id.clone());
        MachineActions {
            on_reconnect: with(|state, id, cx| state.reconnect_machine(id, cx)),
            on_fix: with(|state, id, cx| {
                if let Some(task) = state.machine(id).and_then(MachineTask::fix) {
                    state.start_machine_run(task, cx);
                }
            }),
            on_rename: Rc::new(move |label, _, cx| {
                let task = rename_state
                    .update(cx, |state, cx| state.rename_machine(&rename_id, &label, cx));
                rename_report("Rename machine", task, cx);
            }),
            on_set_enabled: Rc::new(move |enable, _, cx| {
                let task = enable_state.update(cx, |state, cx| {
                    if enable {
                        state.enable_machine(&enable_id, cx)
                    } else {
                        state.disable_machine(&enable_id, cx)
                    }
                });
                enable_report(
                    if enable {
                        "Enable machine"
                    } else {
                        "Disable machine"
                    },
                    task,
                    cx,
                );
            }),
            on_remove: Rc::new(move |_, cx| {
                let task =
                    remove_state.update(cx, |state, cx| state.remove_machine(&remove_id, cx));
                report("Remove machine", task, cx);
            }),
        }
    }

    /// The Workspaces "+" menu once machines are saved, drawn in the overlay.
    /// Without saved machines "+" makes a workspace and the same entries are
    /// in the panel's title-bar menu.
    pub fn open_new_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.new_menu_entries(cx);
        machines::open_machine_menu(entries, position, window, cx);
    }

    /// The "+" menu's and the title-bar menu's entries: New Workspace (on the
    /// selected machine), New Workspace on › (Local and each connected
    /// machine, once machines are saved), New Worktree…, Open Worktree…, Add
    /// Machine….
    pub(super) fn new_menu_entries(&self, cx: &Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let mut entries = vec![MenuEntry::item(
            "New Workspace",
            list_command(&this, |this, window, cx| this.new_workspace(window, cx)),
        )];
        let state = self.state.read(cx);
        if state.machines().count() > 1 {
            let targets = state
                .machines()
                .filter(|m| m.id().is_local() || !MachineState::of(m.status()).0.is_dimmed())
                .map(|m| {
                    let id = m.id().clone();
                    MenuEntry::item(
                        m.label().to_owned(),
                        list_command(&this, move |this, window, cx| {
                            this.new_workspace_on(&id, window, cx)
                        }),
                    )
                })
                .collect();
            entries.push(MenuEntry::Submenu {
                label: "New Workspace on".into(),
                entries: targets,
            });
        }
        entries.extend([
            MenuEntry::item(
                "New Worktree…",
                list_command(&this, |this, window, cx| {
                    this.new_worktree(None, window, cx)
                }),
            ),
            MenuEntry::item(
                "Open Worktree…",
                list_command(&this, |this, window, cx| this.open_worktree(window, cx)),
            ),
            MenuEntry::Separator,
            MenuEntry::item(
                "Add Machine…",
                list_command(&this, |this, window, cx| this.add_machine(window, cx)),
            ),
        ]);
        entries
    }

    /// Asks for an SSH host (suggesting `~/.ssh/config`'s), then runs
    /// `herdr machine add` in a terminal beside the terminal area.
    pub fn add_machine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hosts = cx.background_executor().spawn(async {
            ssh_config::default_path()
                .map(|path| ssh_config::hosts(&path))
                .unwrap_or_default()
        });
        let state = self.state.clone();
        machines::open_add_machine_with(
            hosts,
            move |host, _, cx| {
                state.update(cx, |state, cx| {
                    state.start_machine_run(MachineTask::Add { host }, cx)
                });
            },
            window,
            cx,
        );
    }
}

/// A menu entry's command that runs `f` on the list.
fn list_command(
    this: &WeakEntity<WorkspaceList>,
    f: impl Fn(&mut WorkspaceList, &mut Window, &mut Context<WorkspaceList>) + 'static,
) -> MenuCommand {
    let this = this.clone();
    MenuCommand::Callback(Rc::new(move |window, cx| {
        this.update(cx, |this, cx| f(this, window, cx)).ok();
    }))
}

/// An error toast through the chrome, from any context.
fn toast_error(
    chrome: &WeakEntity<Chrome>,
    title: &'static str,
    message: String,
    cx: &mut AsyncApp,
) {
    chrome
        .update(cx, |chrome, cx| {
            chrome.floating(cx, move |window, cx| {
                window.push_notification(Notification::error(message).title(title), cx);
            });
        })
        .ok();
}

fn open_workspace_menu(
    machine: &MachineId,
    workspace_id: &WorkspaceId,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    window.dispatch_action(
        Box::new(OpenContextMenu {
            target: MenuTarget::Workspace {
                machine: machine.clone(),
                workspace_id: workspace_id.to_owned(),
            },
            position,
        }),
        cx,
    );
}

/// Closes through the chrome, which asks first when agents are running there.
pub(super) fn close_workspace(
    machine: &MachineId,
    workspace_id: &WorkspaceId,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(chrome) = chrome::get(window, cx) else {
        return;
    };
    let command = HerdrCommand::CloseWorkspace {
        machine: machine.clone(),
        workspace_id: workspace_id.to_owned(),
    };
    chrome.update(cx, |chrome, cx| chrome.run(command, window, cx));
}

/// The chrome's rename prompt for a workspace.
pub(super) fn rename_workspace(
    machine: &MachineId,
    workspace_id: &WorkspaceId,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(chrome) = chrome::get(window, cx) else {
        return;
    };
    let command = MenuCommand::Rename(MenuTarget::Workspace {
        machine: machine.clone(),
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
        let has_machines = self.state.read(cx).machines().count() > 1;
        Some(vec![
            Button::new("spaces-new")
                .icon(IconName::Plus)
                .tooltip(if has_machines { "New" } else { "New Workspace" })
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    // With no saved machines it's one click, as before machines.
                    if this.state.read(cx).machines().count() > 1 {
                        this.open_new_menu(event.position(), window, cx)
                    } else {
                        this.new_workspace(window, cx)
                    }
                })),
        ])
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let entries = self.new_menu_entries(cx);
        chrome::fill_popup(menu, &entries, window, cx)
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl WorkspaceList {
    /// One machine's rows, numbered from `first` for drag ticks; an empty
    /// note when a connected machine has no workspaces.
    fn render_machine_rows(
        &self,
        machine: &Machine,
        first: usize,
        indent_empty: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let snapshot = machine.snapshot().clone();
        let entries = self.entries(machine.id(), &snapshot);
        let mut rows: Vec<AnyElement> = entries
            .iter()
            .enumerate()
            .map(|(row, entry)| self.render_space(first + row, machine, entry, cx))
            .collect();
        let ready = machine.id().is_local()
            || (snapshot.generation != 0 && !MachineState::of(machine.status()).0.is_dimmed());
        if rows.is_empty() && ready {
            rows.push(
                text("No workspaces")
                    .px_2()
                    .when(indent_empty, |this| this.pl(px(6.) + CHEVRON + px(16.)))
                    .text_color(cx.theme().muted_foreground)
                    .into_any_element(),
            );
        }
        rows
    }
}

impl Render for WorkspaceList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn_rows.borrow_mut().clear();
        self.drawn_headers.borrow_mut().clear();
        let state = self.state.clone();
        let state = state.read(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        // With no saved machines the list is Local's alone, without headers.
        let grouped = state.machines().count() > 1;
        for machine in state.machines() {
            if grouped {
                rows.push(self.render_header(machine, cx));
                if self.collapsed.contains(machine.id()) {
                    continue;
                }
            }
            let first = rows.len();
            rows.extend(self.render_machine_rows(machine, first, grouped, cx));
        }
        let theme = cx.theme();
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
    }
}

/// Debug builds only: drives both panels from `RT_SIDEBAR_SCRIPT`.
#[cfg(debug_assertions)]
pub fn start_script(
    spaces: WeakEntity<WorkspaceList>,
    agents: WeakEntity<AgentList>,
    window: &mut Window,
    cx: &mut App,
) {
    script::start(spaces, agents, window, cx);
}

mod agents;
pub(crate) mod git;
mod groups;
#[cfg(debug_assertions)]
mod script;
#[cfg(test)]
mod tests;
