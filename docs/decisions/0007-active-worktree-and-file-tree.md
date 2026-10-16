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
- Names come from a Kit dialog with one input and a `DialogFooter` of Cancel (`DialogClose`) and Create/Rename (`DialogAction`). OK and Enter both reach the dialog's `Confirm`, which runs `on_ok` (the single-line input propagates Enter; an extra `PressEnter` subscription would submit twice and re-enter the view). Cancel, Escape or × cancel.
- Actions dispatched from inside a dialog don't reach `Workspace` (dialogs render in the Kit's root layer), so New File defers `OpenFile` until the dialog has closed and dispatches it from the tree's focus handle.

## Editor follows the tree

- Rename in the tree emits `AppEvent::FileMoved { from, to }`; the editor re-points every tab at or under `from` (a file or a whole folder) and moves its folder watch. Trash emits `AppEvent::FileTrashed(path)`: clean tabs at or under it close, tabs with unsaved edits stay open marked deleted (saving recreates the file).
- Renames on disk (an agent's `mv`) are caught by the editor's watcher: when an open file vanishes, the editor looks for the same file (device + inode) among the event's paths and in the old folder, and re-points the tab. Not found → the existing rule: the tab stays, marked deleted.

## Known gaps

- A rename that changes the extension keeps the old syntax highlighting until the tab is reopened (`EditorState` takes its language only at construction).
- A file moved on disk into a folder the editor isn't watching, or a folder renamed on disk, isn't followed; the tab is marked deleted. Renames done from the tree are always followed.
