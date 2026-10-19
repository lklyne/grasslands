//! `Session`: one herdr server (Local, or a saved SSH machine) as its own
//! entity. It owns the connection (client, link, sync), the latest cache,
//! the focused pane, the active worktree and the status, and announces each
//! change as a [`SessionEvent`]. Every herdr command on that machine goes
//! through it ([`Session::call`]).
//!
//! `AppState` keeps the list and the selection; a view that shows one
//! machine subscribes to its session, so another machine's traffic doesn't
//! wake it (docs/decisions/0002-views-get-state.md).

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::*;

use crate::attention::{Attention, StatusChange};
use crate::fs::{Fs, SummaryCache};
use crate::herdr::{
    self, Client, ConnectionStatus, MachineLink, MachineProfile, MachineStatus, PaneId,
    SessionSync, Snapshot, TabId, TerminalId, Update, WorkspaceId,
};
use crate::setup::Setup;
use crate::sounds;
use cues::{Cue, PlayOptions};

use super::machine_list::{self, MachineId};
use super::status::Status;

/// What changed on one session. Within one update they come in this order:
/// `SnapshotReplaced`, each `AgentStatusChanged`, `FocusedPaneChanged`,
/// then `WorktreeChanged`; a (re)connect sends `Connected`, maybe
/// `WorktreeChanged`, then `StatusChanged`.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// The herdr cache was replaced (startup, events, reconnect). The new
    /// one is [`Session::snapshot`].
    SnapshotReplaced,
    /// A known agent's status changed between two caches.
    AgentStatusChanged(StatusChange),
    /// The focused pane changed.
    FocusedPaneChanged(Option<PaneId>),
    /// The active worktree ([`Session::worktree`]) changed.
    WorktreeChanged,
    /// The status ([`Session::status`]) may have changed.
    StatusChanged,
    /// The link (re)connected. Terminals whose attach died with the old
    /// connection attach again.
    Connected,
}

/// One herdr server: Local, or a saved SSH machine.
pub struct Session {
    id: MachineId,
    label: String,
    /// What the app asks; derived from `link_status`, `connection` and
    /// Local's setup by `refresh_status`, the only place that compares them.
    status: Status,
    /// The SSH link's last status; `None` for Local, which has no link.
    link_status: Option<MachineStatus>,
    /// The sync's connection to the machine's herdr server.
    connection: ConnectionStatus,
    /// The latest herdr cache: workspaces → tabs → split trees, panes, agents.
    snapshot: Arc<Snapshot>,
    focused_pane: Option<PaneId>,
    /// The worktree its focused pane works in (see `follow_dir`).
    worktree: Option<PathBuf>,
    /// The focused pane's folder `worktree` was last resolved from.
    focused_dir: Option<PathBuf>,
    /// Status changes, for toasts and sounds.
    attention: Attention,
    /// Was connected, and hasn't been since: the next connection is a
    /// reconnect.
    lost: bool,
    /// Taken out of the list: every command fails, nothing reconnects.
    gone: bool,
    /// `None` when Local's endpoint was refused, or before a remote machine
    /// first connects.
    client: Option<Client>,
    /// Local's herdr, which its sync connects to; `None` for a saved
    /// machine and when the endpoint was refused.
    config: Option<herdr::Config>,
    /// Local's setup check, which its status follows.
    setup: Option<Entity<Setup>>,
    /// Remote machines only.
    link: Option<MachineLink>,
    /// The Changes tab's patch reads' summaries, for the file tree's footer
    /// (remote machines only; see `SummaryCache`).
    summaries: Arc<SummaryCache>,
    sync: Option<(SessionSync, Task<()>)>,
    _statuses: Option<Task<()>>,
    /// An in-memory disk standing in for this machine's files (see
    /// `AppState::set_test_fs`).
    #[cfg(test)]
    pub(super) test_fs: Option<Arc<crate::fs::fake::FakeFs>>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    fn new(id: MachineId, label: String, link_status: Option<MachineStatus>) -> Self {
        let mut session = Self {
            id,
            label,
            status: Status::Connecting,
            link_status,
            connection: ConnectionStatus::Connecting,
            snapshot: Arc::default(),
            focused_pane: None,
            worktree: None,
            focused_dir: None,
            attention: Attention::default(),
            lost: false,
            gone: false,
            client: None,
            config: None,
            setup: None,
            link: None,
            summaries: Arc::default(),
            sync: None,
            _statuses: None,
            #[cfg(test)]
            test_fs: None,
        };
        session.status = session.derive_status(false);
        session
    }

    /// Local, disconnected for `reason` until [`Self::connect_local`]; its
    /// root starts at the app's folder.
    pub(super) fn local(setup: Entity<Setup>, reason: String, cx: &App) -> Self {
        let mut local = Self::new(MachineId::Local, "Local".into(), None);
        local.connection = ConnectionStatus::Disconnected { reason };
        local.setup = Some(setup);
        local.worktree = std::env::current_dir().ok();
        local.refresh_status(cx);
        local
    }

    /// Gives Local its herdr: commands work at once (starting the server if
    /// needed); the sync waits for [`Self::start_local_sync`].
    pub(super) fn connect_local(&mut self, config: herdr::Config, cx: &App) {
        self.client = Some(Client::new(config.clone()));
        self.connection = ConnectionStatus::Connecting;
        self.config = Some(config);
        self.refresh_status(cx);
    }

    /// A saved machine: starts its SSH link, and its sync once the link
    /// connects.
    pub(super) fn remote(
        profile: MachineProfile,
        dir: PathBuf,
        bin: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let id = MachineId::Remote(profile.id.clone());
        let label = profile.label.clone();
        let link = MachineLink::start(profile, dir, bin);
        let mut session = Self::new(id, label, Some(link.status()));
        let statuses = link.statuses();
        session._statuses = Some(cx.spawn(async move |this, cx| {
            while let Ok(status) = statuses.recv().await {
                if this
                    .update(cx, |this, cx| this.link_status_changed(status, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        let connected = session.status.is_usable();
        session.link = Some(link);
        if connected {
            session.start_remote_sync(cx);
        }
        session
    }

    /// A remote machine with no link, for tests.
    #[cfg(test)]
    pub(super) fn test(id: MachineId, label: String, snapshot: Arc<Snapshot>) -> Self {
        let mut session = Self::new(id, label, Some(MachineStatus::Connected));
        session.snapshot = snapshot;
        session
    }

    /// Sets the link's status as its supervisor would, for tests.
    #[cfg(test)]
    pub(super) fn set_link_status(&mut self, status: MachineStatus, cx: &mut Context<Self>) {
        self.link_status_changed(status, cx);
    }

    pub fn id(&self) -> &MachineId {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether it can be used, and why not: the one status views ask.
    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    pub fn focused_pane(&self) -> Option<&str> {
        self.focused_pane.as_deref()
    }

    /// The worktree its focused pane works in.
    pub fn worktree(&self) -> Option<&PathBuf> {
        self.worktree.as_ref()
    }

    /// The machine's files over its link; `None` for Local and while the
    /// link isn't connected.
    pub fn remote_fs(&self) -> Option<Arc<herdr_client::RemoteFs>> {
        self.link.as_ref()?.remote_fs()
    }

    /// herdr's saved profile; `None` for Local.
    pub fn profile(&self) -> Option<&MachineProfile> {
        self.link.as_ref().map(MachineLink::profile)
    }

    /// The shell command that attaches a terminal surface to `terminal_id`
    /// on this machine. `None` until a remote machine's herdr is found.
    /// `takeover` replaces another client attached to it.
    pub fn attach_command(&self, terminal_id: &TerminalId, takeover: bool) -> Option<String> {
        self.link.as_ref()?.attach_command(terminal_id, takeover)
    }

    /// The files on this machine: Local always, a saved machine while it's
    /// connected.
    pub fn fs(&self) -> Option<Fs> {
        if self.id.is_local() {
            return Some(Fs::Local);
        }
        #[cfg(test)]
        if let Some(fake) = &self.test_fs {
            let online = !fake.offline.load(std::sync::atomic::Ordering::SeqCst);
            return online.then(|| Fs::Fake(fake.clone()));
        }
        if !self.status.is_usable() {
            return None;
        }
        Some(Fs::Remote {
            machine: self.id.clone(),
            files: self.link.as_ref()?.remote_fs()?,
            summaries: self.summaries.clone(),
        })
    }

    // Status

    fn derive_status(&self, needs_setup: bool) -> Status {
        match &self.link_status {
            Some(link) => Status::remote(link),
            None => Status::local(&self.connection, needs_setup),
        }
    }

    /// Derives `status`: a saved machine's from its link, Local's from its
    /// setup check and its sync (see `Status`).
    pub(super) fn refresh_status(&mut self, cx: &App) {
        let needs_setup = self
            .setup
            .as_ref()
            .is_some_and(|setup| setup.read(cx).needs_setup());
        self.status = self.derive_status(needs_setup);
    }

    /// Re-derives the status and announces it.
    pub(super) fn status_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_status(cx);
        cx.emit(SessionEvent::StatusChanged);
        cx.notify();
    }

    pub(super) fn set_label(&mut self, label: String, cx: &mut Context<Self>) {
        self.label = label;
        cx.notify();
    }

    fn link_status_changed(&mut self, status: MachineStatus, cx: &mut Context<Self>) {
        // Local has no link.
        let Some(previous) = self.link_status.clone() else {
            return;
        };
        if previous == status || self.gone {
            return;
        }
        tracing::info!("machine {}: {status:?}", self.label);
        if let Some(cue) = machine_list::status_cue(&previous, &status, self.lost) {
            sounds::play(cue, PlayOptions::default());
        }
        let connected = status == MachineStatus::Connected;
        if connected {
            self.lost = false;
        } else if previous == MachineStatus::Connected {
            self.lost = true;
        }
        self.link_status = Some(status);
        self.refresh_status(cx);
        // The forwarded socket is fixed for the link's life, so one sync
        // started on the first connection follows every reconnect.
        if connected && self.sync.is_none() {
            self.start_remote_sync(cx);
        }
        if connected {
            cx.emit(SessionEvent::Connected);
            // Its files can be read now: find the focused pane's worktree.
            self.focused_dir = None;
            self.follow_focused_pane(cx);
        }
        cx.emit(SessionEvent::StatusChanged);
        cx.notify();
    }

    /// Retries the connection now. For Local, checks herdr again when it
    /// isn't ready.
    pub(super) fn reconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(setup) = &self.setup {
            setup.update(cx, |setup, cx| {
                if !setup.is_ready() {
                    setup.check_herdr(cx);
                }
            });
        } else if let Some(link) = &self.link {
            link.reconnect();
        }
    }

    /// Takes the link out (it waits on ssh, so the caller stops it off the
    /// main thread).
    pub(super) fn take_link(&mut self) -> Option<MachineLink> {
        self.link.take()
    }

    /// Takes the session out of the list: drops its sync and client so every
    /// later command fails, and hands back its link to stop.
    pub(super) fn retire(&mut self) -> Option<MachineLink> {
        self.gone = true;
        self.client = None;
        self.sync = None;
        self._statuses = None;
        self.take_link()
    }

    // Sync

    /// Starts Local's sync thread, once. Whether it started now.
    pub(super) fn start_local_sync(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(config) = self.config.clone().filter(|_| self.sync.is_none()) else {
            return false;
        };
        match SessionSync::start(config) {
            Ok(sync) => {
                let task = Self::forward_updates(&sync, cx);
                self.sync = Some((sync, task));
                true
            }
            Err(error) => {
                self.connection = ConnectionStatus::Reconnecting {
                    reason: error.to_string(),
                };
                self.status_changed(cx);
                false
            }
        }
    }

    fn start_remote_sync(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &self.link else {
            return;
        };
        match SessionSync::start(link.endpoint()) {
            Ok(sync) => {
                self.client = Some(sync.client().clone());
                let task = Self::forward_updates(&sync, cx);
                self.sync = Some((sync, task));
            }
            Err(error) => {
                tracing::warn!("machine {}: {error}", self.label);
                self.connection = ConnectionStatus::Reconnecting {
                    reason: error.to_string(),
                };
            }
        }
    }

    fn forward_updates(sync: &SessionSync, cx: &mut Context<Self>) -> Task<()> {
        let updates = sync.updates();
        cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this.update(cx, |this, cx| this.apply(update, cx)).is_err() {
                    break;
                }
            }
        })
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::State(snapshot) => self.apply_snapshot(snapshot, cx),
            // The re-read it triggers arrives as a `State`.
            Update::Event { .. } => {}
            Update::Status(status) => {
                if let Some(setup) = &self.setup {
                    match (&self.connection, &status) {
                        (ConnectionStatus::Connected, ConnectionStatus::Reconnecting { .. }) => {
                            sounds::play(Cue::Warning, PlayOptions::default());
                        }
                        (ConnectionStatus::Reconnecting { .. }, ConnectionStatus::Connected) => {
                            sounds::play(Cue::Success, sounds::subtle());
                        }
                        _ => {}
                    }
                    if !matches!(status, ConnectionStatus::Connected) {
                        setup.update(cx, |setup, _| setup.connection_lost());
                    }
                }
                self.connection = status;
                self.status_changed(cx);
            }
        }
    }

    /// Replaces the cache, as a sync update does, and announces what changed
    /// (in the order `SessionEvent` documents).
    pub fn apply_snapshot(&mut self, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        if let Some(setup) = &self.setup {
            setup.update(cx, |setup, cx| setup.server_seen(&snapshot.version, cx));
        }
        let focused = snapshot.focused_pane_id.clone();
        let changes = self.attention.update(&snapshot);
        self.snapshot = snapshot;
        let focus_changed = self.focused_pane != focused;
        self.focused_pane.clone_from(&focused);
        cx.emit(SessionEvent::SnapshotReplaced);
        for change in changes {
            cx.emit(SessionEvent::AgentStatusChanged(change));
        }
        if focus_changed {
            cx.emit(SessionEvent::FocusedPaneChanged(focused));
        }
        self.follow_focused_pane(cx);
        cx.notify();
    }

    // Worktree

    /// Sets the active worktree.
    pub fn set_worktree(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.worktree == root {
            return;
        }
        self.worktree = root;
        cx.emit(SessionEvent::WorktreeChanged);
        cx.notify();
    }

    /// Re-roots on the focused pane's `foreground_cwd` (falling back to
    /// `cwd`), so it also follows an agent that moves into another folder.
    /// With no focused pane, or a folder that doesn't exist there, the root
    /// stays put.
    fn follow_focused_pane(&mut self, cx: &mut Context<Self>) {
        let dir = self
            .snapshot
            .focused_pane()
            .and_then(herdr::PaneInfo::working_dir)
            .map(PathBuf::from);
        self.follow_dir(dir, cx);
    }

    /// Re-reads the focused pane's folder (one `pane.get`) and follows it:
    /// herdr 0.9.5 sends no event when it changes.
    pub(super) fn poll_focused_dir(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(pane_id) = self.focused_pane.clone() else {
            return Task::ready(());
        };
        let request = self.call(cx, move |client| client.pane_get(&pane_id));
        cx.spawn(async move |this, cx| {
            let Ok(pane) = request.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.focused_pane.as_ref() == Some(&pane.pane_id) {
                    this.follow_dir(pane.working_dir().map(PathBuf::from), cx);
                }
            })
            .ok();
        })
    }

    /// Resolves `dir` to its worktree (see `Fs::resolve_root`): at once where
    /// that's cheap (`Caps::cheap`), on the background executor elsewhere. A
    /// machine whose files can't be read yet tries again on the next
    /// snapshot, poll or connect.
    pub(super) fn follow_dir(&mut self, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(dir) = dir else { return };
        let Some(fs) = self.fs() else { return };
        if self.focused_dir.as_ref() == Some(&dir) {
            return;
        }
        self.focused_dir = Some(dir.clone());
        if fs.caps().cheap {
            if let Some(root) = fs.resolve_root(&dir) {
                self.set_worktree(Some(root), cx);
            }
            return;
        }
        let resolve = cx.background_executor().spawn({
            let dir = dir.clone();
            async move { fs.resolve_root(&dir) }
        });
        cx.spawn(async move |this, cx| {
            let root = resolve.await;
            this.update(cx, |this, cx| match root {
                // A pane that has moved on since.
                _ if this.focused_dir.as_ref() != Some(&dir) => {}
                Some(root) => this.set_worktree(Some(root), cx),
                // Ask again next time.
                None => this.focused_dir = None,
            })
            .ok();
        })
        .detach();
    }

    // Commands

    /// Runs one blocking herdr call on this machine, on the background
    /// executor. The typed methods below cover the common ones; use this for
    /// anything else.
    #[track_caller]
    pub fn call<T: Send + 'static>(
        &self,
        cx: &App,
        call: impl FnOnce(&Client) -> herdr::Result<T> + Send + 'static,
    ) -> Task<herdr::Result<T>> {
        // Every herdr command error is logged here, whatever the caller does
        // with it, so a detached or `.ok()`ed command never fails silently.
        let caller = std::panic::Location::caller();
        let machine = &self.id;
        if self.gone {
            tracing::warn!("herdr call on {machine} from {caller} skipped: no such machine");
            return Task::ready(Err(gone(machine)));
        }
        let Some(client) = self.client.clone() else {
            tracing::warn!("herdr call on {machine} from {caller} skipped: disconnected");
            return Task::ready(Err(herdr::Error::Refused(
                "herdr is disconnected; see the connection status".into(),
            )));
        };
        cx.background_executor().spawn(async move {
            call(&client)
                .inspect_err(|error| tracing::warn!("herdr call from {caller} failed: {error}"))
        })
    }

    // Panes

    /// Focuses `pane_id`. To show it, select this machine too
    /// (`AppState::focus_pane` does both).
    #[track_caller]
    pub fn focus_pane(&self, pane_id: &PaneId, cx: &App) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_focus(&pane_id))
    }

    /// Splits `pane_id` (herdr's focused pane when `None`) and focuses the new
    /// pane. `TerminalArea` sets its right-click target, as for every pane.
    #[track_caller]
    pub fn split_pane(
        &self,
        pane_id: Option<&PaneId>,
        direction: herdr::SplitDirection,
        cx: &App,
    ) -> Task<herdr::Result<herdr::PaneInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let mut split = herdr::PaneSplit::new(pane_id.cloned(), direction);
        split.focus = true;
        self.call(cx, move |c| c.pane_split(&split))
    }

    #[track_caller]
    pub fn close_pane(&self, pane_id: &PaneId, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::subtle());
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_close(&pane_id))
    }

    #[track_caller]
    pub fn swap_panes(
        &self,
        source: &PaneId,
        target: &PaneId,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        let (source, target) = (source.to_owned(), target.to_owned());
        self.call(cx, move |c| c.pane_swap(&source, &target).map(drop))
    }

    #[track_caller]
    pub fn move_pane(
        &self,
        pane_id: &PaneId,
        destination: herdr::PaneMoveDestination,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| {
            c.pane_move(&pane_id, &destination, true).map(drop)
        })
    }

    #[track_caller]
    pub fn set_right_click(
        &self,
        pane_id: &PaneId,
        target: herdr::RightClickTarget,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_set_right_click(&pane_id, target))
    }

    /// Sends a divider's final ratio. `path` comes from `SplitTree::splits`.
    #[track_caller]
    pub fn set_split_ratio(
        &self,
        tab_id: &TabId,
        path: Vec<bool>,
        ratio: f32,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| {
            c.layout_set_split_ratio(&tab_id, &path, ratio).map(drop)
        })
    }

    // Tabs

    /// Focuses `tab_id`, with a Select cue when it isn't focused yet. To
    /// show it, select this machine too (`AppState::focus_tab` does both).
    #[track_caller]
    pub fn focus_tab(&self, tab_id: &TabId, cx: &App) -> Task<herdr::Result<()>> {
        if self.snapshot.focused_tab_id.as_deref() != Some(tab_id) {
            sounds::play(Cue::Select, PlayOptions::default());
        }
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_focus(&tab_id))
    }

    #[track_caller]
    pub fn new_tab(
        &self,
        workspace_id: Option<&WorkspaceId>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::TabInfo>> {
        self.new_tab_named(workspace_id, None, cx)
    }

    /// A new, focused tab named `label` (herdr names it when `None`).
    #[track_caller]
    pub fn new_tab_named(
        &self,
        workspace_id: Option<&WorkspaceId>,
        label: Option<String>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::TabInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let params = herdr::NewTab {
            workspace_id: workspace_id.cloned(),
            label,
            focus: true,
            ..Default::default()
        };
        self.call(cx, move |c| c.tab_create(&params).map(|t| t.tab))
    }

    #[track_caller]
    pub fn rename_tab(&self, tab_id: &TabId, label: &str, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        let (tab_id, label) = (tab_id.to_owned(), label.to_owned());
        self.call(cx, move |c| c.tab_rename(&tab_id, &label))
    }

    #[track_caller]
    pub fn move_tab(&self, tab_id: &TabId, index: usize, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_move(&tab_id, index))
    }

    #[track_caller]
    pub fn close_tab(&self, tab_id: &TabId, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::subtle());
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_close(&tab_id))
    }

    // Workspaces and worktrees

    /// Whether `workspace_id` is herdr's focused workspace here.
    pub fn is_focused_workspace(&self, workspace_id: &WorkspaceId) -> bool {
        self.snapshot.focused_workspace_id.as_ref() == Some(workspace_id)
    }

    /// Focuses `workspace_id`, with a Select cue when it isn't focused yet.
    /// To show it, select this machine too (`AppState::focus_workspace` does
    /// both).
    #[track_caller]
    pub fn focus_workspace(&self, workspace_id: &WorkspaceId, cx: &App) -> Task<herdr::Result<()>> {
        if !self.is_focused_workspace(workspace_id) {
            sounds::play(Cue::Select, PlayOptions::default());
        }
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.workspace_focus(&id))
    }

    /// A new, focused workspace. The caller selects the machine to show it.
    #[track_caller]
    pub fn new_workspace(
        &self,
        cwd: Option<PathBuf>,
        label: Option<String>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let params = herdr::NewWorkspace {
            cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
            label,
            focus: true,
            ..Default::default()
        };
        self.call(cx, move |c| {
            c.workspace_create(&params).map(|w| w.workspace)
        })
    }

    #[track_caller]
    pub fn rename_workspace(
        &self,
        workspace_id: &WorkspaceId,
        label: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        let (id, label) = (workspace_id.to_owned(), label.to_owned());
        self.call(cx, move |c| c.workspace_rename(&id, &label))
    }

    /// Puts every workspace in `order` (all of them, first to last).
    #[track_caller]
    pub fn reorder_workspaces(&self, order: Vec<WorkspaceId>, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        self.call(cx, move |c| c.workspace_move_block(&order, None))
    }

    /// Closes immediately; confirm first if it holds running agents.
    #[track_caller]
    pub fn close_workspace(
        &self,
        workspace_id: &WorkspaceId,
        close_group: bool,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::strong());
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.workspace_close(&id, close_group))
    }

    /// Closes each of `workspace_ids` with its own `workspace.close`, all of
    /// them even when one fails (the first error is returned). For a group
    /// herdr's `close_group` doesn't define.
    #[track_caller]
    pub fn close_workspaces(
        &self,
        workspace_ids: Vec<WorkspaceId>,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::strong());
        self.call(cx, move |c| {
            let mut outcome = Ok(());
            for id in &workspace_ids {
                let closed = c.workspace_close(id, false);
                if outcome.is_ok() {
                    outcome = closed;
                }
            }
            outcome
        })
    }

    #[track_caller]
    pub fn create_worktree(
        &self,
        params: herdr::WorktreeParams,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
        self.call(cx, move |c| c.worktree_create(&params).map(|w| w.workspace))
    }

    /// The worktrees of `workspace_id`'s repo (`worktree.list`).
    #[track_caller]
    pub fn list_worktrees(
        &self,
        workspace_id: &WorkspaceId,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorktreeList>> {
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.worktree_list(Some(&id), None))
    }

    /// Opens an existing worktree as a workspace (`worktree.open`); herdr
    /// focuses the open one if there is one.
    #[track_caller]
    pub fn open_worktree(
        &self,
        params: herdr::WorktreeParams,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceCreated>> {
        self.call(cx, move |c| c.worktree_open(&params))
    }

    /// `worktree.remove`: deletes the linked worktree's checkout and closes
    /// its workspace. Without `force` herdr refuses a dirty checkout.
    ///
    /// herdr removes only workspaces it knows as worktrees. One made with a
    /// plain `workspace.create` in a linked checkout has no worktree info
    /// (the spaces list found it with git), so herdr first adopts it:
    /// `worktree.list` names the checkout that workspace has open, and
    /// `worktree.open` on that path answers `already_open` and records it.
    #[track_caller]
    pub fn remove_worktree(
        &self,
        workspace_id: &WorkspaceId,
        force: bool,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorktreeRemoved>> {
        let adopt = self
            .snapshot
            .workspace(workspace_id)
            .is_some_and(|w| w.info.worktree.is_none());
        let id = workspace_id.to_owned();
        self.call(cx, move |c| {
            if adopt {
                adopt_worktree(c, &id)?;
            }
            c.worktree_remove(&id, force)
        })
    }

    // Plugins

    #[track_caller]
    pub fn plugin_actions(&self, cx: &App) -> Task<herdr::Result<Vec<herdr::PluginActionInfo>>> {
        self.call(cx, |c| c.plugin_action_list(None))
    }

    #[track_caller]
    pub fn invoke_plugin_action(
        &self,
        action: &herdr::PluginActionInfo,
        context: herdr::PluginInvocationContext,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let (plugin_id, action_id) = (action.plugin_id.clone(), action.action_id.clone());
        self.call(cx, move |c| {
            c.plugin_action_invoke(Some(&plugin_id), &action_id, Some(&context))
                .map(drop)
        })
    }
}

/// The error for a command aimed at a machine no longer in the list.
pub(super) fn gone(machine: &MachineId) -> herdr::Error {
    herdr::Error::Refused(format!("machine {machine} is no longer saved"))
}

/// Has herdr record `workspace_id`'s linked checkout as its worktree, so
/// `worktree.remove` accepts it. Does nothing when herdr lists no linked
/// checkout open in that workspace; the remove then says why.
fn adopt_worktree(client: &herdr::Client, workspace_id: &WorkspaceId) -> herdr::Result<()> {
    let list = client.worktree_list(Some(workspace_id), None)?;
    let Some(checkout) = list
        .worktrees
        .into_iter()
        .find(|w| w.is_linked_worktree && w.open_workspace_id.as_deref() == Some(workspace_id))
    else {
        return Ok(());
    };
    let params = herdr::WorktreeParams {
        workspace_id: Some(workspace_id.to_owned()),
        path: Some(checkout.path),
        focus: false,
        ..herdr::WorktreeParams::default()
    };
    client.worktree_open(&params).map(drop)
}
