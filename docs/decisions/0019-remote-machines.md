# 0019: remote machines

grasslands shows herdr's saved SSH machines next to local work, the way
herdr's own sidebar does: a "Local" group, then one collapsible group per
machine with a status dot. Mockup A, with C's add flow
(`target/mockups/remote/`).

## What herdr gives us

- Machines are saved by `herdr machine add <target> --label <label>` and
  listed by `herdr machine list --json`:
  `[{id, label, target: "ssh://user@host:port", session: "default", enabled, selected}]`.
  grasslands keeps no machine list of its own; herdr's is the source of truth.
  herdr keeps it in `$XDG_STATE_HOME/herdr/client/endpoints.json` (default
  `~/.local/state`), which every herdr client shares, the dev sessions
  included.
- `selected` is the machine a herdr TUI client last showed
  (`endpoint-selection.json`), saved for that TUI's next start. API calls go
  to whichever socket you talk to, so it means nothing to grasslands, which
  keeps its own selection and ignores herdr's.
- `herdr machine status [<id>] --json` checks a machine afresh, without
  prompts: `[{id, label, status, error}]`. Seen with herdr 0.9.5:

  | status | when | `error`, e.g. |
  |---|---|---|
  | `reachable` | SSH works and a matching herdr runs as a detached daemon | `null` |
  | `auth required` | ssh refused the key | `remote platform detection failed: u@h: Permission denied (publickey).` |
  | `error` | connection refused, unknown host | `remote platform detection failed: ssh: connect to host … Connection refused` |
  | `error` | unknown host key | `… No ED25519 host key is known for [localhost]:2222 … Host key verification failed.` |
  | `error` | herdr missing or not matching | ``matching Herdr is not ready on ssh://…; run `herdr --remote ssh://…` interactively to install or update it`` |
  | `error` | server stopped, or not a detached daemon | ``remote Herdr server is stopped or incompatible; run `herdr --remote ssh://… --session S` `` |
  | `disabled` | `herdr machine disable` | `null` |

  It exits 1 when any machine isn't reachable and 2 for an unknown id (with
  plain text), and wants 32-hex-digit ids. "Detached daemon" is the server's
  `detached_server_daemon` capability: a server started by hand from an SSH
  shell lacks it, and herdr then calls the machine stopped, though its
  `herdr status` says running and compatible.
- Machines are a client feature. The local server's snapshot knows nothing
  about them, so grasslands connects to each machine's server itself.
- Each machine runs its own herdr server. IDs (workspaces, tabs, panes,
  terminals, agent names) are scoped to one server and collide across machines.

## Transport

One `ssh` control master per machine, which forwards the remote herdr socket
to a local socket file. `Endpoint` and `SessionSync` work unchanged against it:
each request is a new channel on the open connection (about 0.2s round trip
measured locally), and the event subscription is one long-lived channel.

- Each connection starts with herdr's own check, `herdr machine status <id>
  --json` (table above), and its verdict is the machine's status, worded as
  herdr words it: `auth required` and an unknown host key are sign-in (`herdr
  machine reconnect` handles both), an `error` whose fix is `herdr --remote`
  is setup, any other `error` is offline. Only `reachable` goes on to the
  master. herdr's check is stricter than the app's old one (it wants a
  matching version and a detached daemon), so a machine herdr's TUI won't
  use doesn't connect here either. When the check can't run or doesn't know
  the machine, the connection goes ahead and its own ssh failure, offline,
  says why. herdr's check opens its own SSH connection, once per connection
  attempt (about 0.5 s to the local docker box).
- Connect with `ssh -M -T -o BatchMode=yes -o ControlPath=<ctl>
  -o ServerAliveInterval=15 -o ConnectTimeout=10 [-p port] [user@]host
  'printenv SSH_AUTH_SOCK; echo; cat >/dev/null'`, its stdin a pipe the app
  holds. When the app exits, even without running destructors (GPUI's quit
  doesn't), the pipe closes, the remote `cat` ends and the master with it.
  BatchMode means grasslands never answers prompts; a sign-in problem
  becomes a status, fixed in a terminal (`herdr machine reconnect <id>`).
- herdr's check doesn't say where herdr is or which socket its server uses,
  and the socket API is only reachable through a forward, so the link still
  finds them itself: one command through the master, `command -v herdr`,
  then herdr's usual install paths (`~/.local/bin`, Homebrew, mise shims, Nix
  profile), then `<remote herdr> [--session S] status --json` for
  `server.socket`, `server.running` and `server.capabilities`. Version and
  compatibility are herdr's check's business now. (herdr's own clients don't
  forward the socket: they run stdio bridges on the machine over SSH, `herdr
  remote-client-bridge` for the TUI, and `herdr remote-api-bridge`, whose
  `--check` answers `herdr-api-bridge-v1`. Neither is documented. Using the
  API bridge would mean a local socket proxy running one ssh channel per
  request, since herdr answers one request per connection; the forward does
  the same inside ssh.)
- Forward with `ssh -S <ctl> -O forward -L <sock>:<server.socket>`.
- Files are per process and per link: `<dir>/<pid>-<n>-<id8>.{ctl,sock}`, so
  two running instances (or an old link still tearing down) never delete each
  other's sockets. At start, files of pids no longer running are cleaned up.
- Socket paths must fit `sun_path` (104 bytes on macOS), so they live in a
  short directory: `$HOME/.local/share/grasslands-herdr/m` in debug builds
  (inside the dev root, so `check_dev_socket` passes), and
  `$HOME/Library/Caches/grasslands/m` in release builds. The directory is
  0700: anyone who can open a socket controls that machine's herdr.
- The local socket path is fixed for a link's life. When the connection drops, a
  supervisor reopens it with backoff at the same path, and `SessionSync`
  reconnects on its own.
- grasslands never installs, starts, restarts or upgrades a remote server.
  A missing or incompatible remote herdr is a status, with "Set Up…" running
  `herdr --remote <target> [--session S]` in a terminal, which is herdr's own
  interactive setup (and the command herdr's check names).
- Terminals in a remote pane run `ssh -t -S <ctl> <target> '<remote herdr>
  [--session S] terminal attach <id>'`, reusing the master. See "Terminal
  ownership" for `--takeover`.

## SSH agent forwarding

herdr lends an SSH agent to a server's panes through
`server.ssh_agent.register {socket_path}` (capability
`ssh_agent_registration`): every pane's `SSH_AUTH_SOCK` is a fixed path next
to the server's socket (`…/herdr.sock.agent`), which points at the
registered agent socket for as long as the registering API connection stays
open. `herdr --remote` does this from its remote client, with the agent that
its SSH session forwards.

- **Opt-in is the user's ssh_config `ForwardAgent`**, as for `herdr --remote`
  and plain ssh: herdr has no setting of its own for it (none in
  `herdr --default-config`, and the binary never sets `ForwardAgent`), so
  grasslands doesn't add one. `Host box` + `ForwardAgent yes` lends the
  local agent to that machine's panes; without it nothing is forwarded or
  registered. Forwarding an agent lets the machine's root use your keys
  while connected, which is why it stays the user's per-host choice.
- The master's remote command prints its session's `SSH_AUTH_SOCK` first
  (empty without forwarding). When it's a path and the server has the
  capability, the link opens one more API connection on the forwarded
  socket, registers that path, and keeps the connection open until the link
  ends. herdr sends nothing on it, so a read that hits EOF means the
  registration ended (server restart, tunnel gone); the link then registers
  again, retrying every 5 s. A new master (reconnect) brings a new agent
  path and a new registration.
- Debug builds: `RT_FORWARD_AGENT=1` adds `-o ForwardAgent=yes`, so tests
  can forward a throwaway agent without editing `~/.ssh/config`.
- Checked against dockerbox with a throwaway key in a temp agent: a new
  remote pane's `ssh-add -l` lists the key; after the registering connection
  closes it says "Error connecting to agent"; after a server restart the
  link registers again (`machine_link` tests
  `link_should_lend_a_forwarded_agent_to_remote_panes` and
  `link_should_register_the_agent_again_after_a_server_restart`).

## Terminal ownership

Probed with herdr 0.9.5 (dev session, PTY clients):

- One direct-attach client owns a terminal. A second `terminal attach`
  without `--takeover` fails at once (exit 1): "terminal attach failed:
  terminal … already has an attached client; retry with --takeover". With
  `--takeover` it wins, and the old client exits 1 with "terminal attach
  taken over".
- **A herdr TUI client doesn't count.** It draws panes through its own
  client protocol, not `terminal attach`, so grasslands and a TUI on the same
  session never take terminals from each other, with or without
  `--takeover`; typing in either reaches the pane, whichever attached first. Only other direct attaches compete: another grasslands
  instance on the same session, `herdr agent attach`, a manual `herdr
  terminal attach`.
- When an attach client's PTY closes (the app quits), herdr frees the
  terminal within a second (0.8 s measured), so a relaunched app attaches
  without `--takeover`.
- `terminal session observe` is read-only and prints JSON frames
  (`{"type":"terminal.frame","bytes":"<base64>",…}`), not a terminal stream,
  so a Ghostty surface can't run it. `client_shell.surface.set` (surface
  interest: only the selected machine streams) answers
  `connection_local_only` to an API client; it's for herdr's own client
  shells.

So grasslands takes the herdr way: attaching on its own (a new pane, every
pane again after a reconnect), it runs plain `terminal attach`, and never
detaches another client. A terminal someone else has stays theirs, and its
pane shows "Detached: another client has this terminal." with Reattach.
Reattach is the person's choice, so it alone passes `--takeover`. Local and
remote panes follow the same rule.

The cost: after a connection drops without the machine noticing (a network
partition rather than a restart), the old remote attach can outlive the
drop until sshd gives up on it, and the reconnect's plain attach is refused.
Those panes then show Detached until Reattach. A machine restart or a
server restart has no such leftovers.

Terminals of unselected machines stay attached, as before. Without surface
interest the only way to stop their streams would be to end the attaches,
and re-attaching on every switch would drop Ghostty's own scrollback and
add an SSH channel per pane to each switch.
- Debug builds connect only to machines whose label is in
  `GRASSLANDS_DEV_MACHINES` (comma-separated); others show as disabled. They
  also only rename, disable or remove those machines, and only add one with
  `RT_ALLOW_MACHINE_EDITS=1`: the saved-machine list is shared with the
  user's live herdr, and a developer's real machines run real agents.

## App model

- `AppState` holds one connection per machine: Local plus each enabled saved
  machine, each with its own `SessionSync`, snapshot and status.
- **One status per machine.** herdr-client keeps two state machines: the
  link's `MachineStatus` (saved machines) and the sync's `ConnectionStatus`
  (every machine); each drives its own retries, so they stay apart there.
  The app asks only `state::Status`, derived in one place
  (`Machine::refresh_status`): a saved machine's from its link, which
  already probes herdr there; Local's, which has no link, from the setup
  check and then its sync. It answers "usable?" (`is_usable`), "why not?"
  in herdr's words (`reason`), and whether it's retrying (`Offline {
  retrying }`). The files panel, Changes, the terminal area, `fs_for`, the
  badge's live machines and the sidebar (`MachineState::of`) all read it.
  Sound cues still follow the raw transitions: the link's for saved
  machines, the sync's for Local.
- One machine is **selected**, as in herdr: the one whose workspace is
  active. `snapshot`, `call` and the terminal area all refer to the selected
  machine, so most existing code is unchanged. Activating a workspace on
  another machine selects that machine first.
- Terminals and anything else keyed by herdr IDs are keyed by machine plus ID.
- The sidebar groups workspaces by machine. Agents stay one list, each row
  with its machine's label once there is more than one machine.
- Local-disk features (file tree, editor, Changes, worktree tracking, file
  drops) follow the active workspace only on Local. For a remote workspace,
  the file panel says "Files on <label> aren't available yet".
- Offline machines keep their last snapshot, dimmed, with input disabled; the
  terminal area shows "<label> is offline" with Reconnect.

## UI

- With no saved machines, the sidebar looks exactly as before: no group headers.
- Group header: chevron, status dot (connected, connecting, needs attention,
  offline, disabled), label, and a ··· menu: Reconnect, Set Up… / Sign In…
  when needed, Rename (`herdr machine rename --label <l> <id>`), Disable, Remove
  (confirmation; `herdr machine remove`).
- The Workspaces "+" menu: New Workspace, New Workspace on › (machines), Add
  Machine…. Add Machine asks for an SSH host, suggesting `Host` entries from
  `~/.ssh/config`, then runs `herdr machine add <host>` in a new terminal tab
  on Local, so its prompts are answered there. The machine list refreshes when
  that command exits, and on a slow timer.

## What herdr does, what the app still does

herdr's:
- The machine list, and every edit to it (`herdr machine add/rename/
  disable/enable/remove`).
- Reachability and its classification, with herdr's wording
  (`herdr machine status`). The app's own ssh-error and version checks are
  gone.
- Sign-in (`herdr machine reconnect`) and setup (`herdr --remote`), run in a
  terminal.
- Agent forwarding to remote panes (`server.ssh_agent.register`), opted into
  by ssh_config as herdr's own remote client is.
- Who owns a terminal: herdr's attach rule, with `--takeover` only on
  Reattach.

The app's, for want of a herdr API:
- The ssh master and socket forward: the socket API has no remote
  transport a non-herdr client can use, apart from `remote-api-bridge`
  (see Transport).
- Finding the remote herdr and its socket (`FIND_HERDR`, remote `status
  --json`): `machine status` reports neither.
- Polling `herdr machine list --json` (30 s, and after an add): no socket
  event announces machine changes ("changes apply automatically to open
  local Herdr clients" goes over herdr's client protocol).
- Its own selected machine (herdr's `selected` is a TUI's).

To ask herdr for (file upstream):
- `machine status --json` with the remote herdr binary and server socket,
  which would retire `FIND_HERDR` and the second status call.
- A machine event (`machine.added/changed/removed`), or machines in
  `session.snapshot`, to retire the poll.
- An API-client form of surface interest, so unselected machines' terminals
  could stop streaming without detaching.
- A way to tell, over the API, which client holds a terminal's attach, so
  the app could say "taken over by …" and not just Detached.
