# 0006 — Chrome: agent list, menus, palette, confirmations, notifications

Date: 2026-10-15. Status: accepted (chrome agent). Modules: `chrome`, `sidebar::agents`, `attention`.

## Who needs attention (`attention`)

- `Attention` lives in `AppState` and is updated in `replace_snapshot`. It diffs
  each agent's status against the previous cache and emits
  `AppEvent::AgentStatusChanged(StatusChange)` for agents it already knew.
  First sight of an agent (startup, reconnect) is not news, so old statuses
  are not replayed as toasts.
- Why diff caches instead of using `pane.agent_status_changed` events: the
  event carries the status the agent reported (`idle`), while the cache has
  herdr's effective status (`done` = finished and not yet seen). Toasts want
  the effective one. The cache is also re-read after every event, so diffing
  sees every change the events would.
- herdr's `done` already means "not seen yet". The app adds `seen_done`: panes
  it focused or whose agent the user clicked. Unseen = `done` and not in
  `seen_done`. This survives an app restart because herdr keeps `done`.
- Badge count = blocked + unseen done. It drives the agent list header, the
  per-row badges and the dock badge.

## Notifications

- On `AgentStatusChanged` to `blocked` ("needs your input") or `done`
  ("finished"): a GPUI Kit toast keyed per pane (a newer status replaces the
  older toast), clicking it sends `FocusPane`. If the window is not active
  the toast also goes to the OS (`InAppAndSystem`).
- Sounds: macOS system sounds `Funk` (blocked) and `Glass` (done) via
  `NSSound`. The dock badge uses `NSDockTile.badgeLabel`. GPUI has neither, so
  `chrome/dock.rs` calls AppKit through `objc2-app-kit`.
- herdr's own toasts: the dev config (`$XDG_CONFIG_HOME/herdr/config.toml`,
  shared by every dev session) sets `[ui.toast] delivery = "system"`, so
  herdr's toasts reach the OS when no herdr client is attached. The app does
  not edit the user's real herdr config; set the same line there. Toasts from
  `herdr notification show` still have no event (see 0005).

## Context menus

- API: `Chrome::context_menu(target, position, extra, window, cx)`, or from any
  view the action `actions::OpenContextMenu { target, position }`, which
  `Workspace` forwards with no extras. `position` is in window coordinates.
- `MenuTarget` is `Pane { pane_id } | Tab { tab_id } | Workspace { workspace_id }`.
  The menu shows the target's herdr actions (`Chrome::default_entries`), then
  `extra: Vec<MenuEntry>` from the caller, then plugin actions whose
  `contexts` include the target's kind (invoked with a filled
  `PluginInvocationContext`).
- `MenuEntry::Item { label, command, disabled } | Separator`;
  `MenuCommand::Herdr(HerdrCommand) | Rename(MenuTarget) | Action(Rc<dyn Action>) | Callback(Rc<dyn Fn(&mut Window, &mut App)>)`.
  The terminal area adds pane-specific entries (copy, paste, …) as `Callback`s.
- Pane: Focus, Split Right/Down, Toggle Zoom, Rename…, Close. Tab: Focus, New
  Tab, Rename…, Close. Workspace: Focus, New Tab, Rename…, Close, and Close
  Worktree Group on a repo's main checkout when linked worktrees of it are
  open. Checked in the dev session: herdr's `close_group` closes the group
  only from the main checkout (it plus the linked worktrees); on a linked
  worktree it closes just that workspace, so the entry isn't offered there.
- The menu is a GPUI Kit `PopupMenu` placed with `deferred(anchored())`. On
  close it gives focus back to whatever had it; without that, app key
  bindings (cmd-shift-p) stopped reaching `Workspace` after a menu.

## Confirmations

- Every herdr command from the chrome goes through `Chrome::run(HerdrCommand)`,
  which asks first (`confirm::needed_for`) when it would close a pane with an
  agent, a tab or workspace holding agents, or a whole worktree group (always).
- `Chrome::confirm(Confirmation { title, message, confirm_label }, on_confirm)`
  is public for other modules (an alert dialog with a danger button).

## Command palette

- GPUI Kit `Command` in a dialog. Groups: Commands (pane/tab/workspace actions
  on herdr's focused pane, theme, quit), Agents (go to each agent), Plugins
  (every `plugin.action.list` entry as `plugin: title`, run with
  `plugin.action.invoke` and the focused pane's context). Plugin actions are
  re-listed whenever the palette or a menu opens.

## Plugin panes

Checked in the dev session: `plugin pane open` with `split`, `tab`, `overlay`
and `zoomed` all create ordinary panes (ids, tabs, layouts, `zoomed` flag) in
`session.snapshot`, so they render like any pane once the terminal area draws
layouts. `popup` stays invisible (0005).

## Callbacks never nest Chrome updates

Menu items, palette entries, confirmations and the rename dialog run their
work through `update_later` (`window.defer`), and `Chrome::run` sends
unconfirmed commands directly. A nested `Chrome` update panics in GPUI, and
these callbacks can fire while the chrome is itself being updated.

## Driving it live (debug builds)

`RT_CHROME_SCRIPT` runs `;`-separated steps after startup (`menu pane <id>`,
`run close-pane <id>`, `palette`, `query <text>`, `type <text>`,
`key down|enter|escape`, `wait <ms>`, `log <label>`); see `chrome/script.rs`.
Keys are GPUI actions dispatched to the window, not OS input. `log` prints
what is open plus herdr's workspaces, tabs, panes and agents, which is what
the live checks assert on.

## Overlay window

Everything floating is drawn in-window for now (menus via `deferred`, toasts
and dialogs in GPUI Kit's root layers), so it will be hidden behind native
Ghostty views. When the overlay API lands, only `chrome/` changes: render the
open menu, dialogs and toasts into the overlay instead.
