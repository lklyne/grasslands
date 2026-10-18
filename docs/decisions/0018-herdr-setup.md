# 0018: herdr setup

grasslands uses the herdr the user's terminal runs (`HERDR_BIN`, else `herdr`
on `PATH`). It never bundles one, so the CLI and the app can't disagree on
version. A missing or old herdr is fixed in the app, on a setup screen in the
terminal area, before the app connects.

- **One check, at launch.** `herdr status --json` reports the binary's
  version and path and the running server's version and `compatible` flag in
  one call (`herdr_client::setup`, `Endpoint::check`). The sync thread starts
  only once the check passes. A herdr without `status --json` falls back to
  `herdr --version`.
- **Outcomes:** missing, can't run, binary older than 0.9.5, server older
  than 0.9.5 (or incompatible), ready. A snapshot from a server older than
  0.9.5 runs the check again, for a server started outside the app.
- **Fixes go through whatever installed herdr**, judged from where the binary
  really lives. A `/Cellar/` path means `brew upgrade herdr`, a `/mise/` path
  `mise upgrade herdr`, and anything else `herdr update`. Nix has no command;
  the screen says to update the profile or flake. herdr's docs say
  `herdr update` is only for its own installer's installs. Install is
  `brew install herdr` when brew is there, else herdr's
  `curl -fsSL https://herdr.dev/install.sh | sh`.
- **The app runs the fix on a click** and streams its output (stdout and
  stderr on one pipe) into the screen, then checks again. The command is
  shown with a copy button for anyone who'd rather run it themselves.
- **Restarting the server always asks first.** An update leaves the old
  server running, which herdr supports (endpoint generation 1). A server
  older than 0.9.5 has to go, though, and `herdr server stop` ends every pane.
  So Restart is a danger button behind a confirmation that counts the agents
  that will stop. After the stop, the sync thread's auto-start brings up a
  server from the new binary.
- **No `depends_on formula: "herdr"` in the cask.** It can't pin a version,
  and it would install a second, Homebrew herdr next to one from herdr's own
  installer or mise. The caveats point to the app's setup.
- **Failures show instead of spinning.** Until the first snapshot, a failed
  connection attempt shows its reason ("Can't reach herdr: … Retrying…")
  rather than "Connecting…". A server the app starts writes its output to
  `~/Library/Logs/grasslands/herdr-server.log`, a file and not a pipe, since
  the server outlives the app. Before this it went to `/dev/null`, so a first
  launch on a fresh machine could spin with no trace.
