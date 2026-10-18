//! Saved SSH machines and their connections.
//!
//! Stub with the contract's signatures; the transport implementation replaces it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::endpoint::{DEV_ROOT, Endpoint};
use crate::error::{Error, Result};

/// A saved SSH machine, from `herdr machine list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MachineProfile {
    pub id: String,
    pub label: String,
    /// e.g. "ssh://lyle@127.0.0.1:2222" or an ssh_config alias.
    pub target: String,
    /// "default" means no `--session`.
    pub session: String,
    pub enabled: bool,
}

/// Runs `<herdr_bin> machine list --json`.
pub fn list_machines(herdr_bin: &Path) -> Result<Vec<MachineProfile>> {
    let output = Command::new(herdr_bin)
        .args(["machine", "list", "--json"])
        .output()?;
    if !output.status.success() {
        return Err(Error::Protocol(format!(
            "herdr machine list: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStatus {
    Connecting,
    Connected,
    /// SSH needs a password, passphrase or host-key approval (BatchMode failed).
    NeedsSignIn(String),
    /// herdr on the machine is missing, not running, too old or incompatible.
    NeedsSetup(String),
    /// Couldn't reach it, or the connection dropped; retrying with backoff.
    Offline(String),
    /// Disabled in herdr, or not in GRASSLANDS_DEV_MACHINES in a debug build.
    Disabled(String),
}

/// One machine's SSH connection, kept open by a supervisor thread.
pub struct MachineLink {
    profile: MachineProfile,
    socket_dir: PathBuf,
    statuses: async_channel::Receiver<MachineStatus>,
}

impl MachineLink {
    /// Starts the supervisor and returns at once, status `Connecting`
    /// (or `Disabled` without starting anything).
    pub fn start(profile: MachineProfile, socket_dir: PathBuf) -> Self {
        let (_tx, statuses) = async_channel::unbounded();
        Self {
            profile,
            socket_dir,
            statuses,
        }
    }

    pub fn profile(&self) -> &MachineProfile {
        &self.profile
    }

    pub fn status(&self) -> MachineStatus {
        MachineStatus::Disabled("remote machines aren't built yet".into())
    }

    /// Every status change, latest last.
    pub fn statuses(&self) -> async_channel::Receiver<MachineStatus> {
        self.statuses.clone()
    }

    /// The forwarded socket; fixed for the link's life.
    pub fn endpoint(&self) -> Endpoint {
        let id8: String = self.profile.id.chars().take(8).collect();
        Endpoint {
            bin: PathBuf::from("herdr"),
            session: None,
            socket_path: self.socket_dir.join(format!("{id8}.sock")),
            auto_start: false,
            server_log: None,
        }
    }

    /// Shell command for a terminal surface in a remote pane, like
    /// `Launch::attach_command`. `None` until the remote herdr is found.
    pub fn attach_command(&self, _terminal_id: &str) -> Option<String> {
        None
    }

    /// Retry now, skipping the backoff wait.
    pub fn reconnect(&self) {}
}

/// `$HOME/.local/share/grasslands-herdr/m` in debug builds,
/// `$HOME/Library/Caches/grasslands/m` in release; created 0700.
pub fn socket_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let dir = if cfg!(debug_assertions) {
        home.join(DEV_ROOT).join("m")
    } else {
        home.join("Library/Caches/grasslands/m")
    };
    Some(dir)
}

/// Debug builds: whether `label` is listed in GRASSLANDS_DEV_MACHINES
/// (comma-separated). Release builds: always true.
pub fn dev_allowed(label: &str) -> bool {
    if !cfg!(debug_assertions) {
        return true;
    }
    std::env::var("GRASSLANDS_DEV_MACHINES")
        .is_ok_and(|list| list.split(',').any(|l| l.trim() == label))
}
