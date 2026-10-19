//! Where a workspace sits in its repo. (Resolving a pane's folder to its
//! worktree is `crate::fs::Fs::resolve_root`.)

use crate::herdr::WorkspaceWorktreeInfo;

/// The repo a workspace belongs to, the key its group is built on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The repo's common git dir, like herdr's `repo_key`.
    pub key: String,
    pub name: String,
    /// The main checkout's folder.
    pub root: String,
}

/// A workspace's place in its repo: the main checkout or a linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub repo: Repo,
    pub linked: bool,
}

impl Placement {
    pub fn from_herdr(worktree: &WorkspaceWorktreeInfo) -> Self {
        Self {
            repo: Repo {
                key: worktree.repo_key.trim_end_matches('/').to_owned(),
                name: worktree.repo_name.clone(),
                root: worktree.repo_root.trim_end_matches('/').to_owned(),
            },
            linked: worktree.is_linked_worktree,
        }
    }
}
