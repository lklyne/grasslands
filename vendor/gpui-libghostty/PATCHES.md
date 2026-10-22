# Local patches to gpui-libghostty 0.3.1

Vendored from crates.io and applied through `[patch.crates-io]`. See
`docs/decisions/0001-ghostty-gpui-kit.md`.

- `TerminalConfiguration::File(PathBuf)`: a surface starts from Ghostty's
  built-in defaults plus one config file chosen by the application, so each
  terminal can have its own font, theme and (later) shader.
- `Terminal::load_config_file(&Path)`: reconfigures a running terminal from a
  new config file without restarting its process.
- `Terminal::update_theme` on a `File` surface layers the new colors over a
  copy of that file instead of dropping its settings.
- `Terminal::paste_text(&str)`: inserts text through Ghostty's paste path
  (bracketed paste when the program enables it), for dropped file paths.
- `impl Focusable for Terminal`, so an app can tell which terminal holds
  keyboard focus and hand focus between them.

- The macOS shim's `set_frame` also updates the renderer layer's
  `contentsScale`, so a terminal moved to a display with a different scale
  isn't composited at the old scale (tiny or oversized text).

- The macOS shim handles `GHOSTTY_ACTION_OPEN_URL`, opening clicked links
  (cmd-click, OSC 8) with `NSWorkspace`. Upstream drops every action but
  render, so links did nothing.
- `send_key` drops control-character text (GPUI sends "\n" for enter), as
  Ghostty's macOS app does. Otherwise shift was marked consumed and
  shift-enter reached the program as a plain enter.
- Left-drag policy for programs that have the mouse (herdr's attach always
  does): a plain drag or double-click turns Ghostty's `mouse-reporting` off
  for the gesture and selects; a plain click and an option-drag (option
  stripped) go to the program; shift and cmd keep Ghostty's handling. The
  shims add `mouse_captured`, `toggle_mouse_reporting` (a binding action)
  and `selection_text` (for the live `selection` scenario). The wheel is
  ignored mid-selection (Ghostty would send arrow keys with reporting off).
- Cmd adds shift to pointer events while the program has the mouse: Ghostty
  only looks for links under the pointer then with shift held, so cmd-hover
  and cmd-click on links (OSC 8 included) did nothing in herdr panes.
- `Terminal::columns()`: the grid width, so the app can read a pane's whole
  scrollback from herdr at the width its attach set.
  See `docs/decisions/0010-single-window-terminals.md`.

`vendor/ghostty` is untouched, so the cached native Ghostty build is shared
with the unpatched crate.
