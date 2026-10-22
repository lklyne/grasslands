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
  rides along with the buffer and defaults to showing the page. Its web view
  is made the first time it's shown, since each one is a WebKit process.
- **Visibility is explicit.** A native view stays on screen when GPUI stops
  drawing it. The workspace's `sync_native_views` calls `Editor::set_shown`
  whenever the main area switches, or a panel drag starts or ends. The editor
  re-syncs on every change to what's selected (activate, Preview / Source,
  worktree switch, rename, removal), not in render, and shows only the
  selected tab's page while it's on screen.
- **Keys.** While a page is shown, GPUI focus sits on the editor's own focus
  handle, so the `Editor` bindings (cmd-w, cmd-s) still apply. A clicked page
  becomes AppKit's first responder and passes keys it ignores up to GPUI's
  view, which handles them a second time. So the page hands first responder
  back to GPUI's view when it hides, when it's dropped, on any mouse-down
  GPUI sees, and when the editor loses GPUI focus.
- **Loading.** A local file loads with `loadFileURL`. Its read access is the
  worktree it's in, so links between the worktree's files resolve; a file
  outside the active worktree may read only itself. A remote file loads its
  text with `loadHTMLString` and no base URL. Disk changes, saves and Reload
  load the file again rather than reloading the current URL, so a page that
  followed a link comes back to the file.
- **Navigation.** A navigation delegate keeps the page on `file:`, `about:`,
  `data:` and `blob:` URLs and lets frames load freely. A clicked link to
  anything else opens in the default browser; a redirect or script that
  navigates away is dropped (logged at debug), so a page can't launch apps on
  its own. A clicked `target=_blank` link to a file loads in the page, but
  only if the file is inside the page's read access, since the app's own
  load isn't held to the page's limits.
- **Floating UI.** Chrome's menus, palette and dialogs already draw in the
  overlay window above native views (0008), and the editor's error
  notifications go there too (`chrome::floating_in`), so pages can't cover
  them.
- Web Inspector is on in debug builds only.

## Not yet

- Reloading when a sibling stylesheet or script changes; only the HTML file
  is watched.
- Opening URLs and Claude artifacts (`claude.ai/.../artifact/...`). Those
  need a sign-in inside the app's WebKit data store, which is separate from
  Safari's.
- Remote pages with relative assets. That needs a `WKURLSchemeHandler` that
  serves files through `Fs`.
- Pages run JavaScript and load web resources unrestricted; only navigation
  of the page itself is limited.
- Script hooks and a live scenario for pages.
