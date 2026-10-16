# 0009 — Logging

Date: 2026-10-15. Status: accepted.

- `tracing` + `tracing-subscriber` (`logging.rs`, installed first thing in
  `main`). Two outputs: stderr, and a daily file
  `~/Library/Logs/rusted-tools/rusted-tools.YYYY-MM-DD.log` (7 kept, no ANSI)
  via `tracing-appender`'s non-blocking writer. Lines carry file and line.
- `RUST_LOG` filters both; default `warn,app=info,herdr_client=info`.
- `log` records are bridged in (tracing-subscriber's `tracing-log`), so
  herdr-client, GPUI and `detach_and_log_err` land in the same place without
  changing their code. Before this, no logger was installed and all of those
  went nowhere.
- Panics are logged before the default hook runs.
- Failure points that used to be silent:
  - herdr commands: `AppState::call` logs every failed (or skipped, when
    disconnected) call with the caller's location (`#[track_caller]` on the
    typed command methods), whatever the caller then does with the result.
  - terminals: spawn errors, and each attach process that exits (once per
    exit, `Terminals::report_detached`).
  - watchers: notify error events in the editor and file tree watchers.
  - the dev-socket guard refusal (0007) is logged at error.
- Not covered: Ghostty's own Zig logging writes straight to stderr
  (`info(io_exec): ...`) and is not in the file. Script modes (`RT_SCRIPT`,
  `RT_CHROME_SCRIPT`) keep printing their `ok`/`FAIL` lines to stderr.
