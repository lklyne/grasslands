# 0023: one live test harness

There were three script systems: `RT_SCRIPT` scenarios hanging off
`TerminalArea` (they read its private fields and started from
`TerminalArea::new`), and two `;`-separated step languages,
`RT_CHROME_SCRIPT` and `RT_SIDEBAR_SCRIPT`, each with its own copy of
`wait`, `key`, `keys` and `log`. Now there is one harness and two tiers.

## Headless first

Flows that need neither a real herdr nor native views are `TestAppContext`
tests: an offline endpoint (`state::offline_config`), an injected snapshot
(`AppState::replace_snapshot`) and a kit window holding a `Chrome`, so
prompts and confirmations open as they do in the app. The step flows that
moved there: tab and workspace menus, Remove Worktree asking first, the
worktree picker filtering by query, the rename dialog starting from the
label, the palette query, closing a group asking first, and the sidebar's
New Worktree prompt, close and rename (`chrome/tests.rs`,
`sidebar/workspaces/tests.rs`). What they can't show (herdr acting on the
command, the overlay window, Ghostty frames) stays live.

## The live tier

- `crates/app/src/script/`, debug builds only, one file per scenario,
  started from `main` after the main window opens. Scenarios reach views
  through public getters: `workspace::get` and `Workspace::terminal_area`,
  `chrome::get`, `overlay::get`, `sidebar::{spaces, agents, files}`, and
  `terminal_area::hooks::Hooks`, a read-only view of the terminal area
  (snapshot, machine, terminals, what was drawn, divider overrides). Its
  fields stay private, so the area can be split without touching the
  scenarios.
- A run exits 1 when any check failed (it runs GPUI's quit handlers first;
  AppKit's terminate would always exit 0), 2 for an unknown scenario.
- One step language, `RT_STEPS` (`script/steps.rs`), with one parser for
  the shared steps and the pointer drag. The chrome's and the sidebar's
  verbs stay beside the code they call (`chrome/script.rs`,
  `sidebar/workspaces/script.rs`), since they use private methods. The old
  variable names are aliases with their old pacing and log prefixes, so the
  commands in other decisions still work.
- `sounds::played` keeps the last 256 cues, cleared when a scenario starts;
  checks take a `mark()` and read `since(mark)`.

## The drawn-bounds maps stay

GPUI's `debug_selector` would replace the `Rc<RefCell<HashMap<_, Bounds>>>`
maps that canvases fill so scripts know where to click, but only in
headless tests: gpui-pre 0.3.9 records selector bounds only with its
`test-support` feature (a dev-dependency here), and reads them back only
through `VisualTestContext::debug_bounds` (`rendered_frame` is
crate-private). A live window has no way to ask. So the maps stay for the
live tier. The terminal area's divider and pane slots also drive divider
drags and keyboard focus moves. The script-only ones (pane headers, tabs and
Reattach buttons in the terminal area, the sidebar's rows and machine
headers, the titlebar's tabs) could be compiled only in debug builds; that
is left for after the row-actions work in those files lands.
