# Brief: GPUI Agent Workspace v0 (GPUI Kit + Ghostty + herdr)

Source of truth: https://claude.ai/code/artifact/37addbff-024a-4b6e-ae1c-43b0208d36ea (copied 2026-10-15; versions updated to latest the same day).

## Vision

A native Rust desktop app that replaces herdr's terminal UI with a real GUI, while keeping herdr as the engine. herdr keeps owning the agents, terminals, worktrees, agent state and session restore. Our app owns everything you see and touch: the window, tabs, draggable split layout, file tree, code editor, workspace and agent list, menus and notifications.

The file tree and editor always follow the **active git worktree**: whichever worktree the focused agent pane is working in.

Agents keep using the herdr agent skill unchanged. When an agent splits a pane or opens a tab through herdr, it appears in our window.

## v0 scope

**In v0:**
- herdr running as a headless server, managed by the app
- Full two-way layout sync: workspaces, tabs, and a draggable, resizable split layout
- One real Ghostty terminal per herdr pane
- File tree and code editor that follow the active worktree
- Workspace list with each workspace's agents and their live status, rebuilt in GPUI after herdr's own sidebar
- Our own context menus, command palette, confirmations and notifications, replacing herdr's TUI chrome

**Not in v0:**
- Per-terminal custom shaders (planned for later; see the end)
- LSP, code intelligence, git staging UI
- Remote/SSH machines

## Stack (latest as of 2026-10-15)

| Piece | Version | Notes |
| --- | --- | --- |
| `gpui-kit` (github.com/longbridge/gpui-kit) | 0.7.3 | The single UI dependency. Pins `gpui-pre =0.3.9` and re-exports GPUI, `gpui-base 0.7.3`, `gpui-component 0.7.3`. Use its resizable panels, tabs, tree view, code editor, menus, popovers, notifications, theming. |
| `gpui-libghostty` (github.com/behzade/gpui-libghostty) | 0.3.1 | Embeds Ghostty's native renderer as a GPUI component. Consumers bind their own GPUI via `gpui_libghostty::bind_gpui!(gpui)`. Needs Rust ≥1.95, Zig 0.16, Xcode CLT. Alpha. |
| herdr (herdr.dev, github.com/herdrdev/herdr) | 0.9.5 | Agent runtime, driven through its local socket API (newline-delimited JSON over a Unix socket). Schema: `docs/herdr-api-0.9.5.schema.json`. |
| `notify` | 8.2.0 | Filesystem watching for the tree and open files. |
| `git2` | 0.21.0 | Resolves a folder to its repo and worktree root. |
| `ignore` | 0.4.33 | .gitignore-aware walking for the file tree. |
| Rust toolchain | 1.99.0 | Installed. |

Always check crates.io for a newer patch before adding a dependency.

## Architecture

```
┌──────────────────────── GPUI app ────────────────────────┐
│ Sidebar          │ Editor           │ Terminal area       │
│ - Workspaces,    │ - File tabs      │ - herdr tab bar     │
│   each with its  │ - Code editor    │ - Split tree of     │
│   agents         │                  │   Ghostty views     │
│ - File tree      │                  │   (focused          │
│                  │                  │   workspace)        │
└───────▲──────────────────────────────────────▲───────────┘
        │ socket: snapshot, events, commands  │ PTY: herdr terminal attach
┌───────┴──────────────────────────────────────┴───────────┐
│ herdr server (headless): agents, PTYs, worktrees, state   │
└───────────────────────────────────────────────────────────┘
```

herdr is the source of truth for layout and agents. The app keeps a local cache, sends commands, and re-renders from herdr's events. It never assumes a change happened until herdr confirms it.

## herdr connection

- **Startup:** connect to the herdr socket. If no server is running, start one with `herdr server`. Then subscribe to events first, and fetch `session.snapshot` to build the initial cache.
- **Events we subscribe to:** workspace, tab, pane and layout lifecycle (`pane.created`, `pane.closed`, `pane.focused`, `pane.moved`, `layout.updated`, `tab.*`, `workspace.*`), worktree events, and `pane.agent_status_changed`.
- **Reconnect:** on event loss (the spec called it `events_lost`; find the real name in the 0.9.5 schema) or a dropped connection, resubscribe, fetch a fresh snapshot, and replace the cache. Events only mark state as stale; we re-read rather than replay them.
- **Session restore:** comes free from herdr. Reopening the app reattaches to the running server and its layout.

## Layout and sync

The window mirrors herdr's model: **workspaces** (one per repo or worktree) in the sidebar, **tabs** in a tab bar, and each tab's **split tree** in the terminal area.

**herdr → app:** `layout.export` returns each tab as a binary tree of splits (direction and ratio) and panes. We render it as nested two-panel GPUI Kit resizable groups.

**app → herdr:**

| User action | herdr call |
| --- | --- |
| Drag a divider | `layout.set_split_ratio` |
| Split a pane | `pane.split` |
| Drag a pane onto a neighbour | `pane.swap` |
| Drag a pane to another tab or new tab | `pane.move` |
| Close a pane or tab | `pane.close` / `tab.close` |
| Click to focus | `pane.focus` / `tab.focus` |
| Create, rename or close a workspace or tab | `workspace.*` / `tab.*` |
| New worktree | `worktree.create` |

After each drag or command, we re-render from the next `layout.updated` event, so herdr and the app can never disagree for long. Divider drags update locally during the drag, then send the final ratio on release.

Keyboard shortcuts for splitting, focusing, resizing and closing are owned by the app and mapped to the same calls.

## Terminals

- Each herdr pane gets one gpui-libghostty view, running `herdr terminal attach <terminal_id> --takeover`. The terminal id comes from the pane records in the snapshot.
- Terminal views are created on `pane.created` and destroyed on `pane.closed`, and moved without restarting on `pane.moved`.
- Each view loads its own Ghostty config file (font, theme, and later, its shader). This may need a small patch to gpui-libghostty if it still starts every view from an empty config (check `TerminalConfiguration` in 0.3.1 first).
- Ghostty draws into native views layered over the GPUI window. The app repositions them every frame during drags and resizes, and hides views that aren't on screen.
- Panes are set to forward right-clicks (`right_click: "pane"`), so mouse-aware apps get them and our context menu handles the rest.

## Active worktree

1. On `pane.focused`, read that pane's `foreground_cwd`, falling back to its `cwd`.
2. Resolve it to the repo worktree root with git2.
3. If the root changed, re-root the file tree and restart the file watcher.

This also follows agents that move into another folder, since `foreground_cwd` tracks the running process.

## File tree

- GPUI Kit tree view rooted at the active worktree, with .gitignore respected.
- Live updates from the file watcher as agents create, edit and delete files.
- Click to open in the editor. Basic actions: new file, rename, delete, reveal in Finder.

## Editor

- GPUI Kit code editor with syntax highlighting, in tabs.
- Open, edit, save, close. Unsaved changes are marked on the tab.
- Open files are grouped per worktree; switching worktrees swaps to that worktree's tabs.
- When an agent changes an open file on disk, it reloads automatically. If you have unsaved edits, you're asked whether to keep yours or take the agent's.

## Workspaces and agents

The upper sidebar panel lists every herdr workspace, modeled on herdr's own TUI sidebar. The terminal area shows only herdr's focused workspace, so this list is how you see and switch to the others.

- Grouped by repo, like herdr's sidebar: a heading per repo, the main checkout's workspaces first and its linked worktrees nested beneath them, and workspaces with no repo in a last section. Repo info comes from herdr's `worktree` field, or from git2 when herdr has none. Repo headings and workspaces fold; a folded heading rolls up its workspaces' status and badges.
- Each workspace row: label, current git branch (short SHA when detached; read with git2 and cached, refreshed when `HEAD` changes), tab and pane counts, and the aggregate agent status as a colored dot.
- Each row expands to its agents (name and status, blocked first). Badges count blocked agents and unseen "done" ones, per workspace and in the panel header.
- Click a workspace to focus it (`workspace.focus`); the terminal area follows. Click an agent to focus its pane.
- Create a workspace (in the active worktree's folder) or a worktree (`worktree.create`, asks for a branch name) from the header's +; a repo heading's + creates a worktree of that repo. Rename and close from the row's menu; × closes. Closing asks first when agents are running there.
- Built from `session.snapshot` (workspaces and agents) and kept current by re-reading on every event, including `pane.agent_status_changed`.

## Replacing herdr's UI

- **Notifications:** agent status changes become our own toasts, badges and sounds. herdr's toast delivery is set to system so nothing is lost when no herdr client is attached.
- **Context menus:** our own GPUI Kit menus on panes, tabs and workspaces.
- **Command palette:** lists every plugin action from `plugin.action.list` and runs it with `plugin.action.invoke`, alongside the app's own commands.
- **Plugin panes:** plugins that open as a split, tab, zoomed pane or overlay create real panes, which we show like any other pane.
- **Confirmations:** herdr API closes happen immediately, so the app asks before closing a running agent or a whole worktree group.
- **Popovers over terminals:** because Ghostty views sit above the GPUI layer, menus, popovers, the command palette and toasts are drawn in a transparent, borderless overlay window above the main window.

**Known gaps in herdr's API (re-check against 0.9.5; file feature requests if still missing):**
- Plugin **popup** placements have no pane id or events, so they can't be shown in our app.
- Notifications sent by tools via `herdr notification show` have no event we can subscribe to.

## Risks

- The full libghostty embedding API isn't stable yet and may change between Ghostty releases. Pin versions.
- gpui-libghostty is alpha.
- Only one controller can attach to a herdr terminal at a time. Our app takes over; attaching from a regular herdr TUI would take it back.

## After v0: per-terminal shaders

Each terminal already loads its own Ghostty config, so adding a `custom-shader` line to a terminal's config gives it its own shader using Ghostty's standard shader system. On top of that, the app can swap a pane's shader when herdr reports a status change, such as a glow when an agent is blocked.

## References

- herdr socket API: herdr.dev/docs/socket-api
- herdr CLI reference (direct terminal attach): herdr.dev/docs/cli-reference
- GPUI Kit: gpui-kit.com
- gpui-libghostty: github.com/behzade/gpui-libghostty
- Ghostty custom shaders: ghostty.org/docs/config/reference (custom-shader)
- Prior art: moai-studio and others on github.com/Uzaaft/awesome-libghostty
- GPUI Kit spike in our own code: github.com/lklyne/specular, branch `rust-native`, `native/bakeoff/gpui-kit-hybrid` (a child NSView layered under/over GPUI Kit — same problem as the Ghostty views)
