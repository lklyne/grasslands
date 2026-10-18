//! Where a workspace's files live: this Mac's disk or a saved machine's,
//! behind one blocking API so the file tree, search, editor and Changes tab
//! don't care which. Run every call off the UI thread. See
//! docs/decisions/0020-remote-files.md.
//!
//! STUB: signatures are the contract between the agents building remote
//! files; bodies are filled in by the tree/search/changes agent (Local first,
//! moving today's code here unchanged, then Remote over
//! `herdr_client::RemoteFs`).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use crate::state::MachineId;

/// One row of a folder listing, as the file tree shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub is_dir: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// What removing did, so the UI can word it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Moved to the macOS Trash (Local).
    Trashed,
    /// Gone for good (remote machines have no trash).
    Deleted,
}

/// A file system: cheap to clone, `Send + Sync`.
#[derive(Clone)]
pub enum Fs {
    Local,
    Remote {
        machine: MachineId,
        label: String,
        files: Arc<herdr_client::RemoteFs>,
    },
}

impl Fs {
    pub fn machine(&self) -> MachineId {
        match self {
            Self::Local => MachineId::Local,
            Self::Remote { machine, .. } => machine.clone(),
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// One level deep, respecting .gitignore inside a repo; dotfiles shown,
    /// `.git` hidden; folders first, then case-insensitive by name.
    pub fn read_dir(&self, _dir: &Path) -> io::Result<Vec<Entry>> {
        Err(stub())
    }

    pub fn metadata(&self, _path: &Path) -> io::Result<Metadata> {
        Err(stub())
    }

    pub fn read_to_string(&self, _path: &Path) -> io::Result<String> {
        Err(stub())
    }

    /// Atomic replace, keeping permissions.
    pub fn write(&self, _path: &Path, _contents: &str) -> io::Result<()> {
        Err(stub())
    }

    pub fn create_file(&self, _path: &Path) -> io::Result<()> {
        Err(stub())
    }

    pub fn create_dir(&self, _path: &Path) -> io::Result<()> {
        Err(stub())
    }

    /// Refuses to overwrite `to`.
    pub fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(stub())
    }

    pub fn remove(&self, _path: &Path) -> io::Result<Removal> {
        Err(stub())
    }

    /// Names and contents below `root` matching `query` (smart case).
    pub fn search(
        &self,
        _root: &Path,
        _query: &str,
        _cancel: &AtomicBool,
    ) -> crate::sidebar::files::SearchResults {
        crate::sidebar::files::SearchResults::default()
    }

    /// The Changes summary for the checkout containing `dir`; `None`
    /// outside git.
    pub fn git_summary(&self, _dir: &Path) -> Option<crate::changes::git::Summary> {
        None
    }

    /// The Changes tab's patch for the checkout containing `dir`.
    pub fn git_patch(&self, _dir: &Path) -> Option<String> {
        None
    }
}

fn stub() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "fs isn't built yet")
}
