//! The app's only door to herdr: re-exports of `herdr-client` under the names
//! the views use. Nothing outside this module and `state` imports
//! `herdr_client` directly.

#[allow(
    unused_imports,
    reason = "the views pick these up as they are filled in"
)]
pub use herdr_client::{
    AgentInfo as Agent, AgentStatus, Client, ConnectionStatus, Error, Event, LayoutDescription,
    NewTab, NewWorkspace, PaneDirection, PaneId, PaneInfo, PaneMoveDestination, PaneSplit,
    PluginActionInfo, PluginInvocationContext, RightClickTarget, SessionSync, SplitDirection,
    SplitRef, SplitTree, State as Snapshot, Tab, TabId, TabInfo, Update, Workspace, WorkspaceId,
    WorkspaceInfo, WorktreeInfo, WorktreeParams,
};

/// Where to find herdr: `HERDR_BIN`, `HERDR_SESSION`, `HERDR_SOCKET_PATH`
/// (`Config::from_env`), so a dev run never touches the user's live session.
pub type Config = herdr_client::Endpoint;

pub type Result<T> = herdr_client::Result<T>;
