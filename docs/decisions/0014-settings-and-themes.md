# 0014: Settings window and bundled themes

- **Themes.** The published gpui-kit crate only registers Default Light and
  Default Dark. The theme files the Kit keeps in its repo (`themes/` at
  v0.7.3, Apache-2.0) are vendored in `crates/app/themes/` and compiled in
  with `include_str!`. To refresh them, re-download that directory for the
  pinned Kit version.
- **Choice.** Appearance (Match System / Light / Dark) plus one light and one
  dark theme, like macOS and Zed. Picking a theme of the other mode while the
  appearance is fixed switches the appearance, so a pick is always visible.
  The View menu's Toggle Light/Dark sets a fixed appearance. A fresh
  install starts Dark in Catppuccin Macchiato, with Aurora Light as the
  light theme (falling back to the Kit's defaults if either is missing).
- **Saved** to `~/.config/grasslands/settings.json` (next to `ghostty.conf`;
  not `XDG_CONFIG_HOME`, which the dev scripts redirect). Unknown keys are
  kept. Nothing is written while `RT_SCRIPT` is set, since script runs
  toggle the theme.
- **Window.** GPUI Kit's `Settings` page in its own window (cmd-,, app menu,
  palette) rather than an overlay dialog: it is a full page with a sidebar
  and search, and a separate window never sits under the native terminals.
