//! Typed herdr commands. Each call is a blocking round trip on its own
//! connection; run them off the UI thread.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::types::*;

/// Cheap, cloneable handle for sending commands to herdr.
#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneSplit {
    /// Pane to split; herdr's focused pane when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_pane_id: Option<PaneId>,
    pub direction: SplitDirection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub focus: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_click: Option<RightClickTarget>,
}

impl PaneSplit {
    pub fn new(target_pane_id: Option<PaneId>, direction: SplitDirection) -> Self {
        Self {
            target_pane_id,
            direction,
            ratio: None,
            cwd: None,
            env: BTreeMap::new(),
            focus: false,
            right_click: None,
        }
    }
}

/// Where `pane.move` puts a pane.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneMoveDestination {
    /// Into an existing tab, splitting `target_pane_id` (or the tab's focused pane).
    Tab {
        tab_id: TabId,
        split: SplitDirection,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_pane_id: Option<PaneId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ratio: Option<f32>,
    },
    NewTab {
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace_id: Option<WorkspaceId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    NewWorkspace {
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tab_label: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NewWorkspace {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub focus: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NewTab {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub focus: bool,
}

/// Parameters shared by `worktree.create` and `worktree.open`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorktreeParams {
    /// Workspace whose repo the worktree belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `worktree.create` only: start point for a new branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub focus: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_repository: Option<bool>,
}

/// Result of commands that create a workspace (`workspace.create`, `worktree.*`).
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceCreated {
    pub workspace: WorkspaceInfo,
    pub tab: TabInfo,
    pub root_pane: PaneInfo,
    #[serde(default)]
    pub worktree: Option<WorktreeInfo>,
    #[serde(default)]
    pub already_open: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TabCreated {
    pub tab: TabInfo,
    pub root_pane: PaneInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeList {
    pub source: WorktreeSourceInfo,
    pub worktrees: Vec<WorktreeInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeRemoved {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub forced: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaneSwapResult {
    pub changed: bool,
    pub source_pane_id: PaneId,
    #[serde(default)]
    pub target_pane_id: Option<PaneId>,
    pub focused_pane_id: PaneId,
    #[serde(default)]
    pub reason: Option<String>,
    pub layout: PaneLayoutSnapshot,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaneMoveResult {
    pub changed: bool,
    pub pane: PaneInfo,
    pub previous_pane_id: PaneId,
    pub previous_tab_id: TabId,
    pub previous_workspace_id: WorkspaceId,
    pub focused_pane_id: PaneId,
    #[serde(default)]
    pub closed_tab_id: Option<TabId>,
    #[serde(default)]
    pub closed_workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub created_tab: Option<TabInfo>,
    #[serde(default)]
    pub created_workspace: Option<WorkspaceInfo>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationShown {
    pub shown: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginActionInvoked {
    #[serde(default)]
    pub action: Option<PluginActionInfo>,
    #[serde(default)]
    pub log: Option<Value>,
}

/// A `server.ssh_agent.register` in force: herdr drops it when this
/// connection closes, so dropping this ends it.
#[derive(Debug)]
pub struct SshAgentRegistration {
    stream: UnixStream,
}

impl SshAgentRegistration {
    /// Whether the server still holds the connection (and so the
    /// registration). herdr sends nothing on it, so EOF or an error means
    /// the server, or the tunnel to it, went away.
    pub fn is_open(&self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut byte = [0u8; 1];
        let open = match (&self.stream).read(&mut byte) {
            Ok(n) => n > 0,
            Err(err) => err.kind() == io::ErrorKind::WouldBlock,
        };
        let _ = self.stream.set_nonblocking(false);
        open
    }
}

#[derive(Serialize)]
struct Empty {}

impl Client {
    pub fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }

    pub fn from_env() -> crate::Result<Self> {
        Endpoint::from_env().map(Self::new)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Any method, for calls without a typed wrapper. Returns the `result` object.
    pub fn call<P: Serialize>(&self, method: &str, params: &P) -> Result<Value> {
        self.endpoint.request_value(method, params)
    }

    /// Calls `method` and deserializes the `key` field of its result.
    fn field<P: Serialize, T: DeserializeOwned>(
        &self,
        method: &str,
        key: &str,
        params: &P,
    ) -> Result<T> {
        let mut result = self.endpoint.request_value(method, params)?;
        let value = result
            .get_mut(key)
            .map(Value::take)
            .ok_or_else(|| Error::Protocol(format!("{method} result has no {key:?}")))?;
        serde_json::from_value(value).map_err(|source| Error::Decode {
            method: method.to_owned(),
            source,
        })
    }

    fn ok<P: Serialize>(&self, method: &str, params: &P) -> Result<()> {
        self.endpoint.request_value(method, params).map(drop)
    }

    pub fn ping(&self) -> Result<Pong> {
        self.endpoint.request("ping", &Empty {})
    }

    pub fn session_snapshot(&self) -> Result<SessionSnapshot> {
        self.field("session.snapshot", "snapshot", &Empty {})
    }

    // Layout

    /// The split tree of `tab_id` (herdr's focused tab when `None`).
    pub fn layout_export(&self, tab_id: Option<&str>) -> Result<LayoutDescription> {
        self.field("layout.export", "layout", &json!({ "tab_id": tab_id }))
    }

    /// Sets the ratio of the split at `path` (from the root: `false` = first child,
    /// `true` = second child; empty = root split).
    pub fn layout_set_split_ratio(
        &self,
        tab_id: &str,
        path: &[bool],
        ratio: f32,
    ) -> Result<LayoutDescription> {
        self.field(
            "layout.set_split_ratio",
            "layout",
            &json!({ "tab_id": tab_id, "path": path, "ratio": ratio }),
        )
    }

    // Panes

    pub fn pane_split(&self, params: &PaneSplit) -> Result<PaneInfo> {
        self.field("pane.split", "pane", params)
    }

    pub fn pane_swap(&self, source_pane_id: &str, target_pane_id: &str) -> Result<PaneSwapResult> {
        self.field(
            "pane.swap",
            "swap",
            &json!({ "source_pane_id": source_pane_id, "target_pane_id": target_pane_id }),
        )
    }

    pub fn pane_swap_direction(
        &self,
        pane_id: &str,
        direction: PaneDirection,
    ) -> Result<PaneSwapResult> {
        self.field(
            "pane.swap",
            "swap",
            &json!({ "pane_id": pane_id, "direction": direction }),
        )
    }

    pub fn pane_move(
        &self,
        pane_id: &str,
        destination: &PaneMoveDestination,
        focus: bool,
    ) -> Result<PaneMoveResult> {
        self.field(
            "pane.move",
            "move_result",
            &json!({ "pane_id": pane_id, "destination": destination, "focus": focus }),
        )
    }

    pub fn pane_close(&self, pane_id: &str) -> Result<()> {
        self.ok("pane.close", &json!({ "pane_id": pane_id }))
    }

    pub fn pane_focus(&self, pane_id: &str) -> Result<()> {
        self.ok("pane.focus", &json!({ "pane_id": pane_id }))
    }

    pub fn pane_get(&self, pane_id: &str) -> Result<PaneInfo> {
        self.field("pane.get", "pane", &json!({ "pane_id": pane_id }))
    }

    pub fn pane_list(&self, workspace_id: Option<&str>) -> Result<Vec<PaneInfo>> {
        self.field(
            "pane.list",
            "panes",
            &json!({ "workspace_id": workspace_id }),
        )
    }

    pub fn pane_rename(&self, pane_id: &str, label: Option<&str>) -> Result<()> {
        self.ok(
            "pane.rename",
            &json!({ "pane_id": pane_id, "label": label }),
        )
    }

    /// `mode` is `"toggle"`, `"on"` or `"off"`.
    pub fn pane_zoom(&self, pane_id: &str, mode: &str) -> Result<Value> {
        self.call("pane.zoom", &json!({ "pane_id": pane_id, "mode": mode }))
    }

    /// Chooses who gets right-clicks in the pane (`pane.input.set`).
    pub fn pane_set_right_click(&self, pane_id: &str, target: RightClickTarget) -> Result<()> {
        self.ok(
            "pane.input.set",
            &json!({ "pane_id": pane_id, "right_click": target }),
        )
    }

    // Tabs

    pub fn tab_create(&self, params: &NewTab) -> Result<TabCreated> {
        self.endpoint.request("tab.create", params)
    }

    pub fn tab_list(&self, workspace_id: Option<&str>) -> Result<Vec<TabInfo>> {
        self.field("tab.list", "tabs", &json!({ "workspace_id": workspace_id }))
    }

    pub fn tab_get(&self, tab_id: &str) -> Result<TabInfo> {
        self.field("tab.get", "tab", &json!({ "tab_id": tab_id }))
    }

    pub fn tab_focus(&self, tab_id: &str) -> Result<()> {
        self.ok("tab.focus", &json!({ "tab_id": tab_id }))
    }

    pub fn tab_rename(&self, tab_id: &str, label: &str) -> Result<()> {
        self.ok("tab.rename", &json!({ "tab_id": tab_id, "label": label }))
    }

    pub fn tab_move(&self, tab_id: &str, insert_index: usize) -> Result<()> {
        self.ok(
            "tab.move",
            &json!({ "tab_id": tab_id, "insert_index": insert_index }),
        )
    }

    pub fn tab_close(&self, tab_id: &str) -> Result<()> {
        self.ok("tab.close", &json!({ "tab_id": tab_id }))
    }

    // Workspaces

    pub fn workspace_create(&self, params: &NewWorkspace) -> Result<WorkspaceCreated> {
        self.endpoint.request("workspace.create", params)
    }

    pub fn workspace_list(&self) -> Result<Vec<WorkspaceInfo>> {
        self.field("workspace.list", "workspaces", &Empty {})
    }

    pub fn workspace_get(&self, workspace_id: &str) -> Result<WorkspaceInfo> {
        self.field(
            "workspace.get",
            "workspace",
            &json!({ "workspace_id": workspace_id }),
        )
    }

    pub fn workspace_focus(&self, workspace_id: &str) -> Result<()> {
        self.ok("workspace.focus", &json!({ "workspace_id": workspace_id }))
    }

    pub fn workspace_rename(&self, workspace_id: &str, label: &str) -> Result<()> {
        self.ok(
            "workspace.rename",
            &json!({ "workspace_id": workspace_id, "label": label }),
        )
    }

    pub fn workspace_move(&self, workspace_id: &str, insert_index: usize) -> Result<()> {
        self.ok(
            "workspace.move",
            &json!({ "workspace_id": workspace_id, "insert_index": insert_index }),
        )
    }

    /// Moves `workspace_ids`, in that order, to just before
    /// `before_workspace_id`, or to the end when `None`.
    pub fn workspace_move_block(
        &self,
        workspace_ids: &[String],
        before_workspace_id: Option<&str>,
    ) -> Result<()> {
        self.ok(
            "workspace.move_block",
            &json!({ "workspace_ids": workspace_ids, "before_workspace_id": before_workspace_id }),
        )
    }

    /// Closes a workspace; `close_group` also closes its worktree group.
    pub fn workspace_close(&self, workspace_id: &str, close_group: bool) -> Result<()> {
        self.ok(
            "workspace.close",
            &json!({ "workspace_id": workspace_id, "close_group": close_group }),
        )
    }

    // Worktrees

    pub fn worktree_list(
        &self,
        workspace_id: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<WorktreeList> {
        self.endpoint.request(
            "worktree.list",
            &json!({ "workspace_id": workspace_id, "cwd": cwd }),
        )
    }

    pub fn worktree_create(&self, params: &WorktreeParams) -> Result<WorkspaceCreated> {
        self.endpoint.request("worktree.create", params)
    }

    pub fn worktree_open(&self, params: &WorktreeParams) -> Result<WorkspaceCreated> {
        self.endpoint.request("worktree.open", params)
    }

    pub fn worktree_remove(&self, workspace_id: &str, force: bool) -> Result<WorktreeRemoved> {
        self.endpoint.request(
            "worktree.remove",
            &json!({ "workspace_id": workspace_id, "force": force }),
        )
    }

    // Agents

    pub fn agent_list(&self) -> Result<Vec<AgentInfo>> {
        self.field("agent.list", "agents", &Empty {})
    }

    /// `target` is a pane id or agent name.
    pub fn agent_get(&self, target: &str) -> Result<AgentInfo> {
        self.field("agent.get", "agent", &json!({ "target": target }))
    }

    pub fn agent_focus(&self, target: &str) -> Result<()> {
        self.ok("agent.focus", &json!({ "target": target }))
    }

    // Plugins

    pub fn plugin_action_list(&self, plugin_id: Option<&str>) -> Result<Vec<PluginActionInfo>> {
        self.field(
            "plugin.action.list",
            "actions",
            &json!({ "plugin_id": plugin_id }),
        )
    }

    pub fn plugin_action_invoke(
        &self,
        plugin_id: Option<&str>,
        action_id: &str,
        context: Option<&PluginInvocationContext>,
    ) -> Result<PluginActionInvoked> {
        self.endpoint.request(
            "plugin.action.invoke",
            &json!({ "plugin_id": plugin_id, "action_id": action_id, "context": context }),
        )
    }

    // Server

    /// Makes `socket_path`, an agent socket on the server's host, the SSH
    /// agent of the server's panes (they see it through their
    /// `SSH_AUTH_SOCK`). It lasts as long as the returned registration.
    pub fn ssh_agent_register(&self, socket_path: &str) -> Result<SshAgentRegistration> {
        let (stream, _) = self.endpoint.request_open(
            "server.ssh_agent.register",
            &json!({ "socket_path": socket_path }),
        )?;
        Ok(SshAgentRegistration { stream })
    }

    // Notifications

    /// Asks herdr to show a toast. With `ui.toast.delivery = "system"` it goes to the OS.
    pub fn notification_show(&self, title: &str, body: Option<&str>) -> Result<NotificationShown> {
        self.endpoint.request(
            "notification.show",
            &json!({ "title": title, "body": body }),
        )
    }
}
