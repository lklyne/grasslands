# 0010 — One window: Ghostty terminals below GPUI

Date: 2026-10-16. Status: deferred 2026-10-16. The overlay window stays for now; this is open to revisit. The spike is in `spikes/single-window`.

## Verdict

Drop the overlay window. The Ghostty views can sit **below** GPUI's view in
the same window. GPUI then draws on a transparent layer and leaves the terminal
slots unpainted. Menus, dialogs, the palette and toasts become ordinary GPUI
elements on top, with GPUI's own focus, keyboard and hit-testing. There is no
second window, no key-window handoff and no click-through bookkeeping.

The spike's script passes all 21 checks, using in-app GPUI events only. The
checks cover placement, typing, a divider drag checked on every frame, a menu
and a text-input dialog over the terminals, drag-select + cmd-c, cmd-v,
scrolling, right-click into a mouse-reporting app, hide/show, and window
resize. Screenshots match. Two things are still open and need the human's
hands: real AppKit live resize, and real clicks and typing (list at the end).

## Answers

### 1. Can GPUI render transparent so a view behind it shows through?

Yes, with no GPUI patch. `WindowBackgroundAppearance::Transparent` sets the
following (gpui-pre-macos 0.3.9, `window.rs` `set_background_appearance`):
- `NSWindow.opaque = NO` and a near-clear window background color;
- `CAMetalLayer.opaque = NO` (`metal_renderer.rs` `update_transparency`);
- each frame clears to `(0,0,0,0)` instead of opaque black (`metal_renderer.rs:722`).

GPUI's blending writes premultiplied color with accumulated alpha
(`SourceAlpha / OneMinusSourceAlpha` on rgb, `One / One` on alpha). That is
what Core Animation expects, so translucent GPUI pixels composite correctly
over the terminal. A Kit dialog's scrim visibly darkens the terminal under
it (`shots/06-dialog-over-terminals.png`).

The window's view tree is `contentView` → `GPUIView`: GPUI adds its own view
*inside* the content view (`window.rs:1199`). GPUI already uses this slot for
its blur view, which it inserts into `contentView` "positioned: Below
relativeTo: nil". So `contentView` is a supported place for siblings below
GPUI.

Kit's `Root` paints the theme background over the whole window, so it must
be built as `Root::new(view, window, cx).bg(transparent_black())`, as the
overlay already does. Every non-terminal region then paints its own
background.

### 2. Does Ghostty render correctly behind it? What patch?

Yes. The Ghostty view is in the right place, sharp, and follows the divider
on every frame:
- 40-step divider drag: on every step, both views' frames matched the
  layout within 1.5px (`views on slots every drag step`);
- 60-step window-resize burst, one resize per frame: 59 of 60 steps
  matched. The one miss was a 4px difference against Kit's
  `ResizableState.sizes()` (fractional sizes being rescaled), not against
  the bounds actually drawn. The view takes its frame from the drawn bounds
  in prepaint, so it can't disagree with them;
- shots `01`–`04`, `14`.

Placement is unchanged: the shim still flips y against `GPUIView`'s bounds,
and `GPUIView` fills `contentView` (autoresizing), so the same frame is right
in `contentView`'s coordinates.

gpui-libghostty 0.3.1 puts the view **above**: `[parent addSubview:view]`,
where `parent` is the `GPUIView` from the raw window handle. The spike
patches nothing. It moves each view after `Terminal::spawn`
(`spikes/single-window/src/native.rs` `lower_terminal_views`). For the app,
patch the shim instead, so every surface, including a respawn on Reattach,
is born in the right place:

```objc
// vendor/gpui-libghostty/shim/ghostty_surface.m, gpui_ghostty_surface_new
state->parent = (NSView *)parent_view;          // still GPUIView: frames flip against it
state->view = [[GpuiGhosttyView alloc] initWithFrame:NSMakeRect(0, 0, 800, 600)];
[state->view setHidden:YES];
NSView *host = state->parent.superview;        // the window's contentView
if (host != nil) {
    [host addSubview:state->view positioned:NSWindowBelow relativeTo:state->parent];
} else {
    [state->parent addSubview:state->view];     // no window yet: old behavior
}
```

That is the whole patch. `gpui_ghostty_surface_free` already calls
`removeFromSuperview`, and `hitTest:` keeps returning nil. The shim is
compiled on its own (`build.rs` `compile_shim`), and the libghostty
fingerprint covers only the Ghostty sources, so the cached libghostty build
is reused. Record it in `vendor/gpui-libghostty/PATCHES.md`.

Also set the window's background color, opaque, to the terminal background
(`native.rs` `set_backdrop`; GPUI only sets the color in
`set_background_appearance`). Without it, a strip that neither GPUI nor a
Ghostty view has drawn yet, such as a terminal mid-resize, shows the desktop
through the window. With it, the strip shows the terminal color. Update it on
theme change.

### 3. Input

Nothing changes. Input already never goes to the Ghostty view: its `hitTest:`
returns nil, GPUI receives every event, and the `Terminal` element forwards
keys, text, mouse position, buttons and scroll through libghostty's surface
API (`adapter.rs`). Putting the view below GPUI keeps that path exactly. A
click-through hole isn't needed and would be worse, because it would split
input between two responders again.

Verified in the spike, all by GPUI event dispatch:

| Input | Result |
| --- | --- |
| Typing | `herdr pane read` shows the typed command and its output |
| Drag-select + cmd-c | clipboard reads `pastemarker-abc` (`shots/09-selecting.png`) |
| cmd-v | the pasted command runs |
| Scroll | the terminal's rendered frame changes (snapshot hash) |
| Right-click with mouse reporting on | `cat -v` shows `^[[M"…` / `^[[M#…` (press and release, button 2) |
| Menu and dialog keys | never reach either terminal (`pane read` unchanged) |

Two findings apply to both designs; neither comes from this change:

- **Plain drags don't select in herdr panes.** herdr's attach client turns
  on mouse reporting (`?1000h`/`?1002h`/`?1003h`/`?1006h` are in its
  binary), and Ghostty then reports the drag to herdr instead of selecting
  (`Surface.zig` `cursorPosCallback`: `isMouseReporting` returns before the
  selection code). Shift+drag selects, as in Ghostty.app; the spike's copy
  check uses shift. If the human expects plain-drag selection, that's a
  herdr setting or an adapter policy (e.g. always add shift for selection
  drags), not a layering question.
- **IME (marked text) isn't wired anywhere today.** Neither gpui-libghostty
  nor the app registers a GPUI input handler for the terminal, so there is no
  preedit. In the single-window design GPUI's view is the first responder
  for everything, which is the simplest place to add it. The patch:
  1. In the `Terminal` element's paint, call
     `window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx)`.
  2. Implement `EntityInputHandler` for `Terminal`:
     - `replace_and_mark_text_in_range` → `ghostty_surface_preedit`;
     - `replace_text_in_range` → `ghostty_surface_text`;
     - `bounds_for_range` → `ghostty_surface_ime_point`.
  3. Add a shim wrapper for each of those three functions.
  4. In `key_down`, don't send text for a keystroke while
     `marked_text_range` is `Some`.

  This is separate work and the same work in either design.

### 4. Popover and text-input dialog over a terminal, one window

Works (`shots/05-menu-over-terminal.png`, `06-dialog-over-terminals.png`, `07-toast.png`):

- **Menu.** A Kit `PopupMenu` in `deferred(anchored())` takes focus.
  down, down, enter picks "Rename pane", and the menu closes and hands focus
  back to the terminal.
- **Dialog.** A Kit dialog with an `Input`, opened with
  `window.open_dialog`, receives "hello dialog". `NSApp.windows` holds one
  visible window, and it stays key the whole time. The second entry in the
  list is AppKit's invisible `TUINSWindow` text-input window.
- **Back to the terminal.** A click on the terminal after Escape gives it
  the keyboard again.
- **Toast.** `window.push_notification` draws over the right terminal.

There are no `makeKeyAndOrderFront` calls, no `ignoresMouseEvents`, no
deferred attach and no capture flags.

### 5. Cost

- **CPU.** `window.draw` for the spike's small scene takes 0.7–1.2ms median
  (debug build), the same as an opaque window. Transparency changes no
  scene-building work.
- **GPU / compositor.** WindowServer blends one full-window non-opaque layer
  over the Ghostty layers instead of covering them with an opaque one. Not
  measured. On Apple silicon this is the same path every transparent or
  vibrant macOS window takes. If it shows up in Instruments, the fix is to
  keep the window opaque and make only the terminal area's backing
  transparent, which GPUI doesn't support today.
- **Flicker.** During a live resize, GPUI already presents with the Core
  Animation transaction (`display_layer` → `set_presents_with_transaction(true)`),
  so the holes and the views' frames move together. Ghostty redraws on its
  own renderer thread, so a terminal's contents can trail its frame by a
  frame, in either design. The difference: in the overlay design that strip
  showed GPUI's background; here it shows the window backdrop. Set the
  backdrop to the terminal background and it reads as terminal. The
  programmatic resize burst showed no tearing and no desktop showing
  through (`shots/14-mid-window-resize.png`). A real live resize hasn't been
  tested (see below).
- **Per-terminal Ghostty config and shaders.** These are unaffected: fonts,
  colors and `custom-shader` all render inside each Ghostty layer. What
  breaks:
  - `background-opacity < 1` and `background-blur` show the window backdrop,
    not the desktop. Keep terminals opaque, or set the backdrop to clear and
    accept desktop show-through.
  - Each view still needs its own config file as now (0008), unchanged.
- **Gained.** GPUI can draw on top of terminals in the main window:
  - focus rings over the view's edge, so the 5px gap and the inset are no
    longer needed;
  - status glows and badges (the spike draws a badge inside each hole);
  - GPUI's own drag preview;
  - Kit's resizable divider hairline, which 0001 couldn't see.

## What stays the same

- **Hide what isn't laid out.** `Terminals::show_only` / `set_visible(false)`
  is still needed. Views are siblings that stack in insertion order. A stale
  view that isn't hidden sits *above* another terminal's view wherever that
  terminal's hole now covers it; the spike's left terminal growing into the
  hidden right pane's slot is exactly this case. It also saves GPU work.
  Over opaque GPUI regions such as a file tab, a stale view is simply
  covered, so that class of visual bug goes away.
- **Move focus before removing a focused element.** If the focused element
  drops out of the tree, actions stop reaching ancestor views. The spike hit
  this when hiding a focused right pane. The app's terminal area already
  re-focuses on layout changes; it must keep doing so.

## Migration plan for crates/app

1. **vendor.** Apply the shim patch above, note it in `PATCHES.md`, and run
   the vendored crate's tests.
2. **main.rs.** Open the main window with `WindowBackgroundAppearance::Transparent`.
   Build Root as `cx.open_window` + `Root::new(view, window, cx).bg(transparent_black())`
   instead of `gpui_kit::open_window`. Set the window backdrop to the terminal
   background, and again on theme change.
3. **Backgrounds.** Paint an explicit `bg` on every region that used to rely
   on Root: titlebar, sidebar, editor, file tabs, terminal tab bar, pane
   headers, dividers, empty and Detached panes. Paint nothing inside a
   terminal slot's body. A transparent pixel left outside a slot shows the
   backdrop, which makes a missed background easy to spot in a screenshot.
4. **Delete `overlay/`** (`mod.rs`, `native.rs`, about 650 lines), plus:
   - `workspace`'s `overlay::track_pointer` forwarding;
   - `Overlay`, `Capture`, `LayerId`, `dispatch_keystroke`/`dispatch_action`/
     `key_target`, `is_capturing`, `make_key_later`, `set_frame_later`,
     `attach_later`;
   - the `hidesOnDeactivate`, child-window and level code.
5. **chrome.**
   - `floating` calls `f` on the main window directly (it already falls back
     to that when there's no overlay), and `overlay_for` goes.
   - Context menus stop being overlay layers. Render the open `PopupMenu` in
     `deferred(anchored().position(p))` from the chrome's view, or use Kit's
     `ContextMenu`. Dismiss on `DismissEvent`, as now.
   - `menu_open` becomes a `bool`, and `floating_window` goes.
   - Palette, confirm and rename prompts are unchanged apart from the window
     they run in. The `update_later` re-routing that exists because callbacks
     fired in the overlay window can collapse to plain `cx.listener` code.
6. **terminal_area.**
   - Replace the overlay drag chip (`Capture::Passive` layer) with GPUI's own
     `on_drag` preview view.
   - Drop the 5px divider gap and the inset that existed only because native
     views covered GPUI. Draw the focus ring and divider on top instead.
   - Keep `show_only`.
7. **Scripts and tests.**
   - `overlay::dispatch_*` becomes `window.dispatch_*`.
   - Delete the overlay capture, key-status and overlay-follows-resize checks.
   - Screenshots capture only the main window.
   - Add: one visible window and still key while a dialog is open; dialog and
     menu keys don't reach `pane read`; views below `GPUIView`
     (`native::placement`).
   - The `update_in` / `current_window_by_entity` trap described in 0008
     disappears with one window.
8. **Docs.** Mark 0008's overlay sections and 0001's "Layering" and "Overlay
   window" bullets superseded by this note.

## What the human should try by hand

Real clicks and live resizes can't be scripted without OS input. Run:

```sh
scripts/herdr-dev workspace create --label spike --no-focus   # if missing
scripts/herdr-dev pane split <that root pane> --direction right
scripts/dev-run cargo run --manifest-path spikes/single-window/Cargo.toml
```

Then:
1. Click in each terminal and type. Check the focus ring follows the click.
2. Drag the divider back and forth quickly. Watch the terminal edges for
   flicker or a wrong-colored strip.
3. Drag the window's corner (a real live resize), quickly and slowly.
4. Press cmd-2 for the menu. Click an item with the mouse. Open it again and
   click outside it.
5. Press cmd-3 for the dialog. Type, click into the input, click the × and
   the scrim. None of these should beep or leave the dialog unable to type.
6. Shift+drag to select text, cmd-c, then cmd-v into the other terminal.
   Also try a plain drag (expected: no selection, since herdr has mouse
   reporting on).
7. Scroll with the trackpad over each terminal.
8. Press cmd-4 for a toast and click it.
9. Switch to another app and back. The terminal you were in should still
   have the keyboard.
10. With a Japanese or Pinyin input source, type into a terminal. Expected:
    composition doesn't show (no IME support yet, as today).

## Reproducing the checks

```sh
swiftc -O scripts/capture-windows.swift -o spikes/single-window/target/capture-windows
scripts/dev-run cargo run --manifest-path spikes/single-window/Cargo.toml -- --script --quit
```

It needs a `spike` workspace with two panes in the dev session. It prints
`PASS`/`FAIL` per check and writes window-only shots to
`spikes/single-window/shots/` (git-ignored PNGs). It saves and restores the
system clipboard around the copy/paste checks.
