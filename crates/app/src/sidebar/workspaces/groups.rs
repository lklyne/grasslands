//! Groups herdr's workspaces by repo, like herdr's own sidebar: one group per
//! repo in the order its first workspace appears, the main checkout's
//! workspaces first and its linked worktrees after them, then every workspace
//! with no repo in an ungrouped section. Within each part herdr's order holds.

use super::git::{Placement, Repo};
use crate::herdr::WorkspaceInfo;

pub struct RepoGroup<'a> {
    pub repo: Repo,
    /// Each workspace with `true` for a linked worktree.
    pub workspaces: Vec<(&'a WorkspaceInfo, bool)>,
}

pub struct Groups<'a> {
    pub repos: Vec<RepoGroup<'a>>,
    pub ungrouped: Vec<&'a WorkspaceInfo>,
}

/// `place` says which repo a workspace is in (`None`: no repo).
pub fn group<'a>(
    workspaces: impl IntoIterator<Item = &'a WorkspaceInfo>,
    place: impl Fn(&WorkspaceInfo) -> Option<Placement>,
) -> Groups<'a> {
    let mut repos: Vec<RepoGroup<'a>> = Vec::new();
    let mut ungrouped = Vec::new();
    for info in workspaces {
        let Some(Placement { repo, linked }) = place(info) else {
            ungrouped.push(info);
            continue;
        };
        match repos.iter_mut().find(|g| g.repo.key == repo.key) {
            Some(group) => group.workspaces.push((info, linked)),
            None => repos.push(RepoGroup {
                repo,
                workspaces: vec![(info, linked)],
            }),
        }
    }
    for group in &mut repos {
        // Stable, so herdr's order holds among main and among linked.
        group.workspaces.sort_by_key(|&(_, linked)| linked);
    }
    Groups { repos, ungrouped }
}
