# 0003 — Editor

Date: 2026-10-15. Status: accepted (app-shell, Part 2).

- **Component.** GPUI Kit's `EditorState`/`Editor` (code-editor mode). The
  language comes from the file name (`editor/language.rs`).
- **Grammars.** All of gpui-kit's grammars (`tree-sitter-languages`). Its SQL
  grammar, `tree-sitter-sequel`, pins `cc ~1.2.1` in every release up to
  0.3.11, while a fresh resolve picks cc 1.8 for gpui's `embed-resource`
  (`^1.2`). Cargo.lock therefore holds cc at 1.2.67, which satisfies both. If
  a new dependency needs cc >= 1.3, either drop SQL highlighting (enable the
  grammars one by one) or wait for a sequel release without the pin. A test
  checks every extension we map has a compiled grammar.
- **Tabs live in the titlebar** (see 0002, Window layout). The editor exposes
  `tabs()` (path, title, dirty), `active_index()`, `activate`,
  `request_close` and `focus_active`, and emits `EditorEvent::Activated`; it
  draws no tab strip and no empty state. The conflict and close-confirmation
  bars still render above the editor.
- **Tab titles.** The file name, plus the fewest parent folders that make it
  unique within the tab group (`one/mod.rs`, `two/mod.rs`).
- **Groups.** Tabs are grouped by the active worktree root (`AppState`'s
  `ActiveRootChanged`). `OpenFile` opens into the active worktree's group,
  even if the file lies outside it. Switching worktrees swaps the titlebar's
  file tabs (herdr is selected if the new group is empty); it does not take
  focus from the terminal that caused the switch.
- **Dirty.** A buffer is dirty when its text differs from what was last read
  from or written to disk, so undoing back to the saved text clears the dot.
- **External changes.** `notify` watches the *folders* of open files
  (non-recursive), so rename-over saves are seen. On a change, the file is
  re-read: same as last seen (including our own save) → ignore; clean buffer →
  reload, keeping the cursor; dirty buffer → ask. Deleted → the buffer stays
  open and becomes dirty; saving recreates it.
  A file that vanished because it was moved (same inode among the event's
  paths or in its old folder) is followed instead. Renames and trashes from
  the file tree arrive as `AppEvent::FileMoved` / `FileRemoved` (see 0007).
- **Questions are an inline bar, not a dialog.** "Keep mine / Take theirs"
  and "Save / Don't save / Cancel" on closing a dirty tab render as a bar
  above the editor. A modal would be covered by Ghostty's native views (see
  0002, Overlays) and would block work in other panes for a per-file question.
  "Keep mine" leaves the buffer dirty; the next save overwrites the file.
- **Keys** (context `Editor`): `cmd-s` save, `cmd-w` close tab. Cycling moved
  to the `Workspace` context (0002).
- **CLI.** Paths given as arguments to the app open as file tabs at start.
- **Tests.** `editor/tests.rs` drives a headless window through real key
  bindings and text input. It uses `gpui-pre`'s `test-support` directly:
  gpui-kit's `test-support` names gpui-fast weakly, Cargo still resolves it,
  and gpui-fast's exact pins conflict with gpui-pre's. The watcher is off in
  GPUI tests (its thread trips the deterministic scheduler) and is tested on
  its own.
