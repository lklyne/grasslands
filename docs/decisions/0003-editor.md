# 0003 — Editor

Date: 2026-10-15. Status: accepted (app-shell, Part 2).

- **Component.** GPUI Kit's `EditorState`/`Editor` (code-editor mode). The
  language comes from the file name (`editor/language.rs`).
- **Grammars.** gpui-kit's tree-sitter grammars are enabled one by one, not
  through `tree-sitter-languages`: that bundle includes SQL, whose
  `tree-sitter-sequel` pins `cc ~1.2` against gpui's `cc 1.8`. No SQL,
  Astro, ERB, EJS, Elixir, GraphQL, PHP, Proto, Scala or Svelte highlighting
  for now.
- **Groups.** Tabs are grouped by the active worktree root (`AppState`'s
  `ActiveWorktreeChanged`). `OpenFile` opens into the active worktree's group,
  even if the file lies outside it. Switching worktrees swaps the tab bar; it
  does not take focus from the terminal that caused the switch.
- **Dirty.** A buffer is dirty when its text differs from what was last read
  from or written to disk, so undoing back to the saved text clears the dot.
- **External changes.** `notify` watches the *folders* of open files
  (non-recursive), so rename-over saves are seen. On a change, the file is
  re-read: same as last seen (including our own save) → ignore; clean buffer →
  reload, keeping the cursor; dirty buffer → ask. Deleted → the buffer stays
  open and becomes dirty; saving recreates it.
- **Questions are an inline bar, not a dialog.** "Keep mine / Take theirs"
  and "Save / Don't save / Cancel" on closing a dirty tab render as a bar
  above the editor. A modal would be covered by Ghostty's native views (see
  0002, Overlays) and would block work in other panes for a per-file question.
  "Keep mine" leaves the buffer dirty; the next save overwrites the file.
- **Keys** (context `Editor`): `cmd-s` save, `cmd-w` close tab,
  `ctrl-tab`/`cmd-shift-]` next, `ctrl-shift-tab`/`cmd-shift-[` previous.
- **CLI.** Paths given as arguments to the app open in the editor at start.
- **Tests.** `editor/tests.rs` drives a headless window through real key
  bindings and text input. It uses `gpui-pre`'s `test-support` directly:
  gpui-kit's `test-support` names gpui-fast weakly, Cargo still resolves it,
  and gpui-fast's exact pins conflict with gpui-pre's. The watcher is off in
  GPUI tests (its thread trips the deterministic scheduler) and is tested on
  its own.
