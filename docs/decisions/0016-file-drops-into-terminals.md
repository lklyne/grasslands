# 0016: File drops into terminals

Dropping files on a pane, from Finder or from the Files sidebar, pastes their
paths into that pane's terminal and focuses it. This is how images get into
agent prompts (Claude Code reads a pasted image path as an attachment).

- **Ghostty's paste path.** The native Ghostty view passes every hit test
  through to GPUI, so GPUI gets the drop as `ExternalPaths`. The pane body
  handles it and calls `Terminal::paste_text`, a small patch to the vendored
  adapter that wraps `ghostty_surface_text`. That is a real paste: bracketed
  when the program turns it on, so the agent sees one paste, not typed keys.
- **Escaping matches Ghostty and Terminal.app.** Each path gets shell
  specials backslash-escaped, and multiple paths are joined with spaces.
- **GPUI modality workaround.** GPUI treats every hitbox as unhovered while
  the last input was the keyboard. Only a mouse move or press clears that,
  and an OS drag sends neither (file-drop events don't change modality). So
  after typing in a terminal, a drop from Finder matched nothing. When a drag
  starts in keyboard modality, the workspace dispatches an in-app
  `MouseMoveEvent` at the current pointer, the same event GPUI builds from
  `FileDropEvent::Pending`. Remove this once gpui-pre sets mouse modality
  for `FileDrop` input.
- The `terminals` script checks the whole path. It types, drops a path with
  spaces and parentheses through `FileDropEvent`, and reads the echoed path
  back from herdr.
