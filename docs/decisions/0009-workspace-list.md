# 0009 Workspace list

Date: 2026-10-15. Status: accepted.

The terminal area shows only herdr's focused workspace (`focused_workspace_id`), so without a list the other workspaces were invisible and unreachable. The upper sidebar panel is now `sidebar::workspaces::WorkspaceList`, modeled on herdr's TUI sidebar. It replaces `sidebar::agents::AgentList`: agents are listed under their workspace instead of in a flat panel.

## Grouping by repo

Like herdr's TUI sidebar, workspaces sit under a heading for their repo (`sidebar/workspaces/groups.rs`, a pure function over the snapshot plus cached git info; herdr stays the source of truth):

- A workspace's repo is herdr's `WorkspaceInfo::worktree` (`repo_key`, `repo_name`, `repo_root`, `is_linked_worktree`). herdr fills it only for workspaces it opened as worktrees or has since resolved: a plain `workspace.create` in a repo has none until a worktree is created from it. So when herdr says nothing, git2 fills in from the workspace's folder: the key is the canonical common git dir (the same string herdr uses as `repo_key`, trailing `/` trimmed), the root is the main checkout. herdr's answer wins when both exist.
- Repo groups appear in the order of their first workspace in herdr's list. Inside a group the main checkout's workspaces come first, then linked worktrees (indented), each part in herdr's order. Workspaces with no repo (not in git, or the folder isn't on this machine) go last under "No repo"; that label is left out when nothing is grouped.
- Fold state is kept per repo (by key) and per workspace (by id) for the life of the window. It isn't saved across launches; the app has no settings store yet.
- A folded repo heading shows its workspaces' rolled-up status (the one that needs you most: blocked > working > done > idle) and summed blocked/unseen badges. Open, it shows only the name, since each row carries its own.
- Hovering a heading shows +: New Worktree in that repo (`worktree.create` with `cwd` = the repo root, `focus: true`). The header's + still makes a worktree from the focused workspace.

## Branch

herdr's socket API has no branch, so `sidebar/workspaces/git.rs` reads it with git2: the branch HEAD names (also before the first commit), or the 7-character SHA when detached. The folder is herdr's `checkout_path`, falling back to the folder of the focused pane in the workspace's active tab (`foreground_cwd`, then `cwd`).

Results are cached per folder in `WorkspaceList`, read on the background executor, never during render:

- read once when a folder first appears in a snapshot; dropped when no workspace uses it;
- re-read for every cached folder on `AppEvent::ActiveWorktreeChanged` (the focused-pane poll in `AppState`);
- re-read when a watched `HEAD` changes. The list runs its own small `notify` watcher on each checkout's git dir (non-recursive, `HEAD`/`HEAD.lock` only). The file tree's watcher belongs to the files agent, skips `.git`, and only covers the active worktree, so it can't see other workspaces' branches.

Under `cargo test` the watcher is off (GPUI's test scheduler, as for `SessionSync`).

## What a row shows

- A dot in the workspace's aggregate agent status (`WorkspaceInfo::agent_status`, computed by herdr), the label (bold when focused, row highlighted), and badges counting its blocked agents and unseen finished ones. The panel header carries the totals.
- The branch (see above). The repo name moved to the group heading, and nesting replaces the old "worktree" tag.
- "N tabs · N panes".
- A chevron folds the agent rows away. Everything starts unfolded.
- Agent rows keep the old list's rules: sorted blocked, working, done, idle (then name), a "blocked" badge, a "done" badge until seen (`Attention`), click marks it seen and focuses the pane, right-click opens the pane menu. The toast hooks were never in the list (chrome reacts to `AppEvent::AgentStatusChanged`), so nothing moved there.

## Actions

- Click a row: `AppState::focus_workspace` → `workspace.focus`. The terminal area, file tree and editor follow from the next snapshot. Nothing local changes first.
- Header +: a Kit dropdown with New Workspace (`workspace.create` in the active worktree's folder, focused; herdr picks the label) and New Worktree… (asks for a branch, then `worktree.create` from the focused workspace with `focus: true`; failure becomes an error toast).
- Row "…" and right-click: the chrome's workspace menu (focus, new tab, rename, close, close worktree group, plugin actions) through `OpenContextMenu`.
- Row ×: `Chrome::run(CloseWorkspace)` via `chrome::get`, so the chrome asks first when agents are running there.
- The chrome is reached only through its public API (`OpenContextMenu`, `chrome::get` → `run`, `run_menu_command`), because the chrome agent owns `chrome/`.
- The one-field name dialog moved from the file tree into `crate::prompt` so both use it. It opens through `Chrome::floating`, so it draws in the overlay above the terminals; its submit runs back in the main window after the dialog closes.

## Verifying

- Headless tests (`sidebar/workspaces/tests.rs`, `git.rs`): grouping and ordering (main before linked, repos by first workspace, no-repo last, `repo_key` with a trailing slash), rolled-up status, counts; a heading per repo, main row above linked row, folding a repo hides its workspaces and agents but not the no-repo ones; rows for every workspace and agent; folding hides agent rows; clicking a finished agent marks it seen; branch read for a workspace folder; git2 reads branch, unborn branch, detached SHA, and places a linked worktree under its main repo.
- Debug builds take `RT_SIDEBAR_SCRIPT` (steps listed in `sidebar/workspaces/script.rs`), which calls the same methods the clicks call. Against the dev session with four workspaces (one linked worktree, blocked/working/done agents) it focused another workspace (the terminal area and file tree switched), focused an agent's pane, folded a workspace, opened the close confirmation and cancelled it, opened the rename prompt, created a worktree and a workspace. Screenshots were taken by window id; no OS input.

Grouping, against the dev session with rusted-tools (this worktree's own workspace, a linked worktree), alpha-repo (main checkout + two linked worktrees from `worktree create --path` in a temp dir; one agent blocked, one working) and beta-repo (a plain `workspace.create` herdr gave no repo info, HEAD detached, agent working) plus a workspace in a non-git folder. `log` showed each in its group with the right branch and `b46a257` for the detached one; after `git checkout` in a linked worktree the HEAD watcher updated its branch within the step; `toggle-repo` folded alpha-repo (heading showed the red dot and "1"); `repo-worktree beta-repo beta-feat` created w9 nested under beta-repo and focused it. Window-only screenshots via `scripts/capture-windows.swift`.

## Known gaps

- herdr 0.9.5 puts worktrees it creates under `~/.herdr/worktrees/<repo>/<branch>` even for a dev session with its own config dir. That folder is shared with the default herdr. Remove test worktrees with `scripts/herdr-dev worktree remove --workspace <id>`.
- When herdr has no `worktree` info for a workspace, its group and branch follow the focused pane's folder, so it moves to another group if that pane `cd`s into another repo.
- The panel is fixed at 400px tall by default (drag the split to change it); a long list scrolls.
