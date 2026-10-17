//! Each workspace's branch and how far it is ahead of / behind its upstream,
//! and its repo when herdr doesn't say, read with git2 from the workspace's
//! checkout folder. herdr's socket API has neither (its own sidebar reads git
//! in the server), and fills `WorkspaceInfo::worktree` only for workspaces it
//! has resolved.
//!
//! Results are cached per folder and re-read when a workspace's folder first
//! appears, when the active worktree changes, when a watched `HEAD` file
//! changes (checkout), and on a slow timer (commits, fetches, pushes), never
//! per frame.

use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

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

/// What git2 found for one folder.
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

/// The checkout containing `dir`, or `None` outside git.
pub fn read(dir: &Path) -> Option<Checkout> {
    let repo = git2::Repository::discover(dir).ok()?;
    let git_dir = repo.path().canonicalize().ok()?;
    let placement = placement(&repo);
    let branch = branch(&repo);
    Some(Checkout {
        ahead_behind: branch.as_deref().and_then(|b| ahead_behind(&repo, b)),
        branch,
        placement,
        git_dir,
    })
}

fn placement(repo: &git2::Repository) -> Option<Placement> {
    if repo.is_bare() {
        return None;
    }
    let common = repo.commondir().canonicalize().ok()?;
    let root = if repo.is_worktree() {
        // A linked worktree's common dir is the main checkout's `.git`.
        common.parent()?.to_path_buf()
    } else {
        repo.workdir()?.canonicalize().ok()?
    };
    Some(Placement {
        repo: Repo {
            key: common.to_string_lossy().into_owned(),
            name: root.file_name()?.to_string_lossy().into_owned(),
            root: root.to_string_lossy().into_owned(),
        },
        linked: repo.is_worktree(),
    })
}

/// The branch HEAD points at, also before its first commit.
fn branch(repo: &git2::Repository) -> Option<String> {
    let head = repo.find_reference("HEAD").ok()?;
    let target = head.symbolic_target().ok().flatten()?;
    Some(
        target
            .strip_prefix("refs/heads/")
            .unwrap_or(target)
            .to_owned(),
    )
}

fn ahead_behind(repo: &git2::Repository, branch: &str) -> Option<(usize, usize)> {
    let local = repo.find_branch(branch, git2::BranchType::Local).ok()?;
    let head = local.get().target()?;
    let upstream = local.upstream().ok()?.get().target()?;
    repo.graph_ahead_behind(head, upstream).ok()
}

/// Watches git dirs for `HEAD` changes and reports which git dir changed.
pub struct HeadWatcher {
    watcher: RecommendedWatcher,
    watched: Vec<PathBuf>,
}

impl HeadWatcher {
    pub fn new(changes: async_channel::Sender<PathBuf>) -> notify::Result<Self> {
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!("workspaces: HEAD watch error: {error}");
                    return;
                }
            };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            // git writes `HEAD.lock` and renames it over `HEAD`.
            for path in event.paths {
                let is_head = path
                    .file_name()
                    .is_some_and(|name| name == "HEAD" || name == "HEAD.lock");
                if let (true, Some(git_dir)) = (is_head, path.parent()) {
                    // The receiver is gone only with the list; nothing to do then.
                    let _ = changes.send_blocking(git_dir.to_path_buf());
                }
            }
        })?;
        Ok(Self {
            watcher,
            watched: Vec::new(),
        })
    }

    /// Watches exactly `git_dirs` (non-recursively), adding and dropping
    /// watches as workspaces come and go.
    pub fn set(&mut self, mut git_dirs: Vec<PathBuf>) {
        git_dirs.sort();
        git_dirs.dedup();
        for dir in self.watched.iter().filter(|d| !git_dirs.contains(d)) {
            let _ = self.watcher.unwatch(dir);
        }
        for dir in git_dirs.iter().filter(|d| !self.watched.contains(d)) {
            if let Err(error) = self.watcher.watch(dir, RecursiveMode::NonRecursive) {
                tracing::warn!("workspaces: can't watch {}: {error}", dir.display());
            }
        }
        self.watched = git_dirs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grasslands-ws-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn repo_with_commit(dir: &Path) -> git2::Repository {
        let repo = git2::Repository::init_opts(
            dir,
            git2::RepositoryInitOptions::new().initial_head("main"),
        )
        .unwrap();
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("f.txt")).unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        drop(tree);
        repo
    }

    #[test]
    fn read_should_name_branch_and_main_checkout() {
        let dir = temp_dir("main");
        repo_with_commit(&dir);
        let checkout = read(&dir).unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            (
                checkout.branch.as_deref(),
                checkout.placement.map(|p| (p.repo.name, p.linked))
            ),
            (Some("main"), Some((name, false)))
        );
    }

    #[test]
    fn read_should_place_linked_worktree_under_main_repo() {
        let dir = temp_dir("linked");
        let main = dir.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let repo = repo_with_commit(&main);
        repo.worktree("feat", &dir.join("feat"), None).unwrap();
        let placement = read(&dir.join("feat")).unwrap().placement.unwrap();
        assert_eq!(
            (placement.repo.root, placement.repo.key, placement.linked),
            (
                main.to_string_lossy().into_owned(),
                main.join(".git").to_string_lossy().into_owned(),
                true
            )
        );
    }

    #[test]
    fn read_should_show_no_branch_when_detached() {
        let dir = temp_dir("detached");
        let repo = repo_with_commit(&dir);
        let head = repo.head().unwrap().target().unwrap();
        repo.set_head_detached(head).unwrap();
        assert_eq!(read(&dir).unwrap().branch, None);
    }

    #[test]
    fn read_should_count_commits_ahead_of_upstream() {
        let dir = temp_dir("ahead");
        let remote = dir.join("remote.git");
        git2::Repository::init_bare(&remote).unwrap();
        let local = dir.join("local");
        std::fs::create_dir_all(&local).unwrap();
        let repo = repo_with_commit(&local);
        repo.remote("origin", remote.to_str().unwrap()).unwrap();
        repo.find_remote("origin")
            .unwrap()
            .push(&["refs/heads/main:refs/heads/main"], None)
            .unwrap();
        repo.find_remote("origin")
            .unwrap()
            .fetch(&["main"], None, None)
            .unwrap();
        repo.find_branch("main", git2::BranchType::Local)
            .unwrap()
            .set_upstream(Some("origin/main"))
            .unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        for message in ["two", "three"] {
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            let tree = parent.tree().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
                .unwrap();
        }
        assert_eq!(read(&local).unwrap().ahead_behind, Some((2, 0)));
    }

    #[test]
    fn read_should_have_no_ahead_count_without_upstream() {
        let dir = temp_dir("no-upstream");
        repo_with_commit(&dir);
        assert_eq!(read(&dir).unwrap().ahead_behind, None);
    }

    #[test]
    fn read_should_name_branch_before_first_commit() {
        let dir = temp_dir("unborn");
        git2::Repository::init_opts(&dir, git2::RepositoryInitOptions::new().initial_head("dev"))
            .unwrap();
        assert_eq!(read(&dir).unwrap().branch.as_deref(), Some("dev"));
    }
}
