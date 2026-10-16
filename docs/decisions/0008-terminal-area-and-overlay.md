# 0008 — Terminal area and overlay window

Date: 2026-10-15. Status: accepted (ghostty-terminals, Part 2).

The terminal area (`crates/app/src/terminal_area/`) renders herdr's tabs and
split trees with one Ghostty view per pane. The overlay (`crates/app/src/overlay/`)
is a transparent child window for anything that must float above those views.
Background and spike results are in 0001.

## Terminal views

- **Keyed by `terminal_id`, not pane id.** `Terminals::reconcile` runs on every
  `AppEvent::SnapshotReplaced`. It spawns a view for each new terminal and drops
  views whose terminal is gone. herdr-client re-reads the snapshot right after
  `pane.created`, `pane.closed` and `pane.moved`, so views are created,
  destroyed and moved on those events:
  - A moved pane keeps its terminal id, so it keeps its view and its process.
    The script checks that the view entity is the same before and after.
  - A restarted herdr hands out new terminal ids, so every view is replaced.
    This is what "re-read terminal ids from each fresh snapshot after
    reconnect" means here, and the script checks it with a real server restart.
- **Every pane gets a view, not only visible ones.** Views start hidden. Each
  render computes the set of terminals in the active tab's layout and calls
  `Terminals::show_only`, which hides everything else explicitly. A native view
  that drops out of the layout would otherwise stay on screen at its last frame.
- **The attach command** is `$bin [--session S] terminal attach <id> --takeover`,
  built from `AppState::herdr_config`. With no config (the socket guard refused
  the endpoint), nothing is spawned.
- **When the attach dies** (another client took the terminal over), the view is
  kept and the pane shows "Detached" with a Reattach button. Respawning on its
  own would fight the other client.
- **One Ghostty config file per terminal**, in `$TMPDIR/rusted-tools-<pid>/ghostty/`.
  Each is built from three parts, in order:
  1. `~/.config/rusted-tools/ghostty.conf`, if present.
  2. Colors from the GPUI Kit theme.
  3. Per-terminal overrides (`Terminals::set_overrides`, the hook for status
     shaders later).

  A theme change rewrites every file and calls the patched
  `Terminal::load_config_file`, so nothing restarts.
- **`right_click: "pane"`** is set once for every pane id seen. Our context menu
  opens from right-clicks on the pane header and on tabs, through chrome's
  `OpenContextMenu`.

## Split tree: our own element, not GPUI Kit's resizable panels

The spec suggested nested GPUI Kit resizable groups. I wrote a small two-sided
flex split instead, for three reasons:

- herdr's ratio is the source of truth. Kit's `ResizableState` stores pixel
  sizes, so syncing herdr's ratios back into it on every `layout.updated` would
  mean converting through the container size, both ways.
- Kit's divider is a hairline. The native views cover it, so it can't be seen.
- Kit has no hook for "send the ratio on release, keep the local value until
  herdr agrees".

How it works:
- Each side gets `flex-grow = ratio` (respectively `1 - ratio`), with a fixed
  5px divider between them.
- That gap holds the visible hairline and is the drag handle.
- A 1px border around each terminal body carries the focus ring in the strip
  the native view leaves uncovered.
- Pane headers (22px, drawn by GPUI and so always visible) show the agent
  status and label, a close button, a drag handle, and the context menu.

Divider drag:
- During the drag, `ratio_at` sets a local override, and the views follow on
  every frame. The script checks view frames against layout slots on every
  drag step.
- On release, `layout.set_split_ratio` is sent with the path from the root.
- The override stays until a snapshot shows herdr's ratio within 0.005 of it,
  or 3 seconds pass.

Zoom: a zoomed tab renders only its focused pane.

## Sync table as built

| User action | Call |
| --- | --- |
| Drag a divider (release) | `layout.set_split_ratio` (tab, path, ratio) |
| cmd-ctrl-arrows | the same, on the innermost split along that axis, ±0.05 |
| cmd-d / cmd-shift-d | `pane.split` right/down of herdr's focused pane (AppState sets `right_click: pane`) |
| Drop a pane header on another pane | `pane.swap` (same tab) or `pane.move` next to it (other tab) |
| Drop a pane header on a tab / on `+` | `pane.move` to that tab / to a new tab |
| × on a pane, cmd-w | `pane.close`, after a native confirmation if its agent is working or blocked |
| × on a tab, cmd-shift-w | `tab.close`, with the same confirmation for any running agent in it |
| Click a pane | `pane.focus` (plus GPUI keyboard focus at once) |
| Click a tab, cmd-shift-[ / ] | `tab.focus` |
| `+`, cmd-t | `tab.create` in the shown workspace |
| cmd-alt-arrows | `pane.focus` on the neighbour, using the bounds drawn last frame |
| cmd-shift-enter | `pane.zoom` toggle |

The bindings live in the `TerminalArea` key context. GPUI matches bindings
before the focused terminal's key handler, so they work while typing in a
terminal (the script checks cmd-ctrl-left on a focused terminal). Everything
else, including ctrl keys, goes to the terminal.

Keyboard focus follows herdr's `pane.focused` only if focus is already in the
terminal area. An agent focusing a pane must not pull typing out of the editor.

## Overlay API (for chrome and anything else floating)

```rust
let overlay = overlay::get_or_open(window, cx)?;   // window: the main window
// GPUI Kit window-level UI, drawn above the terminals:
overlay.update(cx, |o, cx| o.update_window(cx, |w, cx| w.push_notification("Saved", cx)));
overlay.update(cx, |o, cx| o.update_window(cx, |w, cx| w.open_dialog(cx, |d, _, _| d.title("…"))));
// Your own floating views, positioned in main-window coordinates:
let id = overlay.update(cx, |o, cx| o.push_layer(menu_view, Capture::Modal, cx));
overlay.update(cx, |o, cx| o.remove_layer(id, cx));
```

- It is one borderless, transparent child window per main window. Its content
  rect matches the main window's, so main-window coordinates (element bounds,
  `event.position`) apply unchanged. Its root is a GPUI Kit `Root` with a
  transparent background.
- **Mouse.** The overlay is click-through, except where the pointer is over
  something it drew that occludes. GPUI Kit's toasts, menus and dialogs all do;
  give your own views `.occlude()`. It works by checking the overlay's bottom
  hitbox with `Hitbox::is_hovered_at`, on every pointer move (forwarded from the
  main window by the terminal area) and after every overlay frame.
- **Modal.** While a `Capture::Modal` layer, a Kit dialog or a Kit sheet is
  open, the overlay takes every click and becomes the key window, so menus and
  the palette get the keyboard. Key status goes back to the main window when
  the last modal piece closes. A menu should close itself on click-outside;
  Kit's `PopupMenu` emits `DismissEvent`.
- **AppKit setup** (in `overlay/native.rs`):
  - borderless, non-opaque, clear background, no shadow;
  - `addChildWindow:ordered:Above`, so it moves, hides and orders with the main
    window;
  - level reset to the parent's, because GPUI opens pop-ups above every app;
  - frame matched to the parent's content rect on `observe_window_bounds`.
- Always use `cx.update_window(handle.into(), …)`, never the typed
  `WindowHandle<Root>::update`, to reach the overlay window. The typed form
  leases the `Root` entity, and `has_active_dialog` then panics reading it.

Chrome still draws its menu, palette and toasts in the main window (0006). They
are hidden wherever a terminal is. Moving them means swapping
`window.push_notification` / `deferred(anchored())` for the calls above.

## Verifying: `RT_SCRIPT=terminals`

`RT_SCRIPT=terminals scripts/dev-run cargo run -p app` creates a `script`
workspace in the dev session, runs 32 checks, saves window-only screenshots to
`target/script-shots/`, closes the workspace and quits. Set `RT_SCRIPT_KEEP=1`
to leave the app open.

All input goes through GPUI inside the app (`dispatch_action`,
`dispatch_keystroke`, `dispatch_event`); nothing is sent through the OS. The
checks read state back:
- the snapshot;
- `pane.read` text, for typing;
- the native subviews' frames and hidden flags against the slots the layout
  drew, for placement and hiding.

If the screen is locked, GPUI's display link stops. The script then calls
`Window::draw` itself, and the screenshots are reported as skipped. The script
restarts this worktree's own dev server (`server stop`; herdr-client starts it
again). Never point it at the default session; it refuses when
`HERDR_SESSION` is unset.

## Known gaps

- No logger is installed in the app, so the terminal area's `log::` messages
  (spawn failures, command errors) are not printed anywhere yet.
- The drag preview for a dragged pane header is drawn by GPUI in the main
  window, so it is hidden while over a terminal. Drop targets still work and
  highlight the target's header.
- Close confirmations use a native alert (`window.prompt`), not chrome's
  `Confirmation`, because the terminal area has no handle on `Chrome`. Route
  them through chrome once it draws in the overlay.
