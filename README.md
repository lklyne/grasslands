# grasslands

A native macOS GUI for [herdr](https://herdr.dev) 0.9.5. herdr stays the source of truth; this is a window on top of it. Alpha.

Spec: [docs/spec.md](docs/spec.md). Decisions: [docs/decisions/](docs/decisions/).

<img width="1387" height="930" alt="image" src="https://github.com/user-attachments/assets/e26fd95b-1585-45ff-81fc-4eec5c482787" />

## Requires

Rust 1.99, Xcode 27, herdr 0.9.5, Zig 0.16 at `~/.local/zig-0.16/zig` (0.17 won't build libghostty; override with `ZIG`).

## Run

```sh
scripts/dev-run cargo run -p app   # isolated dev session
scripts/herdr-dev <args>           # herdr CLI for that session
scripts/relaunch-main              # rebuild and run against your real herdr session
scripts/bundle.sh                  # build target/bundle/grasslands.app
```

Debug builds only connect to the dev session unless `RT_ALLOW_LIVE_HERDR=1`. The app takes over each terminal (`herdr terminal attach --takeover`), so don't drive the same panes from a herdr TUI.

Ghostty settings: `~/.config/grasslands/ghostty.conf`.
