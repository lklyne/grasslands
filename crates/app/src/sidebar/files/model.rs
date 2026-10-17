//! What the file tree shows: folder listings loaded lazily, one level at a
//! time, and which folders are expanded. The GPUI Kit tree is rebuilt from
//! this after every change.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use gpui_kit::component::tree::TreeItem;

/// One row of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// Lists `dir` one level deep, respecting .gitignore (and git's other ignore
/// files) inside a repo. Dotfiles are shown; `.git` is not. Folders first,
/// then case-insensitive by name.
pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    // `ignore` reports an unreadable root as an empty walk; surface it instead.
    std::fs::read_dir(dir)?;
    let walk = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut entries: Vec<Entry> = walk
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: e.file_type().is_some_and(|t| t.is_dir()),
            path: e.into_path(),
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Placeholder rows give unloaded and empty folders a child, which is what
/// makes the Kit tree treat them as folders. Their ids can't collide with a path.
const PLACEHOLDER: &str = "\0placeholder";

pub fn is_placeholder(id: &str) -> bool {
    id.ends_with(PLACEHOLDER)
}

#[derive(Debug)]
pub struct FileModel {
    root: PathBuf,
    listings: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
}

impl FileModel {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            listings: HashMap::new(),
            expanded: HashSet::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_loaded(&self, dir: &Path) -> bool {
        self.listings.contains_key(dir)
    }

    /// Stores a fresh listing of `dir`, forgetting listings of subfolders that
    /// are gone from it.
    pub fn set_listing(&mut self, dir: PathBuf, entries: Vec<Entry>) {
        if let Some(old) = self.listings.get(&dir) {
            let kept: HashSet<&Path> = entries.iter().map(|e| e.path.as_path()).collect();
            let gone: Vec<PathBuf> = old
                .iter()
                .filter(|e| e.is_dir && !kept.contains(e.path.as_path()))
                .map(|e| e.path.clone())
                .collect();
            for path in gone {
                self.forget(&path);
            }
        }
        self.listings.insert(dir, entries);
    }

    /// Drops `dir` and everything below it (it was deleted or became unreadable).
    pub fn forget(&mut self, dir: &Path) {
        self.listings.retain(|path, _| !path.starts_with(dir));
        self.expanded.retain(|path| !path.starts_with(dir));
    }

    pub fn set_expanded(&mut self, dir: &Path, expanded: bool) {
        if expanded {
            self.expanded.insert(dir.to_path_buf());
        } else {
            self.expanded.remove(dir);
        }
    }

    /// Which loaded folders to re-list after the watcher reported `changed`.
    pub fn dirs_to_rescan(&self, changed: &[PathBuf]) -> BTreeSet<PathBuf> {
        let mut dirs = BTreeSet::new();
        for path in changed {
            if path.components().any(|c| c.as_os_str() == ".git") {
                continue;
            }
            if path.file_name().is_some_and(|n| n == ".gitignore") {
                // Ignore rules changed: anything below may appear or vanish.
                let base = path.parent().unwrap_or(&self.root);
                dirs.extend(
                    self.listings
                        .keys()
                        .filter(|d| d.starts_with(base))
                        .cloned(),
                );
                continue;
            }
            for dir in [Some(path.as_path()), path.parent()].into_iter().flatten() {
                if self.is_loaded(dir) {
                    dirs.insert(dir.to_path_buf());
                }
            }
        }
        dirs
    }

    /// The Kit tree's items: the root's contents (the root itself is the header).
    pub fn items(&self) -> Vec<TreeItem> {
        self.children(&self.root)
    }

    fn children(&self, dir: &Path) -> Vec<TreeItem> {
        let Some(entries) = self.listings.get(dir) else {
            return vec![placeholder(dir, "Loading…")];
        };
        if entries.is_empty() {
            return vec![placeholder(dir, "Empty")];
        }
        entries.iter().map(|e| self.item(e)).collect()
    }

    fn item(&self, entry: &Entry) -> TreeItem {
        let item = TreeItem::new(
            entry.path.to_string_lossy().into_owned(),
            entry.name.clone(),
        );
        if !entry.is_dir {
            return item;
        }
        item.expanded(self.expanded.contains(&entry.path))
            .children(self.children(&entry.path))
    }
}

fn placeholder(dir: &Path, label: &str) -> TreeItem {
    TreeItem::new(format!("{}{PLACEHOLDER}", dir.display()), label.to_owned()).disabled(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grasslands-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn read_dir_should_list_folders_first_and_hide_git_and_ignored() {
        let dir = temp_dir("read");
        git2::Repository::init(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), "target\n*.log\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("b.rs"), "").unwrap();
        std::fs::write(dir.join("A.md"), "").unwrap();
        std::fs::write(dir.join("debug.log"), "").unwrap();
        assert_eq!(
            names(&read_dir(&dir).unwrap()),
            vec!["src", ".gitignore", "A.md", "b.rs"]
        );
    }

    #[test]
    fn read_dir_should_apply_parent_gitignore_in_subfolder() {
        let dir = temp_dir("nested");
        git2::Repository::init(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/keep.rs"), "").unwrap();
        std::fs::write(dir.join("sub/drop.tmp"), "").unwrap();
        assert_eq!(names(&read_dir(&dir.join("sub")).unwrap()), vec!["keep.rs"]);
    }

    #[test]
    fn read_dir_should_fail_for_missing_folder() {
        assert!(read_dir(Path::new("/no/such/grasslands-dir")).is_err());
    }

    fn entry(path: &str, is_dir: bool) -> Entry {
        Entry {
            path: PathBuf::from(path),
            name: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into(),
            is_dir,
        }
    }

    #[test]
    fn items_should_give_unloaded_folders_a_placeholder_child() {
        let mut model = FileModel::new("/r".into());
        model.set_listing("/r".into(), vec![entry("/r/src", true)]);
        let items = model.items();
        assert!(items[0].is_folder() && is_placeholder(&items[0].children[0].id));
    }

    #[test]
    fn set_listing_should_forget_removed_subfolders() {
        let mut model = FileModel::new("/r".into());
        model.set_listing("/r".into(), vec![entry("/r/src", true)]);
        model.set_listing("/r/src".into(), vec![]);
        model.set_listing("/r".into(), vec![]);
        assert!(!model.is_loaded(Path::new("/r/src")));
    }

    #[test]
    fn dirs_to_rescan_should_pick_loaded_parent_and_skip_git() {
        let mut model = FileModel::new("/r".into());
        model.set_listing("/r".into(), vec![entry("/r/src", true)]);
        let dirs = model.dirs_to_rescan(&[
            "/r/new.rs".into(),
            "/r/.git/index".into(),
            "/r/src/x.rs".into(),
        ]);
        assert_eq!(
            dirs.into_iter().collect::<Vec<_>>(),
            vec![PathBuf::from("/r")]
        );
    }

    #[test]
    fn dirs_to_rescan_should_relist_everything_below_changed_gitignore() {
        let mut model = FileModel::new("/r".into());
        model.set_listing("/r".into(), vec![entry("/r/src", true)]);
        model.set_listing("/r/src".into(), vec![]);
        assert_eq!(model.dirs_to_rescan(&["/r/.gitignore".into()]).len(), 2);
    }
}
