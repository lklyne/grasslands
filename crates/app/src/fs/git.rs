//! A worktree's changes: its branch, the branch it was cut from, and the
//! diff since then, as line counts and changed paths (the file tree's footer
//! and colors) or a patch (the Changes tab); and a workspace folder's
//! checkout (the spaces list's branch, ahead/behind counts and repo). Read
//! by `crate::fs::Fs` with git itself, through the shell scripts below, on
//! this Mac (`local::run`) and on a saved machine (over its SSH master)
//! alike, so one script holds each rule. Each read is one command, so a poll
//! costs one round trip, and a patch read brings the summary with it: a
//! saved machine's footer reuses that while the Changes tab polls.
//!
//! The parent is the repo's default branch: the local branch `origin/HEAD`
//! names, else `main`, else `master`. On that branch itself the parent is
//! its upstream. The diff runs from the merge base to the working tree, so
//! it counts commits, staged and unstaged edits and untracked files alike,
//! like the PR the branch would become. Renames count as a deletion and an
//! addition.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::Run;
use crate::worktree::{Placement, Repo};

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

/// Finds the checkout's root, branch, parent and the diff's base (see
/// `crate::fs::git` for the rules), then prints `top\0HEAD's target\0parent\0`
/// and diffs from the base against a copy of the index where untracked files
/// are intent-to-add, so they count as added: with `$1` = `patch` the patch's
/// length, a NUL and the patch, then always the summary's `--raw --numstat`.
/// Exits 3 outside a work tree. Each git it starts costs a few milliseconds,
/// so it starts as few as the rules allow.
///
/// It writes nothing in the repo: Local's file tree watches the worktree,
/// `.git` included, and re-reads git on any change, so a write would have it
/// re-reading forever. Hence the copied index, the empty blob that `add -N`
/// writes (or freshens) going to a temp object store, and no fsmonitor
/// (its daemon writes cookie files in `.git`).
const GIT: &str = r#"r=$(git rev-parse --show-toplevel --git-path index 2>/dev/null) || exit 3
top=${r%%
*}
i=${r#*
}
case $i in /*) ;; *) i=$PWD/$i ;; esac
[ -n "$top" ] && cd "$top" || exit 3
h=$(git symbolic-ref -q HEAD)
b=${h#refs/heads/}
d=$(git symbolic-ref -q refs/remotes/origin/HEAD)
case $d in refs/remotes/origin/?*) d=${d#refs/remotes/origin/} ;; *) d= ;; esac
refs=$(git for-each-ref --format='%(refname)' refs/heads/main refs/heads/master \
  ${d:+"refs/heads/$d" "refs/remotes/origin/$d"})
has() {
  case "
$refs
" in *"
$1
"*) return 0 ;; esac
  return 1
}
if [ -z "$d" ]; then
  if has refs/heads/main; then d=main; elif has refs/heads/master; then d=master; fi
fi
p=
if [ -n "$b" ] && [ -n "$d" ]; then
  if [ "$b" != "$d" ]; then
    if has "refs/heads/$d"; then p=$d
    elif has "refs/remotes/origin/$d"; then p=origin/$d
    fi
  else
    p=$(git rev-parse -q --verify --abbrev-ref "$b@{upstream}" 2>/dev/null) || p=
  fi
fi
if [ -n "$p" ]; then
  base=$(git merge-base HEAD "$p" 2>/dev/null)
else
  base=$(git rev-parse -q --verify 'HEAD^{commit}' 2>/dev/null)
fi
[ -n "$base" ] || base=$(git hash-object -t tree /dev/null)
t=$(mktemp -d) || exit 4
trap 'rm -rf "$t"' EXIT
# -p keeps the index's time, by which git tells racily clean entries.
cp -p "$i" "$t/index" 2>/dev/null
GIT_INDEX_FILE=$t/index
export GIT_INDEX_FILE
mkdir "$t/objects"
GIT_OBJECT_DIRECTORY=$t/objects git -c core.fsmonitor=false add -N . >/dev/null 2>&1
printf '%s\0%s\0%s\0' "$top" "$h" "$p"
diff() {
  git -c core.quotePath=false -c core.fsmonitor=false diff --no-renames --no-ext-diff --no-color --no-relative \
    --src-prefix=a/ --dst-prefix=b/ "$@" "$base" --
}
if [ "$1" = patch ]; then
  diff >"$t/patch"
  printf '%s\0' "$(wc -c <"$t/patch" | tr -d ' ')"
  cat "$t/patch"
fi
diff --raw --numstat -z"#;

/// The checkout's root, HEAD's target and the parent, then the rest.
struct Header<'a> {
    top: PathBuf,
    head: String,
    parent: String,
    rest: &'a [u8],
}

/// `Ok(None)` outside git; an error when the machine couldn't be asked (ssh
/// itself failed, say as the connection drops), so callers keep what they
/// show rather than taking it for "not a repo".
fn run_git<'a>(
    run: &Run<'_>,
    dir: &Path,
    mode: &str,
    stdout: &'a mut Vec<u8>,
) -> io::Result<Option<Header<'a>>> {
    let out = run(&["sh", "-c", GIT, "sh", mode], Some(dir))
        .inspect_err(|error| tracing::warn!("changes: git in {}: {error}", dir.display()))?;
    if out.status.code() == Some(3) {
        return Ok(None);
    }
    if !out.status.success() {
        let error = io::Error::other(format!(
            "git in {} exited {}: {}",
            dir.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
        tracing::warn!("changes: {error}");
        return Err(error);
    }
    *stdout = out.stdout;
    parse_header(stdout)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unexpected git output"))
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

pub(super) fn summary(run: &Run<'_>, dir: &Path) -> io::Result<Option<Summary>> {
    let mut stdout = Vec::new();
    let header = run_git(run, dir, "summary", &mut stdout)?;
    Ok(header.map(|header| parse_summary(&header)))
}

/// The Changes tab's patch and the footer's summary, from one run.
pub(super) fn patch(run: &Run<'_>, dir: &Path) -> io::Result<Option<(String, Summary)>> {
    let mut stdout = Vec::new();
    let Some(mut header) = run_git(run, dir, "patch", &mut stdout)? else {
        return Ok(None);
    };
    let unexpected = || io::Error::new(io::ErrorKind::InvalidData, "unexpected git output");
    let len: usize = take_field(&mut header.rest)
        .and_then(|len| len.parse().ok())
        .ok_or_else(unexpected)?;
    let (patch, rest) = header.rest.split_at_checked(len).ok_or_else(unexpected)?;
    let patch = String::from_utf8_lossy(patch).into_owned();
    header.rest = rest;
    Ok(Some((patch, parse_summary(&header))))
}

/// How long a patch read's summary stands in for the footer's own read:
/// the poll interval, so while the Changes tab polls the footer needn't.
const SHARE_FOR: Duration = Duration::from_secs(3);

/// The summaries one machine's patch reads brought, by folder, kept for
/// `SHARE_FOR`. Each saved machine owns one, handed to its `Fs`.
#[derive(Default)]
pub struct SummaryCache {
    entries: Mutex<HashMap<PathBuf, (Instant, Option<Summary>)>>,
}

impl SummaryCache {
    /// Keeps the summary a patch read of `dir` brought, dropping stale ones.
    pub(super) fn put(&self, dir: &Path, summary: Option<Summary>) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        let now = Instant::now();
        entries.retain(|_, (at, _)| now.duration_since(*at) < SHARE_FOR);
        entries.insert(dir.to_path_buf(), (now, summary));
    }

    /// A summary of `dir` kept within `SHARE_FOR`.
    pub(super) fn get(&self, dir: &Path) -> Option<Option<Summary>> {
        let entries = self.entries.lock().ok()?;
        let (at, summary) = entries.get(dir)?;
        (at.elapsed() < SHARE_FOR).then(|| summary.clone())
    }
}

/// `--raw --numstat -z` output: `:modes hashes status` and path pairs, then
/// `added\tremoved\tpath` records.
fn parse_summary(header: &Header<'_>) -> Summary {
    let mut records = header.rest.split(|b| *b == 0);
    let (mut added, mut removed) = (0, 0);
    let mut files = BTreeMap::new();
    let mut folders = BTreeSet::new();
    while let Some(record) = records.next() {
        if record.first() != Some(&b':') {
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
            continue;
        }
        let Some(path) = records.next().filter(|p| !p.is_empty()) else {
            break;
        };
        let change = match record.last() {
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

/// The checkout containing the folder: its own git dir, the common git
/// dir (both resolved), the work tree's root (empty in a bare repo), HEAD's
/// target, and commits ahead and behind its upstream, each NUL-terminated.
/// Exits 3 outside git.
const CHECKOUT: &str = r#"r=$(git rev-parse --absolute-git-dir --git-common-dir --show-toplevel 2>/dev/null)
[ -n "$r" ] || exit 3
g=${r%%
*}
r=${r#*
}
c=${r%%
*}
top=
case $r in *"
"*) top=${r#*
} ;; esac
g=$(cd "$g" && pwd -P) && c=$(cd "$c" && pwd -P) || exit 3
h=$(git symbolic-ref -q HEAD)
ab=
if [ -n "$h" ]; then
  ab=$(git rev-list --left-right --count "$h...${h#refs/heads/}@{upstream}" 2>/dev/null)
fi
printf '%s\0%s\0%s\0%s\0%s\0' "$g" "$c" "$top" "$h" "$ab""#;

/// The checkout containing `dir`; `Ok(None)` outside git.
pub(super) fn checkout(run: &Run<'_>, dir: &Path) -> io::Result<Option<Checkout>> {
    let out = run(&["sh", "-c", CHECKOUT], Some(dir))
        .inspect_err(|error| tracing::warn!("workspaces: git in {}: {error}", dir.display()))?;
    if out.status.code() == Some(3) {
        return Ok(None);
    }
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git in {} exited {}",
            dir.display(),
            out.status
        )));
    }
    parse_checkout(&out.stdout)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unexpected git output"))
}

fn parse_checkout(stdout: &[u8]) -> Option<Checkout> {
    let mut rest = stdout;
    let git_dir = PathBuf::from(take_field(&mut rest)?);
    let common = PathBuf::from(take_field(&mut rest)?);
    let top = take_field(&mut rest)?;
    let head = take_field(&mut rest)?;
    let counts = take_field(&mut rest)?;
    let linked = git_dir != common;
    let root = if linked {
        // A linked worktree's common dir is the main checkout's `.git`.
        common.parent().map(Path::to_path_buf)
    } else {
        (!top.is_empty()).then(|| PathBuf::from(&top))
    };
    let placement = root.and_then(|root| {
        Some(Placement {
            repo: Repo {
                key: common.to_string_lossy().into_owned(),
                name: root.file_name()?.to_string_lossy().into_owned(),
                root: root.to_string_lossy().into_owned(),
            },
            linked,
        })
    });
    let mut counts = counts.split_whitespace().map(str::parse::<usize>);
    let ahead_behind = match (counts.next(), counts.next()) {
        (Some(Ok(ahead)), Some(Ok(behind))) => Some((ahead, behind)),
        _ => None,
    };
    Some(Checkout {
        branch: (!head.is_empty())
            .then(|| head.strip_prefix("refs/heads/").unwrap_or(&head).to_owned()),
        ahead_behind,
        placement,
        git_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::super::local::run;
    use super::super::test_repo::{TestRepo, temp_dir};
    use super::*;

    fn feature_branch(name: &str) -> TestRepo {
        let repo = TestRepo::on_main(&temp_dir(name));
        repo.git(&["switch", "-q", "-c", "feat"]);
        repo.write("a.txt", "one\n2\n");
        repo.commit_all("edit");
        repo.write("lib/new.rs", "x\ny\n");
        repo
    }

    #[test]
    fn summary_should_leave_the_real_index_alone() {
        let repo = feature_branch("git-index");
        summary(&run, &repo.dir).unwrap();
        assert_eq!(repo.git(&["ls-files", "lib"]), "");
    }

    #[test]
    fn patch_should_hold_edits_and_new_files_since_parent() {
        let repo = feature_branch("git-patch");
        let (patch, _) = patch(&run, &repo.dir).unwrap().unwrap();
        for line in [
            "diff --git a/a.txt b/a.txt",
            " one",
            "-two",
            "+2",
            "+x",
            "+y",
        ] {
            assert!(patch.lines().any(|l| l == line), "{line:?} in {patch}");
        }
    }

    #[test]
    fn patch_should_bring_the_same_summary_as_a_summary_read() {
        let repo = feature_branch("git-patch-summary");
        let (_, from_patch) = patch(&run, &repo.dir).unwrap().unwrap();
        assert_eq!(Some(from_patch), summary(&run, &repo.dir).unwrap());
    }

    #[test]
    fn cached_summary_should_be_found_for_its_folder_only() {
        let cache = SummaryCache::default();
        cache.put(Path::new("/r"), None);
        assert_eq!(
            (cache.get(Path::new("/r")), cache.get(Path::new("/s"))),
            (Some(None), None)
        );
    }

    #[test]
    fn cache_should_drop_stale_summaries_when_one_is_put() {
        let cache = SummaryCache::default();
        let stale = Instant::now().checked_sub(SHARE_FOR).unwrap();
        cache
            .entries
            .lock()
            .unwrap()
            .insert(PathBuf::from("/old"), (stale, None));
        cache.put(Path::new("/r"), None);
        assert!(
            !cache
                .entries
                .lock()
                .unwrap()
                .contains_key(Path::new("/old"))
        );
    }

    #[test]
    fn resolve_root_should_be_none_for_a_missing_folder() {
        let missing = Path::new("/no/such/grasslands-dir");
        assert_eq!(resolve_root(&run, missing), None);
    }

    #[test]
    fn parse_summary_should_read_fixture_output() {
        let stdout = b"/r\0refs/heads/feat\0main\0\
            :100644 100644 1111111 0000000 M\0a.txt\0\
            :000000 100644 0000000 0000000 A\0bin.dat\0\
            :000000 100644 0000000 0000000 A\0lib/new.rs\0\
            :100644 000000 2222222 0000000 D\0src/old/gone.txt\0\
            1\t1\ta.txt\0-\t-\tbin.dat\x002\t0\tlib/new.rs\x000\t0\tsrc/old/gone.txt\0";
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

    /// The test machine's `~/api`: branch `feature` off `main`,
    /// `src/main.rs` modified, `notes.txt` untracked, `target/` ignored.
    #[test]
    #[ignore = "needs GRASSLANDS_SSH_TEST_TARGET and the dockerbox container"]
    fn remote_api_repo_should_read_like_local() {
        use super::super::remote::{filter_listings, search, ssh_run};
        let api = Path::new("/home/lyle/api");
        let summary = summary(&ssh_run, api).unwrap().expect("a summary");
        let files: Vec<_> = summary.files.iter().map(|(p, c)| (p.clone(), *c)).collect();
        assert_eq!(
            (summary.branch.as_deref(), summary.parent.as_deref(), files),
            (
                Some("feature"),
                Some("main"),
                vec![
                    (api.join("notes.txt"), FileChange::Added),
                    (api.join("src/main.rs"), FileChange::Modified),
                ]
            )
        );
        let listing = vec![
            herdr_client::RemoteEntry {
                name: "target".into(),
                file_name: "target".into(),
                is_dir: true,
                is_symlink: false,
                len: 0,
                modified: None,
            },
            herdr_client::RemoteEntry {
                name: "notes.txt".into(),
                file_name: "notes.txt".into(),
                is_dir: false,
                is_symlink: false,
                len: 0,
                modified: None,
            },
        ];
        let shown: Vec<_> = filter_listings(&ssh_run, vec![(api, Ok(listing))])
            .remove(0)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(shown, vec!["notes.txt"]);
        let found = search(
            &ssh_run,
            api,
            "main",
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(
            found.names.iter().any(|h| h.relative == "src/main.rs"),
            "{found:?}"
        );
        assert!(
            patch(&ssh_run, api)
                .unwrap()
                .unwrap()
                .0
                .contains("diff --git a/src/main.rs")
        );
        assert_eq!(
            resolve_root(&ssh_run, &api.join("src")),
            Some(api.to_path_buf())
        );
        assert_eq!(
            checkout(&ssh_run, api).unwrap().and_then(|c| c.branch),
            Some("feature".into())
        );
    }
}
