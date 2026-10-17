# 0011 Sidebar dock

Date: 2026-10-16. Status: accepted.

The sidebar is a GPUI Kit `DockArea` (`gpui_kit::component::dock`) wearing the stock `DockSkin`, with three panels: spaces (`WorkspaceList`), agents (`AgentList`, split out of the workspace list) and files (`FileTree`). The human asked for the agents section as its own panel, drag-and-drop rearranging, and the Kit's built-in look and theme colors so themes switch cleanly.

- **Scope: the sidebar only.** The dock's center is the sidebar column; the terminal area and editor stay outside it. The Ghostty terminals draw in the overlay window (0008), which sits above GPUI, so drags and drop indicators over them would never reach GPUI. Moving the main area into a dock waits on the single-window decision (0010).
- **Built-in chrome.** The skin draws each panel's title bar. Panel controls are `toolbar_buttons` (spaces: New Workspace; agents: the Grouped/Priority toggle; files: new file, new folder, refresh) and the skin's ⋯ menu (spaces: New Workspace, New Worktree…, Command Palette…). The old herdr-style "spaces" label row, the "new"/"menu" footer and the files header are gone. Count badges are `Tag::danger()` / `Tag::info()`. Status colors use the theme's semantic `danger`/`warning`/`info`/`success` instead of `red`/`yellow`/`cyan`/`green`.
- **Rows unchanged.** Workspace and agent rows keep herdr's monospace layout and rules (0009).
- **Panels can't be closed or zoomed.** `closable` is false so a panel can't be lost, and zoom is withheld in a sidebar this narrow.
- **Persistence.** On every `DockEvent::LayoutChanged`, debounced 500 ms, the layout is written to `~/Library/Application Support/rusted-tools/sidebar-layout[-<HERDR_SESSION>].json`, so a dev session never overwrites the real one. On launch it loads if the version matches and all three panels come back; otherwise the default (spaces / agents / files stacked) is used. Saved panels are rebuilt through `register_panel` closures that hand back the live entities.
- **Script hooks.** `RT_SIDEBAR_SCRIPT` still drives both panels; `toggle-agents` and `menu` are gone with the controls they clicked.
