//! A saved machine's listings and search: SFTP listings filtered by the
//! machine's `git check-ignore`, and search by `git ls-files`/`git grep` in
//! a repo or `find`/`grep -rIn` outside one, parsed into the same results as
//! Local's. Commands go through a [`Run`] so tests can run them on this Mac.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use herdr_client::{ExecOutput, RemoteEntry, RemoteFs};

use super::{Entry, sort_entries};
use crate::sidebar::files::SearchResults;
use crate::sidebar::files::search::Collector;

/// Runs `argv` in `cwd` on the machine.
pub(super) type Run<'a> = dyn Fn(&[&str], Option<&Path>) -> io::Result<ExecOutput> + 'a;

/// Long enough for a search of a big repo over a slow link.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Output lines kept from a content search (`SEARCH` stops there too): the
/// line cap, with room for files that run one past their own cap.
const MAX_GREP_LINES: usize = 2000;

pub(super) fn runner(
    files: &RemoteFs,
) -> impl Fn(&[&str], Option<&Path>) -> io::Result<ExecOutput> {
    move |argv, cwd| files.exec(argv, cwd, TIMEOUT)
}

/// Drops `.git` and what git ignores (folders asked with a trailing `/`, so
/// `target/` patterns match), then sorts like Local. Outside a repo, or if
/// git can't be asked, nothing is dropped.
pub(super) fn filter_listing(run: &Run<'_>, dir: &Path, listing: Vec<RemoteEntry>) -> Vec<Entry> {
    let listing: Vec<RemoteEntry> = listing.into_iter().filter(|e| e.name != ".git").collect();
    let ignored = ignored_names(run, dir, &listing);
    let mut entries: Vec<Entry> = listing
        .into_iter()
        .filter(|e| !ignored.contains(&e.name))
        .map(|e| Entry {
            path: dir.join(&e.name),
            name: e.name,
            is_dir: e.is_dir,
        })
        .collect();
    sort_entries(&mut entries);
    entries
}

const CHECK_IGNORE: &str = r#"printf '%s\0' "$@" | git check-ignore -z --stdin"#;

fn ignored_names(run: &Run<'_>, dir: &Path, listing: &[RemoteEntry]) -> HashSet<String> {
    if listing.is_empty() {
        return HashSet::new();
    }
    let asked: Vec<String> = listing
        .iter()
        .map(|e| {
            if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            }
        })
        .collect();
    let mut argv = vec!["sh", "-c", CHECK_IGNORE, "sh"];
    argv.extend(asked.iter().map(String::as_str));
    match run(&argv, Some(dir)) {
        // 0: some are ignored; 1: none; 128: not in a repo.
        Ok(out) if out.status.success() => parse_ignored(&out.stdout),
        Ok(_) => HashSet::new(),
        Err(error) => {
            tracing::warn!("files: check-ignore in {}: {error}", dir.display());
            HashSet::new()
        }
    }
}

fn parse_ignored(stdout: &[u8]) -> HashSet<String> {
    stdout
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            String::from_utf8_lossy(name)
                .trim_end_matches('/')
                .to_owned()
        })
        .collect()
}

/// `$1` is the query, `$2` is `-i` or empty (smart case). Prints matching
/// paths one per line, an empty NUL-terminated record, then grep's matches.
const SEARCH: &str = r#"q=$1; i=$2
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git ls-files -z -co --exclude-standard | tr '\0' '\n' | grep -F $i -e "$q" | head -n 101
  printf '\0'
  git -c core.quotePath=false grep -n -z -I -F --untracked -m 21 $i -e "$q" | head -n 2000
else
  find . -name .git -prune -o -type f -print | sed 's|^\./||' | grep -F $i -e "$q" | head -n 101
  printf '\0'
  grep -rIn --null -F -m 21 --exclude-dir=.git $i -e "$q" . | head -n 2000
fi"#;

/// Names and contents below `root` matching `query`, with Local's caps and
/// smart case.
pub(super) fn search(
    run: &Run<'_>,
    root: &Path,
    query: &str,
    cancel: &AtomicBool,
) -> SearchResults {
    if query.is_empty() {
        return SearchResults::default();
    }
    let mut collector = Collector::new(query);
    let case = if collector.ignore_case() { "-i" } else { "" };
    let out = match run(&["sh", "-c", SEARCH, "sh", query, case], Some(root)) {
        Ok(out) => out,
        Err(error) => {
            tracing::warn!("files: search in {}: {error}", root.display());
            return SearchResults::default();
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return SearchResults::default();
    }
    let (names, grep) = split_search_output(&out.stdout);
    for relative in names {
        collector.offer_name(&root.join(&relative), &relative);
    }
    for (relative, lines) in grep {
        if !collector.wants_lines() {
            break;
        }
        let lines = lines.iter().map(|(line, text)| (*line, text.as_str()));
        collector.offer_lines(&root.join(&relative), relative.clone(), lines);
    }
    collector.finish()
}

/// One file's matching lines from grep: 0-based numbers and text.
type FileLines = (String, Vec<(u32, String)>);

/// The paths (sorted the way Local walks) and the matches grouped by file
/// (sorted the same way), from `SEARCH`'s output.
fn split_search_output(stdout: &[u8]) -> (Vec<String>, Vec<FileLines>) {
    let (names, grep) = match stdout.iter().position(|b| *b == 0) {
        Some(ix) => (&stdout[..ix], &stdout[ix + 1..]),
        None => (stdout, &[][..]),
    };
    let mut names: Vec<String> = String::from_utf8_lossy(names)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    names.sort_by(|a, b| Path::new(a).cmp(Path::new(b)));
    (names, parse_grep(grep))
}

/// `git grep -n -z` (`path\0line\0text`) or `grep -n --null`
/// (`./path\0line:text`)
/// output, one match per line.
fn parse_grep(output: &[u8]) -> Vec<FileLines> {
    let mut files: Vec<FileLines> = Vec::new();
    for record in output.split(|b| *b == b'\n').take(MAX_GREP_LINES) {
        let Some(nul) = record.iter().position(|b| *b == 0) else {
            continue;
        };
        let path = String::from_utf8_lossy(&record[..nul]);
        let path = path.strip_prefix("./").unwrap_or(&path).to_owned();
        let rest = &record[nul + 1..];
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let Some(line) = std::str::from_utf8(&rest[..digits])
            .ok()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let text = rest.get(digits + 1..).unwrap_or_default();
        let text = String::from_utf8_lossy(text)
            .trim_end_matches('\r')
            .to_owned();
        match files.last_mut() {
            Some((last, lines)) if *last == path => lines.push((line.saturating_sub(1), text)),
            _ => files.push((path, vec![(line.saturating_sub(1), text)])),
        }
    }
    files.sort_by(|a, b| Path::new(&a.0).cmp(Path::new(&b.0)));
    files
}

/// Runs commands on this Mac, as a machine would.
#[cfg(test)]
pub(super) fn local_run(argv: &[&str], cwd: Option<&Path>) -> io::Result<ExecOutput> {
    let mut command = std::process::Command::new(argv[0]);
    command.args(&argv[1..]);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let out = command.output()?;
    Ok(ExecOutput {
        status: out.status,
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

/// Runs commands on the test machine over plain ssh, the way `RemoteFs`
/// does over its master: `GRASSLANDS_SSH_TEST_TARGET=ssh://user@host:port`.
#[cfg(test)]
pub(super) fn ssh_run(argv: &[&str], cwd: Option<&Path>) -> io::Result<ExecOutput> {
    let target = std::env::var("GRASSLANDS_SSH_TEST_TARGET").map_err(io::Error::other)?;
    let target = target.trim_start_matches("ssh://");
    let (host, port) = target.rsplit_once(':').unwrap_or((target, "22"));
    let quote = |word: &str| format!("'{}'", word.replace('\'', r"'\''"));
    let mut line = argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
    if let Some(cwd) = cwd {
        line = format!("cd {} && {line}", quote(&cwd.to_string_lossy()));
    }
    let ssh = ["ssh", "-o", "BatchMode=yes", "-p", port, host, &line];
    local_run(&ssh, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::sidebar::files::search::MAX_HITS_PER_FILE;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "grasslands-remote-fs-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn listing(dir: &Path) -> Vec<RemoteEntry> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                RemoteEntry {
                    name: e.file_name().to_string_lossy().into_owned(),
                    file_name: e.file_name(),
                    is_dir: e.path().is_dir(),
                    is_symlink: false,
                    len: 0,
                    modified: None,
                }
            })
            .collect()
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn filter_listing_should_match_local_listing() {
        let dir = temp_dir("list");
        git2::Repository::init(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("b.rs"), "").unwrap();
        std::fs::write(dir.join("A.md"), "").unwrap();
        std::fs::write(dir.join("debug.log"), "").unwrap();
        let remote = filter_listing(&local_run, &dir, listing(&dir));
        assert_eq!(remote, super::super::local::read_dir(&dir).unwrap());
    }

    #[test]
    fn filter_listing_should_keep_everything_outside_git() {
        let dir = temp_dir("plain");
        // temp dirs can sit inside a repo on some machines; only assert when not.
        if git2::Repository::discover(&dir).is_ok() {
            return;
        }
        std::fs::write(dir.join("x.log"), "").unwrap();
        assert_eq!(
            names(&filter_listing(&local_run, &dir, listing(&dir))),
            vec!["x.log"]
        );
    }

    #[test]
    fn parse_grep_should_read_git_grep_and_grep_output() {
        let output = b"src/main.rs\x001\x00fn main() {\n./notes.txt\x0012:a: b\r\n";
        assert_eq!(
            parse_grep(output),
            vec![
                ("notes.txt".to_owned(), vec![(11, "a: b".to_owned())]),
                (
                    "src/main.rs".to_owned(),
                    vec![(0, "fn main() {".to_owned())]
                ),
            ]
        );
    }

    #[test]
    fn split_search_output_should_sort_names_like_a_walk() {
        let (names, _) = split_search_output(b"a-b.rs\na/x.rs\n\x00");
        assert_eq!(names, vec!["a/x.rs", "a-b.rs"]);
    }

    fn search_repo(name: &str) -> PathBuf {
        let dir = temp_dir(&format!("search-{name}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git2::Repository::init(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/needle.txt"), "needle").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0").unwrap();
        std::fs::write(
            dir.join("src/needle.rs"),
            "one\n  Needle here\nthree needle\n",
        )
        .unwrap();
        std::fs::write(dir.join("a.txt"), "needle").unwrap();
        dir
    }

    #[test]
    fn search_should_match_local_search_in_a_repo() {
        let dir = search_repo("repo");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            search(&local_run, &dir, "needle", &cancel),
            super::super::local::search(&dir, "needle", &cancel)
        );
    }

    #[test]
    fn search_should_respect_case_when_query_has_capitals() {
        let dir = search_repo("case");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            search(&local_run, &dir, "Needle", &cancel),
            super::super::local::search(&dir, "Needle", &cancel)
        );
    }

    #[test]
    fn search_should_cap_lines_per_file_outside_a_repo() {
        let dir = temp_dir("search-plain");
        if git2::Repository::discover(&dir).is_ok() {
            return;
        }
        let text: String = (0..30).map(|i| format!("needle {i}\n")).collect();
        std::fs::write(dir.join("many.txt"), text).unwrap();
        let results = search(&local_run, &dir, "needle", &AtomicBool::new(false));
        assert_eq!(
            (results.contents[0].lines.len(), results.truncated),
            (MAX_HITS_PER_FILE, true)
        );
    }
}
