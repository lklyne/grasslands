# grasslands

A native Rust desktop GUI for herdr: GPUI Kit for the UI, gpui-libghostty for terminals, herdr 0.9.5 as the headless engine. Read `docs/spec.md` in full before starting; it is the product spec. The herdr socket API schema is `docs/herdr-api-0.9.5.schema.json`; the docs are at herdr.dev/docs/socket-api and herdr.dev/docs/cli-reference.

Load the `rust-best-practices` skill before writing Rust.

## Never touch the user's live herdr

The user runs many real agents on the default herdr session (`~/.config/herdr/herdr.sock`). Your shell inherits `HERDR_SOCKET_PATH` pointing at it. The app takes over terminals and closes panes, so pointing it at the live session would wreck real work.

- Run every herdr command as `scripts/herdr-dev <args>` (herdr 0.9.5, isolated per-checkout `dev-<worktree dir>` session, own config dir). Never run bare `herdr` for testing the app.
- Run the app, its tests and examples through `scripts/dev-run <command>`, e.g. `scripts/dev-run cargo run -p app`. It sets `HERDR_BIN`, `HERDR_SESSION`, `HERDR_SOCKET_PATH` and `ZIG` for the dev session.
- The app finds herdr from `HERDR_BIN` (default `herdr`), `HERDR_SESSION` (default none) and `HERDR_SOCKET_PATH` (default herdr's own default). Code must honor these so dev runs stay isolated.
- Never stop, restart or upgrade the default herdr server. Creating and closing workspaces/panes in your own `dev-*` session is fine. Each worktree gets its own session (see scripts/dev-env.sh), so stopping it only affects you.

## Toolchain

- Rust 1.99 (edition 2024). Zig 0.16 at `~/.local/zig-0.16/zig` (`ZIG` is set by `scripts/dev-env.sh`; Homebrew's 0.17 won't build libghostty). Xcode 27.
- Latest versions as of 2026-10-15: gpui-kit 0.7.3 (pins gpui-pre =0.3.9), gpui-libghostty 0.3.1, notify 8.2.0, git2 0.21.0, ignore 0.4.33, serde_json 1.0.154, anyhow 1.0.104, thiserror 2.0.21. Check crates.io before adding anything else and use the latest stable.
- Reference GPUI Kit code: `github.com/lklyne/specular` branch `rust-native`, `native/bakeoff/gpui-kit-hybrid` and `native/bakeoff/gpui-proof` (a native child NSView composed with GPUI Kit, the same layering problem the Ghostty views have). Read it with `gh api` or a sparse clone into a temp dir.

## Layout

```
Cargo.toml            workspace (members = crates/*)
crates/herdr-client   socket protocol, connection, server start, snapshot cache, events, reconnect, commands. No GPUI.
crates/app            the GPUI Kit application (binary)
spikes/               standalone throwaway experiments, not workspace members
docs/                 spec, herdr schema, decisions (docs/decisions/NNNN-title.md for anything non-obvious)
```

## Working as one of several agents

- You work in your own git worktree on your own branch. Stay inside the files your task owns; if you need a change in someone else's area, make the smallest change possible and say so in your report.
- The orchestrator merges branches into `main`. When told main moved, `git rebase main` (or merge) and fix conflicts in your area.
- Commit in small, working steps with terse semantic messages. `cargo build` and `cargo clippy` must pass on every commit; `cargo test` for crates with tests.
- Never send synthetic keyboard or mouse input through the OS (osascript/System Events, cliclick, CGEvent posting). It goes to whatever app has focus, which may be the human's terminal or another agent. Drive the app through its own script/test hooks (e.g. a `--script` mode or GPUI test harness) instead.
- Never kill processes by broad patterns (`pkill -f target/debug/app` matches other agents' apps). Kill only PIDs you started.
- Prove it works. Run the app through `scripts/dev-run`, drive the dev session with `scripts/herdr-dev` (split panes, create tabs, start a shell) and check the app reacts. Screenshot with `screencapture -x <file>` (or `-l <windowid>`) and look at the image. If screen recording is denied, say so and verify by logs/state instead.
- Don't stop to ask the human; they're away. Make the call, record non-obvious decisions in `docs/decisions/`, and keep going. Stop only when your task is done or truly blocked.
- When you finish, end with a short report: what works (and how you verified it), what doesn't, what you changed outside your area, and your branch name.
