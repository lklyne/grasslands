//! A Markdown file's rendered preview (decision 0026): GPUI Kit's text view
//! over the buffer's text, so it follows unsaved edits as they're typed.

use std::path::{Component, Path, PathBuf};

use gpui_kit::base::text::{MarkdownExtensions, TextView, TextViewState};
use gpui_kit::base::{InteractiveElementExt as _, Scrollbar};
use gpui_kit::component::text::FrontmatterPlugin;
use gpui_kit::*;

use super::Editor;
use super::buffer::Buffer;

/// The preview's column, side padding included: about 90 characters of body
/// text, centered in a wider tab.
const MAX_WIDTH: Pixels = px(816.);

/// Rides along with a Markdown file's buffer. The parsed text and scroll
/// position live here rather than in element state, which GPUI drops on the
/// first frame the preview isn't drawn (another tab, or the source).
pub struct Markdown {
    /// The tab shows the rendered text, not the source.
    pub shown: bool,
    text: Entity<TextViewState>,
    scroll: ScrollHandle,
}

impl Markdown {
    pub fn new(text: &str, shown: bool, cx: &mut App) -> Self {
        Self {
            shown,
            text: cx.new(|cx| TextViewState::markdown(text, cx)),
            scroll: ScrollHandle::new(),
        }
    }
}

/// The rendered text of `buffer`. Links to files open in the editor, other
/// links in the default browser; a local file's images load from its folder.
pub fn preview(
    buffer: &Buffer,
    markdown: &Markdown,
    editor: WeakEntity<Editor>,
    cx: &mut App,
) -> AnyElement {
    // Follows unsaved edits and reloads alike; unchanged text is a no-op.
    let text = buffer.state.read(cx).value();
    markdown
        .text
        .update(cx, |state, cx| state.set_text(&text, cx));
    let folder = buffer
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let machine = buffer.machine.clone();
    let links = folder.clone();
    let id = buffer.state.entity_id();
    let mut extensions = MarkdownExtensions::default()
        .frontmatter()
        .plugin(FrontmatterPlugin);
    if buffer
        .path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdx"))
    {
        extensions = extensions.mdx();
    }
    let mut view = TextView::new(&markdown.text)
        .markdown_extensions(extensions)
        .on_link_click(move |url, event, window, cx| {
            // A custom handler gets right-clicks too; those aren't follows.
            if event.is_secondary() {
                return;
            }
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
                None if url.is_empty() || url.starts_with(['#', '?']) => {}
                None => cx.open_url(url),
            }
        });
    if buffer.machine.is_local() {
        view = view.image_source(move |url| match link_target(&folder, url) {
            Some(path) => path.into(),
            None => url.clone().into(),
        });
    }
    // Our scroll area rather than the view's own, which pads only its
    // viewport: here the margins scroll away with the text and the scrollbar
    // stays at the tab's edge. The cost is laying out the whole document.
    let column = view.w_full().max_w(MAX_WIDTH).px_6().py_16();
    let area = div()
        .id(("markdown-area", id))
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .overflow_y_scroll()
        .lock_scroll_axis()
        .track_scroll(&markdown.scroll)
        .child(column.flex_none());
    div()
        .id(("markdown-scroll", id))
        .size_full()
        .relative()
        .child(area)
        .child(
            div().absolute().inset_0().child(
                Scrollbar::vertical(&markdown.scroll)
                    .id(("markdown-scrollbar", id))
                    .viewport_from_layout(),
            ),
        )
        .into_any_element()
}

/// The file a link or image `url` in a document in `folder` points at, or
/// `None` for a URL with a scheme other than `file:` or a bare `#anchor`.
fn link_target(folder: &Path, url: &str) -> Option<PathBuf> {
    let url = match url.split_once(':') {
        Some((scheme, rest)) if is_scheme(scheme) => {
            if !scheme.eq_ignore_ascii_case("file") {
                return None;
            }
            // `file://host/path`: the path starts at the authority's end.
            match rest.strip_prefix("//") {
                Some(authority) => &authority[authority.find('/').unwrap_or(authority.len())..],
                None => rest,
            }
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
    let path = if path.is_absolute() {
        path
    } else {
        folder.join(path)
    };
    Some(normalize(&path))
}

/// `path` with `.` and `..` folded away, without touching the disk: a remote
/// file can't be canonicalized, and its tab is matched by path.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
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
        assert_eq!(target, Some(PathBuf::from("/repo/README.md")));
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
    fn file_url_with_a_host_drops_the_host() {
        let target = link_target(Path::new(FOLDER), "file://localhost/tmp/a.md");
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
