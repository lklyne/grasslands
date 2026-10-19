# 0001 herdr-client: plain threads, async-channel updates, re-read on every event

## Runtime

gpui-pre 0.3.9 brings its own executor (async-task, async-channel, futures); no tokio or smol runtime. So `herdr-client` uses no async runtime at all: blocking Unix sockets on plain threads. The app gets:

- `SessionSync::updates() -> async_channel::Receiver<Update>`: `State(Arc<State>)` (the whole immutable cache, replaced), `Event { kind }` (an event arrived; its kind only, since nothing reacts to events except through the re-read) and `Status(ConnectionStatus)`. GPUI can `recv().await` it inside `cx.spawn`; tests drain it with `try_recv()`. async-channel 2.5 is already in GPUI's tree. The engine thread keeps its own last state; the app keeps its copy from these updates.
- `Client`: typed commands, one blocking round trip each. Call them from `cx.background_executor().spawn(...)`.

## Methods

Every herdr method the crate sends is a type in `protocol::method` implementing `Method` (`NAME`, `Params: Serialize`, `Output: DeserializeOwned`, plus the `RESULT` type herdr answers and the result `FIELD` holding the output). `Client::send::<M>(&params)` is the only way to call one; `Client`'s named methods (`pane_focus`, `tab_create`, ...) are thin wrappers over it. There is no stringly `call(name, json)` any more.

The table is hand-written rather than generated from `docs/herdr-api-0.9.5.schema.json` (272 KB, about 100 methods; generated `oneOf`/`const` types read badly and drag in methods we never call). Instead `protocol/tests.rs` checks it against the schema with no server, using the `jsonschema` crate (dev-dependency only): every `NAME` is a `method` const in `schemas.request.oneOf`; sample params for every method validate against that method's params `$defs` entry; and every `Output` decodes from two answers generated from the schema's `ResponseResult` variant for its `RESULT` (only the required fields, and all of them). A herdr upgrade that renames a method, a param or a result field fails there. The `RESULT` types of the methods that answer only success were confirmed by probing a 0.9.5 dev session (`pane.focus` answers `pane_info`, `tab.move` answers `tab_list`, `pane.close` answers `ok`, ...).

## Ids

herdr's ids are newtypes (`ids.rs`): `PaneId`, `TabId`, `WorkspaceId`, `TerminalId`, and `ProfileId` for a saved machine. Each is an `Arc<str>` that (de)serializes as a plain string (`#[serde(transparent)]`), borrows as `str` (so `BTreeMap<PaneId, _>` still looks up by `&str`) and derefs to `str` for reading. Anything that acts on an id (`Client`'s commands, `AppState`'s commands) takes the typed id, so a tab id can't go where a pane id belongs. Ids are only unique within one server; the app pairs them with their machine as `On<T> { machine, id }` wherever a key spans machines (terminal views, the right-click set, sidebar rows).

## Protocol facts found by probing 0.9.5 (not stated in the schema)

- **One request per connection.** A second request on the same connection gets no answer. Every `Client` call opens a fresh connection (cheap on a Unix socket). Only `events.subscribe` keeps its connection open.
- Pushed lines are `{"event": "<kind>", "data": {...}}` with no `id`. Lifecycle kinds are snake_case (`pane_created`); `pane.agent_status_changed` keeps its dotted name.
- **`pane.agent_status_changed` needs a `pane_id`.** There is no all-panes form, and `pane.updated`/`workspace.updated` do not fire on status changes. The subscription therefore lists one status entry per known pane and is reopened whenever the pane set changes. The new subscription is acknowledged (`subscription_started`) before the old one closes and before the next read, so nothing falls in the gap. If a listed pane has vanished herdr rejects the whole request; the engine re-reads and retries.
- **Event loss** is not an event. It is an error response on the subscription's request id with `code: "events_lost"`, after which herdr closes that connection (herdr.dev/docs/socket-api). The engine treats it like a dropped connection.
- `session.snapshot` has flat cell rects per tab, not trees. The split tree comes from `layout.export` per tab. `layout.set_split_ratio` paths are booleans from the root, `true` = second child.

## Sync loop

subscribe → `session.snapshot` + `layout.export` for each tab (up to 8 at once, since over ssh each is a ~0.2 s round trip) → build `State` → publish if it differs. Every event (after a 10 ms coalescing window) only marks the cache stale and triggers the same full read; events are never applied to the cache. If the trees disagree with the snapshot's pane membership (reads raced a change) or the pane set moved, it re-reads (up to 4 passes, then publishes and queues another pass).

On a dropped connection or `events_lost`: status `Reconnecting`, back off 100 ms doubling to 5 s, then subscribe and read again, replacing the cache. If nothing answers on the socket, `Endpoint::connect` starts `$HERDR_BIN [--session S] server` in its own process group (so it outlives the app) and waits up to 10 s for it.

## Endpoint resolution

`HERDR_BIN` (default `herdr`), `HERDR_SESSION`, `HERDR_SOCKET_PATH`. Without `HERDR_SOCKET_PATH`, herdr's rule: `$XDG_CONFIG_HOME/herdr[/sessions/<name>]/herdr.sock`, `XDG_CONFIG_HOME` defaulting to `~/.config`.

## Tests

`scripts/dev-run cargo test -p herdr-client`. Tests that touch a server skip unless `HERDR_SESSION` is a `dev-*` session and the socket is under `grasslands-herdr`, so a plain `cargo test` can never touch the live session. `tests/reconnect.rs` stops the current checkout's own `dev-*` session; each worktree has its own, so it only affects that checkout.
