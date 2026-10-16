# rusted-tools

A native macOS app for [herdr](https://herdr.dev). herdr keeps running your agents, terminals, worktrees and session restore as a headless server. This app replaces herdr's terminal UI with a real window: workspaces and tabs, a split layout you can drag, a real Ghostty terminal in every pane, a file tree and code editor that follow whichever worktree the focused agent is working in, and an agent list with live status.

It is built with [GPUI Kit](https://gpui-kit.com) for the UI and [gpui-libghostty](https://github.com/behzade/gpui-libghostty) for the terminals, and talks to herdr 0.9.5 over its socket API. The product spec is [docs/spec.md](docs/spec.md). Design decisions live in [docs/decisions/](docs/decisions/).

This is v0 and alpha software. gpui-libghostty is itself alpha.

## What it does today

- Mirrors herdr's workspaces, tabs and split trees, and keeps them in sync both ways. Splitting, closing, swapping, moving panes between tabs and dragging dividers all go through herdr, and the window redraws from herdr's next snapshot.
- Runs one Ghostty view per herdr pane via `herdr terminal attach <id> --takeover`.
- Follows the focused pane's folder (`foreground_cwd`, so it also follows an agent that `cd`s) to its git worktree root. The file tree re-roots, respects `.gitignore`, and updates live as agents create, edit and delete files.
- Opens files in a tabbed code editor, grouped per worktree. Files an agent changes on disk reload, and you're asked when you have unsaved edits. Renames and trashes from the tree (and renames on disk) carry open tabs along.
- Shows an agent list with status, blocked agents first, plus toasts, context menus, a command palette (app commands and herdr plugin actions) and confirmations before closing a running agent.

## Prerequisites

- macOS with Xcode 27 command line tools.
- Rust 1.99 (edition 2024).
- Zig 0.16 at `~/.local/zig-0.16/zig`. libghostty builds with Zig and needs exactly 0.16. Homebrew's Zig 0.17 won't build it. If yours lives elsewhere, set `ZIG=/path/to/zig`. To install it, download the macOS aarch64 0.16 tarball from [ziglang.org/download](https://ziglang.org/download/) and unpack it so the binary sits at `~/.local/zig-0.16/zig`.
- herdr 0.9.5. The app speaks protocol 22 and uses APIs older servers don't have.

### herdr versions on this machine

Two herdr installs exist side by side, on purpose.

- `/opt/homebrew/bin/herdr` is 0.7.4, from Homebrew. Your default session (`~/.config/herdr/herdr.sock`) runs on it, with your real agents.
- `~/.local/herdr-0.9.5/herdr` is 0.9.5. The dev scripts use it, with a separate config dir and a separate session per checkout. It never touches your default session.

The dev build works today without upgrading anything. To use the app for real you need your default session on 0.9.5.

### Upgrading your default herdr to 0.9.5

Homebrew already has 0.9.5. Upgrading the client is harmless. Restarting the server is the part that costs something, because stopping a herdr server ends every process in its panes. herdr restores the layout afterwards, and agents that registered a native session through a current herdr integration resume their conversations. Shells, dev servers and test runs in panes do not come back.

1. Let your agents reach a stopping point. `herdr integration status` shows which agent integrations can resume.
2. `brew upgrade herdr`. The client is now 0.9.5. The running 0.7.4 server keeps going.
3. `herdr server stop`, then start herdr again (`herdr`, or `herdr server` for headless). A 0.7.4 server is most likely older than herdr's "endpoint generation 1", and the docs say that one-time jump needs a stop. `herdr update --handoff`, which can keep processes alive, is disabled for Homebrew installs.
4. `herdr integration install <agent>` for anything step 1 listed as outdated.

Optional, but the app expects it: add this to `~/.config/herdr/config.toml` so herdr's own notifications reach macOS when no herdr TUI is attached.

```toml
[ui.toast]
delivery = "system"
```

## Running the dev build (safe)

```sh
scripts/dev-run cargo run -p app
```

`scripts/dev-run` sets `HERDR_BIN`, `HERDR_SESSION`, `HERDR_SOCKET_PATH` and `ZIG` for an isolated dev session, then runs the command. The session is named after the checkout's folder (`dev-<dir>`), lives under `~/.local/share/rusted-tools-herdr`, and runs herdr 0.9.5. The app starts that session's server if it isn't running.

Drive the dev session from a terminal with `scripts/herdr-dev`, which is the herdr CLI pointed at the same session:

```sh
scripts/herdr-dev workspace create --cwd ~/Developer/some-repo --focus
scripts/herdr-dev pane split w1:p1 --direction right
scripts/herdr-dev pane list
```

Debug builds refuse to talk to any herdr socket outside `~/.local/share/rusted-tools-herdr` ([decision 0007](docs/decisions/0007-dev-herdr-guard.md)). A plain `cargo run -p app` from a shell inside your real herdr would inherit your live `HERDR_SOCKET_PATH`, so it shows "Disconnected" and does nothing instead. `RT_ALLOW_LIVE_HERDR=1` overrides the guard. Don't.

Tests:

```sh
scripts/dev-run cargo test --workspace
```

The herdr-client integration tests create and close workspaces in your dev session, and `tests/reconnect.rs` stops and restarts that session's server.

## Running it for real

> **Warning.** The app attaches to every herdr terminal with `herdr terminal attach <id> --takeover`. herdr allows one controller per terminal, so the app takes each terminal away from any herdr TUI attached to it, and the TUI loses control of those panes. If you open a herdr TUI and attach again, the TUI takes it back and the app's pane shows "Detached" with a Reattach button. Don't run both against the same panes. The app also sends real commands. × on a pane or tab closes it in herdr, and that kills its process (it asks first when an agent is working or blocked).

Release builds are not socket-guarded. They connect to whatever `HERDR_BIN`, `HERDR_SESSION` and `HERDR_SOCKET_PATH` point at, and with none set, to your default herdr session. Upgrade it to 0.9.5 first (above).

Build the app bundle:

```sh
scripts/bundle.sh
open target/bundle/rusted-tools.app
```

The bundle is unsigned. If macOS blocks it the first time, right-click it in Finder and choose Open. Its launcher adds `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin` and `~/.cargo/bin` to `PATH`, because apps started from Finder don't get your shell's `PATH` and wouldn't find `herdr`.

Or run the release binary from a terminal:

```sh
ZIG=~/.local/zig-0.16/zig cargo run --release -p app
```

Start it from a terminal that is not itself a herdr pane. Otherwise the app takes over the terminal you launched it from.

To try a release build against the dev session instead of your real one:

```sh
scripts/dev-run target/bundle/rusted-tools.app/Contents/MacOS/rusted-tools
```

Per-terminal Ghostty settings (font, size, anything Ghostty accepts) go in `~/.config/rusted-tools/ghostty.conf`. The app layers its theme colors on top.

## Keys

| Keys | Does |
| --- | --- |
| cmd-d / cmd-shift-d | Split the focused pane right / down |
| cmd-w | Close the focused pane (in the editor, close the file tab) |
| cmd-shift-w | Close the tab |
| cmd-t | New tab |
| cmd-shift-[ / ] | Previous / next tab (in the editor, previous / next file) |
| cmd-alt-arrows | Focus the neighbouring pane |
| cmd-ctrl-arrows | Resize the split along that axis |
| cmd-shift-enter | Zoom the focused pane |
| cmd-shift-p | Command palette |
| cmd-s | Save the file (editor) |

Terminal bindings work while you type in a terminal. Everything else, ctrl keys included, goes to the terminal. Drag a pane's header onto another pane to swap them, onto another tab to move it there, or onto `+` to give it a new tab.

## Script check modes

Debug builds have two self-driving check modes. They run inside the app and send input through GPUI, never through the OS, so they don't touch whatever window has focus. Run them against the dev session only. The terminal script refuses when `HERDR_SESSION` is unset.

`RT_SCRIPT=terminals` creates a `script` workspace, runs 32 checks on the terminal area (typing, splits, divider drags, moves, hiding, a real server restart), saves window screenshots to `target/script-shots/`, cleans up and quits. `RT_SCRIPT_KEEP=1` leaves the app open afterwards.

```sh
RT_SCRIPT=terminals scripts/dev-run cargo run -p app
```

`RT_CHROME_SCRIPT` runs `;`-separated steps against the menus, palette and dialogs and logs what's open. The step list is at the top of `crates/app/src/chrome/script.rs`.

```sh
RT_CHROME_SCRIPT='menu pane w1:p1; wait 1500; key escape; log after' scripts/dev-run cargo run -p app
```

## Known gaps

- herdr itself has two gaps: plugin panes opened as popups have no pane id or events, so the app can't show them, and toasts sent with `herdr notification show` have no event the app can listen to. See [0005](docs/decisions/0005-herdr-0.9.5-api-gaps.md).
- Terminal area ([0008](docs/decisions/0008-terminal-area-and-overlay.md)):
  - The app installs no logger yet, so spawn failures and command errors aren't printed anywhere.
  - A dragged pane header's preview is hidden while it's over a terminal (drop targets still highlight).
  - Close confirmations are native alerts rather than the app's own dialogs.
- Files and editor ([0007](docs/decisions/0007-active-worktree-and-file-tree.md)):
  - A rename that changes the extension keeps the old syntax highlighting until you reopen the tab.
  - A file moved on disk into a folder the editor isn't watching, or a folder renamed on disk, isn't followed. The tab stays open, marked deleted.
- Menus, the palette and toasts still draw in the main window, so a terminal can cover them. The overlay window that fixes this exists ([0008](docs/decisions/0008-terminal-area-and-overlay.md)), and chrome hasn't moved onto it yet ([0006](docs/decisions/0006-chrome.md)).
- Not in v0: per-terminal shaders, LSP, a git staging UI, remote machines.

## Layout

```
crates/herdr-client   herdr socket client: server start, typed commands, events, cache, reconnect. No GPUI.
crates/app            the GPUI Kit app
vendor/               gpui-libghostty with a per-terminal config patch (see decision 0001)
scripts/              dev-env.sh, dev-run, herdr-dev, bundle.sh
spikes/               throwaway experiments
docs/                 spec, herdr API schema, decisions
```
