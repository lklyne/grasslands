# 0002 — App structure and module seams

Date: 2026-10-15. Status: accepted (app-shell, Part 1).

## Modules (crates/app/src)

| Module | View | Owner |
| --- | --- | --- |
| `workspace` | `Workspace`, the window root: `h_resizable` of sidebar / editor / terminal area. Routes cross-module actions. | app-shell |
| `sidebar` | `Sidebar`: `v_resizable` of `agents::AgentList` over `files::FileTree` | app-shell |
| `sidebar::agents` | `AgentList` | chrome agent |
| `sidebar::files` | `FileTree` | files agent |
| `editor` | `Editor` (file tabs + code editor) | app-shell, Part 2 |
| `terminal_area` | `TerminalArea` (herdr tab bar + split tree of Ghostty views) | ghostty-terminals agent |
| `overlay` | the transparent overlay window above the terminals (API for chrome) | ghostty-terminals agent |
| `chrome`, `attention` | `Chrome`: command palette, context menus, toasts, confirmations; `Attention`: status changes and unseen results (see 0006) | chrome agent (ghostty-terminals agent provides the overlay window) |
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
  (`FocusedPaneChanged`), raw events are re-emitted as `AppEvent::Herdr` and
  `connection` tracks the status. Under `cargo test` it stays offline.
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
transparent overlay window. Callers only use `Chrome`'s API, so moving from
in-window rendering to the overlay touches only `chrome/`:

- `OpenContextMenu { target, position }` (action, from any view) or
  `Chrome::context_menu(target: MenuTarget, position, extra: Vec<MenuEntry>, ..)`.
  `MenuTarget` is `Pane { pane_id } | Tab { tab_id } | Workspace { workspace_id }`;
  the chrome supplies the herdr actions and plugin actions for the target,
  `extra` adds the caller's own entries (`MenuEntry::item(label,
  MenuCommand::Callback(..))`). The terminal area calls this for pane and tab
  right-clicks.
- `Chrome::run(HerdrCommand)` sends a herdr command, confirming closes first;
  `Chrome::confirm(Confirmation, on_confirm)` asks anything else.
- `Chrome::toast(message)`, `Chrome::open_command_palette()`.

Details in 0006-chrome.md.

The overlay window itself is `crate::overlay` (ghostty-terminals):
`overlay::get_or_open(window, cx)` returns the main window's overlay;
`Overlay::update_window` runs GPUI Kit window UI in it (`push_notification`,
`open_dialog`, `open_sheet`) and `Overlay::push_layer(view, Capture::Modal |
Capture::Passive)` / `remove_layer` add floating views in main-window
coordinates. It is click-through except over what it draws that occludes, and
takes every click plus the keyboard while a modal layer, dialog or sheet is
open. Until chrome moves its menu, palette and toasts there, they are hidden
behind terminals. Details in 0008-terminal-area-and-overlay.md.

## Notes

- `extern crate gpui_kit as gpui;` in `main.rs` lets GPUI derive macros
  (`#[derive(Action)]`) resolve through the Kit.
- `#![allow(dead_code)]` in `main.rs` covers unfilled seams; remove it once
  the stand-ins are wired.
