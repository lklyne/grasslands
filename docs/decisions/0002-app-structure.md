# 0002 — App structure and module seams

Date: 2026-10-15. Status: accepted (app-shell, Part 1).

## Modules (crates/app/src)

| Module | View | Owner |
| --- | --- | --- |
| `workspace` | `Workspace`, the window root: `h_resizable` of sidebar / editor / terminal area. Routes cross-module actions. | app-shell |
| `sidebar` | `Sidebar`: `v_resizable` of `agents::AgentList` over `files::FileTree` | app-shell |
| `sidebar::agents` | `AgentList` | agents agent |
| `sidebar::files` | `FileTree` | files agent |
| `editor` | `Editor` (file tabs + code editor) | app-shell, Part 2 |
| `terminal_area` | `TerminalArea` (herdr tab bar + split tree of Ghostty views) | ghostty-terminals agent |
| `chrome` | `Chrome`: command palette, context menus, toasts, confirmations | ghostty-terminals agent proves the overlay window; API lives here |
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
- `crate::herdr` holds stand-in types (`Snapshot`, `Agent`, `Workspace`,
  `ConnectionStatus`, `Config::from_env`). When `crates/herdr-client` lands,
  add it as a path dependency and turn these into re-exports or thin wrappers;
  views keep importing from `crate::herdr`. The connection task lives in
  `AppState` (spawned from `AppState::new`), applies events to the cache with
  `cx.notify()`, and replaces it with `replace_snapshot` on reconnect.
- Commands to herdr go through methods on `AppState` (e.g. `focus_pane`,
  `split_pane`), never straight from a view, so there is one place that talks
  to the socket.
- `Config::from_env` honors `HERDR_BIN`, `HERDR_SESSION`, `HERDR_SOCKET_PATH`.

## How actions flow

- Cross-module intents are GPUI actions in `crate::actions`: `OpenFile { path }`,
  `FocusPane { pane_id }`, `CommandPalette`, `ToggleTheme`, `Quit`.
- A view sends one with `window.dispatch_action(Box::new(OpenFile { path }), cx)`.
  Actions bubble up the focus path, not sideways, so `Workspace` handles them
  (`on_action` in its render) and forwards to the owning view
  (`OpenFile` → `Editor::open`, `FocusPane` → `TerminalArea::focus_pane`,
  `CommandPalette` → `Chrome::open_command_palette`).
- To add a cross-module action: define it in `actions.rs`, add a handler in
  `Workspace`, implement the method on the target view.
- Actions that never leave a module (e.g. editor save) are defined in that
  module and handled by its own view.
- herdr-originated changes are not actions: they come in through `AppState`.
- Data actions use `#[action(namespace = app, no_json)]` so they need no serde.

## Key bindings

- Each module has `pub fn init(cx: &mut App)`, called once from `main`, which
  calls `cx.bind_keys` for its own bindings. Scope them with a key context
  (`.key_context("Editor")` on the view's root, `KeyBinding::new("cmd-s", Save,
  Some("Editor"))`). Global bindings (`None` context) live in `actions::init`.
- The app menu is set in `actions::init`; modules may add items there.

## Overlays

Ghostty views sit above GPUI's layer, so `Chrome` will render floating UI in a
transparent overlay window. Callers only use `Chrome`'s methods (`toast`,
`open_command_palette`, later `confirm`, `context_menu`), so moving from the
in-window placeholder (GPUI Kit notifications) to the overlay window touches
only `chrome.rs`.

## Notes

- `extern crate gpui_kit as gpui;` in `main.rs` lets GPUI derive macros
  (`#[derive(Action)]`) resolve through the Kit.
- `#![allow(dead_code)]` in `main.rs` covers unfilled seams; remove it once
  the stand-ins are wired.
