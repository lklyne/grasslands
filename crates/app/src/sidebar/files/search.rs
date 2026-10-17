//! Searching the worktree by file name and by contents, .gitignore respected.
//! Runs on the background executor; `cancel` stops a search the user has
//! typed past.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Caps that keep a one-letter query from listing the whole repo.
const MAX_NAME_HITS: usize = 100;
const MAX_LINE_HITS: usize = 500;
const MAX_HITS_PER_FILE: usize = 20;
/// Files larger than this are matched by name only.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// Lines are cut to this many characters around the match.
const MAX_LINE_CHARS: usize = 160;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Results {
    /// Files whose path below the root matches.
    pub names: Vec<NameHit>,
    /// Files whose contents match, in walk order.
    pub contents: Vec<FileHits>,
    /// A cap was hit and some matches are missing.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameHit {
    pub path: PathBuf,
    /// The path below the root, as shown.
    pub relative: String,
    /// The match within `relative`, in bytes.
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHits {
    pub path: PathBuf,
    pub relative: String,
    pub lines: Vec<LineHit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineHit {
    /// 0-based.
    pub line: u32,
    /// The line, trimmed and cut to fit.
    pub text: String,
    /// The match within `text`, in bytes.
    pub range: Range<usize>,
}

/// Matches `query` against every path and file below `root`. Smart case: a
/// query with no capitals ignores case.
pub fn search(root: &Path, query: &str, cancel: &AtomicBool) -> Results {
    let mut results = Results::default();
    if query.is_empty() {
        return results;
    }
    let matcher = Matcher::new(query);
    let walk = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .sort_by_file_path(Ord::cmp)
        .build();
    let mut line_hits = 0;
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
        if let Some(range) = matcher.find(&relative) {
            if results.names.len() < MAX_NAME_HITS {
                results.names.push(NameHit {
                    path: path.to_path_buf(),
                    relative: relative.clone(),
                    range,
                });
            } else {
                results.truncated = true;
            }
        }
        if line_hits >= MAX_LINE_HITS {
            results.truncated = true;
            continue;
        }
        let lines = search_file(path, &matcher, &mut results.truncated);
        if !lines.is_empty() {
            line_hits += lines.len();
            results.contents.push(FileHits {
                path: path.to_path_buf(),
                relative,
                lines,
            });
        }
    }
    results
}

fn search_file(path: &Path, matcher: &Matcher, truncated: &mut bool) -> Vec<LineHit> {
    if std::fs::metadata(path).map_or(true, |m| m.len() > MAX_FILE_BYTES) {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    // Binary files, by the same test git uses.
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut hits = Vec::new();
    for (line, content) in text.lines().enumerate() {
        let Some(range) = matcher.find(content) else {
            continue;
        };
        if hits.len() == MAX_HITS_PER_FILE {
            *truncated = true;
            break;
        }
        let (text, range) = excerpt(content, range);
        hits.push(LineHit {
            line: line as u32,
            text,
            range,
        });
    }
    hits
}

/// `line` with leading whitespace dropped and cut to `MAX_LINE_CHARS`,
/// keeping the match in view; `range` is moved to match.
fn excerpt(line: &str, range: Range<usize>) -> (String, Range<usize>) {
    let trimmed = line.trim_start();
    let mut start = line.len() - trimmed.len();
    if range.start < start {
        start = range.start;
    }
    // Start a little before a match that would fall off the end.
    let chars_before = line[start..range.start].chars().count();
    if chars_before + (range.end - range.start) > MAX_LINE_CHARS {
        let skip = chars_before.saturating_sub(MAX_LINE_CHARS / 4);
        start += line[start..]
            .char_indices()
            .nth(skip)
            .map_or(0, |(ix, _)| ix);
    }
    let end = line[start..]
        .char_indices()
        .nth(MAX_LINE_CHARS)
        .map_or(line.len(), |(ix, _)| start + ix)
        .max(range.end);
    (
        line[start..end].trim_end().to_owned(),
        range.start - start..(range.end - start).min(end - start),
    )
}

struct Matcher {
    query: String,
    ignore_case: bool,
}

impl Matcher {
    fn new(query: &str) -> Self {
        let ignore_case = !query.chars().any(char::is_uppercase);
        Self {
            query: query.to_owned(),
            ignore_case,
        }
    }

    /// The first match in `haystack`, as a byte range of `haystack`.
    fn find(&self, haystack: &str) -> Option<Range<usize>> {
        if !self.ignore_case {
            let start = haystack.find(&self.query)?;
            return Some(start..start + self.query.len());
        }
        // Compare char by char so ranges stay valid in the original text,
        // which lowercasing can change the length of.
        let needle: Vec<char> = self.query.chars().collect();
        let chars: Vec<(usize, char)> = haystack.char_indices().collect();
        let first = needle.first()?;
        chars.windows(needle.len()).find_map(|window| {
            let (start, c) = window[0];
            if !eq_ignore_case(c, *first) {
                return None;
            }
            window
                .iter()
                .zip(&needle)
                .all(|((_, a), b)| eq_ignore_case(*a, *b))
                .then(|| {
                    let (last, c) = window[window.len() - 1];
                    start..last + c.len_utf8()
                })
        })
    }
}

fn eq_ignore_case(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grasslands-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git2::Repository::init(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn run(dir: &Path, query: &str) -> Results {
        search(dir, query, &AtomicBool::new(false))
    }

    #[test]
    fn search_should_match_names_below_root() {
        let dir = temp_dir("names");
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
        let dir = temp_dir("lines");
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
        let dir = temp_dir("ignored");
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/x.txt"), "needle").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0").unwrap();
        assert_eq!(run(&dir, "needle"), Results::default());
    }

    #[test]
    fn search_should_respect_case_when_query_has_capitals() {
        let dir = temp_dir("case");
        std::fs::write(dir.join("a.txt"), "Needle\nneedle\n").unwrap();
        assert_eq!(run(&dir, "Needle").contents[0].lines.len(), 1);
    }

    #[test]
    fn find_should_give_ranges_in_the_original_text() {
        let matcher = Matcher::new("straße");
        assert_eq!(matcher.find("Die STRASSE, die Straße"), Some(17..24));
    }

    #[test]
    fn excerpt_should_keep_a_late_match_in_view() {
        let line = format!("{}needle", "x".repeat(400));
        let (text, range) = excerpt(&line, 400..406);
        assert_eq!(&text[range], "needle");
    }
}
