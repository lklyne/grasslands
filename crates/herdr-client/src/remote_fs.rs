//! Files and commands on a saved machine, over its [`MachineLink`]'s ssh
//! master: an SFTP session for file operations and `ssh -S <ctl>` for
//! commands (git, grep). Blocking calls, like the rest of the crate; run them
//! off the UI thread. See docs/decisions/0020-remote-files.md.
//!
//! STUB: the transport agent replaces every body. Signatures are the contract.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, SystemTime};

/// One entry of a remote folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    /// Follows symlinks: a link to a folder is a folder.
    pub is_symlink: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMetadata {
    pub is_dir: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A machine's files. Cheap to share (`Arc`); one SFTP session inside,
/// reopened on the next call after the connection drops. Every call fails
/// with `io::ErrorKind::NotConnected` while the machine isn't connected.
pub struct RemoteFs {
    _private: (),
}

impl RemoteFs {
    /// The remote user's home directory (SFTP `realpath(".")`).
    pub fn home(&self) -> io::Result<PathBuf> {
        Err(stub())
    }

    /// One level, unsorted, without `.` and `..`.
    pub fn read_dir(&self, _dir: &Path) -> io::Result<Vec<RemoteEntry>> {
        Err(stub())
    }

    pub fn metadata(&self, _path: &Path) -> io::Result<RemoteMetadata> {
        Err(stub())
    }

    pub fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
        Err(stub())
    }

    /// Atomic: writes a sibling temp file, keeps the old file's permissions,
    /// then renames over `path`.
    pub fn write(&self, _path: &Path, _contents: &[u8]) -> io::Result<()> {
        Err(stub())
    }

    /// Fails with `AlreadyExists` if `path` exists.
    pub fn create_file(&self, _path: &Path) -> io::Result<()> {
        Err(stub())
    }

    pub fn create_dir(&self, _path: &Path) -> io::Result<()> {
        Err(stub())
    }

    /// Fails with `AlreadyExists` if `to` exists.
    pub fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(stub())
    }

    /// Permanent: a file, or a folder and everything in it. There is no
    /// remote trash.
    pub fn remove(&self, _path: &Path) -> io::Result<()> {
        Err(stub())
    }

    /// Runs `argv` on the machine (each word shell-quoted for the remote
    /// shell), in `cwd` if given, killed after `timeout`.
    pub fn exec(
        &self,
        _argv: &[&str],
        _cwd: Option<&Path>,
        _timeout: Duration,
    ) -> io::Result<ExecOutput> {
        Err(stub())
    }
}

fn stub() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "remote files aren't built yet")
}
