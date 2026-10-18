# grasslands

A native Rust desktop GUI for herdr: GPUI Kit for the UI, gpui-libghostty for terminals, herdr 0.9.5 as the headless engine. The product spec is `docs/spec.md`. The herdr socket API schema is `docs/herdr-api-0.9.5.schema.json`; the docs are at herdr.dev/docs/socket-api and herdr.dev/docs/cli-reference. We aim to wrap some helper ui around herdr, but keep herdr the fully-functional source of truth. 

Load the `rust-best-practices` skill before writing Rust.

## Important codebase conventions
- For all UI related tasks, look at GPUI https://gpui.rs/ and GPUI-KIT https://gpui-kit.com/ use base primitives unless explicitly asked to build custom
- Use built in theme colors, type ramp, and component conventions unless explicitly requested
- Follow conventions to add sounds to new features when relevant

## herdr

- The live herdr session is the default. Running `herdr` and the app against it is fine; when asked to launch or run the app, use `scripts/relaunch-main` (rebuilds and restarts the main instance).
- The user runs real agents there. Don't do anything destructive: no closing their workspaces, tabs or panes, no stopping, restarting or upgrading the herdr server unless you're asked to
- For testing, use the demo session: `scripts/herdr-dev <args>` for herdr commands and `scripts/dev-run <command>` for the app (e.g. `scripts/dev-run cargo run -p app`). Create and close whatever you like there.
- Code must honor `HERDR_BIN`, `HERDR_SESSION` and `HERDR_SOCKET_PATH`.

## Testing

Only run tests and live verification when asked to open a PR, or when doing large work async. Otherwise make the change, check that `cargo build` and `cargo clippy` pass, and stop.

- Never send synthetic keyboard or mouse input through the OS (osascript, cliclick, CGEvent); drive the app through its script/test hooks.
- Never kill processes by broad patterns; kill only PIDs you started.

## Toolchain

- Rust 1.99 (edition 2024). Zig 0.16 at `~/.local/zig-0.16/zig` (Homebrew's 0.17 won't build libghostty). Bare `cargo build` fails without `ZIG` set, so build through `scripts/dev-run`, which sets it. Xcode 27.
- Latest versions as of 2026-10-15: gpui-kit 0.7.3 (pins gpui-pre =0.3.9), gpui-libghostty 0.3.1, notify 8.2.0, git2 0.21.0, ignore 0.4.33, serde_json 1.0.154, anyhow 1.0.104, thiserror 2.0.21. Check crates.io before adding anything else and use the latest stable.
- Reference GPUI Kit code: `github.com/lklyne/specular` branch `rust-native`, `native/bakeoff/gpui-kit-hybrid` and `native/bakeoff/gpui-proof`.

## Layout

```
Cargo.toml            workspace (members = crates/*)
crates/herdr-client   socket protocol, connection, server start, snapshot cache, events, reconnect, commands. No GPUI.
crates/app            the GPUI Kit application (binary)
spikes/               standalone throwaway experiments, not workspace members
docs/                 spec, herdr schema, decisions (docs/decisions/NNNN-title.md for anything non-obvious)
```

Commit in small, working steps with terse semantic messages.
