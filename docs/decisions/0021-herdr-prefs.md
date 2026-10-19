# 0021: herdr's preferences in the app

herdr is the source of truth for its own settings. The app reads herdr's
`config.toml`, follows the keys below, and edits them from Settings → herdr.
herdr has no socket method that reads or writes config, so the file is the
interface. Remote machines keep their own config on the remote; only
Local's is in scope.

## Where the file is

`herdr_client::config::config_path`: `HERDR_CONFIG_PATH`, else
`$XDG_CONFIG_HOME/herdr/config.toml`, else `~/.config/herdr/config.toml`.
Checked against herdr 0.9.5 by putting a deliberately invalid value in a
file at each location and running `herdr config check` under each
environment. herdr reads `XDG_CONFIG_HOME` on macOS too. One edge differs:
herdr treats a set-but-empty variable as set, while the app treats it as
unset (as `Endpoint::from_env` already does for the socket).

## Reading it

- herdr rejects the whole file when any value has the wrong type or an
  unknown variant ("…; using defaults") and ignores unknown keys with a
  warning. `config::parse` does the same for the keys the app knows:
  a bad value for any of them gives every default, not only that key's.
  Values this parser doesn't know about can also make herdr reject the
  file, so the app also runs `herdr config check` (with `HERDR_CONFIG_PATH`
  set to the same file) after every reread. When that says "using
  defaults", the app falls back to the defaults as well. Nothing crashes:
  a missing file is herdr's defaults, and an unreadable file or bad TOML is
  the defaults plus a problem shown on the settings page.
- `[ui.sound.agents]` only takes the 20 agent ids herdr 0.9.5 knows
  (`config::SOUND_AGENTS`). `copilot`, `opencode`, `omp` and `mastracode`
  are agents herdr detects but can't mute ("unknown config key").
- A `notify` watcher on the file's folder (editors rename over the file;
  the file may not exist yet; a symlink's target folder is watched too)
  rereads it 150 ms after a burst. Activating the main window or the
  Settings window rereads it too.
- An outside edit only changes what the app does. herdr's server applies
  it on its own reload (`herdr server reload-config`, its menu, or "Reload
  in herdr" on the settings page). The app doesn't reload herdr for
  edits it didn't make.

## Writing it

- `config::write` edits with `toml_edit`. Comments, blank lines, key order
  and a replaced value's trailing comment are all kept. Missing tables are
  added (`[ui]` stays implicit when only `[ui.toast]` needs it), and inline
  tables are written as inline. The new text goes to a temp file next to
  the target and is renamed over it. A symlinked config is edited at its
  target, so a dotfiles link survives. A file that isn't TOML is left
  alone and the error is shown.
- After each write the app calls `server.reload_config` on Local and shows
  its diagnostics when the status isn't `applied`.
- Writes happen on the main thread, one at a time, so quick edits never
  read over each other. Tests never write. Script runs write only in
  `RT_SCRIPT=herdr-prefs`. Debug builds write only under the dev root
  (`~/.local/share/grasslands-herdr`) unless `RT_ALLOW_LIVE_HERDR=1`.

## What the app follows

| Key | In the app |
| --- | --- |
| `[ui.sound] enabled`, `[ui.sound.agents]`, `HERDR_DISABLE_SOUND` | Agent-state sounds (`Attention` when blocked, `Ready` when done). `enabled = false` silences every agent. Otherwise the per-agent value wins, and `default` means on, except for droid. An agent of unknown kind follows `enabled`. `HERDR_DISABLE_SOUND` (set and non-empty) silences them all. |
| background workspaces only | No agent sound for an agent in the workspace you're looking at: the main window is active, its machine is selected and that workspace is focused. herdr's docs say "suppresses popups for the active tab", so a toast is skipped only for the focused tab. |
| `[ui.toast] delivery` | `off`: no agent toast. `herdr`: a grasslands toast. `terminal`: a grasslands toast, plus an OS notification while the window is in the background (grasslands stands in for herdr's outer terminal). `system`: a grasslands toast while the window is active (macOS shows no banners for the frontmost app), and only the OS notification while it's in the background. herdr's default is `off`, so a fresh install gets sounds, badges and sidebar status but no agent toasts until delivery is set. |
| `[ui.toast] delay_seconds` | The sound and the toast wait this long and fire only if the pane is still in the same state, as herdr does. A newer change for the pane cancels a pending alert. herdr says the delay applies to "agent notifications", which here include the sound. |
| `[ui.sound] path`, `done_path`, `request_path` | Played through `NSSound` in place of the cue (relative paths resolve against the config's folder). A missing or unplayable file falls back to the cue, as herdr falls back to its default sound. Shown read-only on the settings page. |
| `ui.agent_panel_sort` | The agents panel's order (`spaces`/`workspaces` → grouped, `priority`). The panel's sort toggle writes the key, so herdr's own panel follows. |
| `ui.status_indicators` | Exposed as `herdr_prefs::get(cx).status_indicators` for the status module; the app doesn't render it yet. |
| `ui.confirm_close` | On (the default): every workspace or group close asks, with the agent summary when agents would stop. Off: workspace closes run straight away, as in herdr. Pane and tab closes still ask when they would stop an agent; that's the app's own safety net, and herdr has no key for it. |
| `ui.prompt_new_tab_name` | On (the default): cmd-t, the tab menus and the palette ask for a name in the shared prompt (`prompt::ask_name`). An empty name lets herdr pick one. |
| `ui.prompt_new_workspace_name` | On: New Workspace asks for a name the same way. Off (the default): herdr names it. |

## grasslands' sounds and herdr's

Settings → Sounds stays grasslands' own (`settings.json`, decision 0017).
Its switch is the master for every sound grasslands makes, and its theme
and volume shape the agent cues as well. herdr's keys decide *which agent
events* make a sound. An agent sound plays only when both allow it. Every
interface cue is unchanged and answers only to grasslands' switch. The
settings controls added here play cues by the cues guide: switches play
`Toggle`, dropdown and number picks `Select`, buttons `Tap`.

## Script runs

Every `RT_SCRIPT` scenario except `herdr-prefs` overrides
`prompt_new_tab_name` and `prompt_new_workspace_name` to off in memory
(`herdr_prefs::override_for_script`, never written), so cmd-t doesn't stop
at a dialog. `remote` overrides `confirm_close` the same way before it
closes a workspace. `RT_SCRIPT=herdr-prefs` writes prefs the way the page
does, checks the file and its comments, runs `herdr config check`, checks
the reload, then edits the file from outside, and finally restores it.
Point `HERDR_CONFIG_PATH` at a scratch copy under the dev root for it
(module docs).

## Known gaps

- herdr's per-agent `on` versus `enabled = false`: the docs don't say which
  wins. The app treats `enabled` as the master switch.
- Whether herdr's server itself posts `system` notifications for agent
  state changes when no TUI client is attached is unverified. If it does,
  `system` delivery could notify twice.
- `HERDR_DISABLE_SOUND` is read from the app's own environment. A herdr
  server started with it set doesn't pass it on.
