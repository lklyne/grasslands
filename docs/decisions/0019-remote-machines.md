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
- Machines are a client feature. The local server's snapshot knows nothing
  about them, so grasslands connects to each machine's server itself.
- Each machine runs its own herdr server. IDs (workspaces, tabs, panes,
  terminals, agent names) are scoped to one server and collide across machines.

## Transport

One `ssh` control master per machine, which forwards the remote herdr socket
to a local socket file. `Endpoint` and `SessionSync` work unchanged against it:
each request is a new channel on the open connection (about 0.2s round trip
measured locally), and the event subscription is one long-lived channel.

- Connect with `ssh -M -T -o BatchMode=yes -o ControlPath=<ctl>
  -o ServerAliveInterval=15 -o ConnectTimeout=10 [-p port] [user@]host
  'cat >/dev/null'`, its stdin a pipe the app holds. When the app exits, even
  without running destructors (GPUI's quit doesn't), the pipe closes, the
  remote `cat` ends and the master with it. BatchMode means grasslands never
  answers prompts; a sign-in problem becomes a status, fixed in a terminal
  (`herdr machine reconnect <id>`).
- Find the remote herdr with one command through the master: `command -v
  herdr`, then herdr's usual install paths (`~/.local/bin`, Homebrew, mise
  shims, Nix profile). Then `<remote herdr> [--session S] status --json` for
  `server.socket`, `server.running`, `server.compatible` and the version.
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
  interactive setup.
- Terminals in a remote pane run `ssh -t -S <ctl> <target> '<remote herdr>
  [--session S] terminal attach <id> --takeover'`, reusing the master.
- Debug builds connect only to machines whose label is in
  `GRASSLANDS_DEV_MACHINES` (comma-separated); others show as disabled. They
  also only rename, disable or remove those machines, and only add one with
  `RT_ALLOW_MACHINE_EDITS=1`: the saved-machine list is shared with the
  user's live herdr, and a developer's real machines run real agents.

## App model

- `AppState` holds one connection per machine: Local plus each enabled saved
  machine, each with its own `SessionSync`, snapshot and status.
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
