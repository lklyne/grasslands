//! A worktree's changes: its branch, the branch it was cut from, and the
//! diff since then, as line counts and changed paths (the file tree's footer
//! and colors) or a patch (the Changes tab). Read by `crate::fs::Fs`
//! (`git_summary`, `git_patch`): git2 on Local, the machine's git elsewhere.
//!
//! The parent is the repo's default branch: the local branch `origin/HEAD`
//! names, else `main`, else `master`. On that branch itself the parent is
//! its upstream. The diff runs from the merge base to the working tree, so
//! it counts commits, staged and unstaged edits and untracked files alike,
//! like the PR the branch would become.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::worktree::Placement;

/// How a file differs from the parent branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    /// New on this branch, committed or not (untracked included).
    Added,
    Modified,
}

/// What the footer and the tree show; every part is optional outside a
/// tidy repo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub parent: Option<String>,
    /// Lines added and removed since the merge base with `parent`, or since
    /// HEAD when there is no parent.
    pub diff: Option<(usize, usize)>,
    /// Changed files still on disk, by absolute path.
    pub files: Arc<BTreeMap<PathBuf, FileChange>>,
    /// Folders holding a change, deleted files included, by absolute path.
    pub folders: Arc<BTreeSet<PathBuf>>,
}

/// A workspace folder's checkout, as the spaces list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The branch HEAD names; `None` when detached, like herdr.
    pub branch: Option<String>,
    /// Commits ahead of and behind the branch's upstream, if it has one.
    pub ahead_behind: Option<(usize, usize)>,
    /// `None` for a bare repo.
    pub placement: Option<Placement>,
    /// This checkout's own git dir, which holds its `HEAD`.
    pub git_dir: PathBuf,
}
