//! What a search of the worktree by file name and by contents finds, and
//! the matching and caps every file system's search shares (see
//! `crate::fs::Fs::search`).

use std::ops::Range;
use std::path::{Path, PathBuf};

/// Caps that keep a one-letter query from listing the whole repo.
const MAX_NAME_HITS: usize = 100;
const MAX_LINE_HITS: usize = 500;
pub(super) const MAX_HITS_PER_FILE: usize = 20;
/// Files larger than this are matched by name only.
pub(super) const MAX_FILE_BYTES: u64 = 1 << 20;
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

/// Gathers matches under the caps from files offered in display order, so
/// every file system's search gives the same `Results`.
pub(super) struct Collector {
    matcher: Matcher,
    results: Results,
    line_hits: usize,
}

impl Collector {
    pub fn new(query: &str) -> Self {
        Self {
            matcher: Matcher::new(query),
            results: Results::default(),
            line_hits: 0,
        }
    }

    /// Smart case: a query with no capitals ignores case.
    pub fn ignore_case(&self) -> bool {
        self.matcher.ignore_case
    }

    pub fn offer_name(&mut self, path: &Path, relative: &str) {
        let Some(range) = self.matcher.find(relative) else {
            return;
        };
        if self.results.names.len() < MAX_NAME_HITS {
            self.results.names.push(NameHit {
                path: path.to_path_buf(),
                relative: relative.to_owned(),
                range,
            });
        } else {
            self.results.truncated = true;
        }
    }

    /// Whether another file's lines still fit under the cap.
    pub fn wants_lines(&mut self) -> bool {
        if self.line_hits >= MAX_LINE_HITS {
            self.results.truncated = true;
            return false;
        }
        true
    }

    /// Matches one file's `(0-based number, line)` pairs.
    pub fn offer_lines<'a>(
        &mut self,
        path: &Path,
        relative: String,
        lines: impl IntoIterator<Item = (u32, &'a str)>,
    ) {
        let mut hits = Vec::new();
        for (line, content) in lines {
            let Some(range) = self.matcher.find(content) else {
                continue;
            };
            if hits.len() == MAX_HITS_PER_FILE {
                self.results.truncated = true;
                break;
            }
            let (text, range) = excerpt(content, range);
            hits.push(LineHit { line, text, range });
        }
        if !hits.is_empty() {
            self.line_hits += hits.len();
            self.results.contents.push(FileHits {
                path: path.to_path_buf(),
                relative,
                lines: hits,
            });
        }
    }

    pub fn finish(self) -> Results {
        self.results
    }
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
