# 0025 — HTML tabs on the system WebKit

Date: 2026-10-22. Status: accepted (first cut).

Opening an `.html` or `.htm` file shows it as a web page. A Preview / Source
toggle above the page switches the tab to the code editor and back.

## Why the system WebKit

`WKWebView` comes from `WebKit.framework`, which ships with macOS, so it adds
next to nothing to the bundle. A bundled Chromium (CEF) would add more than
150 MB.

The binding is `objc2-web-kit`, which comes from the same objc2 family the
app already uses for AppKit. `wry` would also use WKWebView on macOS, but it
brings more dependencies and its own ideas about windows and parent views. It
only pays off if we ship on Windows or Linux.

## How it works

- `crate::web::WebView` wraps one `WKWebView`. It's a subview of GPUI's
  `NSView`, like the Ghostty views (0008): hidden until shown, framed from a
  `canvas` in GPUI window coordinates (y flipped), and removed from the
  window on drop.
- An HTML file is still an editor `Buffer`, so it keeps the editor's tabs,
  worktree groups, save, close prompts, watching and polling. A `Page`
  rides along with the buffer and defaults to showing the page.
- **Visibility is explicit.** A native view stays on screen when GPUI stops
  drawing it. The workspace calls `Editor::set_shown` whenever the main area
  switches, or a panel drag starts or ends. On each render, the editor shows
  only the selected tab's page, and only while the editor is on screen.
- **Keys.** While a page is shown, GPUI focus sits on the editor's own focus
  handle, so the `Editor` bindings (cmd-w, cmd-s) still apply.
- **Reloads.** A local file loads with `loadFileURL`, with read access to
  the file's folder so its relative stylesheets, scripts and images resolve.
  The page reloads when the buffer takes new disk contents or a save
  finishes. A remote file loads its text with `loadHTMLString` and no base
  URL, so relative links don't resolve there.
- **Floating UI.** Chrome's menus, palette and dialogs already draw in the
  overlay window above native views (0008), so they cover pages too.

## Not yet

- Reloading when a sibling stylesheet or script changes; only the HTML file
  is watched.
- Opening URLs and Claude artifacts (`claude.ai/.../artifact/...`). Those
  need a sign-in inside the app's WebKit data store, which is separate from
  Safari's.
- Remote pages with relative assets. That needs a `WKURLSchemeHandler` that
  serves files through `Fs`.
- Script hooks and a live scenario for pages.
