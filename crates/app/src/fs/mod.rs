//! Where a workspace's files live: this Mac's disk or a saved machine's,
//! behind one blocking API so the file tree, search, editor and Changes tab
//! don't care which. Run every call off the UI thread. See
//! docs/decisions/0020-remote-files.md.
//!
//! Local is `ignore`, `std::fs`, git2 and the Trash (`local`, `local_git`).
//! Remote is `herdr_client::RemoteFs`: SFTP for files, and the machine's own
//! git, grep and find for ignore rules, search and Changes (`remote`,
//! `remote_git`), parsed into the same shapes.

#[cfg(test)]
pub mod fake;
pub mod git;
mod local;
mod local_git;
#[cfg(test)]
mod parity_tests;
mod remote;
mod remote_git;
pub mod search;
#[cfg(test)]
mod test_repo;

pub use remote_git::SummaryCache;
pub use search::Results as SearchResults;

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

/// What a machine's file system can do, so views ask about the ability
/// rather than which machine it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// A notify watcher reports its changes (with canonical paths, so paths
    /// are canonicalized to match); otherwise views poll.
    pub watch: bool,
    /// `remove` moves to a Trash, so it needn't ask first.
    pub trash: bool,
    /// A file keeps its identity (device and inode) across a rename, so a
    /// vanished file can be found where it moved.
    pub stable_ids: bool,
    /// Calls are quick enough to make on the UI thread.
    pub cheap: bool,
}

impl Caps {
    /// This Mac's disk.
    pub const LOCAL: Self = Self {
        watch: true,
        trash: true,
        stable_ids: true,
        cheap: true,
    };
    /// A saved machine's, over SSH: every call is a round trip.
    pub const REMOTE: Self = Self {
        watch: false,
        trash: false,
        stable_ids: false,
        cheap: false,
    };

    /// The caps of `machine`'s files, also while it's offline.
    pub fn of(machine: &MachineId) -> Self {
        match machine {
            MachineId::Local => Self::LOCAL,
            MachineId::Remote(_) => Self::REMOTE,
        }
    }
}

/// A file system: cheap to clone, `Send + Sync`.
#[derive(Clone)]
pub enum Fs {
    Local,
    Remote {
        machine: MachineId,
        files: Arc<herdr_client::RemoteFs>,
        /// The machine's own (see `SummaryCache`).
        summaries: Arc<SummaryCache>,
    },
    /// An in-memory remote machine: files only (folders exist only through
    /// the files in them), no git or search.
    #[cfg(test)]
    Fake(Arc<fake::FakeFs>),
}

impl Fs {
    pub fn machine(&self) -> MachineId {
        match self {
            Self::Local => MachineId::Local,
            Self::Remote { machine, .. } => machine.clone(),
            #[cfg(test)]
            Self::Fake(_) => MachineId::Remote("fake".into()),
        }
    }

    pub fn caps(&self) -> Caps {
        Caps::of(&self.machine())
    }

    /// Each of `dirs` one level deep, respecting .gitignore inside a repo;
    /// dotfiles shown, `.git` hidden; folders first, then case-insensitive
    /// by name. A saved machine asks git about all of them in one command.
    pub fn read_dirs(&self, dirs: &[PathBuf]) -> Vec<io::Result<Vec<Entry>>> {
        match self {
            Self::Local => dirs.iter().map(|dir| local::read_dir(dir)).collect(),
            Self::Remote { files, .. } => {
                let listings = dirs
                    .iter()
                    .map(|dir| (dir.as_path(), files.read_dir(dir)))
                    .collect();
                remote::filter_listings(&remote::runner(files), listings)
            }
            #[cfg(test)]
            Self::Fake(fake) => dirs.iter().map(|dir| fake.read_dir(dir)).collect(),
        }
    }

    pub fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        match self {
            Self::Local => {
                let meta = std::fs::metadata(path)?;
                Ok(Metadata {
                    is_dir: meta.is_dir(),
                    len: meta.len(),
                    modified: meta.modified().ok(),
                })
            }
            Self::Remote { files, .. } => {
                let meta = files.metadata(path)?;
                Ok(Metadata {
                    is_dir: meta.is_dir,
                    len: meta.len,
                    modified: meta.modified,
                })
            }
            #[cfg(test)]
            Self::Fake(fake) => fake.metadata(path),
        }
    }

    pub fn read_to_string(&self, path: &Path) -> io::Result<String> {
        match self {
            Self::Local => std::fs::read_to_string(path),
            Self::Remote { files, .. } => String::from_utf8(files.read(path)?)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8 text")),
            #[cfg(test)]
            Self::Fake(fake) => fake.read(path),
        }
    }

    /// Local writes in place (a read-only file refuses, the inode stays);
    /// Remote writes a temp file and renames it over, keeping permissions.
    pub fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        match self {
            Self::Local => local::write(path, contents),
            Self::Remote { files, .. } => files.write(path, contents.as_bytes()),
            #[cfg(test)]
            Self::Fake(fake) => fake.write(path, contents),
        }
    }

    pub fn create_file(&self, path: &Path) -> io::Result<()> {
        match self {
            Self::Local => local::create_file(path),
            Self::Remote { files, .. } => files.create_file(path),
            #[cfg(test)]
            Self::Fake(fake) => fake.create_file(path),
        }
    }

    pub fn create_dir(&self, path: &Path) -> io::Result<()> {
        match self {
            Self::Local => std::fs::create_dir(path),
            Self::Remote { files, .. } => files.create_dir(path),
            #[cfg(test)]
            Self::Fake(_) => Err(io::ErrorKind::Unsupported.into()),
        }
    }

    /// Refuses to overwrite `to`.
    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        match self {
            Self::Local => local::rename(from, to),
            Self::Remote { files, .. } => files.rename(from, to),
            #[cfg(test)]
            Self::Fake(fake) => fake.rename(from, to),
        }
    }

    pub fn remove(&self, path: &Path) -> io::Result<Removal> {
        match self {
            Self::Local => local::trash(path).map(|()| Removal::Trashed),
            Self::Remote { files, .. } => files.remove(path).map(|()| Removal::Deleted),
            #[cfg(test)]
            Self::Fake(fake) => fake.remove_all(path).map(|()| Removal::Deleted),
        }
    }

    /// Names and contents below `root` matching `query` (smart case).
    pub fn search(&self, root: &Path, query: &str, cancel: &AtomicBool) -> SearchResults {
        match self {
            Self::Local => local::search(root, query, cancel),
            Self::Remote { files, .. } => {
                remote::search(&remote::runner(files), root, query, cancel)
            }
            #[cfg(test)]
            Self::Fake(_) => SearchResults::default(),
        }
    }

    /// The Changes summary for the checkout containing `dir`; `None`
    /// outside git, an error if a saved machine couldn't be asked. A saved
    /// machine's comes from a `git_patch` of `dir` moments ago if there was one.
    pub fn git_summary(&self, dir: &Path) -> io::Result<Option<git::Summary>> {
        match self {
            Self::Local => Ok(local_git::read(dir)),
            Self::Remote {
                files, summaries, ..
            } => match summaries.get(dir) {
                Some(summary) => Ok(summary),
                None => remote_git::summary(&remote::runner(files), dir),
            },
            #[cfg(test)]
            Self::Fake(_) => Ok(None),
        }
    }

    /// The Changes tab's patch for the checkout containing `dir`; errors
    /// like `git_summary`.
    pub fn git_patch(&self, dir: &Path) -> io::Result<Option<String>> {
        match self {
            Self::Local => Ok(local_git::patch(dir)),
            Self::Remote {
                files, summaries, ..
            } => {
                let read = remote_git::patch(&remote::runner(files), dir)?;
                summaries.put(dir, read.as_ref().map(|(_, s)| s.clone()));
                Ok(read.map(|(patch, _)| patch))
            }
            #[cfg(test)]
            Self::Fake(_) => Ok(None),
        }
    }

    /// The worktree a pane in `dir` works in: its git checkout's root (a
    /// linked worktree's own), or `dir` outside git. `None` if `dir` doesn't
    /// exist on this file system (or the machine can't be asked).
    pub fn resolve_root(&self, dir: &Path) -> Option<PathBuf> {
        match self {
            Self::Local => crate::worktree::resolve_root(dir),
            Self::Remote { files, .. } => remote_git::resolve_root(&remote::runner(files), dir),
            #[cfg(test)]
            Self::Fake(_) => None,
        }
    }

    /// The branch HEAD names in `dir`: `Some(None)` when detached, `None`
    /// outside git.
    pub fn head_branch(&self, dir: &Path) -> Option<Option<String>> {
        match self {
            Self::Local => local_git::head_branch(dir),
            Self::Remote { files, .. } => remote_git::head_branch(&remote::runner(files), dir),
            #[cfg(test)]
            Self::Fake(_) => None,
        }
    }
}

/// Folders first, then case-insensitive by name.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_files_should_be_watched_trashed_and_read_inline() {
        assert_eq!(Fs::Local.caps(), Caps::LOCAL);
    }

    #[test]
    fn saved_machine_should_have_none_of_local_abilities() {
        assert_eq!(Caps::of(&MachineId::Remote("m".into())), Caps::REMOTE);
    }

    #[test]
    fn fake_files_should_act_like_a_saved_machine() {
        let fake = Fs::Fake(Arc::new(fake::FakeFs::default()));
        assert_eq!(fake.caps(), Caps::REMOTE);
    }
}
