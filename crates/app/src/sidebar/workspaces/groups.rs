//! The rows of the spaces list, in herdr's order (`workspace_entries` in
//! herdr 0.9.5's `client/shell/sidebar.rs`):
//!
//! - A repo forms a group only when it has both a main-checkout workspace and
//!   a linked-worktree workspace. Its rows sit where its first workspace sits
//!   in herdr's list: the main checkout's workspaces (the group's parent rows,
//!   which carry the fold toggle), then its linked worktrees indented beneath.
//! - A folded group keeps only its parent rows, plus the focused linked
//!   worktree if there is one.
//! - Every other workspace is a plain row in herdr's order, repo or not.

use std::collections::HashSet;

use super::git::{Placement, Repo};
use crate::herdr::WorkspaceInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    /// A main checkout heading its repo's group.
    Parent,
    /// A linked worktree under its group's parent.
    Child,
}

pub struct Entry<'a> {
    pub info: &'a WorkspaceInfo,
    pub kind: Kind,
    /// The workspace's repo, grouped or not.
    pub repo: Option<Repo>,
}

/// `place` says which repo a workspace is in (`None`: no repo); `folded`
/// holds the repo keys whose groups are folded.
pub fn entries<'a>(
    workspaces: &[&'a WorkspaceInfo],
    place: impl Fn(&WorkspaceInfo) -> Option<Placement>,
    folded: &HashSet<String>,
) -> Vec<Entry<'a>> {
    let placed: Vec<Option<Placement>> = workspaces.iter().map(|i| place(i)).collect();
    let has = |key: &str, linked: bool| {
        placed
            .iter()
            .flatten()
            .any(|p| p.repo.key == key && p.linked == linked)
    };
    let mut emitted = HashSet::new();
    let mut entries = Vec::new();
    for (info, placement) in workspaces.iter().zip(&placed) {
        let Some(placement) = placement.as_ref().filter(|p| has(&p.repo.key, !p.linked)) else {
            entries.push(Entry {
                info,
                kind: Kind::Plain,
                repo: placement.as_ref().map(|p| p.repo.clone()),
            });
            continue;
        };
        let key = &placement.repo.key;
        if !emitted.insert(key.clone()) {
            continue;
        }
        let members = || {
            workspaces.iter().zip(&placed).filter_map(|(info, p)| {
                p.as_ref()
                    .filter(|p| &p.repo.key == key)
                    .map(|p| (*info, p))
            })
        };
        let row = |(info, p): (&'a WorkspaceInfo, &Placement)| Entry {
            info,
            kind: if p.linked { Kind::Child } else { Kind::Parent },
            repo: Some(p.repo.clone()),
        };
        entries.extend(members().filter(|(_, p)| !p.linked).map(row));
        let children = members().filter(|(_, p)| p.linked);
        if folded.contains(key) {
            entries.extend(children.filter(|(info, _)| info.focused).take(1).map(row));
        } else {
            entries.extend(children.map(row));
        }
    }
    entries
}
