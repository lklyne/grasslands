//! A saved machine's Changes summary and patch, worktree root and branch,
//! from its own git (see `crate::changes::git` for the rules). Each is one
//! command, so a poll costs one round trip.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::remote::Run;
use crate::changes::git::{FileChange, Summary};

/// Finds the checkout's root, branch, parent and the diff's base the way
/// `local_git` does, then prints `top\0HEAD's target\0parent\0` and runs
/// `$1`'s diffs from the base against a copy of the index where untracked
/// files are intent-to-add, so they count as added like Local's. Exits 3
/// outside git.
const GIT: &str = r#"top=$(git rev-parse --show-toplevel 2>/dev/null) || exit 3
[ -n "$top" ] && cd "$top" || exit 3
h=$(git symbolic-ref -q HEAD)
b=${h#refs/heads/}
d=$(git symbolic-ref -q refs/remotes/origin/HEAD)
d=${d#refs/remotes/origin/}
if [ -z "$d" ]; then
  for n in main master; do
    if git show-ref -q --verify "refs/heads/$n"; then d=$n; break; fi
  done
fi
p=
if [ -n "$b" ] && [ -n "$d" ]; then
  if [ "$b" != "$d" ]; then
    if git show-ref -q --verify "refs/heads/$d"; then p=$d
    elif git show-ref -q --verify "refs/remotes/origin/$d"; then p=origin/$d
    fi
  else
    p=$(git rev-parse -q --verify --abbrev-ref "$b@{upstream}" 2>/dev/null) || p=
  fi
fi
base=
c=$(git rev-parse -q --verify 'HEAD^{commit}' 2>/dev/null)
if [ -n "$c" ]; then
  if [ -n "$p" ]; then base=$(git merge-base "$c" "$p" 2>/dev/null); else base=$c; fi
fi
[ -n "$base" ] || base=$(git hash-object -t tree /dev/null)
t=$(mktemp -d) || exit 4
trap 'rm -rf "$t"' EXIT
cp "$(git rev-parse --git-path index)" "$t/index" 2>/dev/null
GIT_INDEX_FILE=$t/index
export GIT_INDEX_FILE
git add -N . >/dev/null 2>&1
printf '%s\0%s\0%s\0' "$top" "$h" "$p"
diff() {
  git -c core.quotePath=false diff --no-renames --no-ext-diff --no-color --no-relative \
    --src-prefix=a/ --dst-prefix=b/ "$@" "$base" --
}
if [ "$1" = patch ]; then
  diff
else
  diff --numstat -z
  printf '\0'
  diff --name-status -z
fi"#;

/// The checkout's root, HEAD's target and the parent, then the rest.
struct Header<'a> {
    top: PathBuf,
    head: String,
    parent: String,
    rest: &'a [u8],
}

fn run_git<'a>(run: &Run<'_>, dir: &Path, mode: &str, stdout: &'a mut Vec<u8>) -> Option<Header<'a>> {
    let out = match run(&["sh", "-c", GIT, "sh", mode], Some(dir)) {
        Ok(out) => out,
        Err(error) => {
            tracing::warn!("changes: git in {}: {error}", dir.display());
            return None;
        }
    };
    if !out.status.success() {
        return None;
    }
    *stdout = out.stdout;
    parse_header(stdout)
}

fn parse_header(stdout: &[u8]) -> Option<Header<'_>> {
    let mut rest = stdout;
    let top = PathBuf::from(take_field(&mut rest)?);
    let head = take_field(&mut rest)?;
    let parent = take_field(&mut rest)?;
    Some(Header {
        top,
        head,
        parent,
        rest,
    })
}

/// The NUL-terminated field at the front of `rest`, which moves past it.
fn take_field(rest: &mut &[u8]) -> Option<String> {
    let end = rest.iter().position(|b| *b == 0)?;
    let field = String::from_utf8_lossy(&rest[..end]).into_owned();
    *rest = &rest[end + 1..];
    Some(field)
}

pub(super) fn summary(run: &Run<'_>, dir: &Path) -> Option<Summary> {
    let mut stdout = Vec::new();
    let header = run_git(run, dir, "summary", &mut stdout)?;
    Some(parse_summary(&header))
}

pub(super) fn patch(run: &Run<'_>, dir: &Path) -> Option<String> {
    let mut stdout = Vec::new();
    let header = run_git(run, dir, "patch", &mut stdout)?;
    Some(String::from_utf8_lossy(header.rest).into_owned())
}

/// `--numstat -z` records, an empty record, then `--name-status -z` pairs.
fn parse_summary(header: &Header<'_>) -> Summary {
    let mut records = header.rest.split(|b| *b == 0);
    let (mut added, mut removed) = (0, 0);
    for record in records.by_ref() {
        if record.is_empty() {
            break;
        }
        // `added\tremoved\tpath`; binary files count `-`.
        let mut counts = record.splitn(3, |b| *b == b'\t');
        let mut count = || {
            counts
                .next()
                .and_then(|n| std::str::from_utf8(n).ok())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0)
        };
        added += count();
        removed += count();
    }
    let mut files = BTreeMap::new();
    let mut folders = BTreeSet::new();
    while let (Some(status), Some(path)) = (records.next(), records.next()) {
        if path.is_empty() {
            break;
        }
        let change = match status.first() {
            Some(b'A') => Some(FileChange::Added),
            Some(b'D') => None,
            _ => Some(FileChange::Modified),
        };
        let path = header.top.join(String::from_utf8_lossy(path).as_ref());
        folders.extend(
            path.ancestors()
                .skip(1)
                .take_while(|dir| dir.starts_with(&header.top) && *dir != header.top)
                .map(Path::to_path_buf),
        );
        if let Some(change) = change {
            files.insert(path, change);
        }
    }
    let branch = (!header.head.is_empty()).then(|| {
        header
            .head
            .strip_prefix("refs/heads/")
            .unwrap_or(&header.head)
            .to_owned()
    });
    Summary {
        branch,
        parent: (!header.parent.is_empty()).then(|| header.parent.clone()),
        diff: Some((added, removed)),
        files: Arc::new(files),
        folders: Arc::new(folders),
    }
}

/// `dir`'s git root, or `dir` itself (resolved) outside git; `None` if it
/// doesn't exist.
const ROOT: &str = "git rev-parse --show-toplevel 2>/dev/null || pwd -P";

pub(super) fn resolve_root(run: &Run<'_>, dir: &Path) -> Option<PathBuf> {
    let out = run(&["sh", "-c", ROOT], Some(dir))
        .inspect_err(|error| tracing::warn!("files: root of {}: {error}", dir.display()))
        .ok()?;
    let root = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    (out.status.success() && !root.is_empty()).then(|| PathBuf::from(root))
}

pub(super) fn head_branch(run: &Run<'_>, dir: &Path) -> Option<Option<String>> {
    let out = run(&["git", "symbolic-ref", "-q", "--short", "HEAD"], Some(dir)).ok()?;
    match out.status.code() {
        Some(0) => Some(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        // Detached.
        Some(1) => Some(None),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::local_git::{self, tests as fixtures};
    use super::super::remote::local_run;
    use super::*;

    fn remote(dir: &Path) -> Option<Summary> {
        summary(&local_run, dir)
    }

    fn feature_branch(name: &str) -> PathBuf {
        let dir = fixtures::temp_dir(name);
        std::fs::create_dir_all(dir.join("src/old")).unwrap();
        std::fs::write(dir.join("src/old/gone.txt"), "x\n").unwrap();
        let repo = fixtures::repo_on_main(&dir);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feat", &head, false).unwrap();
        repo.set_head("refs/heads/feat").unwrap();
        std::fs::write(dir.join("a.txt"), "one\n2\n").unwrap();
        fixtures::commit_all(&repo, "edit");
        std::fs::remove_file(dir.join("src/old/gone.txt")).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/new.rs"), "x\ny\n").unwrap();
        std::fs::write(dir.join("bin.dat"), b"\0bin").unwrap();
        dir
    }

    #[test]
    fn summary_should_match_local_on_a_feature_branch() {
        let dir = feature_branch("remote-feature");
        assert_eq!(remote(&dir), local_git::read(&dir));
    }

    #[test]
    fn summary_should_leave_the_real_index_alone() {
        let dir = feature_branch("remote-index");
        remote(&dir);
        let repo = git2::Repository::open(&dir).unwrap();
        let tracked = repo.index().unwrap().get_path(Path::new("lib/new.rs"), 0);
        assert!(tracked.is_none());
    }

    #[test]
    fn summary_should_match_local_on_main_without_upstream() {
        let dir = fixtures::temp_dir("remote-main");
        fixtures::repo_on_main(&dir);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        assert_eq!(remote(&dir), local_git::read(&dir));
    }

    #[test]
    fn summary_should_match_local_on_main_with_upstream() {
        let dir = fixtures::temp_dir("remote-upstream");
        let origin = dir.join("origin.git");
        git2::Repository::init_bare(&origin).unwrap();
        let local = dir.join("local");
        std::fs::create_dir_all(&local).unwrap();
        let repo = fixtures::repo_on_main(&local);
        repo.remote("origin", origin.to_str().unwrap()).unwrap();
        let mut remote_ref = repo.find_remote("origin").unwrap();
        remote_ref
            .push(&["refs/heads/main:refs/heads/main"], None)
            .unwrap();
        remote_ref.fetch(&["main"], None, None).unwrap();
        repo.find_branch("main", git2::BranchType::Local)
            .unwrap()
            .set_upstream(Some("origin/main"))
            .unwrap();
        std::fs::write(local.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        fixtures::commit_all(&repo, "ahead");
        let summary = remote(&local);
        assert_eq!(
            (summary.as_ref().and_then(|s| s.parent.as_deref()), summary.clone()),
            (Some("origin/main"), local_git::read(&local))
        );
    }

    #[test]
    fn summary_should_match_local_before_first_commit() {
        let dir = fixtures::temp_dir("remote-unborn");
        git2::Repository::init_opts(&dir, git2::RepositoryInitOptions::new().initial_head("dev"))
            .unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        assert_eq!(remote(&dir), local_git::read(&dir));
    }

    #[test]
    fn summary_should_be_none_outside_git() {
        let dir = fixtures::temp_dir("remote-plain");
        if git2::Repository::discover(&dir).is_err() {
            assert_eq!(remote(&dir), None);
        }
    }

    #[test]
    fn patch_should_hold_edits_and_new_files_since_parent() {
        let dir = feature_branch("remote-patch");
        let patch = patch(&local_run, &dir).unwrap();
        for line in ["diff --git a/a.txt b/a.txt", " one", "-two", "+2", "+x", "+y"] {
            assert!(patch.lines().any(|l| l == line), "{line:?} in {patch}");
        }
    }

    #[test]
    fn resolve_root_should_match_local() {
        let dir = feature_branch("remote-root");
        let sub = dir.join("src");
        assert_eq!(
            resolve_root(&local_run, &sub),
            crate::worktree::resolve_root(&sub)
        );
    }

    #[test]
    fn resolve_root_should_be_none_for_a_missing_folder() {
        let missing = Path::new("/no/such/grasslands-dir");
        assert_eq!(resolve_root(&local_run, missing), None);
    }

    #[test]
    fn head_branch_should_name_the_branch() {
        let dir = feature_branch("remote-branch");
        assert_eq!(head_branch(&local_run, &dir), Some(Some("feat".into())));
    }

    #[test]
    fn parse_summary_should_read_fixture_output() {
        let stdout = b"/r\0refs/heads/feat\0main\0\
            1\t1\ta.txt\0-\t-\tbin.dat\x002\t0\tlib/new.rs\0\0\
            M\0a.txt\0A\0bin.dat\0A\0lib/new.rs\0D\0src/old/gone.txt\0";
        let summary = parse_summary(&parse_header(stdout).unwrap());
        assert_eq!(
            (
                summary.branch.as_deref(),
                summary.parent.as_deref(),
                summary.diff,
                summary.files.len(),
                summary.folders.iter().cloned().collect::<Vec<_>>(),
            ),
            (
                Some("feat"),
                Some("main"),
                Some((3, 1)),
                3,
                vec![
                    PathBuf::from("/r/lib"),
                    PathBuf::from("/r/src"),
                    PathBuf::from("/r/src/old")
                ],
            )
        );
    }
}
