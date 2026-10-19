# 0021 Worktree lifecycle

Date: 2026-10-18. Status: accepted. Extends 0009 (workspace list).

Before this, only `worktree.create` was reachable from the app. herdr also binds `open_worktree` and `remove_worktree`, and "Close Worktree Group" closed by herdr's worktree info while the sidebar grouped by herdr's info *or* git2's, so a group the list showed could close only its parent. Now the whole lifecycle is wired, and every worktree decision follows what the list shows.

## What herdr does (probed on 0.9.5, dev session)

- A workspace made with `workspace.create --cwd` in a repo has `worktree: null`, and `worktree.list --workspace` doesn't change that. Only workspaces herdr made or opened as worktrees (and the source workspace of a `worktree.create`) carry it. So the git2 fallback in `sidebar/workspaces/git.rs` stays: it is what groups and labels those workspaces, and the branch and ahead/behind still come from git2.
- `worktree.list --workspace W` works for a null-worktree workspace and names, per checkout, the workspace that has it open (`open_workspace_id`), matched by folder.
- `worktree.open --path P` on a checkout already open in a workspace answers `already_open: true` and **records herdr's worktree info on that workspace**. On a checkout with no workspace it opens one. `--branch` with no checkout fails `worktree_not_found`.
- `worktree.remove` refuses a workspace without herdr's info (`not_linked_worktree`, "not a Herdr-managed worktree checkout"), refuses a dirty checkout (`dirty_worktree_requires_force`, git's message), and with `force` deletes the checkout and closes the workspace. The branch is kept.
- `--base` and `--label` on `worktree.create` work as documented.

## Placement is one function

`chrome::set_placements` registers the spaces list's placement (`WorkspaceList::placement_on`: herdr's `WorkspaceInfo::worktree`, else git2's read of the workspace's folder; remote machines get herdr's info only). The chrome's workspace menu, the palette and group closes all ask it, so they agree with the rows. Without a registered list (headless tests) the chrome uses herdr's info alone. `Repo` and `Placement` moved to `crate::worktree` so both sides share them.

## Open Worktree…

- In the workspace menu (for a workspace in a repo), the Workspaces panel's menu and "+" menu (for the focused workspace), and the palette ("Worktree: Open…").
- `worktree.list` for that workspace, then a GPUI Kit `Command` picker in a dialog, built like the palette: branch (or "detached HEAD") with notes for the main checkout, an open checkout and a prunable one, the folder on the right in muted mono. Bare entries are left out; prunable ones are disabled. Search matches the branch and the folder.
- Picking a checkout that has a workspace focuses it (`FocusWorkspace`, which selects the machine). Otherwise `worktree.open { workspace_id: source, path, focus: true }`, by path so detached checkouts work too.
- Sounds: `Open` subtle as the picker opens, `Close` subtle on cancel, `Success` when the open lands, `Error` and a toast when listing or opening fails. A focus plays the focus's usual `Select`.

## Remove Worktree…

- On rows the list places as linked worktrees, herdr's or git2's, and in the palette ("Worktree: Remove…").
- Asks first: what goes (the folder, when herdr knows it; the workspace closes; the branch stays) and which agents stop, with the close confirmations' wording.
- A git2-found worktree has no herdr info, so herdr would refuse it. `AppState::remove_worktree_on` first has herdr adopt it: `worktree.list` finds the linked checkout open in that workspace, and `worktree.open` on that path records it (`already_open`, no focus). Then `worktree.remove`. If herdr still says `not_linked_worktree`, a warning toast says so and suggests closing the workspace.
- `dirty_worktree_requires_force`: a `Warning` cue and a second confirmation ("Delete uncommitted changes in …?", quoting git), then the remove again with `force`.
- Success plays `Close` strong (a destructive action confirmed), failure `Error` with a toast.

## Close Worktree Group

- `HerdrCommand::CloseGroup { workspace_id, repo_name, members }` carries the members the confirmation lists. `Chrome::group_close` builds it from the registered placement with herdr's rule (a main checkout plus every linked worktree of the same repo), and the menu offers it only when that's more than one workspace, exactly when the list draws a group.
- Sending: when herdr's own info makes the same group (`confirm::herdr_closes_exactly`), one `workspace.close { close_group: true }`; otherwise `AppState::close_workspaces_on` closes each member with its own `workspace.close`, the head last, trying every one and reporting the first error.
- `CloseWorkspace` lost its `close_group` flag; a group close is always `CloseGroup`.

## New Worktree…

`prompt::ask_new_worktree`: the branch (focused, required), then "Start from" (`--base`) and "Label" (`--label`), both optional and left to herdr's defaults when empty. Enter in any field submits. It goes to the row's machine through `AppState::create_worktree_on` (it used to go to whichever machine was selected).

## Dev sessions

`scripts/herdr-dev` appends `[worktrees] directory = "$XDG_CONFIG_HOME/worktrees"` to the dev config when it has no `[worktrees]` section, and reloads a running dev server. `$XDG_CONFIG_HOME` there is `~/.local/share/grasslands-herdr`, never the user's config. This replaces 0009's workaround note (parity W8).

## Script hooks

- `RT_CHROME_SCRIPT`: `open-worktree <workspace>` (then `query`, `key enter`), `remove-worktree <workspace>` (then `key enter` to confirm, again for the force confirmation), `run close-group <workspace>` (now the list's group).
- `RT_SIDEBAR_SCRIPT`: `worktree <branch> [base=<ref>] [label=<text>]`, `repo-worktree <repo> <branch> [base=…] [label=…]`.

## Known gaps

- Adopting a git2-found worktree changes herdr's state (the workspace gains worktree info, so herdr's TUI groups it too). It only happens on Remove, which the user confirmed.
- herdr's `workspace.close` has no dry run, so a workspace that appears between the confirmation and the close isn't closed by an explicit-close group. herdr's own `close_group` path could close a linked worktree that appeared after the confirmation; the window is one round trip.
- The confirmation names the folder only when herdr knows it; a git2-found worktree's says "the worktree's folder".
