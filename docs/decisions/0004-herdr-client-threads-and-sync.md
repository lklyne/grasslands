# 0001 herdr-client: plain threads, async-channel updates, re-read on every event

## Runtime

gpui-pre 0.3.9 brings its own executor (async-task, async-channel, futures); no tokio or smol runtime. So `herdr-client` uses no async runtime at all: blocking Unix sockets on plain threads. The app gets:

- `SessionSync::state() -> Arc<State>`: a lock-and-clone of the latest immutable cache.
- `SessionSync::updates() -> async_channel::Receiver<Update>`: `State(Arc<State>)`, `Event(Box<Event>)`, `Status(ConnectionStatus)`. GPUI can `recv().await` it inside `cx.spawn`; tests use `recv_blocking()`. async-channel 2.5 is already in GPUI's tree.
- `Client`: typed commands, one blocking round trip each. Call them from `cx.background_executor().spawn(...)`.

## Protocol facts found by probing 0.9.5 (not stated in the schema)

- **One request per connection.** A second request on the same connection gets no answer. Every `Client` call opens a fresh connection (cheap on a Unix socket). Only `events.subscribe` keeps its connection open.
- Pushed lines are `{"event": "<kind>", "data": {...}}` with no `id`. Lifecycle kinds are snake_case (`pane_created`); `pane.agent_status_changed` keeps its dotted name.
- **`pane.agent_status_changed` needs a `pane_id`.** There is no all-panes form, and `pane.updated`/`workspace.updated` do not fire on status changes. The subscription therefore lists one status entry per known pane and is reopened whenever the pane set changes. The new subscription is acknowledged (`subscription_started`) before the old one closes and before the next read, so nothing falls in the gap. If a listed pane has vanished herdr rejects the whole request; the engine re-reads and retries.
- **Event loss** is not an event. It is an error response on the subscription's request id with `code: "events_lost"`, after which herdr closes that connection (herdr.dev/docs/socket-api). The engine treats it like a dropped connection.
- `session.snapshot` has flat cell rects per tab, not trees. The split tree comes from `layout.export` per tab. `layout.set_split_ratio` paths are booleans from the root, `true` = second child.

## Sync loop

subscribe → `session.snapshot` + `layout.export` for each tab → build `State` → publish if it differs. Every event (after a 10 ms coalescing window) only marks the cache stale and triggers the same full read; events are never applied to the cache. If the trees disagree with the snapshot's pane membership (reads raced a change) or the pane set moved, it re-reads (up to 4 passes, then publishes and queues another pass).

On a dropped connection or `events_lost`: status `Reconnecting`, back off 100 ms doubling to 5 s, then subscribe and read again, replacing the cache. If nothing answers on the socket, `Endpoint::connect` starts `$HERDR_BIN [--session S] server` in its own process group (so it outlives the app) and waits up to 10 s for it.

## Endpoint resolution

`HERDR_BIN` (default `herdr`), `HERDR_SESSION`, `HERDR_SOCKET_PATH`. Without `HERDR_SOCKET_PATH`, herdr's rule: `$XDG_CONFIG_HOME/herdr[/sessions/<name>]/herdr.sock`, `XDG_CONFIG_HOME` defaulting to `~/.config`.

## Tests

`scripts/dev-run cargo test -p herdr-client`. Tests that touch a server skip unless `HERDR_SESSION` is a `dev-*` session and the socket is under `grasslands-herdr`, so a plain `cargo test` can never touch the live session. `tests/reconnect.rs` stops the current checkout's own `dev-*` session; each worktree has its own, so it only affects that checkout.
