//! The herdr methods this crate sends, typed both ways: one [`Method`] per
//! method, naming its params and what it answers. `Client::send::<M>` is
//! the one way to call them; `Client`'s named wrappers sit on top.
//!
//! The table is hand-written, not generated from the schema: it covers only
//! the methods we call, under readable names. `tests` checks it against
//! `docs/herdr-api-0.9.5.schema.json`, so a herdr upgrade that renames a
//! method or changes its params fails there.

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{
    ConfigReload, NewTab, NewWorkspace, NotificationShown, PaneMoveDestination, PaneMoveResult,
    PaneSplit as SplitParams, PaneSwapResult, PluginActionInvoked, TabCreated, WorkspaceCreated,
    WorktreeList as WorktreeListing, WorktreeParams, WorktreeRemoved,
};
use crate::ids::{PaneId, TabId, WorkspaceId};
use crate::types::*;

/// One herdr socket method.
pub trait Method {
    /// The `method` herdr knows it by.
    const NAME: &'static str;
    /// The `result.type` herdr answers with.
    const RESULT: &'static str;
    /// The `result` field holding [`Self::Output`]; the whole result when
    /// `None`.
    const FIELD: Option<&'static str>;
    type Params: Serialize;
    /// What the caller gets; [`IgnoredAny`] when only success matters.
    type Output: DeserializeOwned;
}

/// Params of a method that takes none.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct NoParams {}

#[derive(Debug, Clone, Serialize)]
pub struct PaneTarget {
    pub pane_id: PaneId,
}

#[derive(Debug, Clone, Serialize)]
pub struct TabTarget {
    pub tab_id: TabId,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceTarget {
    pub workspace_id: WorkspaceId,
}

/// A pane id or an agent name.
#[derive(Debug, Clone, Serialize)]
pub struct AgentTarget {
    pub target: String,
}

/// Narrows a list to one workspace; all of them when `None`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct InWorkspace {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LayoutTarget {
    /// herdr's focused tab when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<TabId>,
}

/// Sets the ratio of the split at `path` (from the root: `false` = first
/// child, `true` = second child; empty = the root split).
#[derive(Debug, Clone, Serialize)]
pub struct SplitRatio {
    pub tab_id: TabId,
    pub path: Vec<bool>,
    pub ratio: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PaneSwapParams {
    Panes {
        source_pane_id: PaneId,
        target_pane_id: PaneId,
    },
    Toward {
        pane_id: PaneId,
        direction: PaneDirection,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneMoveParams {
    pub pane_id: PaneId,
    pub destination: PaneMoveDestination,
    pub focus: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneRenameParams {
    pub pane_id: PaneId,
    /// Clears the label when `None`.
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomMode {
    Toggle,
    On,
    Off,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneZoomParams {
    pub pane_id: PaneId,
    pub mode: ZoomMode,
}

/// Who gets right-clicks in the pane (`pane.input.set`).
#[derive(Debug, Clone, Serialize)]
pub struct PaneInputSetParams {
    pub pane_id: PaneId,
    pub right_click: RightClickTarget,
}

/// Text typed into the pane (`\r` is Enter), then named keys (`"Enter"`).
#[derive(Debug, Clone, Serialize)]
pub struct PaneSendInputParams {
    pub pane_id: PaneId,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

/// An agent state reported for a pane, as an agent's hook would.
#[derive(Debug, Clone, Serialize)]
pub struct PaneReportAgentParams {
    pub pane_id: PaneId,
    /// Who reports it, e.g. a hook's name.
    pub source: String,
    pub agent: String,
    pub state: ReportedState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportedState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    /// What is on screen.
    Visible,
    /// The last lines of scrollback.
    Recent,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneReadParams {
    pub pane_id: PaneId,
    pub source: ReadSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
}

/// `pane.read`'s text (the rest of its answer is left out).
#[derive(Debug, Clone, Deserialize)]
pub struct PaneText {
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TabRenameParams {
    pub tab_id: TabId,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TabMoveParams {
    pub tab_id: TabId,
    pub insert_index: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceRenameParams {
    pub workspace_id: WorkspaceId,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceMoveParams {
    pub workspace_id: WorkspaceId,
    pub insert_index: usize,
}

/// Moves `workspace_ids`, in that order, to just before
/// `before_workspace_id`, or to the end when `None`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceMoveBlockParams {
    pub workspace_ids: Vec<WorkspaceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_workspace_id: Option<WorkspaceId>,
}

/// `close_group` also closes the workspace's worktree group.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceCloseParams {
    pub workspace_id: WorkspaceId,
    pub close_group: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WorktreeListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Without `force` herdr refuses a dirty checkout.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeRemoveParams {
    pub workspace_id: WorkspaceId,
    pub force: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PluginActionListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginActionInvokeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    pub action_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<PluginInvocationContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SshAgentRegisterParams {
    pub socket_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationShowParams {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// `events.subscribe`'s list; see [`crate::events::subscriptions`].
#[derive(Debug, Clone, Serialize)]
pub struct Subscribe {
    pub subscriptions: Value,
}

macro_rules! methods {
    ($(
        $(#[$doc:meta])*
        $ty:ident = $name:literal ($params:ty) -> $result:literal $(.$field:ident)? : $out:ty;
    )*) => {
        /// One type per method; see [`Method`].
        pub mod method {
            use super::*;
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy)]
            pub struct $ty;

            impl Method for $ty {
                const NAME: &'static str = $name;
                const RESULT: &'static str = $result;
                const FIELD: Option<&'static str> = methods!(@field $($field)?);
                type Params = $params;
                type Output = $out;
            }
        )*
        }

        /// Every method in the table, by name.
        #[cfg(test)]
        pub(crate) const ALL: &[&str] = &[$($name),*];
    };
    (@field) => { None };
    (@field $field:ident) => { Some(stringify!($field)) };
}

methods! {
    Ping = "ping" (NoParams) -> "pong": Pong;
    SessionSnapshot = "session.snapshot" (NoParams) -> "session_snapshot".snapshot: crate::types::SessionSnapshot;
    EventsSubscribe = "events.subscribe" (Subscribe) -> "subscription_started": IgnoredAny;

    LayoutExport = "layout.export" (LayoutTarget) -> "layout_export".layout: LayoutDescription;
    LayoutSetSplitRatio = "layout.set_split_ratio" (SplitRatio)
        -> "layout_split_ratio_set".layout: LayoutDescription;

    PaneSplit = "pane.split" (SplitParams) -> "pane_info".pane: PaneInfo;
    PaneSwap = "pane.swap" (PaneSwapParams) -> "pane_swap".swap: PaneSwapResult;
    PaneMove = "pane.move" (PaneMoveParams) -> "pane_move".move_result: PaneMoveResult;
    PaneClose = "pane.close" (PaneTarget) -> "ok": IgnoredAny;
    PaneFocus = "pane.focus" (PaneTarget) -> "pane_info": IgnoredAny;
    PaneGet = "pane.get" (PaneTarget) -> "pane_info".pane: PaneInfo;
    PaneList = "pane.list" (InWorkspace) -> "pane_list".panes: Vec<PaneInfo>;
    PaneRename = "pane.rename" (PaneRenameParams) -> "pane_info": IgnoredAny;
    PaneZoom = "pane.zoom" (PaneZoomParams) -> "pane_zoom": IgnoredAny;
    PaneInputSet = "pane.input.set" (PaneInputSetParams) -> "ok": IgnoredAny;
    PaneSendInput = "pane.send_input" (PaneSendInputParams) -> "ok": IgnoredAny;
    PaneReportAgent = "pane.report_agent" (PaneReportAgentParams) -> "ok": IgnoredAny;
    PaneRead = "pane.read" (PaneReadParams) -> "pane_read".read: PaneText;

    TabCreate = "tab.create" (NewTab) -> "tab_created": TabCreated;
    TabList = "tab.list" (InWorkspace) -> "tab_list".tabs: Vec<TabInfo>;
    TabGet = "tab.get" (TabTarget) -> "tab_info".tab: TabInfo;
    TabFocus = "tab.focus" (TabTarget) -> "tab_info": IgnoredAny;
    TabRename = "tab.rename" (TabRenameParams) -> "tab_info": IgnoredAny;
    TabMove = "tab.move" (TabMoveParams) -> "tab_list": IgnoredAny;
    TabClose = "tab.close" (TabTarget) -> "ok": IgnoredAny;

    WorkspaceCreate = "workspace.create" (NewWorkspace) -> "workspace_created": WorkspaceCreated;
    WorkspaceList = "workspace.list" (NoParams) -> "workspace_list".workspaces: Vec<WorkspaceInfo>;
    WorkspaceGet = "workspace.get" (WorkspaceTarget) -> "workspace_info".workspace: WorkspaceInfo;
    WorkspaceFocus = "workspace.focus" (WorkspaceTarget) -> "workspace_info": IgnoredAny;
    WorkspaceRename = "workspace.rename" (WorkspaceRenameParams) -> "workspace_info": IgnoredAny;
    WorkspaceMove = "workspace.move" (WorkspaceMoveParams) -> "workspace_list": IgnoredAny;
    WorkspaceMoveBlock = "workspace.move_block" (WorkspaceMoveBlockParams)
        -> "workspace_list": IgnoredAny;
    WorkspaceClose = "workspace.close" (WorkspaceCloseParams) -> "ok": IgnoredAny;

    WorktreeList = "worktree.list" (WorktreeListParams) -> "worktree_list": WorktreeListing;
    WorktreeCreate = "worktree.create" (WorktreeParams) -> "worktree_created": WorkspaceCreated;
    WorktreeOpen = "worktree.open" (WorktreeParams) -> "worktree_opened": WorkspaceCreated;
    WorktreeRemove = "worktree.remove" (WorktreeRemoveParams) -> "worktree_removed": WorktreeRemoved;

    AgentList = "agent.list" (NoParams) -> "agent_list".agents: Vec<AgentInfo>;
    AgentGet = "agent.get" (AgentTarget) -> "agent_info".agent: AgentInfo;
    AgentFocus = "agent.focus" (AgentTarget) -> "agent_info": IgnoredAny;

    PluginActionList = "plugin.action.list" (PluginActionListParams)
        -> "plugin_action_list".actions: Vec<PluginActionInfo>;
    PluginActionInvoke = "plugin.action.invoke" (PluginActionInvokeParams)
        -> "plugin_action_invoked": PluginActionInvoked;

    /// Lasts as long as its connection; see `Client::ssh_agent_register`.
    ServerSshAgentRegister = "server.ssh_agent.register" (SshAgentRegisterParams) -> "ok": IgnoredAny;
    ServerReloadConfig = "server.reload_config" (NoParams) -> "config_reload": ConfigReload;
    NotificationShow = "notification.show" (NotificationShowParams)
        -> "notification_show": NotificationShown;
}

#[cfg(test)]
mod tests;
