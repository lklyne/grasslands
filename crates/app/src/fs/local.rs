//! This Mac's disk: `ignore` for listings and search, `std::fs` for the
//! rest, the Trash for removing.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Entry, sort_entries};
use crate::sidebar::files::SearchResults;
use crate::sidebar::files::search::{Collector, MAX_FILE_BYTES};

/// Lists `dir` one level deep, respecting .gitignore (and git's other ignore
/// files) inside a repo. Dotfiles are shown; `.git` is not.
pub(super) fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
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
    sort_entries(&mut entries);
    Ok(entries)
}

/// Writes in place, as editors here always have: the file keeps its inode,
/// owner and links, a read-only file refuses, and a writable file in a
/// read-only folder still saves.
pub(super) fn write(path: &Path, contents: &str) -> io::Result<()> {
    std::fs::write(path, contents)
}

pub(super) fn create_file(path: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
}

/// `std::fs::rename` replaces `to`; this refuses.
pub(super) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    if to.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    std::fs::rename(from, to)
}

/// Moves to the Trash rather than deleting, so a slip is recoverable.
pub(super) fn trash(path: &Path) -> io::Result<()> {
    trash::delete(path).map_err(io::Error::other)
}

/// Matches `query` against every path and file below `root`, .gitignore
/// respected.
pub(super) fn search(root: &Path, query: &str, cancel: &AtomicBool) -> SearchResults {
    if query.is_empty() {
        return SearchResults::default();
    }
    let mut collector = Collector::new(query);
    let walk = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .sort_by_file_path(Ord::cmp)
        .build();
    for entry in walk.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        collector.offer_name(path, &relative);
        if !collector.wants_lines() {
            continue;
        }
        if let Some(text) = read_text(path) {
            let lines = text.lines().enumerate().map(|(ix, l)| (ix as u32, l));
            collector.offer_lines(path, relative, lines);
        }
    }
    collector.finish()
}

/// A file's text, unless it is too large or binary.
fn read_text(path: &Path) -> Option<String> {
    if std::fs::metadata(path).map_or(true, |m| m.len() > MAX_FILE_BYTES) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    // Binary files, by the same test git uses.
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    #[test]
    fn write_should_replace_contents_and_keep_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("write");
        let path = dir.join("run.sh");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&path, "new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            (std::fs::read_to_string(&path).unwrap(), mode),
            ("new".into(), 0o755)
        );
    }

    #[test]
    fn write_should_refuse_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("readonly");
        let path = dir.join("locked.txt");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let error = write(&path, "new").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn write_should_save_in_a_read_only_folder() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("readonlydir");
        let path = dir.join("a.txt");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let written = write(&path, "new");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            (written.is_ok(), std::fs::read_to_string(&path).unwrap()),
            (true, "new".into())
        );
    }

    #[test]
    fn rename_should_refuse_to_replace() {
        let dir = temp_dir("rename");
        std::fs::write(dir.join("a"), "a").unwrap();
        std::fs::write(dir.join("b"), "b").unwrap();
        let error = rename(&dir.join("a"), &dir.join("b")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    fn search_dir(name: &str) -> PathBuf {
        let dir = temp_dir(&format!("search-{name}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git2::Repository::init(&dir).unwrap();
        dir
    }

    fn run(dir: &Path, query: &str) -> SearchResults {
        search(dir, query, &AtomicBool::new(false))
    }

    #[test]
    fn search_should_match_names_below_root() {
        let dir = search_dir("names");
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("readme.md"), "").unwrap();
        let names: Vec<_> = run(&dir, "main")
            .names
            .into_iter()
            .map(|h| h.relative)
            .collect();
        assert_eq!(names, vec!["src/main.rs"]);
    }

    #[test]
    fn search_should_report_matching_lines_with_numbers() {
        let dir = search_dir("lines");
        std::fs::write(dir.join("a.txt"), "one\n  needle here\nthree needle\n").unwrap();
        let hits = run(&dir, "needle").contents;
        let lines: Vec<_> = hits[0]
            .lines
            .iter()
            .map(|l| (l.line, &l.text[l.range.clone()]))
            .collect();
        assert_eq!(lines, vec![(1, "needle"), (2, "needle")]);
    }

    #[test]
    fn search_should_skip_ignored_and_binary_files() {
        let dir = search_dir("ignored");
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/x.txt"), "needle").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0").unwrap();
        assert_eq!(run(&dir, "needle"), SearchResults::default());
    }

    #[test]
    fn search_should_respect_case_when_query_has_capitals() {
        let dir = search_dir("case");
        std::fs::write(dir.join("a.txt"), "Needle\nneedle\n").unwrap();
        assert_eq!(run(&dir, "Needle").contents[0].lines.len(), 1);
    }
}
