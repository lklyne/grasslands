//! Typed herdr commands. Each call is a blocking round trip on its own
//! connection; run them off the UI thread.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::protocol::*;
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

/// `server.reload_config`'s answer.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigReload {
    pub status: ConfigReloadStatus,
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigReloadStatus {
    Applied,
    Partial,
    Failed,
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

    /// Sends `M` and decodes its answer. The named methods below are this
    /// with friendlier arguments.
    pub fn send<M: Method>(&self, params: &M::Params) -> Result<M::Output> {
        let result = self.endpoint.request_value(M::NAME, params)?;
        decode::<M>(result)
    }

    /// [`Self::send`] for methods that answer only success.
    fn ack<M: Method<Output = IgnoredAny>>(&self, params: &M::Params) -> Result<()> {
        self.send::<M>(params).map(drop)
    }

    pub fn ping(&self) -> Result<Pong> {
        self.send::<method::Ping>(&NoParams {})
    }

    pub fn session_snapshot(&self) -> Result<SessionSnapshot> {
        self.send::<method::SessionSnapshot>(&NoParams {})
    }

    // Layout

    /// The split tree of `tab_id` (herdr's focused tab when `None`).
    pub fn layout_export(&self, tab_id: Option<&TabId>) -> Result<LayoutDescription> {
        self.send::<method::LayoutExport>(&LayoutTarget {
            tab_id: tab_id.cloned(),
        })
    }

    /// Sets the ratio of the split at `path` (from the root: `false` = first child,
    /// `true` = second child; empty = root split).
    pub fn layout_set_split_ratio(
        &self,
        tab_id: &TabId,
        path: &[bool],
        ratio: f32,
    ) -> Result<LayoutDescription> {
        self.send::<method::LayoutSetSplitRatio>(&SplitRatio {
            tab_id: tab_id.clone(),
            path: path.to_vec(),
            ratio,
        })
    }

    // Panes

    pub fn pane_split(&self, params: &PaneSplit) -> Result<PaneInfo> {
        self.send::<method::PaneSplit>(params)
    }

    pub fn pane_swap(
        &self,
        source_pane_id: &PaneId,
        target_pane_id: &PaneId,
    ) -> Result<PaneSwapResult> {
        self.send::<method::PaneSwap>(&PaneSwapParams::Panes {
            source_pane_id: source_pane_id.clone(),
            target_pane_id: target_pane_id.clone(),
        })
    }

    pub fn pane_swap_direction(
        &self,
        pane_id: &PaneId,
        direction: PaneDirection,
    ) -> Result<PaneSwapResult> {
        self.send::<method::PaneSwap>(&PaneSwapParams::Toward {
            pane_id: pane_id.clone(),
            direction,
        })
    }

    pub fn pane_move(
        &self,
        pane_id: &PaneId,
        destination: &PaneMoveDestination,
        focus: bool,
    ) -> Result<PaneMoveResult> {
        self.send::<method::PaneMove>(&PaneMoveParams {
            pane_id: pane_id.clone(),
            destination: destination.clone(),
            focus,
        })
    }

    pub fn pane_close(&self, pane_id: &PaneId) -> Result<()> {
        self.ack::<method::PaneClose>(&pane_target(pane_id))
    }

    pub fn pane_focus(&self, pane_id: &PaneId) -> Result<()> {
        self.ack::<method::PaneFocus>(&pane_target(pane_id))
    }

    pub fn pane_get(&self, pane_id: &PaneId) -> Result<PaneInfo> {
        self.send::<method::PaneGet>(&pane_target(pane_id))
    }

    /// All of a pane's text, oldest scrollback line first. `pane.read` stops
    /// at 1000 lines, so this reads the whole range through
    /// `pane.selection.read`, falling back to `pane.read` when herdr can't.
    pub fn pane_scrollback(&self, pane_id: &PaneId) -> Result<String> {
        let rows = self
            .pane_get(pane_id)?
            .scroll
            .map(|s| s.max_offset_from_bottom + s.viewport_rows)
            .unwrap_or_default();
        let width = self
            .send::<method::PaneLayoutGet>(&pane_target(pane_id))?
            .panes
            .into_iter()
            .find(|p| &p.pane_id == pane_id)
            .map(|p| p.rect.width)
            .unwrap_or_default();
        if let (Ok(last_row), Some(last_col)) = (u32::try_from(rows), width.checked_sub(1))
            && let Some(last_row) = last_row.checked_sub(1)
        {
            let selection = self.send::<method::PaneSelectionRead>(&PaneSelectionReadParams {
                pane_id: pane_id.clone(),
                anchor: PaneTextPoint { row: 0, col: 0 },
                cursor: PaneTextPoint {
                    row: last_row,
                    col: last_col,
                },
            });
            match selection {
                Ok(text) => return Ok(text.text),
                Err(err) if err.code() == Some("selection_unavailable") => {}
                Err(err) => return Err(err),
            }
        }
        self.send::<method::PaneRead>(&PaneReadParams {
            pane_id: pane_id.clone(),
            source: ReadSource::RecentUnwrapped,
            lines: Some(1000),
        })
        .map(|read| read.text)
    }

    pub fn pane_list(&self, workspace_id: Option<&WorkspaceId>) -> Result<Vec<PaneInfo>> {
        self.send::<method::PaneList>(&InWorkspace {
            workspace_id: workspace_id.cloned(),
        })
    }

    pub fn pane_rename(&self, pane_id: &PaneId, label: Option<&str>) -> Result<()> {
        self.ack::<method::PaneRename>(&PaneRenameParams {
            pane_id: pane_id.clone(),
            label: label.map(str::to_owned),
        })
    }

    pub fn pane_zoom(&self, pane_id: &PaneId, mode: ZoomMode) -> Result<()> {
        self.ack::<method::PaneZoom>(&PaneZoomParams {
            pane_id: pane_id.clone(),
            mode,
        })
    }

    /// Chooses who gets right-clicks in the pane (`pane.input.set`).
    pub fn pane_set_right_click(&self, pane_id: &PaneId, target: RightClickTarget) -> Result<()> {
        self.ack::<method::PaneInputSet>(&PaneInputSetParams {
            pane_id: pane_id.clone(),
            right_click: target,
        })
    }

    // Tabs

    pub fn tab_create(&self, params: &NewTab) -> Result<TabCreated> {
        self.send::<method::TabCreate>(params)
    }

    pub fn tab_list(&self, workspace_id: Option<&WorkspaceId>) -> Result<Vec<TabInfo>> {
        self.send::<method::TabList>(&InWorkspace {
            workspace_id: workspace_id.cloned(),
        })
    }

    pub fn tab_get(&self, tab_id: &TabId) -> Result<TabInfo> {
        self.send::<method::TabGet>(&tab_target(tab_id))
    }

    pub fn tab_focus(&self, tab_id: &TabId) -> Result<()> {
        self.ack::<method::TabFocus>(&tab_target(tab_id))
    }

    pub fn tab_rename(&self, tab_id: &TabId, label: &str) -> Result<()> {
        self.ack::<method::TabRename>(&TabRenameParams {
            tab_id: tab_id.clone(),
            label: label.to_owned(),
        })
    }

    pub fn tab_move(&self, tab_id: &TabId, insert_index: usize) -> Result<()> {
        self.ack::<method::TabMove>(&TabMoveParams {
            tab_id: tab_id.clone(),
            insert_index,
        })
    }

    pub fn tab_close(&self, tab_id: &TabId) -> Result<()> {
        self.ack::<method::TabClose>(&tab_target(tab_id))
    }

    // Workspaces

    pub fn workspace_create(&self, params: &NewWorkspace) -> Result<WorkspaceCreated> {
        self.send::<method::WorkspaceCreate>(params)
    }

    pub fn workspace_list(&self) -> Result<Vec<WorkspaceInfo>> {
        self.send::<method::WorkspaceList>(&NoParams {})
    }

    pub fn workspace_get(&self, workspace_id: &WorkspaceId) -> Result<WorkspaceInfo> {
        self.send::<method::WorkspaceGet>(&workspace_target(workspace_id))
    }

    pub fn workspace_focus(&self, workspace_id: &WorkspaceId) -> Result<()> {
        self.ack::<method::WorkspaceFocus>(&workspace_target(workspace_id))
    }

    pub fn workspace_rename(&self, workspace_id: &WorkspaceId, label: &str) -> Result<()> {
        self.ack::<method::WorkspaceRename>(&WorkspaceRenameParams {
            workspace_id: workspace_id.clone(),
            label: label.to_owned(),
        })
    }

    pub fn workspace_move(&self, workspace_id: &WorkspaceId, insert_index: usize) -> Result<()> {
        self.ack::<method::WorkspaceMove>(&WorkspaceMoveParams {
            workspace_id: workspace_id.clone(),
            insert_index,
        })
    }

    /// Moves `workspace_ids`, in that order, to just before
    /// `before_workspace_id`, or to the end when `None`.
    pub fn workspace_move_block(
        &self,
        workspace_ids: &[WorkspaceId],
        before_workspace_id: Option<&WorkspaceId>,
    ) -> Result<()> {
        self.ack::<method::WorkspaceMoveBlock>(&WorkspaceMoveBlockParams {
            workspace_ids: workspace_ids.to_vec(),
            before_workspace_id: before_workspace_id.cloned(),
        })
    }

    /// Closes a workspace; `close_group` also closes its worktree group.
    pub fn workspace_close(&self, workspace_id: &WorkspaceId, close_group: bool) -> Result<()> {
        self.ack::<method::WorkspaceClose>(&WorkspaceCloseParams {
            workspace_id: workspace_id.clone(),
            close_group,
        })
    }

    // Worktrees

    pub fn worktree_list(
        &self,
        workspace_id: Option<&WorkspaceId>,
        cwd: Option<&str>,
    ) -> Result<WorktreeList> {
        self.send::<method::WorktreeList>(&WorktreeListParams {
            workspace_id: workspace_id.cloned(),
            cwd: cwd.map(str::to_owned),
        })
    }

    pub fn worktree_create(&self, params: &WorktreeParams) -> Result<WorkspaceCreated> {
        self.send::<method::WorktreeCreate>(params)
    }

    pub fn worktree_open(&self, params: &WorktreeParams) -> Result<WorkspaceCreated> {
        self.send::<method::WorktreeOpen>(params)
    }

    pub fn worktree_remove(
        &self,
        workspace_id: &WorkspaceId,
        force: bool,
    ) -> Result<WorktreeRemoved> {
        self.send::<method::WorktreeRemove>(&WorktreeRemoveParams {
            workspace_id: workspace_id.clone(),
            force,
        })
    }

    // Agents

    pub fn agent_list(&self) -> Result<Vec<AgentInfo>> {
        self.send::<method::AgentList>(&NoParams {})
    }

    /// `target` is a pane id or agent name.
    pub fn agent_get(&self, target: &str) -> Result<AgentInfo> {
        self.send::<method::AgentGet>(&AgentTarget {
            target: target.to_owned(),
        })
    }

    pub fn agent_focus(&self, target: &str) -> Result<()> {
        self.ack::<method::AgentFocus>(&AgentTarget {
            target: target.to_owned(),
        })
    }

    // Plugins

    pub fn plugin_action_list(&self, plugin_id: Option<&str>) -> Result<Vec<PluginActionInfo>> {
        self.send::<method::PluginActionList>(&PluginActionListParams {
            plugin_id: plugin_id.map(str::to_owned),
        })
    }

    pub fn plugin_action_invoke(
        &self,
        plugin_id: Option<&str>,
        action_id: &str,
        context: Option<&PluginInvocationContext>,
    ) -> Result<PluginActionInvoked> {
        self.send::<method::PluginActionInvoke>(&PluginActionInvokeParams {
            plugin_id: plugin_id.map(str::to_owned),
            action_id: action_id.to_owned(),
            context: context.cloned(),
        })
    }

    // Server

    /// Makes `socket_path`, an agent socket on the server's host, the SSH
    /// agent of the server's panes (they see it through their
    /// `SSH_AUTH_SOCK`). It lasts as long as the returned registration.
    pub fn ssh_agent_register(&self, socket_path: &str) -> Result<SshAgentRegistration> {
        let (stream, _) = self.endpoint.request_open(
            method::ServerSshAgentRegister::NAME,
            &SshAgentRegisterParams {
                socket_path: socket_path.to_owned(),
            },
        )?;
        Ok(SshAgentRegistration { stream })
    }

    /// Makes the server re-read `config.toml` (herdr has no method to read or
    /// write it; see `crate::config`).
    pub fn server_reload_config(&self) -> Result<ConfigReload> {
        self.send::<method::ServerReloadConfig>(&NoParams {})
    }

    // Notifications

    /// Asks herdr to show a toast. With `ui.toast.delivery = "system"` it goes to the OS.
    pub fn notification_show(&self, title: &str, body: Option<&str>) -> Result<NotificationShown> {
        self.send::<method::NotificationShow>(&NotificationShowParams {
            title: title.to_owned(),
            body: body.map(str::to_owned),
        })
    }
}

/// `result` as `M` answers it: its [`Method::FIELD`], or all of it.
pub(crate) fn decode<M: Method>(mut result: Value) -> Result<M::Output> {
    let value = match M::FIELD {
        Some(key) => result
            .get_mut(key)
            .map(Value::take)
            .ok_or_else(|| Error::Protocol(format!("{} result has no {key:?}", M::NAME)))?,
        None => result,
    };
    serde_json::from_value(value).map_err(|source| Error::Decode {
        method: M::NAME.to_owned(),
        source,
    })
}

fn pane_target(pane_id: &PaneId) -> PaneTarget {
    PaneTarget {
        pane_id: pane_id.clone(),
    }
}

fn tab_target(tab_id: &TabId) -> TabTarget {
    TabTarget {
        tab_id: tab_id.clone(),
    }
}

fn workspace_target(workspace_id: &WorkspaceId) -> WorkspaceTarget {
    WorkspaceTarget {
        workspace_id: workspace_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_should_be_open_while_the_server_holds_it() {
        let (stream, _server) = UnixStream::pair().unwrap();
        assert!(SshAgentRegistration { stream }.is_open());
    }

    #[test]
    fn registration_should_be_closed_once_the_server_hangs_up() {
        let (stream, server) = UnixStream::pair().unwrap();
        drop(server);
        assert!(!SshAgentRegistration { stream }.is_open());
    }
}
