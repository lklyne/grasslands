# 0020: remote files

A workspace on a saved machine gets the same file tree, file search, editor
and Changes tab as a local one, reading and writing that machine's disk.

- **Transport.** Over the machine's existing ssh master (0019): one SFTP
  session (`ssh -S <ctl> -s <target> sftp`) for file operations, and
  `ssh -S <ctl> <target> <cmd>` for git and search. No sshfs, no FUSE, no
  helper installed on the machine. `herdr_client::RemoteFs` wraps both behind
  blocking calls like the rest of the crate.
- **One API in the app.** `crate::fs::Fs` is `Local` or `Remote`. The tree,
  search, editor, Changes and worktree tracking take an `Fs` for the
  active workspace's machine instead of calling `std::fs` or `ignore`
  directly. Local behaviour is unchanged: the Local arm is today's code
  moved behind the API.
- **Views ask about abilities, not machines.** `Fs::caps()` (or
  `Caps::of(machine)` while it's offline) says whether changes are
  watched (else poll, and don't canonicalize paths), whether removing goes
  to a Trash (else ask first), whether file ids survive a rename, and
  whether a call is cheap enough for the UI thread (`follow_dir` resolves a
  Local root inline, a remote one on the executor). Checks about identity
  stay: Local's setup, Finder, the terminal attach command, herdr's config.
- **Roots.** The active worktree is a machine plus a path. A remote
  workspace's root is its focused pane's cwd on that machine, found the way
  Local's is.
- **Ignore rules.** Remote listings drop what git ignores, asking the
  machine's git (`git check-ignore --stdin`) inside a repo.
- **Search and Changes** run the machine's own tools: `git grep`/`git
  ls-files` in a repo, `grep -rIn`/`find` outside one, with the same caps as
  local search. Changes, branch and ahead/behind come from the same git
  scripts on every machine, Local included (0015).
- **Watching.** There is no remote file watcher. While a remote workspace is
  shown and the window is active, expanded folders, open files and the
  Changes summary are re-read on a short poll (a few seconds), and at once
  when the window becomes active or a terminal on that machine goes idle.
- **Removing** on a remote machine is permanent, so it always asks first;
  Local still moves to the Trash.
- **Editor.** Buffers are keyed by machine and path; a remote buffer's title
  names the machine. Saving is an atomic replace on the machine.
- **Failures** (dropped connection, permission denied) show where the action
  was taken, never as a silent empty tree; an offline machine's file panel
  says so and recovers on reconnect.
