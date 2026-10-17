//! A worktree's changes, read with git2: its branch, the branch it was cut
//! from, and the diff since then, as line counts (the file tree's footer) or
//! a patch (the Changes tab).
//!
//! The parent is the repo's default branch: the local branch `origin/HEAD`
//! names, else `main`, else `master`. On that branch itself the parent is
//! its upstream. The diff runs from the merge base to the working tree, so
//! it counts commits, staged and unstaged edits and untracked files alike,
//! like the PR the branch would become.

use std::path::Path;

/// What the footer shows; every part is optional outside a tidy repo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub parent: Option<String>,
    /// Lines added and removed since the merge base with `parent`, or since
    /// HEAD when there is no parent.
    pub diff: Option<(usize, usize)>,
}

/// The summary for the checkout containing `dir`, or `None` outside git.
pub fn read(dir: &Path) -> Option<Summary> {
    let repo = git2::Repository::discover(dir).ok()?;
    let branch = branch(&repo);
    let parent = branch.as_deref().and_then(|b| parent(&repo, b));
    let base = base(&repo, parent.as_ref().map(|(_, oid)| *oid));
    let diff = diff_since(&repo, base)
        .and_then(|diff| diff.stats().ok())
        .map(|stats| (stats.insertions(), stats.deletions()));
    Some(Summary {
        branch,
        parent: parent.map(|(name, _)| name),
        diff,
    })
}

/// The same diff `read` counts, as a git-style patch.
pub fn patch(dir: &Path) -> Option<String> {
    let repo = git2::Repository::discover(dir).ok()?;
    let parent = branch(&repo).and_then(|branch| parent(&repo, &branch));
    let base = base(&repo, parent.map(|(_, oid)| oid));
    let diff = diff_since(&repo, base)?;
    let mut patch = String::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        // Context and change lines come without their marker.
        if matches!(line.origin(), ' ' | '+' | '-') {
            patch.push(line.origin());
        }
        patch.push_str(&String::from_utf8_lossy(line.content()));
        true
    })
    .ok()?;
    Some(patch)
}

/// The branch HEAD names, `None` when detached. Reads HEAD's target rather
/// than `repo.head()`, which fails before the first commit.
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

/// Where the diff starts: the merge base with the parent's tip, else HEAD.
fn base(repo: &git2::Repository, parent: Option<git2::Oid>) -> Option<git2::Oid> {
    let head = repo.head().ok()?.target()?;
    match parent {
        Some(parent) => repo.merge_base(head, parent).ok(),
        None => Some(head),
    }
}

/// The parent branch's short name and tip.
fn parent(repo: &git2::Repository, branch: &str) -> Option<(String, git2::Oid)> {
    let default = default_branch(repo)?;
    if default != branch {
        let local = repo.find_branch(&default, git2::BranchType::Local).ok();
        let reference = match local {
            Some(local) => local.into_reference(),
            None => repo
                .find_reference(&format!("refs/remotes/origin/{default}"))
                .ok()?,
        };
        let name = reference.shorthand().ok()?.to_owned();
        return Some((name, reference.target()?));
    }
    let upstream = repo
        .find_branch(branch, git2::BranchType::Local)
        .ok()?
        .upstream()
        .ok()?;
    let name = upstream.name().ok()??.to_owned();
    Some((name, upstream.get().target()?))
}

fn default_branch(repo: &git2::Repository) -> Option<String> {
    let remote_head = repo
        .find_reference("refs/remotes/origin/HEAD")
        .ok()
        .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
        .and_then(|t| t.strip_prefix("refs/remotes/origin/").map(str::to_owned));
    remote_head.or_else(|| {
        ["main", "master"]
            .into_iter()
            .find(|name| repo.find_branch(name, git2::BranchType::Local).is_ok())
            .map(str::to_owned)
    })
}

/// From `base` to the working tree; from the empty tree when `base` is
/// `None` (before the first commit).
fn diff_since(repo: &git2::Repository, base: Option<git2::Oid>) -> Option<git2::Diff<'_>> {
    let tree = match base {
        Some(oid) => Some(repo.find_commit(oid).ok()?.tree().ok()?),
        None => None,
    };
    let mut options = git2::DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    repo.diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut options))
        .ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "grasslands-changes-git-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn commit_all(repo: &git2::Repository, message: &str) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap();
    }

    fn repo_on_main(dir: &Path) -> git2::Repository {
        let repo = git2::Repository::init_opts(
            dir,
            git2::RepositoryInitOptions::new().initial_head("main"),
        )
        .unwrap();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        commit_all(&repo, "init");
        repo
    }

    #[test]
    fn read_should_count_commits_and_edits_since_parent() {
        let dir = temp_dir("feature");
        let repo = repo_on_main(&dir);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feat", &head, false).unwrap();
        repo.set_head("refs/heads/feat").unwrap();
        std::fs::write(dir.join("a.txt"), "one\n2\n").unwrap();
        commit_all(&repo, "edit");
        std::fs::write(dir.join("new.txt"), "x\ny\n").unwrap();
        assert_eq!(
            read(&dir).unwrap(),
            Summary {
                branch: Some("feat".into()),
                parent: Some("main".into()),
                diff: Some((3, 1)),
            }
        );
    }

    #[test]
    fn read_should_show_no_parent_on_main_without_upstream() {
        let dir = temp_dir("main");
        repo_on_main(&dir);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        assert_eq!(
            read(&dir).unwrap(),
            Summary {
                branch: Some("main".into()),
                parent: None,
                diff: Some((0, 1)),
            }
        );
    }

    #[test]
    fn read_should_count_untracked_files_before_first_commit() {
        let dir = temp_dir("unborn");
        git2::Repository::init_opts(&dir, git2::RepositoryInitOptions::new().initial_head("dev"))
            .unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let summary = read(&dir).unwrap();
        assert_eq!(
            (summary.branch.as_deref(), summary.diff),
            (Some("dev"), Some((1, 0)))
        );
    }

    #[test]
    fn patch_should_hold_edits_and_new_files_since_parent() {
        let dir = temp_dir("patch");
        let repo = repo_on_main(&dir);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feat", &head, false).unwrap();
        repo.set_head("refs/heads/feat").unwrap();
        std::fs::write(dir.join("a.txt"), "one\n2\n").unwrap();
        commit_all(&repo, "edit");
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();
        let patch = patch(&dir).unwrap();
        for line in ["diff --git a/a.txt b/a.txt", " one", "-two", "+2", "+x"] {
            assert!(patch.lines().any(|l| l == line), "{line:?} in {patch}");
        }
    }

    #[test]
    fn read_should_be_none_outside_git() {
        assert_eq!(read(&temp_dir("plain")), None);
    }
}
