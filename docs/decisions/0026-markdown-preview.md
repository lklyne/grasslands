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
  defaults to the preview. The preview renders the editor state's text each
  frame, so it follows unsaved edits; the view reparses only when the text
  changes.
- The frontmatter plugin shows YAML frontmatter as a description list.
- **Links.** A relative, absolute or `file:` link opens in the editor on the
  buffer's machine; `#anchors` do nothing; anything else opens in the
  default browser.
- **Images.** In a local file, image paths load from disk relative to the
  file; URLs load as usual.

## Not yet

- `data:` images in local files: our `image_source` replaces Base's own
  handling, which decodes them.
- Relative images in remote files.
- Anchor links scrolling to their heading.
- A side-by-side split, and scroll sync between source and preview.
- Mermaid and math.
