# 0007 Active worktree and file tree

## Active worktree

`AppState` re-resolves on every snapshot replacement, not only on the `pane.focused` event: it reads the focused pane's `foreground_cwd` (falling back to `cwd`), and when that folder changed since last time it resolves it with git2 (`Repository::discover` → `workdir()`, canonicalized). That covers focus changes and agents that `cd` elsewhere, since `foreground_cwd` is part of every snapshot.

- Inside a linked worktree the root is that worktree's checkout, not the main repo.
- Outside git the folder itself becomes the root, so non-repo panes still get a tree.
- No focused pane, or a folder that doesn't exist on this machine: the root stays where it was.
- Before the first snapshot the root is the process cwd (unchanged from the shell).

## File tree (`sidebar::files`)

- GPUI Kit's `Tree` needs the full item tree and treats "has children" as "is a folder", so the view keeps its own model (loaded listings + expanded set) and rebuilds the Kit items after each change, restoring the selection by id. Unloaded or empty folders get a disabled placeholder child ("Loading…" / "Empty").
- Folders are listed lazily, one level on first expand, on the background executor, with `ignore::WalkBuilder` (depth 1): .gitignore, git excludes and global ignores apply inside repos; dotfiles show; `.git` is hidden. Folders first, then case-insensitive names.
- Live updates: one recursive notify watcher on the root (FSEvents). Events are debounced 75 ms, mapped to the loaded folders they touch (a path's own listing and its parent's; a changed `.gitignore` re-lists everything below it; anything under `.git` is ignored), and only those are re-listed.
- Click a file → `OpenFile`. Context menu: New File, New Folder, Rename, Move to Trash, Reveal in Finder (`open -R`), Copy Path. The header has New File, New Folder, Refresh.
- Delete moves to the Trash (`trash` crate) instead of unlinking, so it doesn't need a confirmation.
- Names come from a Kit dialog with one input; Enter submits (the single-line input propagates Enter to the dialog's `Confirm`, which runs `on_ok`; an extra `PressEnter` subscription would submit twice and re-enter the view). Escape or × cancels.
- Actions dispatched from inside a dialog don't reach `Workspace` (dialogs render in the Kit's root layer), so New File defers `OpenFile` until the dialog has closed and dispatches it from the tree's focus handle.

## Known gaps

- Renaming or trashing a file that is open in the editor doesn't tell the editor; its tab keeps the old path until closed.
- The name dialog has no visible OK button (the Kit renders a footer only when one is set); Enter is the way to submit.
