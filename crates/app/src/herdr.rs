//! The app's only door to herdr: re-exports of `herdr-client` under the names
//! the views use. Nothing outside this module and `state` imports
//! `herdr_client` directly.

#[allow(
    unused_imports,
    reason = "the views pick these up as they are filled in"
)]
pub use herdr_client::{
    AgentInfo as Agent, AgentStatus, Check, Client, ConnectionStatus, Error, Event, Fix,
    FixCommand, InstallMethod, LayoutDescription, MIN_VERSION, NewTab, NewWorkspace, PaneDirection,
    PaneId, PaneInfo, PaneMoveDestination, PaneSplit, PluginActionContext, PluginActionInfo,
    PluginInvocationContext, RightClickTarget, SessionSync, SplitDirection, SplitRef, SplitTree,
    State as Snapshot, Tab, TabId, TabInfo, Update, Version, Workspace, WorkspaceId, WorkspaceInfo,
    WorkspaceWorktreeInfo, WorktreeInfo, WorktreeParams,
};

/// Where to find herdr: `HERDR_BIN`, `HERDR_SESSION`, `HERDR_SOCKET_PATH`
/// (`Config::from_env`), so a dev run never touches the user's live session.
/// In debug and test builds `from_env` refuses any socket outside
/// `~/.local/share/grasslands-herdr` unless `RT_ALLOW_LIVE_HERDR=1`.
pub type Config = herdr_client::Endpoint;

pub type Result<T> = herdr_client::Result<T>;
