//! `AppState`: the one shared model every view reads.
//!
//! It holds the herdr cache and the active worktree. Views keep an
//! `Entity<AppState>`, read it in `render`, and `cx.observe` it to re-render.
//! Discrete changes that views must react to (not just redraw) are emitted
//! as `AppEvent`s; views `cx.subscribe` to those.
//!
//! The herdr connection runs on `herdr-client`'s own thread (`SessionSync`);
//! its updates are applied here on the foreground. Commands go through the
//! methods below, which run the blocking socket call on the background
//! executor and return a task: `.detach_and_log_err(cx)` it, or await it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;

use crate::attention::{Attention, StatusChange};
use crate::herdr::{self, Client, ConnectionStatus, PaneId, SessionSync, Snapshot, Update};
use crate::{sounds, worktree};
use cues::{Cue, PlayOptions};

/// herdr 0.9.5 sends no event when a pane's folder changes, so the focused
/// pane's folder is re-read this often (one `pane.get`).
const CWD_POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The focused pane's worktree root changed. The file tree re-roots and
    /// the editor swaps to that worktree's tab set.
    ActiveWorktreeChanged(Option<PathBuf>),
    /// herdr's cache was replaced (any change: startup, events, reconnect).
    /// The new one is `AppState::snapshot`.
    SnapshotReplaced,
    /// herdr's focused pane changed.
    FocusedPaneChanged(Option<PaneId>),
    /// A herdr event as it arrived, before the cache re-read it triggers.
    /// For reactions the cache can't express (toasts, "pane just created").
    Herdr(Arc<herdr::Event>),
    /// A known agent's status changed between two caches (chrome toasts it).
    AgentStatusChanged(StatusChange),
    /// The file tree renamed or moved a file or folder; open editor tabs at or
    /// under `from` follow it to `to`.
    FileMoved { from: PathBuf, to: PathBuf },
    /// The file tree moved a file or folder to the Trash.
    FileTrashed(PathBuf),
}

pub struct AppState {
    /// `None` when the endpoint was refused.
    pub herdr_config: Option<herdr::Config>,
    pub connection: ConnectionStatus,
    /// The latest herdr cache: workspaces → tabs → split trees, panes, agents.
    pub snapshot: Arc<Snapshot>,
    pub focused_pane: Option<PaneId>,
    /// Status changes and unseen results, for toasts and badges.
    pub attention: Attention,
    active_worktree: Option<PathBuf>,
    /// The focused pane's folder the active worktree was last resolved from.
    focused_dir: Option<PathBuf>,
    /// `None` when the endpoint was refused.
    client: Option<Client>,
    _sync: Option<(SessionSync, Task<()>)>,
    _cwd_poll: Option<Task<()>>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    /// Connects to herdr (starting its server if needed). A refused endpoint
    /// (see `herdr::Config::from_env`) leaves the app disconnected: no sync,
    /// no client, every call fails. Under `cargo test` it stays offline: GPUI's
    /// test scheduler rejects wakeups from the sync thread, and tests must
    /// never reach a real herdr by accident.
    pub fn new(herdr_config: herdr::Result<herdr::Config>, cx: &mut Context<Self>) -> Self {
        let herdr_config = match herdr_config {
            Ok(config) => config,
            Err(error) => {
                tracing::error!("herdr: {error}");
                return Self::disconnected(error.to_string());
            }
        };
        // `from_env` configs auto-start a server; tests must use an inert one.
        #[cfg(test)]
        assert!(
            !herdr_config.auto_start,
            "tests must not build AppState from the environment; use an auto_start: false config"
        );
        let mut connection = ConnectionStatus::Connecting;
        let sync = if cfg!(test) {
            None
        } else {
            match SessionSync::start(herdr_config.clone()) {
                Ok(sync) => {
                    let task = Self::forward_updates(&sync, cx);
                    Some((sync, task))
                }
                Err(error) => {
                    connection = ConnectionStatus::Reconnecting {
                        reason: error.to_string(),
                    };
                    None
                }
            }
        };
        let cwd_poll = sync.is_some().then(|| Self::poll_focused_dir(cx));
        Self {
            client: Some(Client::new(herdr_config.clone())),
            herdr_config: Some(herdr_config),
            connection,
            _sync: sync,
            _cwd_poll: cwd_poll,
            ..Self::disconnected(String::new())
        }
    }

    fn disconnected(reason: String) -> Self {
        Self {
            herdr_config: None,
            connection: ConnectionStatus::Disconnected { reason },
            snapshot: Arc::default(),
            focused_pane: None,
            attention: Attention::default(),
            active_worktree: std::env::current_dir().ok(),
            focused_dir: None,
            client: None,
            _sync: None,
            _cwd_poll: None,
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

    /// Follows the focused pane into another folder (`cd`, or an agent
    /// changing directory), which herdr reports no event for.
    fn poll_focused_dir(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CWD_POLL).await;
                let Ok(request) = this.update(cx, |this, cx| {
                    let pane_id = this.focused_pane.clone()?;
                    Some(this.call(cx, move |client| client.pane_get(&pane_id)))
                }) else {
                    break;
                };
                let Some(request) = request else {
                    continue;
                };
                let Ok(pane) = request.await else {
                    continue;
                };
                let followed = this.update(cx, |this, cx| {
                    if this.focused_pane.as_ref() == Some(&pane.pane_id) {
                        this.follow_dir(pane.working_dir().map(PathBuf::from), cx);
                    }
                });
                if followed.is_err() {
                    break;
                }
            }
        })
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::State(snapshot) => self.replace_snapshot(snapshot, cx),
            Update::Event(event) => cx.emit(AppEvent::Herdr(Arc::from(event))),
            Update::Status(status) => {
                match (&self.connection, &status) {
                    (ConnectionStatus::Connected, ConnectionStatus::Reconnecting { .. }) => {
                        sounds::play(Cue::Warning, PlayOptions::default());
                    }
                    (ConnectionStatus::Reconnecting { .. }, ConnectionStatus::Connected) => {
                        sounds::play(Cue::Success, sounds::subtle());
                    }
                    _ => {}
                }
                self.connection = status;
                cx.notify();
            }
        }
    }

    pub fn active_worktree(&self) -> Option<&PathBuf> {
        self.active_worktree.as_ref()
    }

    /// Announces a rename done in the app (see `AppEvent::FileMoved`).
    pub fn file_moved(&mut self, from: PathBuf, to: PathBuf, cx: &mut Context<Self>) {
        cx.emit(AppEvent::FileMoved { from, to });
    }

    /// Announces a trash done in the app (see `AppEvent::FileTrashed`).
    pub fn file_trashed(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(AppEvent::FileTrashed(path));
    }

    pub fn set_active_worktree(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.active_worktree == root {
            return;
        }
        self.active_worktree.clone_from(&root);
        cx.emit(AppEvent::ActiveWorktreeChanged(root));
        cx.notify();
    }

    pub fn replace_snapshot(&mut self, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        let focused = snapshot.focused_pane_id.clone();
        let changes = self.attention.update(&snapshot);
        self.snapshot = snapshot;
        cx.emit(AppEvent::SnapshotReplaced);
        for change in changes {
            cx.emit(AppEvent::AgentStatusChanged(change));
        }
        if self.focused_pane != focused {
            self.focused_pane.clone_from(&focused);
            cx.emit(AppEvent::FocusedPaneChanged(focused));
        }
        self.follow_focused_pane(cx);
        cx.notify();
    }

    /// Re-roots on the focused pane's `foreground_cwd` (falling back to `cwd`),
    /// so it also follows an agent that moves into another folder. With no
    /// focused pane, or a folder that doesn't exist here, the root stays put.
    fn follow_focused_pane(&mut self, cx: &mut Context<Self>) {
        let dir = self
            .snapshot
            .focused_pane()
            .and_then(herdr::PaneInfo::working_dir)
            .map(PathBuf::from);
        self.follow_dir(dir, cx);
    }

    fn follow_dir(&mut self, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        if dir.is_none() || dir == self.focused_dir {
            return;
        }
        self.focused_dir.clone_from(&dir);
        if let Some(root) = dir.as_deref().and_then(worktree::resolve_root) {
            self.set_active_worktree(Some(root), cx);
        }
    }

    /// Runs one blocking herdr call on the background executor. The typed
    /// methods below cover the common ones; use this for anything else.
    #[track_caller]
    pub fn call<T: Send + 'static>(
        &self,
        cx: &App,
        call: impl FnOnce(&Client) -> herdr::Result<T> + Send + 'static,
    ) -> Task<herdr::Result<T>> {
        // Every herdr command error is logged here, whatever the caller does
        // with it, so a detached or `.ok()`ed command never fails silently.
        let caller = std::panic::Location::caller();
        let Some(client) = self.client.clone() else {
            tracing::warn!("herdr call from {caller} skipped: disconnected");
            return Task::ready(Err(herdr::Error::Refused(
                "herdr is disconnected; see the connection status".into(),
            )));
        };
        cx.background_executor().spawn(async move {
            call(&client)
                .inspect_err(|error| tracing::warn!("herdr call from {caller} failed: {error}"))
        })
    }

    /// The user looked at `pane_id` (focused it, clicked its agent).
    pub fn mark_seen(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        if self.attention.mark_seen(&self.snapshot, pane_id) {
            cx.notify();
        }
    }

    // Panes

    #[track_caller]
    pub fn focus_pane(&self, pane_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_focus(&pane_id))
    }

    /// Splits `pane_id` (herdr's focused pane when `None`). New panes forward
    /// right-clicks to the program in them; our context menu handles the rest.
    #[track_caller]
    pub fn split_pane(
        &self,
        pane_id: Option<&str>,
        direction: herdr::SplitDirection,
        cx: &App,
    ) -> Task<herdr::Result<herdr::PaneInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let mut split = herdr::PaneSplit::new(pane_id.map(str::to_owned), direction);
        split.focus = true;
        split.right_click = Some(herdr::RightClickTarget::Pane);
        self.call(cx, move |c| c.pane_split(&split))
    }

    #[track_caller]
    pub fn close_pane(&self, pane_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::subtle());
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_close(&pane_id))
    }

    #[track_caller]
    pub fn swap_panes(&self, source: &str, target: &str, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        let (source, target) = (source.to_owned(), target.to_owned());
        self.call(cx, move |c| c.pane_swap(&source, &target).map(drop))
    }

    #[track_caller]
    pub fn move_pane(
        &self,
        pane_id: &str,
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
        pane_id: &str,
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
        tab_id: &str,
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

    #[track_caller]
    pub fn focus_tab(&self, tab_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        if self.snapshot.focused_tab_id.as_deref() != Some(tab_id) {
            sounds::play(Cue::Select, PlayOptions::default());
        }
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_focus(&tab_id))
    }

    #[track_caller]
    pub fn new_tab(
        &self,
        workspace_id: Option<&str>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::TabInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let params = herdr::NewTab {
            workspace_id: workspace_id.map(str::to_owned),
            focus: true,
            ..Default::default()
        };
        self.call(cx, move |c| c.tab_create(&params).map(|t| t.tab))
    }

    #[track_caller]
    pub fn rename_tab(&self, tab_id: &str, label: &str, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        let (tab_id, label) = (tab_id.to_owned(), label.to_owned());
        self.call(cx, move |c| c.tab_rename(&tab_id, &label))
    }

    #[track_caller]
    pub fn move_tab(&self, tab_id: &str, index: usize, cx: &App) -> Task<herdr::Result<()>> {
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_move(&tab_id, index))
    }

    #[track_caller]
    pub fn close_tab(&self, tab_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::subtle());
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_close(&tab_id))
    }

    // Workspaces and worktrees

    #[track_caller]
    pub fn focus_workspace(&self, workspace_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        if self.snapshot.focused_workspace_id.as_deref() != Some(workspace_id) {
            sounds::play(Cue::Select, PlayOptions::default());
        }
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.workspace_focus(&id))
    }

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
        workspace_id: &str,
        label: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        let (id, label) = (workspace_id.to_owned(), label.to_owned());
        self.call(cx, move |c| c.workspace_rename(&id, &label))
    }

    /// Puts every workspace in `order` (all of them, first to last).
    #[track_caller]
    pub fn reorder_workspaces(&self, order: Vec<String>, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        self.call(cx, move |c| c.workspace_move_block(&order, None))
    }

    /// Closes immediately; confirm first if it holds running agents.
    #[track_caller]
    pub fn close_workspace(
        &self,
        workspace_id: &str,
        close_group: bool,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::strong());
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.workspace_close(&id, close_group))
    }

    #[track_caller]
    pub fn create_worktree(
        &self,
        params: herdr::WorktreeParams,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
        self.call(cx, move |c| c.worktree_create(&params).map(|w| w.workspace))
    }

    // Agents and plugins

    #[track_caller]
    pub fn focus_agent(&self, target: &str, cx: &App) -> Task<herdr::Result<()>> {
        let target = target.to_owned();
        self.call(cx, move |c| c.agent_focus(&target))
    }

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

/// A config that can never reach a herdr: no server start, a socket nobody
/// listens on. For tests that need an `AppState`.
#[cfg(test)]
pub fn offline_config() -> herdr::Config {
    herdr::Config {
        bin: "/nonexistent/herdr".into(),
        session: None,
        socket_path: std::env::temp_dir().join("grasslands-test-no-herdr.sock"),
        auto_start: false,
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::{AppState, offline_config};
    use crate::herdr::{self, ConnectionStatus};

    fn refused() -> herdr::Error {
        herdr::Error::Refused("socket outside the dev root".into())
    }

    #[gpui::test]
    fn following_a_folder_inside_a_repo_roots_at_the_repo(cx: &mut TestAppContext) {
        let repo =
            std::env::temp_dir().join(format!("grasslands-state-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git2::Repository::init(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, cx| state.follow_dir(Some(repo.join("src")), cx));
        let root = cx.read(|cx| state.read(cx).active_worktree().cloned());
        assert_eq!(root, Some(repo));
    }

    #[gpui::test]
    fn refused_endpoint_leaves_app_disconnected(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Err(refused()), cx));
        let connection = cx.read(|cx| state.read(cx).connection.clone());
        assert!(matches!(connection, ConnectionStatus::Disconnected { .. }));
    }

    #[gpui::test]
    fn refused_endpoint_fails_every_call(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Err(refused()), cx));
        let task = cx.update(|cx| state.read(cx).focus_pane("w1:p1", cx));
        let result = cx.foreground_executor().block_test(task);
        assert!(matches!(result, Err(herdr::Error::Refused(_))));
    }

    #[gpui::test]
    #[should_panic(expected = "tests must not build AppState from the environment")]
    fn tests_cannot_use_an_auto_starting_config(cx: &mut TestAppContext) {
        let config = herdr::Config {
            auto_start: true,
            ..offline_config()
        };
        cx.new(|cx| AppState::new(Ok(config), cx));
    }
}
