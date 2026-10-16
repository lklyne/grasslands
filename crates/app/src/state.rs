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

use gpui_kit::*;

use crate::herdr::{self, Client, ConnectionStatus, PaneId, SessionSync, Snapshot, Update};

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
}

pub struct AppState {
    pub herdr_config: herdr::Config,
    pub connection: ConnectionStatus,
    /// The latest herdr cache: workspaces → tabs → split trees, panes, agents.
    pub snapshot: Arc<Snapshot>,
    pub focused_pane: Option<PaneId>,
    active_worktree: Option<PathBuf>,
    client: Client,
    _sync: Option<(SessionSync, Task<()>)>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    /// Connects to herdr (starting its server if needed). Under `cargo test`
    /// it stays offline: GPUI's test scheduler rejects wakeups from the sync
    /// thread, and tests must never reach a real herdr by accident.
    pub fn new(herdr_config: herdr::Config, cx: &mut Context<Self>) -> Self {
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
        Self {
            client: Client::new(herdr_config.clone()),
            herdr_config,
            connection,
            snapshot: Arc::default(),
            focused_pane: None,
            active_worktree: std::env::current_dir().ok(),
            _sync: sync,
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
            Update::State(snapshot) => self.replace_snapshot(snapshot, cx),
            Update::Event(event) => cx.emit(AppEvent::Herdr(Arc::from(event))),
            Update::Status(status) => {
                self.connection = status;
                cx.notify();
            }
        }
    }

    pub fn active_worktree(&self) -> Option<&PathBuf> {
        self.active_worktree.as_ref()
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
        self.snapshot = snapshot;
        cx.emit(AppEvent::SnapshotReplaced);
        if self.focused_pane != focused {
            self.focused_pane.clone_from(&focused);
            cx.emit(AppEvent::FocusedPaneChanged(focused));
        }
        cx.notify();
    }

    /// Runs one blocking herdr call on the background executor. The typed
    /// methods below cover the common ones; use this for anything else.
    pub fn call<T: Send + 'static>(
        &self,
        cx: &App,
        call: impl FnOnce(&Client) -> herdr::Result<T> + Send + 'static,
    ) -> Task<herdr::Result<T>> {
        let client = self.client.clone();
        cx.background_executor().spawn(async move { call(&client) })
    }

    // Panes

    pub fn focus_pane(&self, pane_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_focus(&pane_id))
    }

    /// Splits `pane_id` (herdr's focused pane when `None`). New panes forward
    /// right-clicks to the program in them; our context menu handles the rest.
    pub fn split_pane(
        &self,
        pane_id: Option<&str>,
        direction: herdr::SplitDirection,
        cx: &App,
    ) -> Task<herdr::Result<herdr::PaneInfo>> {
        let mut split = herdr::PaneSplit::new(pane_id.map(str::to_owned), direction);
        split.focus = true;
        split.right_click = Some(herdr::RightClickTarget::Pane);
        self.call(cx, move |c| c.pane_split(&split))
    }

    pub fn close_pane(&self, pane_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_close(&pane_id))
    }

    pub fn swap_panes(&self, source: &str, target: &str, cx: &App) -> Task<herdr::Result<()>> {
        let (source, target) = (source.to_owned(), target.to_owned());
        self.call(cx, move |c| c.pane_swap(&source, &target).map(drop))
    }

    pub fn move_pane(
        &self,
        pane_id: &str,
        destination: herdr::PaneMoveDestination,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| {
            c.pane_move(&pane_id, &destination, true).map(drop)
        })
    }

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

    pub fn focus_tab(&self, tab_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_focus(&tab_id))
    }

    pub fn new_tab(
        &self,
        workspace_id: Option<&str>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::TabInfo>> {
        let params = herdr::NewTab {
            workspace_id: workspace_id.map(str::to_owned),
            focus: true,
            ..Default::default()
        };
        self.call(cx, move |c| c.tab_create(&params).map(|t| t.tab))
    }

    pub fn rename_tab(&self, tab_id: &str, label: &str, cx: &App) -> Task<herdr::Result<()>> {
        let (tab_id, label) = (tab_id.to_owned(), label.to_owned());
        self.call(cx, move |c| c.tab_rename(&tab_id, &label))
    }

    pub fn move_tab(&self, tab_id: &str, index: usize, cx: &App) -> Task<herdr::Result<()>> {
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_move(&tab_id, index))
    }

    pub fn close_tab(&self, tab_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_close(&tab_id))
    }

    // Workspaces and worktrees

    pub fn focus_workspace(&self, workspace_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.workspace_focus(&id))
    }

    pub fn new_workspace(
        &self,
        cwd: Option<PathBuf>,
        label: Option<String>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
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

    pub fn rename_workspace(
        &self,
        workspace_id: &str,
        label: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let (id, label) = (workspace_id.to_owned(), label.to_owned());
        self.call(cx, move |c| c.workspace_rename(&id, &label))
    }

    /// Closes immediately; confirm first if it holds running agents.
    pub fn close_workspace(
        &self,
        workspace_id: &str,
        close_group: bool,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.workspace_close(&id, close_group))
    }

    pub fn create_worktree(
        &self,
        params: herdr::WorktreeParams,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
        self.call(cx, move |c| c.worktree_create(&params).map(|w| w.workspace))
    }

    // Agents and plugins

    pub fn focus_agent(&self, target: &str, cx: &App) -> Task<herdr::Result<()>> {
        let target = target.to_owned();
        self.call(cx, move |c| c.agent_focus(&target))
    }

    pub fn plugin_actions(&self, cx: &App) -> Task<herdr::Result<Vec<herdr::PluginActionInfo>>> {
        self.call(cx, |c| c.plugin_action_list(None))
    }

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
