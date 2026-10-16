# 0006 — Chrome: agent list, menus, palette, confirmations, notifications

Date: 2026-10-15. Status: accepted (chrome agent). Modules: `chrome`, `sidebar::workspaces`, `attention`.

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
`key <keystroke>`, `keys <text>`, `wait <ms>`, `log <label>`); see
`chrome/script.rs`. Keys are GPUI key presses (`Window::dispatch_keystroke`,
so bindings, key-down handlers and text input all run), sent to the window
that owns the keyboard (`overlay::dispatch_keystroke`, 0008), not OS input. `log` prints
what is open plus herdr's workspaces, tabs, panes and agents, which is what
the live checks assert on.

## Overlay window

All floating UI draws in the main window's overlay (`crate::overlay`, 0008),
so it shows above the Ghostty views:

- `Chrome::floating(cx, f)` defers to the next tick, gets the overlay
  (`overlay::get_or_open`), and runs `f` in the overlay window: toasts
  (`push_notification`), confirmations and the rename prompt
  (`open_alert_dialog`), the palette (`open_dialog`). Deferring keeps it out
  of any update of either window. With no overlay (no native window, as in
  headless tests) `f` runs in the main window instead.
- A context menu is a `PopupMenu` built in the overlay window and shown as a
  `Capture::Modal` layer (`MenuLayer`, anchored at the main-window point, with
  `occlude()`). The layer makes the overlay take clicks and keys; the menu's
  `DismissEvent` (Escape, click outside, a picked item) removes the layer and
  gives the main window back its previous focus.
- Callbacks fire in the overlay window, so nothing there dispatches actions
  to the main window's views. A toast click calls `AppState::focus_pane`;
  menu, palette and dialog callbacks go through `update_later`, which runs on
  the chrome with the main window. `Chrome::confirm`'s `on_confirm` gets the
  overlay window and should talk to `AppState`.
- Views that only have the main window reach the chrome with
  `chrome::get(window, cx)` (a per-window registry), so constructors stay
  `View::new(state, window, cx)`. The terminal area uses it for its close
  confirmations (native alert only when no chrome exists).

Verified with `RT_SCRIPT=chrome` (in `terminal_area/script/chrome.rs`): it
right-clicks a pane header and a tab with GPUI mouse events, opens the
terminal area's close confirmation, raises a toast and opens the palette, and
checks each is in the overlay and that the overlay takes and releases input.
It drives them with key presses sent where AppKit sends them (the overlay
while it is modal): Escape closes the menus, the name prompt and the palette;
Enter confirms the close confirmation and submits the name prompt and the
rename dialog (herdr gets the typed label); down, down, Enter in a pane menu
runs Split Right; typing fills the palette's query and down moves its
selection. After each, the main window is key again and keyboard focus is on
the terminal that had it (key status checks are skipped while another app is
frontmost). The text fields (name prompt, rename dialog, palette query) are
also typed into with key-down `NSEvent`s handed to the overlay's `NSWindow`
(`sendEvent:`), the path real typing takes, and the script checks that GPUI's
view is the overlay's first responder (0008, "typing into overlay dialogs
beeped").

A right-click on a pane header or tab no longer moves keyboard focus off the
terminal (`TerminalArea::context_menu` calls `prevent_default`, which stops
GPUI's focus-on-mouse-down). Before, the area itself got focus, the menu gave
focus back to it on close, and typing went nowhere.
Screenshots of the main window composited with the overlay come from
`scripts/capture-windows.swift` (ScreenCaptureKit, only this app's windows).
