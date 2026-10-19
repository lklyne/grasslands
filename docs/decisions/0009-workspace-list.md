# 0009 Workspace list

Date: 2026-10-15. Status: accepted; the agents section, header, footer and colors changed in 0011 (sidebar dock).

The terminal area shows only herdr's focused workspace (`focused_workspace_id`), so without a list the other workspaces were invisible and unreachable. The upper sidebar panel is now `sidebar::workspaces::WorkspaceList`, modeled on herdr's TUI sidebar. It replaces `sidebar::agents::AgentList`: agents are listed under their workspace instead of in a flat panel.

## Parity with herdr's sidebar (2026-10-16)

The human compared the list with herdr's TUI sidebar and asked for parity, so the panel now follows herdr's own code, not a GPUI take on it. Sources: herdr v0.9.5 `src/client/shell/sidebar.rs` (spaces), `agent_sidebar.rs` (agents), `shell.rs` (`status_icon`, `status_color`, `status_priority`), `config/sidebar.rs` (default rows), and v0.7.4 `src/ui/sidebar.rs` for the layout the human's screenshot shows. It draws in the theme's monospace font at 13px on 17px lines; colors come from the GPUI Kit theme (`red`, `yellow`, `cyan`, `green`, `magenta`, `blue`, `muted_foreground`) in place of herdr's Catppuccin palette.

Where 0.7.4 (the screenshot) and 0.9.5 differ, the layout follows the screenshot and the rules follow 0.9.5: the fold toggle sits at the left of the parent row and children are indented three cells with no `├─`/`└─` tree lines (0.7.4), while grouping, glyphs, colors and ordering are 0.9.5's (they're the same in both).

## Grouping by repo

`sidebar/workspaces/groups.rs` is herdr's `workspace_entries`, a pure function over the snapshot plus cached git info:

- A workspace's repo is herdr's `WorkspaceInfo::worktree` (`repo_key`, `repo_name`, `repo_root`, `is_linked_worktree`). herdr fills it only for workspaces it opened as worktrees or has since resolved: a plain `workspace.create` in a repo has none. So when herdr says nothing, git2 fills in from the workspace's folder: the key is the canonical common git dir (herdr's `repo_key`, trailing `/` trimmed), the root is the main checkout. herdr's answer wins when both exist.
- A repo forms a group only when it has at least one main-checkout workspace and at least one linked-worktree workspace. The group's rows appear where its first workspace is in herdr's list: the main checkout's workspaces (parent rows, with the ▾/▸ toggle), then its linked worktrees indented beneath. Every other workspace, in a repo or not, is a plain row in herdr's order. There are no repo heading rows and no "No repo" section any more.
- A folded group keeps its parent rows and the focused linked worktree, if any. The parent row then shows the group's rolled-up status (herdr's priority: blocked > done > working > idle) and summed badges.
- Fold state is kept per repo key for the life of the window, not across launches.

## Branch

herdr's socket API has no branch and no ahead/behind (herdr's own sidebar reads git in the server and sends them only over its client protocol; `WorktreeInfo::branch` comes only from `worktree.list`). So `sidebar/workspaces/git.rs` reads them with git2: the branch HEAD names (also before the first commit; nothing when detached, like herdr) and `graph_ahead_behind` against the branch's upstream. The folder is herdr's `checkout_path`, falling back to the folder of the focused pane in the workspace's active tab (`foreground_cwd`, then `cwd`).

Results are cached per folder in `WorkspaceList`, read on the background executor, never during render:

- read once when a folder first appears in a snapshot; dropped when no workspace uses it;
- re-read for every cached folder on `AppEvent::ActiveWorktreeChanged` (the focused-pane poll in `AppState`);
- re-read every 5 s, for what the watcher can't see (commits, fetches and pushes move ahead/behind; herdr polls too);
- re-read when a watched `HEAD` changes. The list runs its own small `notify` watcher on each checkout's git dir (non-recursive, `HEAD`/`HEAD.lock` only). The file tree's watcher belongs to the files agent, skips `.git`, and only covers the active worktree, so it can't see other workspaces' branches.

Under `cargo test` the watcher is off (GPUI's test scheduler, as for `SessionSync`).

## What a row shows

- Row 1: a lead (one cell; a parent row's ▾/▸; three cells for a linked worktree), herdr's glyph (`○` idle, `●` working/blocked/done, `·` unknown, as herdr reports for a workspace with no agents) in its status color (blocked red, working yellow, done teal, idle green), then the label, bold and full-strength when focused. A workspace row shows herdr's own roll-up (`WorkspaceInfo.agent_status`); only a folded group rolls its members up here. herdr's `done` means unseen, and herdr clears it when the app focuses the pane (0005). Glyphs, colors and priority live in `crate::status`; `ui.status_indicators = "symbols"` (`× ◐ ✓ ○ ·`) is `status::Indicators::Symbols`, set as a global.
- Row 2 (not for a group's linked worktrees, as herdr's `suppress_git_details`): the branch, truncated with an ellipsis, then `↑N` green and `↓N` red. Magenta when focused, muted otherwise (herdr's mauve / overlay0).
- The focused row gets the theme's `sidebar_accent` across the full width.
- Badges (not in herdr; kept from the first version): blocked and unseen-done counts on row 1's right end, and totals in the "spaces" label row.
- No "N tabs · N panes" line and no per-workspace agent rows; agents moved to their own section.

## Agents section

- After herdr's agent panel: a rule, " agents" with a fold toggle, and "grouped"/"priority" on the right (clicking it switches, like herdr's `agent_sort_toggle`), then a blank line and the rows.
- Each agent is two lines, herdr's default `[state_icon, workspace, tab]` / `[agent]`: the glyph, the workspace label and, when the workspace has more than one tab, ` · tab`; then the agent label in herdr's precedence (`crate::label`: manual pane name, `name`, `display_agent`, `agent`, terminal title). A "blocked" or (unseen) "done" badge sits on row 1.
- "grouped" is herdr's workspace order; within a workspace, blocked first (herdr's priority, stable). "priority" is herdr's queue: priority, then newest `state_change_seq`.
- Click focuses the pane (`pane.focus`, which is also how herdr marks the result seen); right-click opens the pane menu. The section starts open; folding it gives the spaces the room. It lives inside `WorkspaceList` (one panel) rather than as a third resizable panel, so the panel's default height went from 400 to 560px.

## Actions

- Click a row: `AppState::focus_workspace` → `workspace.focus`. The terminal area, file tree and editor follow from the next snapshot. Nothing local changes first.
- Footer "new": New Workspace (`workspace.create` in the active worktree's folder, focused; herdr picks the label), like herdr's `NewWorkspace`.
- Footer "menu": herdr's global menu (settings, keybinds, reload config, what's new, detach) has no counterpart in this app, so "menu" opens the chrome's workspace menu for the focused workspace with New Workspace, New Worktree… and Command Palette… appended (`Chrome::context_menu` with extra entries; it draws in the overlay), or the command palette when nothing is focused. herdr's "● " before "menu" flags an available update; the app has none, so there's no dot.
- Hover a row: + (rows in a repo, not linked worktrees) asks for a branch and runs `worktree.create` with `cwd` = that repo's root and `focus: true`; … opens the chrome's workspace menu; × is `Chrome::run(CloseWorkspace)`, which asks first when agents are running. The hover buttons are overlaid on the row's right end so the row never moves. Right-click opens the workspace menu too.
- The chrome is reached only through its public API (`OpenContextMenu`, `chrome::get` → `run`, `run_menu_command`, `context_menu`, `open_command_palette`), because the chrome agent owns `chrome/`.
- The one-field name dialog lives in `crate::prompt`; it opens through `Chrome::floating`, so it draws in the overlay above the terminals.

## Verifying

- Headless tests (`sidebar/workspaces/tests.rs`, `git.rs`): main before linked within a group, the group placed at its first workspace, a repo with only a main checkout (or only a linked worktree) stays plain, a folded group keeps the focused child, `repo_key` with a trailing slash; rolled-up status by herdr's priority; grouped and priority agent order; rows for every workspace and agent; folding the agents section hides agent rows; the sort toggle; the fold toggle hides linked worktrees only; clicking an agent dispatches `FocusPane` for it; branch read for a workspace folder; git2 reads branch, unborn branch, no branch when detached, ahead count against a bare-remote upstream, none without upstream, and places a linked worktree under its main repo.
- Live (2026-10-16), against the dev session set up like the human's screenshot: `telescope` (main checkout on `perf/trace-category-timeline-export` plus three linked worktrees, one listed in herdr after an unrelated workspace), `specular-web` (a repo with no worktrees), `zero-daytime-2` on `rebuild-all` 8 commits ahead of a local bare remote, a non-git folder, and agents reported blocked, working, done (working → idle while unfocused) and idle via `pane report-agent`. `RT_SIDEBAR_SCRIPT` (`log`, `fold telescope`, `sort`, `focus wF`, `menu`) and window-only screenshots (`screencapture -l`) showed: `▾ · telescope main` with its branch line and the three worktrees indented beneath it (the later-listed one pulled up into the group), `rebuild-all ↑8` in green, red/yellow/teal/green glyphs, badges, "new"/"menu", the agents section in grouped then priority order; after folding, `▸ ● telescope main` red with badges 1 and 1; focusing wF moved the highlight and the file tree followed. The footer menu was opened by the script, but its overlay window couldn't be captured (ScreenCaptureKit reported the windows off screen), so it isn't confirmed by screenshot.

## Known gaps

- herdr 0.9.5 puts worktrees it creates under `~/.herdr/worktrees/<repo>/<branch>` even for a dev session with its own config dir. That folder is shared with the default herdr. Remove test worktrees with `scripts/herdr-dev worktree remove --workspace <id>`.
- When herdr has no `worktree` info for a workspace, its group and branch follow the focused pane's folder, so it moves to another group if that pane `cd`s into another repo.
- The panel is 560px tall by default (drag the split to change it); spaces and agents split it evenly and each scrolls. herdr remembers its split ratio; this app doesn't yet.
- herdr labels an uncustomized linked worktree by its branch (`worktree/` stripped). The socket API doesn't say whether a label is custom, so the app always shows herdr's label.
