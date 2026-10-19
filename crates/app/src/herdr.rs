//! The app's only door to herdr: re-exports of `herdr-client` under the names
//! the views use. Nothing outside this module and `state` imports
//! `herdr_client` directly.

pub mod machines {
    pub use herdr_client::{dev_allowed, list_machines, socket_dir};
}

/// herdr's `config.toml` (read, edit, watch, check).
pub mod config {
    pub use herdr_client::config::*;
    pub use herdr_client::{ALLOW_LIVE_ENV, ConfigReloadStatus, DEV_ROOT};
}
#[allow(
    unused_imports,
    reason = "the views pick these up as they are filled in"
)]
pub use herdr_client::{
    AgentInfo as Agent, AgentStatus, Check, Client, ConnectionStatus, Error, Fix, FixCommand,
    InstallMethod, LayoutDescription, MIN_VERSION, MachineLink, MachineProfile, MachineStatus,
    NewTab, NewWorkspace, PaneDirection, PaneId, PaneInfo, PaneMoveDestination, PaneSplit,
    PluginActionContext, PluginActionInfo, PluginInvocationContext, ProfileId, RightClickTarget,
    SessionSync, SplitDirection, SplitRef, SplitTree, State as Snapshot, Tab, TabId, TabInfo,
    TerminalId, Update, Version, Workspace, WorkspaceCreated, WorkspaceId, WorkspaceInfo,
    WorkspaceWorktreeInfo, WorktreeInfo, WorktreeList, WorktreeParams, WorktreeRemoved,
};

/// Where to find herdr: `HERDR_BIN`, `HERDR_SESSION`, `HERDR_SOCKET_PATH`
/// (`Config::from_env`), so a dev run never touches the user's live session.
/// In debug and test builds `from_env` refuses any socket outside
/// `~/.local/share/grasslands-herdr` unless `RT_ALLOW_LIVE_HERDR=1`.
pub type Config = herdr_client::Endpoint;

pub type Result<T> = herdr_client::Result<T>;
