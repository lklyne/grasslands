# grasslands

A native macOS window for [herdr](https://herdr.dev), the agent multiplexer. [grasslands.dev](https://grasslands.dev)

```sh
brew install --cask lklyne/tap/grasslands
```

Needs macOS and herdr 0.9.5 (`brew install herdr`).

<img width="1387" height="930" alt="grasslands showing herdr workspaces, terminals and the file tree" src="https://github.com/user-attachments/assets/e26fd95b-1585-45ff-81fc-4eec5c482787" />

This is alpha software. It attaches to every herdr terminal with `--takeover`, so a herdr TUI attached to the same session loses those panes. Closing a pane in the app closes it in herdr and kills its process.

## What it does

herdr keeps running your agents, terminals and worktrees as a headless server. grasslands draws them in a real window:

- herdr's workspaces, tabs and splits, synced both ways, with a Ghostty terminal in every pane.
- A file tree and code editor that follow the worktree of the focused agent and reload when agents change files.
- A sidebar of workspaces and their agents, blocked ones first.
- A command palette for app commands and herdr plugin actions.

The UI is [GPUI Kit](https://gpui-kit.com) and the terminals are [gpui-libghostty](https://github.com/behzade/gpui-libghostty). The product spec is [docs/spec.md](docs/spec.md).

## Building from source

You need Rust 1.99, Xcode 27 command line tools, and Zig 0.16 at `~/.local/zig-0.16/zig` (or set `ZIG`). libghostty needs exactly 0.16. Homebrew's 0.17 won't build it.

```sh
scripts/dev-run cargo run -p app   # debug build against an isolated herdr dev session
scripts/herdr-dev pane list        # herdr CLI pointed at that same session
scripts/relaunch-main              # rebuild and run against your real herdr session
scripts/bundle.sh                  # unsigned release bundle in target/bundle/
```

`scripts/dev-run` sets `ZIG` and points `HERDR_BIN`, `HERDR_SESSION` and `HERDR_SOCKET_PATH` at a dev session under `~/.local/share/grasslands-herdr`. Debug builds refuse to connect anywhere else unless `RT_ALLOW_LIVE_HERDR=1`, so they can't touch your real agents by accident. Ghostty settings live in `~/.config/grasslands/ghostty.conf`. [docs/development.md](docs/development.md) covers tests, script check modes, release builds, keys and known gaps.

The repo is a pnpm workspace around a Cargo workspace. Rust crates live in `crates/`, the website in `website/`, and design decisions in [docs/decisions/](docs/decisions/).

## License

MIT. See [LICENSE](LICENSE).
