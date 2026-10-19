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
/// `target/` patterns match), then sorts like Local. One command asks about
/// every folder. Outside a repo nothing is dropped; if the machine couldn't
/// be asked, each listing fails rather than showing ignored files.
pub(super) fn filter_listings(
    run: &Run<'_>,
    listings: Vec<(&Path, io::Result<Vec<RemoteEntry>>)>,
) -> Vec<io::Result<Vec<Entry>>> {
    let listings: Vec<(&Path, io::Result<Vec<RemoteEntry>>)> = listings
        .into_iter()
        .map(|(dir, listing)| {
            let listing = listing.map(|l| l.into_iter().filter(|e| e.name != ".git").collect());
            (dir, listing)
        })
        .collect();
    let asked: Vec<(&Path, &[RemoteEntry])> = listings
        .iter()
        .filter_map(|(dir, listing)| Some((*dir, listing.as_deref().ok()?)))
        .collect();
    let mut ignored = match ignored_names(run, &asked) {
        Ok(ignored) => ignored.into_iter(),
        Err(error) => {
            let kind = error.kind();
            return listings
                .into_iter()
                .map(|(_, listing)| {
                    listing.and_then(|_| Err(io::Error::new(kind, error.to_string())))
                })
                .collect();
        }
    };
    listings
        .into_iter()
        .map(|(dir, listing)| {
            let listing = listing?;
            let ignored = ignored.next().unwrap_or_default();
            let mut entries: Vec<Entry> = listing
                .into_iter()
                .filter(|e| !ignored.contains(&e.name))
                .map(|e| Entry {
                    path: dir.join(&e.file_name),
                    name: e.name,
                    is_dir: e.is_dir,
                })
                .collect();
            sort_entries(&mut entries);
            Ok(entries)
        })
        .collect()
}

/// Arguments are each folder, its name count, then its names. Prints each
/// folder's ignored names NUL-terminated, then an empty record.
const CHECK_IGNORE: &str = r#"while [ $# -ge 2 ]; do
  d=$1 n=$2; shift 2
  i=0
  for x do
    [ "$i" -lt "$n" ] || break
    printf '%s\0' "$x"; i=$((i + 1))
  done | (cd "$d" 2>/dev/null && git check-ignore -z --stdin)
  printf '\0'
  shift "$n"
done"#;

/// The ignored names in each of `asked`, in order.
fn ignored_names(
    run: &Run<'_>,
    asked: &[(&Path, &[RemoteEntry])],
) -> io::Result<Vec<HashSet<String>>> {
    if asked.iter().all(|(_, listing)| listing.is_empty()) {
        return Ok(vec![HashSet::new(); asked.len()]);
    }
    let mut words: Vec<String> = Vec::new();
    for (dir, listing) in asked {
        words.push(dir.to_string_lossy().into_owned());
        words.push(listing.len().to_string());
        words.extend(listing.iter().map(|e| {
            if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            }
        }));
    }
    let mut argv = vec!["sh", "-c", CHECK_IGNORE, "sh"];
    argv.extend(words.iter().map(String::as_str));
    let out =
        run(&argv, None).inspect_err(|error| tracing::warn!("files: check-ignore: {error}"))?;
    // 255 is ssh's own failure (say sshd's session limit), not the script's.
    if out.status.code() == Some(255) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!(
                "check-ignore failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    let mut sets = parse_ignored(&out.stdout);
    sets.resize_with(asked.len(), HashSet::new);
    Ok(sets)
}

/// One set per folder: names until each empty record.
fn parse_ignored(stdout: &[u8]) -> Vec<HashSet<String>> {
    let mut sets = Vec::new();
    let mut current = HashSet::new();
    for record in stdout.split(|b| *b == 0) {
        if record.is_empty() {
            sets.push(std::mem::take(&mut current));
        } else {
            let name = String::from_utf8_lossy(record);
            current.insert(name.trim_end_matches('/').to_owned());
        }
    }
    sets
}

/// `$1` is the query, `$2` is `-i` or empty (smart case). Prints matching
/// paths one per line, an empty NUL-terminated record, then grep's matches
/// in files up to Local's 1 MiB cap (`MAX_FILE_BYTES`). Runs in C.UTF-8 where
/// the machine has it, so `-i` folds more than ASCII; C otherwise.
const SEARCH: &str = r#"q=$1; i=$2
if locale -a 2>/dev/null | grep -qixE 'c\.utf-?8'; then LC_ALL=C.UTF-8; else LC_ALL=C; fi
export LC_ALL
small() {
  xargs -0 -r sh -c 'for f do shift; set -- "$@" "./$f"; done
    find "$@" -prune -type f -size -1048577c -print0' sh 2>/dev/null
}
matches() {
  xargs -0 -r grep -n -H --null -I -F -m 21 $i -e "$q" -- 2>/dev/null | head -n 2000
}
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git ls-files -z -co --exclude-standard | tr '\0' '\n' | grep -F $i -e "$q" | head -n 101
  printf '\0'
  git ls-files -z -co --exclude-standard | small | matches
else
  find . -name .git -prune -o -type f -print | sed 's|^\./||' | grep -F $i -e "$q" | head -n 101
  printf '\0'
  find . -name .git -prune -o -type f -size -1048577c -print0 | matches
fi"#;

/// Names and contents below `root` matching `query`, with Local's caps and
/// smart case.
pub(super) fn search(
    run: &Run<'_>,
    root: &Path,
    query: &str,
    cancel: &AtomicBool,
) -> SearchResults {
    // No line or name holds a line break, so Local finds nothing either.
    if query.is_empty() || query.contains(['\n', '\r']) {
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

/// `grep -n --null` (`./path\0line:text`) or `git grep -n -z`
/// (`path\0line\0text`) output, one match per line.
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

    fn filter_listing(dir: &Path) -> Vec<Entry> {
        let mut out = filter_listings(&local_run, vec![(dir, Ok(listing(dir)))]);
        out.pop().unwrap().unwrap()
    }

    #[test]
    fn filter_listings_should_ask_about_each_folder_in_its_own_repo() {
        let a = temp_dir("batch-a");
        let b = temp_dir("batch-b");
        for (dir, ignore) in [(&a, "*.log\n"), (&b, "*.tmp\n")] {
            git2::Repository::init(dir).unwrap();
            std::fs::write(dir.join(".gitignore"), ignore).unwrap();
            std::fs::write(dir.join("x.log"), "").unwrap();
            std::fs::write(dir.join("x.tmp"), "").unwrap();
        }
        let out = filter_listings(
            &local_run,
            vec![
                (a.as_path(), Ok(listing(&a))),
                (b.as_path(), Ok(listing(&b))),
            ],
        );
        let names: Vec<Vec<String>> = out
            .into_iter()
            .map(|l| l.unwrap().into_iter().map(|e| e.name).collect())
            .collect();
        assert_eq!(
            names,
            vec![
                vec![".gitignore".to_owned(), "x.tmp".to_owned()],
                vec![".gitignore".to_owned(), "x.log".to_owned()],
            ]
        );
    }

    #[test]
    fn filter_listings_should_fail_listings_when_machine_cannot_be_asked() {
        let dir = temp_dir("batch-down");
        std::fs::write(dir.join("x"), "").unwrap();
        let down = |_: &[&str], _: Option<&Path>| -> io::Result<ExecOutput> {
            Err(io::ErrorKind::NotConnected.into())
        };
        let out = filter_listings(&down, vec![(dir.as_path(), Ok(listing(&dir)))]);
        assert_eq!(
            out[0].as_ref().map_err(io::Error::kind).err(),
            Some(io::ErrorKind::NotConnected)
        );
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
        let remote = filter_listing(&dir);
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
        assert_eq!(names(&filter_listing(&dir)), vec!["x.log"]);
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
    fn search_should_match_large_files_by_name_only_like_local() {
        let dir = search_repo("large");
        let big = "needle\n".repeat(160_000);
        std::fs::write(dir.join("big-needle.txt"), big).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            search(&local_run, &dir, "needle", &cancel),
            super::super::local::search(&dir, "needle", &cancel)
        );
    }

    #[test]
    fn search_should_find_nothing_for_a_query_with_a_line_break() {
        let dir = search_repo("newline");
        let results = search(&local_run, &dir, "one\nthree", &AtomicBool::new(false));
        assert_eq!(results, SearchResults::default());
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
