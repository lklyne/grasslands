# 0001 — Ghostty terminals inside GPUI Kit

Status: accepted (spike `spikes/ghostty-kit`, 2026-10-15)

## Question

Can gpui-kit 0.7.3 and gpui-libghostty 0.3.1 share one window, with one
Ghostty view per herdr pane inside GPUI Kit resizable panels, a Ghostty config
per terminal, and popovers drawn above the native views?

## Answer

Yes. Of the five checks, only the per-terminal config needed a patch.

| Claim | Result | Evidence (`spikes/ghostty-kit/shots/`) |
| --- | --- | --- |
| 1. Compile and link together | `gpui_libghostty::bind_gpui!(gpui_kit);` works unchanged against gpui-kit 0.7.3 (gpui-pre 0.3.9). No feature flags or GPUI patch. | builds |
| 2. Two herdr terminals in a two-panel group, typing and output | Each panel runs `$HERDR_BIN --session gpui-dev terminal attach <id> --takeover`. Keys sent through GPUI's dispatcher and real `CGEvent` keystrokes (posted only to the spike's pid) both reach the shell. `herdr pane read` shows the typed commands and their output. | `2-typed-left.png` |
| 3. Divider and window resize keep views placed; hidden stays hidden | Moving the divider in code (`ResizableState::resize_panel`) and resizing the window (`Window::resize`) move both views to the right place. A hidden view stays hidden through a window resize and comes back in place. | `3-divider-*.png`, `4-window-small.png`, `5`/`6`/`7-right-*.png` |
| 4. Per-terminal Ghostty config file | Needs a patch (below). Left: 11pt dark blue. Right: 18pt light. A live swap to a third file through `Terminal::load_config_file` works without restarting the process. | `1-initial.png`, `8-right-config-swapped.png` |
| 5. Transparent overlay window draws a popover above a Ghostty view | Works, and the overlay follows the window's resize. | `9-overlay-popover.png`, `10-overlay-after-resize.png` |

Not yet verified: a **real mouse drag** of the divider (as opposed to the
programmatic resize) and whether the views stay frame-locked during it. The
screen locked partway through the session. While it is locked, GPUI draws no
frames, `screencapture` fails ("could not create image from window"), and
posted mouse drags don't move the divider. The spike's `--script` mode already
contains the drag: it posts a 90-step drag and screenshots mid-motion and while
the button is held. Re-run it on an unlocked screen:

```sh
swiftc -O spikes/ghostty-kit/tools/post-events.swift -o spikes/ghostty-kit/target/post-events
scripts/dev-run cargo run --manifest-path spikes/ghostty-kit/Cargo.toml -- --script --quit
```

## Versions

- gpui-kit 0.7.3 → gpui-pre 0.3.9, gpui-base 0.7.3, gpui-component 0.7.3
- gpui-libghostty 0.3.1 (vendored Ghostty commit `9f0e1719`, version string 1.3.2-dev), patched as described below
- Zig 0.16 (`ZIG` from `scripts/dev-env.sh`), Rust 1.99, Xcode 27
- objc2 0.6.5, objc2-foundation / objc2-app-kit 0.3.2 for the overlay glue
- herdr 0.9.5 (dev session `gpui-dev`)

## Patch: per-terminal config (`vendor/gpui-libghostty`)

In 0.3.1, `TerminalConfiguration` offers only Ghostty defaults, the user's
config, or a color theme, so a terminal can't be given its own font or shader.
The shim already loads an optional config file path, which the theme feature
writes to a temp file. The patch reuses that path:

- `TerminalConfiguration::File(PathBuf)`: Ghostty's defaults plus this file.
- `Terminal::load_config_file(&Path)`: reconfigures a running terminal from a
  new file without restarting it. This is the hook for per-pane shaders later.
- `update_theme` on a `File` terminal layers the colors over a copy of the
  file, so its other settings survive.

Only `src/terminal.rs` and `src/adapter.rs` change; the shim and
`vendor/ghostty` don't. The native fingerprint is computed from the Ghostty
sources, so the patched crate reuses the cached libghostty build. The crate's
tests (18, one new) and clippy pass. See `vendor/gpui-libghostty/PATCHES.md`.

Use it from any crate in the workspace with:

```toml
[patch.crates-io]
gpui-libghostty = { path = "vendor/gpui-libghostty" }
```

The whole crate is vendored, including its 21 MB Ghostty source tree. That
keeps a clean checkout buildable offline and avoids a fork on GitHub. The root
`.gitignore` re-includes `vendor/**/*.png` because Ghostty's sources contain
PNGs. The vendored `Cargo.toml` has an empty `[workspace]` table so it can be
built and tested on its own from inside the worktree.

If upstream adds a config-file option, drop the vendor copy.

## How the pieces fit

- **Layering.** Each surface is an `NSView` (`GpuiGhosttyView`) added as a
  subview of GPUI's content view, so it sits above everything GPUI draws.
  `hitTest:` returns nil, so mouse events go to GPUI, which forwards them to
  the terminal element. Each surface owns its own `ghostty_app_t` and config.
- **Placement.** The `Terminal` element's canvas prepaint calls
  `set_frame(bounds)` on every frame it is drawn. The shim also calls
  `ghostty_surface_set_size` and refreshes on every change. As a result, a
  divider drag resizes the PTY, and herdr's terminal, on every step. If that
  floods herdr, throttle it.
- **Visibility.** A view that is no longer drawn keeps its last frame and
  stays on screen. The app has to call `Terminal::set_visible(false)` for every
  pane that isn't in the current tab's layout. `set_visible(true)` takes effect
  at the next prepaint, which gives the view a fresh frame first. Part 2: track
  which terminals were drawn in each frame and hide the rest, rather than
  relying on each call site.
- **Dividers are covered.** GPUI Kit's divider hairline sits under the native
  views and can't be seen between two terminals (see the shots). Leave a small
  gap between terminal slots, or inset each Ghostty view by 1–2px, so the
  handle shows. Grabbing it still works, because the views don't hit-test.
- **Overlay window.** Open a GPUI window with `WindowKind::PopUp`, a
  `Transparent` background and `focus: false`. Then, through AppKit: set the
  style mask to borderless, `opaque = NO`, clear background, no shadow,
  `ignoresMouseEvents = YES`, `addChildWindow:ordered:Above` on the main
  window, and set its level to the parent's level. PopUp level would float
  above other apps. Match its frame to the parent's content rect on
  `observe_window_bounds`. A child window moves with its parent on its own.
  GPUI draws normally into it, including `cx.theme()` styling. Its root view
  must not paint a background, so don't wrap it in `base::Root`.
- **Overlay input (Part 2).** For a menu or palette, set `ignoresMouseEvents`
  to `NO` while it is open, and dismiss on any click outside it. A palette also
  needs the overlay to become key. GPUI makes PopUp a non-activating `NSPanel`;
  whether it accepts key status is untested.

## Gotchas

- The shim sets `GHOSTTY_LOG=stderr`, so every surface logs config loads and
  PTY lifecycle to the app's stderr. If installed, Ghostty finds its resources
  in `/Applications/Ghostty.app`. Shell integration isn't injected, which is
  right for `herdr terminal attach`.
- The first build compiles libghostty with Zig (several minutes). The result
  is cached in `~/Library/Caches/gpui-libghostty` and shared across target
  directories and worktrees.
- `herdr pane read` prints plain text, not JSON.
- The `gpui-dev` herdr session is shared with the other agents. At 22:33 the
  server was restarted (not by this spike), and restoring the session gave
  every pane a **new terminal id**. The app must take terminal ids from a fresh
  snapshot after every reconnect, never from a cache.
- When screen-capturing for verification, use `screencapture -l <windowNumber>`
  (or `-R` with the window's own rect after activating it), never full screen.
  A full-screen capture picks up whatever else is open on the user's desktop.
