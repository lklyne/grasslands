# 0007 — Debug builds only talk to dev herdr sessions

Date: 2026-10-15. Status: accepted.

Agent panes inherit `HERDR_SOCKET_PATH` for the user's live herdr. A bare
`cargo run -p app` or `cargo test` (without `scripts/dev-run`) would connect
to it, and the app sends commands at startup (`plugin.action.list`) and can
close panes.

- `herdr_client::Endpoint::from_env` returns `Result`. In debug and test
  builds it refuses any socket that is not lexically under
  `$HOME/.local/share/grasslands-herdr` (relative paths and `..` refused),
  with `Error::Refused` naming the socket and the fix.
  `Endpoint::start_server` re-checks, so no herdr server is started outside
  that root either. `RT_ALLOW_LIVE_HERDR=1` overrides. Release builds are
  unguarded.
- The guard sits on resolution from the environment, not on `connect`:
  explicit `Endpoint` literals (the fake-server test, the inert test
  configs) are deliberate code, not inherited state.
- The app passes the `Result` to `AppState::new`. A refusal leaves it
  `ConnectionStatus::Disconnected { reason }` with no client and no sync:
  every herdr call fails with `Refused`. There is no fallback endpoint.
- Under `cfg(test)`, `AppState::new` panics on a config with
  `auto_start: true` (what `from_env` produces), so no app test can build an
  `AppState` (or a `Chrome`, which needs one) from the environment. Tests use
  `state::offline_config()` or an equivalent inert config.
