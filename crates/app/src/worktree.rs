//! Resolving a pane's folder to the worktree the file tree and editor follow.

use std::path::{Path, PathBuf};

/// The root of the git worktree containing `dir` (a linked worktree's own
/// checkout, not the main repo), or `dir` itself outside git. `None` if `dir`
/// does not exist on this machine.
pub fn resolve_root(dir: &Path) -> Option<PathBuf> {
    let dir = dir.canonicalize().ok()?;
    let root = git2::Repository::discover(&dir)
        .ok()
        .and_then(|repo| repo.workdir().map(Path::to_path_buf))
        .and_then(|root| root.canonicalize().ok());
    Some(root.unwrap_or(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grasslands-worktree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn commit_all(repo: &git2::Repository) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    #[test]
    fn resolve_root_should_find_repo_root_from_subfolder() {
        let dir = temp_dir("sub");
        git2::Repository::init(&dir).unwrap();
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        assert_eq!(resolve_root(&dir.join("a/b")), Some(dir));
    }

    #[test]
    fn resolve_root_should_return_linked_worktree_checkout() {
        let dir = temp_dir("linked");
        let main = dir.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let repo = git2::Repository::init(&main).unwrap();
        std::fs::write(main.join("f.txt"), "x").unwrap();
        commit_all(&repo);
        let linked = dir.join("linked");
        repo.worktree("linked", &linked, None).unwrap();
        std::fs::create_dir_all(linked.join("src")).unwrap();
        assert_eq!(resolve_root(&linked.join("src")), Some(linked));
    }

    #[test]
    fn resolve_root_should_fall_back_to_folder_outside_git() {
        let dir = temp_dir("plain");
        // temp dirs can sit inside a repo on some machines; only assert when not.
        if git2::Repository::discover(&dir).is_err() {
            assert_eq!(resolve_root(&dir), Some(dir));
        }
    }

    #[test]
    fn resolve_root_should_return_none_for_missing_folder() {
        assert_eq!(resolve_root(Path::new("/no/such/dir/grasslands")), None);
    }
}
