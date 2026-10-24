//! A Markdown file's rendered preview (decision 0026): GPUI Kit's text view
//! over the buffer's text, so it follows unsaved edits as they're typed.

use std::path::{Path, PathBuf};

use gpui_kit::base::text::TextView;
use gpui_kit::component::text::FrontmatterPlugin;
use gpui_kit::*;

use super::Editor;
use super::buffer::Buffer;

/// Rides along with a Markdown file's buffer.
pub struct Markdown {
    /// The tab shows the rendered text, not the source.
    pub shown: bool,
}

impl Markdown {
    pub fn new() -> Self {
        Self { shown: true }
    }
}

/// The rendered text of `buffer`. Links to files open in the editor, other
/// links in the default browser; a local file's images load from its folder.
pub fn preview(buffer: &Buffer, editor: WeakEntity<Editor>, cx: &App) -> AnyElement {
    let text = buffer.state.read(cx).value();
    let folder = buffer
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let machine = buffer.machine.clone();
    let links = folder.clone();
    let mut view = TextView::markdown(("markdown", buffer.state.entity_id()), text)
        .scrollable(true)
        .plugin(FrontmatterPlugin)
        .on_link_click(move |url, _, window, cx| {
            crate::sounds::play(cues::Cue::Tap, crate::sounds::subtle());
            match link_target(&links, url) {
                Some(path) => {
                    let opened = editor.update(cx, |editor, cx| {
                        editor.open(&machine, &path, window, cx);
                    });
                    if opened.is_err() {
                        tracing::debug!("editor: gone before opening {}", path.display());
                    }
                }
                None if url.starts_with('#') => {}
                None => cx.open_url(url),
            }
        });
    if buffer.machine.is_local() {
        view = view.image_source(move |url| match link_target(&folder, url) {
            Some(path) => path.into(),
            None => url.clone().into(),
        });
    }
    if buffer
        .path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdx"))
    {
        view = view.markdown_mdx();
    }
    view.size_full().px_6().into_any_element()
}

/// The file a link or image `url` in a document in `folder` points at, or
/// `None` for a URL with a scheme other than `file:` or a bare `#anchor`.
fn link_target(folder: &Path, url: &str) -> Option<PathBuf> {
    let url = match url.split_once(':') {
        Some((scheme, rest)) if is_scheme(scheme) => {
            if !scheme.eq_ignore_ascii_case("file") {
                return None;
            }
            rest.strip_prefix("//").unwrap_or(rest)
        }
        _ => url,
    };
    let path = url
        .split(['#', '?'])
        .next()
        .filter(|path| !path.is_empty())?;
    let path = PathBuf::from(
        percent_encoding::percent_decode_str(path)
            .decode_utf8_lossy()
            .as_ref(),
    );
    Some(if path.is_absolute() {
        path
    } else {
        folder.join(path)
    })
}

/// RFC 3986: a letter, then letters, digits, `+`, `-` or `.`.
fn is_scheme(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::link_target;

    const FOLDER: &str = "/repo/docs";

    #[test]
    fn relative_link_resolves_against_the_document_folder() {
        let target = link_target(Path::new(FOLDER), "../README.md");
        assert_eq!(target, Some(PathBuf::from("/repo/docs/../README.md")));
    }

    #[test]
    fn link_drops_its_anchor_and_decodes_escapes() {
        let target = link_target(Path::new(FOLDER), "my%20notes.md#setup");
        assert_eq!(target, Some(PathBuf::from("/repo/docs/my notes.md")));
    }

    #[test]
    fn file_url_is_a_file() {
        let target = link_target(Path::new(FOLDER), "file:///tmp/a.md");
        assert_eq!(target, Some(PathBuf::from("/tmp/a.md")));
    }

    #[test]
    fn web_link_is_not_a_file() {
        assert_eq!(link_target(Path::new(FOLDER), "https://herdr.dev"), None);
    }

    #[test]
    fn bare_anchor_is_not_a_file() {
        assert_eq!(link_target(Path::new(FOLDER), "#usage"), None);
    }
}
