# 0002 — App structure and module seams

Date: 2026-10-15. Status: accepted (app-shell, Part 1).

## Modules (crates/app/src)

| Module | View | Owner |
| --- | --- | --- |
| `workspace` | `Workspace`, the window root: the titlebar tab strip (`workspace::titlebar`) over an `h_resizable` of sidebar / main area. The main area shows the terminal area (herdr tab) or the editor (a file tab). Routes cross-module actions. | app-shell |
| `sidebar` | `Sidebar`: `v_resizable` of `workspaces::WorkspaceList` over `files::FileTree` | app-shell |
| `sidebar::workspaces` | `WorkspaceList`: every workspace with its agents (replaced `sidebar::agents`, see 0009) | herdr-client agent |
| `sidebar::files` | `FileTree` | files agent |
| `editor` | `Editor` (open files per worktree + code editor; its tabs are drawn in the titlebar) | app-shell, Part 2 |
| `terminal_area` | `TerminalArea` (herdr tab bar + split tree of Ghostty views) | ghostty-terminals agent |
| `overlay` | the transparent overlay window above the terminals (API for chrome) | ghostty-terminals agent |
| `chrome`, `attention` | `Chrome`: command palette, context menus, toasts, confirmations; `Attention`: status changes for toasts, and the badge count (see 0006) | chrome agent (ghostty-terminals agent provides the overlay window) |
| `state` | `AppState`, the shared model | shared, small edits |
| `herdr` | the only module that names herdr-client types | herdr-client integration |
| `actions` | app-wide actions, app menu, global key bindings | shared |
| `theme` | light/dark; follows the system, View → Toggle Light/Dark | app-shell |

Every view is constructed as `View::new(state: Entity<AppState>, window, cx)`.
To replace a placeholder, keep that constructor and the type name; everything
else in the module is yours.

## How views get herdr state

- `AppState` (one `Entity`, created in `main`) holds the herdr config, the
  connection status, the snapshot cache, the focused pane and the active
  worktree. Views hold the entity and read it in `render`.
- Redraw on any change: `cx.observe(&state, |_, _, cx| cx.notify())`.
- React to discrete changes: `cx.subscribe(&state, ...)` on `AppEvent`
  (`ActiveWorktreeChanged(Option<PathBuf>)`, `SnapshotReplaced`; add variants
  as needed).
- `crate::herdr` re-exports `herdr-client` types under the names views use
  (`Snapshot` = `herdr_client::State`, `Agent`, `AgentStatus`, `SplitTree`,
  `ConnectionStatus`, `Config` = `Endpoint`, ...). `AppState::new` starts
  `SessionSync` and applies its updates on the foreground: `snapshot:
  Arc<Snapshot>` is replaced whole (`SnapshotReplaced`), `focused_pane` follows
  (`FocusedPaneChanged`) and `connection` tracks the status. Under `cargo test` it stays offline.
- Commands to herdr go through methods on `AppState` (`focus_pane`, `split_pane`,
  `set_split_ratio`, `new_tab`, ..., or `call` for anything else; each returns a
  `Task<herdr::Result<T>>` to await or `.detach_and_log_err(cx)`), never straight from a view, so there is one place that talks
  to the socket.
- `Config::from_env` honors `HERDR_BIN`, `HERDR_SESSION`, `HERDR_SOCKET_PATH`.

## How actions flow

- Cross-module intents are GPUI actions in `crate::actions`: `OpenFile { path }`,
  `FocusPane { pane_id }`, `CommandPalette`, `ToggleTheme`, `Quit`.
- A view sends one with `window.dispatch_action(Box::new(OpenFile { path }), cx)`.
  Actions bubble up the focus path, not sideways, so `Workspace` handles them
  (`on_action` in its render) and forwards to the owning view
  (`OpenFile` → `Editor::open`, `FocusPane` → select the herdr tab, then `TerminalArea::focus_pane`,
  `CommandPalette` → `Chrome::open_command_palette`).
- To add a cross-module action: define it in `actions.rs`, add a handler in
  `Workspace`, implement the method on the target view.
- Actions that never leave a module (e.g. editor save) are defined in that
  module and handled by its own view.
- herdr-originated changes are not actions: they come in through `AppState`.
- Data actions use `#[action(namespace = app, no_json)]` so they need no serde.

## Window layout and titlebar tabs

```
● ● ●  [herdr] [a.rs ● ×] [b.rs ×]          ← GPUI Kit TitleBar, transparent
sidebar (workspaces / files) │ main area     ← h_resizable
```

- The window uses `TitleBar::window_options()` (transparent, unified,
  `app_owns_titlebar_drag`); `workspace::window_options` adds the title for
  the Window menu. Kit's `TitleBar` moves the window on drag and zooms on
  double-click; it leaves 80px for the traffic lights.
- Tabs stop mouse-down (no window move) and click (no zoom) propagation;
  everywhere else in the bar drags the window.
- The first tab is herdr: fixed, selected at start. The rest are
  `Editor::tabs()` for the active worktree, with `Editor::active_index()`
  marking the selected one. `Workspace` holds `MainTab::{Herdr, Files}`.
- The editor emits `EditorEvent::Activated` when a file is opened, picked, or
  brought forward for a close question; `Workspace` then selects `Files`.
  When the editor has no file for the active worktree (last tab closed, or a
  worktree switch), `Workspace` selects herdr. The editor has no empty state.
- Selecting a file tab calls `TerminalArea::set_visible(false)`, which hides
  every Ghostty view (native views drawn over GPUI would otherwise stay at
  their last frames); the terminal area also leaves the element tree.
  Selecting herdr calls `set_visible(true)`: views are placed on the next
  frame and herdr's focused pane gets the keyboard.
- `Workspace` forwards pointer moves to the overlay (`overlay::track_pointer`)
  so floating UI works on both tabs; it used to live in the terminal area.
- Keys (context `Workspace`): `ctrl-tab` / `ctrl-shift-tab` and
  `cmd-shift-]` / `cmd-shift-[` cycle herdr plus file tabs; `cmd-1` selects
  herdr. In the terminal area its own `cmd-shift-[ ]` (herdr's tabs) wins as
  the deeper context, so there only `ctrl-tab` cycles the titlebar. `cmd-w`
  closes a file tab in the editor and a pane in the terminal area.
- `workspace::get(window, cx)` returns the window's `Workspace` (script hooks).

## Key bindings

- Each module has `pub fn init(cx: &mut App)`, called once from `main`, which
  calls `cx.bind_keys` for its own bindings. Scope them with a key context
  (`.key_context("Editor")` on the view's root, `KeyBinding::new("cmd-s", Save,
  Some("Editor"))`). Global bindings (`None` context) live in `actions::init`.
- The app menu is set in `actions::init`; modules may add items there.

## Overlays

Ghostty views sit above GPUI's layer, so `Chrome` will render floating UI in a
transparent overlay window. Callers only use `Chrome`'s API, so moving from
in-window rendering to the overlay touches only `chrome/`:

- `OpenContextMenu { target, position }` (action, from any view) or
  `Chrome::context_menu(target: MenuTarget, position, extra: Vec<MenuEntry>, ..)`.
  `MenuTarget` is `Pane { pane_id } | Tab { tab_id } | Workspace { workspace_id }`;
  the chrome supplies the herdr actions and plugin actions for the target,
  `extra` adds the caller's own entries (`MenuEntry::item(label,
  MenuCommand::Callback(..))`). The terminal area calls this for pane and tab
  right-clicks; the workspace list for workspace and agent rows.
- `Chrome::run(HerdrCommand)` sends a herdr command, confirming closes first;
  `Chrome::confirm(Confirmation, on_confirm)` asks anything else.
- `Chrome::toast(message)`, `Chrome::open_command_palette()`.
- Views that only have the main window get the chrome with
  `chrome::get(window, cx)`. The terminal area does this for its close
  confirmations (`Chrome::confirm`); the workspace list for close
  (`Chrome::run`) and rename (`run_menu_command(MenuCommand::Rename(..))`). Everything chrome shows draws in the
  overlay; `on_confirm` callbacks run in the overlay window, so they should
  call `AppState`, not dispatch actions.

Details in 0006-chrome.md.

The overlay window itself is `crate::overlay` (ghostty-terminals):
`overlay::get_or_open(window, cx)` returns the main window's overlay;
`Overlay::update_window` runs GPUI Kit window UI in it (`push_notification`,
`open_dialog`, `open_sheet`) and `Overlay::push_layer(view, Capture::Modal |
Capture::Passive)` / `remove_layer` add floating views in main-window
coordinates. It is click-through except over what it draws that occludes, and
takes every click plus the keyboard while a modal layer, dialog or sheet is
open. Chrome's menus, palette, dialogs and toasts draw there. Details in
0008-terminal-area-and-overlay.md.

## Notes

- `extern crate gpui_kit as gpui;` in `main.rs` lets GPUI derive macros
  (`#[derive(Action)]`) resolve through the Kit.
- `#![allow(dead_code)]` in `main.rs` covers unfilled seams; remove it once
  the stand-ins are wired.
