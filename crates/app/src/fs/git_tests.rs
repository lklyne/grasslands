//! The git scripts' reads on this Mac, as `Fs::Local` makes them, over
//! fixture repos covering the Changes and spaces-list rules. Until the
//! scripts replaced git2 on Local, these also had to match git2's reads.

use std::path::{Path, PathBuf};

use super::Fs;
use super::git::{self, Checkout, Summary};
use super::local::run;
use super::test_repo::{TestRepo, git, temp_dir};

#[derive(Debug, PartialEq, Eq)]
struct Readout {
    summary: Option<Summary>,
    checkout: Option<Checkout>,
    root: Option<PathBuf>,
}

fn read(dir: &Path) -> Readout {
    let summary = Fs::Local.git_summary(dir).unwrap();
    let (_, from_patch) = git::patch(&run, dir).unwrap().unzip();
    assert_eq!(from_patch, summary, "a patch read's summary");
    let root = Fs::Local.resolve_root(dir);
    assert_eq!(git::resolve_root(&run, dir), root, "the script's root");
    Readout {
        summary,
        checkout: Fs::Local.checkout(dir),
        root,
    }
}

/// The readout in short, with paths below `base`: what the footer, the
/// tree's colors, the spaces row and the followed root would show.
fn render(readout: &Readout, base: &Path) -> String {
    let rel = |path: &Path| -> String {
        let rel = path.strip_prefix(base).unwrap_or(path).to_string_lossy();
        if rel.is_empty() {
            ".".into()
        } else {
            rel.into_owned()
        }
    };
    let mut out = Vec::new();
    match &readout.summary {
        None => out.push("summary: none".to_owned()),
        Some(s) => {
            out.push(format!(
                "branch {:?} parent {:?} diff {:?}",
                s.branch, s.parent, s.diff
            ));
            for (path, change) in s.files.iter() {
                out.push(format!("{change:?} {}", rel(path)));
            }
            let folders: Vec<String> = s.folders.iter().map(|p| rel(p)).collect();
            let folders = if folders.is_empty() {
                "none".to_owned()
            } else {
                folders.join(" ")
            };
            out.push(format!("folders {folders}"));
        }
    }
    match &readout.checkout {
        None => out.push("checkout: none".to_owned()),
        Some(c) => out.push(format!(
            "checkout {:?} {:?} {} git dir {}",
            c.branch,
            c.ahead_behind,
            c.placement
                .as_ref()
                .map_or("unplaced".to_owned(), |p| format!(
                    "repo {} at {} key {}{}",
                    p.repo.name,
                    rel(Path::new(&p.repo.root)),
                    rel(Path::new(&p.repo.key)),
                    if p.linked { " linked" } else { "" }
                )),
            rel(&c.git_dir)
        )),
    }
    out.push(format!("root {:?}", readout.root.as_deref().map(rel)));
    out.join("\n")
}

/// Local reads `dir` as `expected` (paths below `base`).
fn assert_reads(dir: &Path, base: &Path, expected: &[&str]) {
    assert_eq!(render(&read(dir), base), expected.join("\n"));
}

/// `base` and the repo folder in it.
fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let base = temp_dir(name);
    let repo = base.join("repo");
    (base, repo)
}

/// `feat` off `main`: committed, staged, unstaged, deleted, untracked
/// (nested folders, binary, odd names), ignored and mode changes.
fn feature_branch(name: &str) -> (PathBuf, PathBuf) {
    let (base, dir) = fixture(name);
    let repo = TestRepo::on_main(&dir);
    repo.write("src/old/gone.txt", "x\n");
    repo.write("a b.txt", "spaced\n");
    repo.write("é.txt", "accent\n");
    repo.write("run.sh", "echo\n");
    repo.write(".gitignore", "target/\n");
    repo.commit_all("more");
    repo.git(&["switch", "-q", "-c", "feat"]);
    repo.write("a.txt", "one\n2\n");
    repo.commit_all("edit");
    repo.write("a b.txt", "spaced\nstaged\n");
    repo.git(&["add", "a b.txt"]);
    repo.write("é.txt", "accent\nunstaged\n");
    repo.remove("src/old/gone.txt");
    repo.write("lib/new.rs", "x\ny\n");
    repo.write("lib/deep/er/leaf.txt", "leaf\n");
    repo.write("bin.dat", b"\0bin");
    repo.write("target/out.txt", "ignored\n");
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    std::fs::set_permissions(
        dir.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    (base, dir)
}

const FEATURE_BRANCH: &[&str] = &[
    r#"branch Some("feat") parent Some("main") diff Some((6, 2))"#,
    "Modified repo/a b.txt",
    "Modified repo/a.txt",
    "Added repo/bin.dat",
    "Added repo/lib/deep/er/leaf.txt",
    "Added repo/lib/new.rs",
    "Modified repo/run.sh",
    "Modified repo/é.txt",
    "folders repo/lib repo/lib/deep repo/lib/deep/er repo/src repo/src/old",
    r#"checkout Some("feat") None repo repo at repo key repo/.git git dir repo/.git"#,
    r#"root Some("repo")"#,
];

/// Every entry below `dir` with its modification time and size.
fn snapshot(dir: &Path) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            }
            out.push((entry.path(), meta.modified().unwrap(), meta.len()));
        }
    }
    out.sort();
    out
}

/// The files panel watches the whole worktree, `.git` included, and
/// re-reads git on any change there: a read that wrote into `.git` would
/// keep re-reading itself.
#[test]
fn reads_should_write_nothing_in_the_git_dir() {
    let (_, dir) = feature_branch("git-dir-untouched");
    let git_dir = dir.join(".git");
    let before = snapshot(&git_dir);
    // The second read finds what the first one would have written.
    read(&dir);
    read(&dir);
    let after = snapshot(&git_dir);
    let changed: Vec<_> = after.iter().filter(|e| !before.contains(e)).collect();
    let gone: Vec<_> = before.iter().filter(|e| !after.contains(e)).collect();
    assert_eq!((changed, gone), (vec![], vec![]));
}

#[test]
fn feature_branch_should_count_every_kind_of_change() {
    let (base, dir) = feature_branch("parity-feature");
    assert_reads(&dir, &base, FEATURE_BRANCH);
}

#[test]
fn subfolder_should_read_as_its_checkout() {
    let (base, dir) = feature_branch("parity-sub");
    assert_reads(&dir.join("lib"), &base, FEATURE_BRANCH);
}

#[test]
fn main_without_upstream_should_diff_from_head() {
    let (base, dir) = fixture("parity-main");
    let repo = TestRepo::on_main(&dir);
    repo.write("a.txt", "one\n");
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("main") parent None diff Some((0, 1))"#,
            "Modified repo/a.txt",
            "folders none",
            r#"checkout Some("main") None repo repo at repo key repo/.git git dir repo/.git"#,
            r#"root Some("repo")"#,
        ],
    );
}

/// `local`'s `main`, two commits ahead of `origin/main` and one behind, with
/// an uncommitted edit.
fn ahead_and_behind(name: &str) -> (PathBuf, PathBuf) {
    let base = temp_dir(name);
    let origin = base.join("origin.git");
    git(&base, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    let local = TestRepo::on_main(&base.join("local"));
    local.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
    local.git(&["push", "-q", "-u", "origin", "main"]);
    git(&base, &["clone", "-q", "origin.git", "other"]);
    let other = TestRepo::open(&base.join("other"));
    other.write("theirs.txt", "theirs\n");
    other.commit_all("theirs");
    other.git(&["push", "-q", "origin", "main"]);
    local.git(&["fetch", "-q", "origin"]);
    for message in ["two", "three"] {
        local.write(&format!("{message}.txt"), message);
        local.commit_all(message);
    }
    local.write("a.txt", "edited\n");
    (base, local.dir)
}

#[test]
fn default_branch_should_diff_from_its_upstream_and_count_ahead_behind() {
    let (base, dir) = ahead_and_behind("parity-upstream");
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("main") parent Some("origin/main") diff Some((3, 2))"#,
            "Modified local/a.txt",
            "Added local/three.txt",
            "Added local/two.txt",
            "folders none",
            r#"checkout Some("main") Some((2, 1)) repo local at local key local/.git git dir local/.git"#,
            r#"root Some("local")"#,
        ],
    );
}

#[test]
fn branch_should_fall_back_to_origins_default_branch() {
    let (base, dir) = ahead_and_behind("parity-origin-head");
    let repo = TestRepo::open(&dir);
    repo.git(&["remote", "set-head", "origin", "main"]);
    repo.git(&["switch", "-q", "-c", "feat"]);
    repo.git(&["branch", "-q", "-D", "main"]);
    repo.write("feat.txt", "feat\n");
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("feat") parent Some("origin/main") diff Some((4, 2))"#,
            "Modified local/a.txt",
            "Added local/feat.txt",
            "Added local/three.txt",
            "Added local/two.txt",
            "folders none",
            r#"checkout Some("feat") None repo local at local key local/.git git dir local/.git"#,
            r#"root Some("local")"#,
        ],
    );
}

#[test]
fn origin_head_should_name_the_default_branch() {
    let (base, dir) = ahead_and_behind("parity-develop");
    let repo = TestRepo::open(&dir);
    repo.git(&["switch", "-q", "-c", "develop"]);
    repo.write("develop.txt", "d\n");
    repo.commit_all("develop");
    repo.git(&["push", "-q", "origin", "develop"]);
    repo.git(&["remote", "set-head", "origin", "develop"]);
    repo.git(&["switch", "-q", "-c", "feat"]);
    repo.write("feat.txt", "feat\n");
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("feat") parent Some("develop") diff Some((1, 0))"#,
            "Added local/feat.txt",
            "folders none",
            r#"checkout Some("feat") None repo local at local key local/.git git dir local/.git"#,
            r#"root Some("local")"#,
        ],
    );
}

#[test]
fn master_should_be_the_default_branch_without_main() {
    let (base, dir) = fixture("parity-master");
    let repo = TestRepo::init(&dir, "master");
    repo.write("a.txt", "one\n");
    repo.commit_all("init");
    repo.git(&["switch", "-q", "-c", "feat"]);
    repo.write("a.txt", "one\ntwo\n");
    repo.commit_all("edit");
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("feat") parent Some("master") diff Some((1, 0))"#,
            "Modified repo/a.txt",
            "folders none",
            r#"checkout Some("feat") None repo repo at repo key repo/.git git dir repo/.git"#,
            r#"root Some("repo")"#,
        ],
    );
}

#[test]
fn detached_head_should_diff_from_head_without_a_branch() {
    let (base, dir) = fixture("parity-detached");
    let repo = TestRepo::on_main(&dir);
    repo.write("b.txt", "b\n");
    repo.commit_all("b");
    repo.git(&["switch", "-q", "--detach", "HEAD~1"]);
    // The same size as before, likely within the same second: racily clean.
    repo.write("a.txt", "changed\n");
    repo.write("new/file.txt", "new\n");
    assert_reads(
        &dir,
        &base,
        &[
            "branch None parent None diff Some((2, 2))",
            "Modified repo/a.txt",
            "Added repo/new/file.txt",
            "folders repo/new",
            "checkout None None repo repo at repo key repo/.git git dir repo/.git",
            r#"root Some("repo")"#,
        ],
    );
}

#[test]
fn unborn_branch_should_count_everything_as_added() {
    let (base, dir) = fixture("parity-unborn");
    let repo = TestRepo::init(&dir, "dev");
    repo.write("a.txt", "x\n");
    repo.write("src/deep/b.txt", "y\nz\n");
    repo.write("staged.txt", "s\n");
    repo.git(&["add", "staged.txt"]);
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("dev") parent None diff Some((4, 0))"#,
            "Added repo/a.txt",
            "Added repo/src/deep/b.txt",
            "Added repo/staged.txt",
            "folders repo/src repo/src/deep",
            r#"checkout Some("dev") None repo repo at repo key repo/.git git dir repo/.git"#,
            r#"root Some("repo")"#,
        ],
    );
}

#[test]
fn renames_should_count_as_deleted_and_added() {
    let (base, dir) = fixture("parity-renames");
    let repo = TestRepo::on_main(&dir);
    repo.write("c.txt", "c\nc\nc\n");
    repo.write("e.txt", "e\ne\n");
    repo.commit_all("more");
    repo.git(&["switch", "-q", "-c", "feat"]);
    repo.git(&["mv", "a.txt", "b.txt"]);
    repo.commit_all("committed rename");
    std::fs::rename(dir.join("c.txt"), dir.join("d.txt")).unwrap();
    repo.git(&["mv", "e.txt", "f.txt"]);
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("feat") parent Some("main") diff Some((7, 7))"#,
            "Added repo/b.txt",
            "Added repo/d.txt",
            "Added repo/f.txt",
            "folders none",
            r#"checkout Some("feat") None repo repo at repo key repo/.git git dir repo/.git"#,
            r#"root Some("repo")"#,
        ],
    );
}

/// A linked worktree `wt` of `main` on branch `feat`, with edits in `src`.
fn linked_worktree(name: &str) -> PathBuf {
    let base = temp_dir(name);
    let repo = TestRepo::on_main(&base.join("main"));
    repo.write("src/lib.rs", "lib\n");
    repo.commit_all("src");
    repo.git(&["worktree", "add", "-q", "-b", "feat", "../wt"]);
    let wt = TestRepo::open(&base.join("wt"));
    wt.write("src/lib.rs", "lib\nmore\n");
    wt.write("src/new.rs", "new\n");
    base
}

const LINKED_WORKTREE: &[&str] = &[
    r#"branch Some("feat") parent Some("main") diff Some((2, 0))"#,
    "Modified wt/src/lib.rs",
    "Added wt/src/new.rs",
    "folders wt/src",
    r#"checkout Some("feat") None repo main at main key main/.git linked git dir main/.git/worktrees/wt"#,
    r#"root Some("wt")"#,
];

#[test]
fn linked_worktree_should_be_placed_under_its_main_checkout() {
    let base = linked_worktree("parity-linked");
    assert_reads(&base.join("wt"), &base, LINKED_WORKTREE);
}

#[test]
fn linked_worktree_subfolder_should_read_as_the_worktree() {
    let base = linked_worktree("parity-linked-sub");
    assert_reads(&base.join("wt/src"), &base, LINKED_WORKTREE);
}

#[test]
fn main_checkout_should_not_count_as_linked() {
    let base = linked_worktree("parity-linked-main");
    assert_reads(
        &base.join("main"),
        &base,
        &[
            r#"branch Some("main") parent None diff Some((0, 0))"#,
            "folders none",
            r#"checkout Some("main") None repo main at main key main/.git git dir main/.git"#,
            r#"root Some("main")"#,
        ],
    );
}

#[test]
fn unrelated_parent_should_diff_from_the_empty_tree() {
    let (base, dir) = fixture("parity-orphan");
    let repo = TestRepo::on_main(&dir);
    repo.git(&["switch", "-q", "--orphan", "feat"]);
    repo.write("orphan.txt", "o\n");
    repo.commit_all("orphan");
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("feat") parent Some("main") diff Some((1, 0))"#,
            "Added repo/orphan.txt",
            "folders none",
            r#"checkout Some("feat") None repo repo at repo key repo/.git git dir repo/.git"#,
            r#"root Some("repo")"#,
        ],
    );
}

#[test]
fn gone_upstream_should_be_no_parent() {
    let (base, dir) = ahead_and_behind("parity-gone");
    let repo = TestRepo::open(&dir);
    repo.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    assert_reads(
        &dir,
        &base,
        &[
            r#"branch Some("main") parent None diff Some((1, 2))"#,
            "Modified local/a.txt",
            "folders none",
            r#"checkout Some("main") None repo local at local key local/.git git dir local/.git"#,
            r#"root Some("local")"#,
        ],
    );
}

#[test]
fn folder_outside_git_should_be_its_own_root() {
    let dir = temp_dir("parity-plain");
    // temp dirs can sit inside a repo on some machines; only compare when not.
    if !super::test_repo::in_git(&dir) {
        assert_reads(
            &dir,
            &dir,
            &["summary: none", "checkout: none", r#"root Some(".")"#],
        );
    }
}
