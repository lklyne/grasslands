# 0026 — Markdown tabs on GPUI Kit's text view

Date: 2026-10-24. Status: accepted (first cut).

Opening a `.md`, `.mdx` or `.markdown` file shows it rendered. The same
Preview / Source toggle as HTML tabs (0025) switches to the code editor.

## Why GPUI Kit's text view

gpui-kit 0.7.3 already ships a Markdown renderer (`gpui_kit::base::text::
TextView`): GFM, MDX, a plugin API, and the theme's colors and tree-sitter
code highlighting, installed globally by the component theme. It draws GPUI
elements, so none of 0025's native-view work (visibility, frames, first
responder) applies. The alternatives were HTML from `comrak` in a web view
(a WebKit process per tab, theme as generated CSS, but mermaid and math for
free) and third-party GPUI renderers, which pin other GPUI versions.

We use Base's `TextView` rather than the component facade: the facade has
no `image_source`, and with no style of its own it renders the same.

## How it works

- `editor::markdown::Markdown` rides along with the buffer like `Page`, and
  defaults to the preview. It owns the parsed text (`TextViewState`) and
  the scroll position, so both survive switching tabs or to the source:
  element state, which `TextView::markdown` and `overflow_y_scrollbar` use,
  is dropped the first frame the preview isn't drawn. Each render hands the
  state the editor's text, so it follows unsaved edits and reloads; the
  state reparses only when the text changes.
- A file opened at a line (a search hit) shows its source, at that line. A
  file renamed to Markdown keeps showing its source.
- **Layout.** A centered column of at most 816px, with 64px above and
  below, in a GPUI Kit scroll area that spans the tab. The text view's own
  scrolling is a virtualized list that can only pad its viewport, so the
  margins wouldn't scroll away and the scrollbar would sit at the column's
  edge. The price is laying out the whole document; revisit if long files
  get slow.
- YAML frontmatter is parsed (`MarkdownExtensions::frontmatter`, off by
  default) and the frontmatter plugin shows it as a description list.
- **Links.** A relative, absolute or `file:` link opens in the editor on the
  buffer's machine, its `..` folded away so a remote file matches its open
  tab; `#anchors` and empty links do nothing; anything else opens in the
  default browser. A right-click follows nothing.
- **Images.** In a local file, image paths load from disk relative to the
  file; URLs load as usual.

## Not yet

- `data:` images in local files: our `image_source` replaces Base's own
  handling, which decodes them.
- Relative images in remote files.
- Anchor links scrolling to their heading.
- A side-by-side split, and scroll sync between source and preview.
- Mermaid and math.
