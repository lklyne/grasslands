//! The app's only door to herdr.
//!
//! `crates/herdr-client` is being built in parallel. Until it lands, this
//! module holds stand-in types with the shape the views need; when it lands,
//! these become re-exports of (or thin wrappers over) its types and the views
//! do not change. Nothing outside this module and `state` names herdr types.

use std::path::PathBuf;

/// Where to find herdr. Honors the same variables as the dev scripts so a
/// dev run never touches the user's live session.
#[derive(Debug, Clone)]
pub struct Config {
    pub bin: PathBuf,
    pub session: Option<String>,
    pub socket_path: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bin: std::env::var_os("HERDR_BIN").map_or_else(|| "herdr".into(), PathBuf::from),
            session: std::env::var("HERDR_SESSION").ok(),
            socket_path: std::env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from),
        }
    }
}

/// The connection's state, shown in the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

/// Stand-in for herdr-client's snapshot cache. Replaced by
/// `herdr_client::Snapshot` (or a wrapper) when that crate lands.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub workspaces: Vec<Workspace>,
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub pane_id: String,
    pub name: String,
    pub workspace_id: String,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Working,
    Blocked,
    Done,
    Idle,
}
