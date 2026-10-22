//! A pane's scrollback as a read-only tab: all of herdr's text for the pane,
//! so selection and search aren't limited to what the terminal shows.
//! Cmd-hover underlines a URL and cmd-click opens it.

use std::ops::Range;
use std::str::FromStr;

use gpui_kit::component::input::{DefinitionProvider, Rope, RopeExt as _};
use gpui_kit::{App, Task, Window};

/// Answers "go to definition" with the URL under the pointer, which the
/// editor opens in the browser.
pub struct UrlLinks;

impl DefinitionProvider for UrlLinks {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        Task::ready(Ok(link_at(text, offset).into_iter().collect()))
    }
}

fn link_at(text: &Rope, offset: usize) -> Option<lsp_types::LocationLink> {
    let row = text.offset_to_point(offset).row;
    let start = text.line_start_offset(row);
    let line = text.slice_line(row).to_string();
    let range = url_at(&line, offset - start)?;
    let target_uri = lsp_types::Uri::from_str(&line[range.clone()]).ok()?;
    Some(lsp_types::LocationLink {
        origin_selection_range: Some(lsp_types::Range::new(
            text.offset_to_position(start + range.start),
            text.offset_to_position(start + range.end),
        )),
        target_uri,
        target_range: lsp_types::Range::default(),
        target_selection_range: lsp_types::Range::default(),
    })
}

/// The http(s) URL in `line` covering byte `col`, without trailing
/// punctuation or a closing bracket it didn't open.
fn url_at(line: &str, col: usize) -> Option<Range<usize>> {
    line.match_indices("http")
        .map(|(start, _)| start)
        .filter(|&start| {
            let rest = &line[start..];
            rest.starts_with("https://") || rest.starts_with("http://")
        })
        .map(|start| start..start + url_len(&line[start..]))
        .find(|range| range.contains(&col))
}

fn url_len(url: &str) -> usize {
    let mut end = url
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
        .unwrap_or(url.len());
    while let Some(last) = url[..end].chars().next_back() {
        let open = match last {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            '.' | ',' | ';' | ':' | '!' | '?' => last,
            _ => break,
        };
        let url = &url[..end];
        if open != last && url.matches(open).count() >= url.matches(last).count() {
            break;
        }
        end -= last.len_utf8();
    }
    end
}

#[cfg(test)]
mod tests {
    use super::url_at;

    fn url(line: &str, col: usize) -> Option<&str> {
        url_at(line, col).map(|range| &line[range])
    }

    #[test]
    fn url_at_should_find_the_url_under_the_column() {
        assert_eq!(url("see https://a.dev/x now", 8), Some("https://a.dev/x"));
    }

    #[test]
    fn url_at_should_skip_text_outside_urls() {
        assert_eq!(url("see https://a.dev/x now", 1), None);
    }

    #[test]
    fn url_at_should_drop_trailing_punctuation() {
        assert_eq!(url("(at http://a.dev/x).", 5), Some("http://a.dev/x"));
    }

    #[test]
    fn url_at_should_keep_balanced_parentheses() {
        assert_eq!(
            url("https://en.wikipedia.org/wiki/Rust_(language)", 0),
            Some("https://en.wikipedia.org/wiki/Rust_(language)")
        );
    }

    #[test]
    fn url_at_should_pick_the_second_of_two_urls() {
        assert_eq!(url("http://a.dev http://b.dev", 15), Some("http://b.dev"));
    }
}
