# 0009 Workspace list

Date: 2026-10-15. Status: accepted.

The terminal area shows only herdr's focused workspace (`focused_workspace_id`), so without a list the other workspaces were invisible and unreachable. The upper sidebar panel is now `sidebar::workspaces::WorkspaceList`, modeled on herdr's TUI sidebar. It replaces `sidebar::agents::AgentList`: agents are listed under their workspace instead of in a flat panel.

## What a row shows

- A dot in the workspace's aggregate agent status (`WorkspaceInfo::agent_status`, computed by herdr), the label (bold when focused, row highlighted), and badges counting its blocked agents and unseen finished ones. The panel header carries the totals.
- Repo line from `WorkspaceInfo::worktree`: the repo name, or "repo · checkout-folder" plus a "worktree" tag for a linked worktree. herdr fills `worktree` only for workspaces it opened as worktrees (`worktree.create`/`open`) or has since resolved; a plain `workspace.create` in a repo can show no repo line at first.
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

- Headless tests (`sidebar/workspaces/tests.rs`): ordering, repo line, counts; rows for every workspace and agent; folding hides agent rows; clicking a finished agent marks it seen.
- Debug builds take `RT_SIDEBAR_SCRIPT` (steps listed in `sidebar/workspaces/script.rs`), which calls the same methods the clicks call. Against the dev session with four workspaces (one linked worktree, blocked/working/done agents) it focused another workspace (the terminal area and file tree switched), focused an agent's pane, folded a workspace, opened the close confirmation and cancelled it, opened the rename prompt, created a worktree and a workspace. Screenshots were taken by window id; no OS input.

## Known gaps

- herdr 0.9.5 puts worktrees it creates under `~/.herdr/worktrees/<repo>/<branch>` even for a dev session with its own config dir. That folder is shared with the default herdr. Remove test worktrees with `scripts/herdr-dev worktree remove --workspace <id>`.
- New Worktree always branches from the focused workspace's repo; there is no repo picker.
- The panel is fixed at 400px tall by default (drag the split to change it); a long list scrolls.
