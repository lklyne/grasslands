//! App chrome: command palette, context menus, toasts and confirmations.
//!
//! Ghostty's native views sit above everything GPUI draws in the main window,
//! so all of it (menus, palette, dialogs, toasts) draws in the main window's
//! overlay (`crate::overlay`). Callers go through `Chrome`'s methods, the
//! `OpenContextMenu` action, or [`get`] for views that only have the window.
//! See docs/decisions/0002-app-structure.md and 0006-chrome.md.

mod command;
mod confirm;
mod dock;
mod palette;
#[cfg(debug_assertions)]
pub(crate) mod script;
mod worktrees;

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::CommandState;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::attention::{StatusChange, find_agent};
use crate::herdr::{
    AgentStatus, PaneId, PluginActionContext, PluginActionInfo, PluginInvocationContext, Snapshot,
    SplitDirection, TabId, WorkspaceId, WorkspaceInfo,
};
use crate::overlay::{self, Capture, LayerId, Overlay};
use crate::sounds;
use crate::state::{AppEvent, AppState, MachineId};
use crate::status;
use crate::window_registry;

pub use command::HerdrCommand;
pub use confirm::Confirmation;
pub use worktrees::{WorktreeCommand, set_placements};

pub fn init(_cx: &mut App) {
    dock::set_icon();
}

/// What a context menu is about. The terminal area opens one per pane and
/// tab, the sidebar per workspace and agent. Each names the machine it was
/// opened on, captured then, so its commands act there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuTarget {
    Pane {
        machine: MachineId,
        pane_id: PaneId,
    },
    Tab {
        machine: MachineId,
        tab_id: TabId,
    },
    Workspace {
        machine: MachineId,
        workspace_id: WorkspaceId,
    },
}

impl MenuTarget {
    /// The machine the target is on.
    fn machine(&self) -> &MachineId {
        match self {
            Self::Pane { machine, .. }
            | Self::Tab { machine, .. }
            | Self::Workspace { machine, .. } => machine,
        }
    }
}

/// A UI callback run by a menu entry or a confirmation.
pub type Callback = Rc<dyn Fn(&mut Window, &mut App)>;

/// What picking a menu or palette entry does.
#[derive(Clone)]
pub enum MenuCommand {
    /// A herdr command, sent through `Chrome::run` (which confirms closes).
    Herdr(HerdrCommand),
    /// Ask for a new label, then rename the target.
    Rename(MenuTarget),
    /// Open Worktree… or Remove Worktree…, which ask before they act.
    Worktree(WorktreeCommand),
    /// Dispatch an app action (e.g. `ToggleTheme`).
    Action(Rc<dyn Action>),
    /// Anything else the caller wants run.
    Callback(Callback),
}

/// One row of a context menu: `Chrome::context_menu` shows the target's
/// default entries, then the caller's `extra` entries, then plugin actions.
#[derive(Clone)]
pub enum MenuEntry {
    Item {
        label: SharedString,
        command: MenuCommand,
        disabled: bool,
    },
    /// Opens `entries` beside the menu.
    Submenu {
        label: SharedString,
        entries: Vec<MenuEntry>,
    },
    Separator,
}

impl MenuEntry {
    pub fn item(label: impl Into<SharedString>, command: MenuCommand) -> Self {
        Self::Item {
            label: label.into(),
            command,
            disabled: false,
        }
    }

    fn herdr(label: &'static str, command: HerdrCommand) -> Self {
        Self::item(label, MenuCommand::Herdr(command))
    }
}

/// `entries` as text, for tests: items by label, `---` for a separator and
/// `Label › [...]` for a submenu.
#[cfg(test)]
pub fn labels(entries: &[MenuEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Item { label, .. } => label.to_string(),
            MenuEntry::Separator => "---".to_owned(),
            MenuEntry::Submenu { label, entries } => {
                format!("{label} › {:?}", labels(entries))
            }
        })
        .collect()
}

struct OpenMenu {
    position: Point<Pixels>,
    menu: Entity<PopupMenu>,
    /// The overlay layer holding the menu; `None` when it is drawn in the
    /// main window (no overlay, e.g. headless tests).
    layer: Option<(Entity<Overlay>, LayerId)>,
    /// Main-window focus to give back on close, so app key bindings keep working.
    previous_focus: Option<FocusHandle>,
    _dismiss: Subscription,
}

/// An open menu as an overlay layer: anchored at a main-window point.
struct MenuLayer {
    position: Point<Pixels>,
    menu: Entity<PopupMenu>,
}

impl Render for MenuLayer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        anchored()
            .position(self.position)
            .snap_to_window_with_margin(px(8.))
            .child(div().occlude().child(self.menu.clone()))
    }
}

/// Keys agent toasts so a newer status for the same pane on the same machine
/// replaces the older.
struct AgentToast;

/// The chrome of the main window `window`, if it has one, for views that
/// only have the window (the terminal area).
pub fn get(window: &Window, cx: &App) -> Option<Entity<Chrome>> {
    window_registry::upgrade(window, cx)
}

pub struct Chrome {
    state: Entity<AppState>,
    /// The main window; floating UI goes to its overlay.
    main_window: AnyWindowHandle,
    /// Every plugin action, from `plugin.action.list`; refreshed on use.
    plugin_actions: Rc<Vec<PluginActionInfo>>,
    open_menu: Option<OpenMenu>,
    /// The open (or last opened) palette's state.
    palette: Option<WeakEntity<CommandState>>,
    rename: Option<Subscription>,
    /// The rename dialog's input, while one is open.
    rename_input: Option<WeakEntity<InputState>>,
    badge: usize,
    /// Agent alerts waiting out herdr's `ui.toast.delay_seconds`, by toast key.
    pending_alerts: HashMap<SharedString, Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Chrome {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe_in(&state, window, |this, state, event, window, cx| {
            match event {
                AppEvent::AgentStatusChanged { machine, change } => {
                    this.agent_toast(machine, change, window, cx);
                }
                // The badge counts the live machines' agents: their caches,
                // and which machines are live.
                AppEvent::MachineSnapshot(_) | AppEvent::MachinesChanged => {
                    this.update_badge(state.read(cx).attention_count(cx));
                }
                _ => {}
            }
        });
        let main_window = window.window_handle();
        window_registry::insert(window, cx.entity().downgrade(), cx);
        let mut chrome = Self {
            state,
            main_window,
            plugin_actions: Rc::default(),
            open_menu: None,
            palette: None,
            rename: None,
            rename_input: None,
            badge: 0,
            pending_alerts: HashMap::new(),
            _subscriptions: vec![events],
        };
        chrome.refresh_plugin_actions(cx);
        chrome
    }

    fn update_badge(&mut self, count: usize) {
        if count != self.badge {
            self.badge = count;
            dock::set_badge(count);
        }
    }

    fn refresh_plugin_actions(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let task = state.on(state.selected(), cx, |session, cx| {
            session.plugin_actions(cx)
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(actions) => this
                .update(cx, |this, cx| {
                    this.plugin_actions = Rc::new(actions);
                    cx.notify();
                })
                .ok(),
            Err(error) => {
                log::warn!("plugin.action.list: {error}");
                None
            }
        })
        .detach();
    }

    /// Runs `f` in the window that draws floating UI: the main window's
    /// overlay (above the terminals), or the main window itself when no
    /// overlay can open. Deferred, so it never runs inside an update of
    /// either window.
    pub fn floating(&self, cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
        let main = self.main_window;
        cx.defer(move |cx| match overlay_for(main, cx) {
            Some(overlay) => {
                overlay.update(cx, |overlay, cx| overlay.update_window(cx, f));
            }
            None => {
                cx.update_window(main, |_, window, cx| f(window, cx)).ok();
            }
        });
    }

    /// Whether a context menu is open, and if so whether it is an overlay
    /// layer (`Some(true)`) or drawn in the main window (`Some(false)`).
    pub fn menu_open(&self) -> Option<bool> {
        self.open_menu.as_ref().map(|open| open.layer.is_some())
    }

    /// The open rename dialog's text field (for checks and scripts).
    pub fn rename_input(&self) -> Option<Entity<InputState>> {
        self.rename_input.as_ref().and_then(WeakEntity::upgrade)
    }

    /// The open (or last opened) palette's state: its query and selection.
    pub fn palette(&self) -> Option<Entity<CommandState>> {
        self.palette.as_ref().and_then(WeakEntity::upgrade)
    }

    /// The window floating UI is drawn in right now (for checks and
    /// scripts); `window` is the main window.
    pub fn floating_window(&self, window: &Window, cx: &App) -> AnyWindowHandle {
        overlay::get(window, cx)
            .map_or(self.main_window, |overlay| overlay.read(cx).window_handle())
    }

    /// After herdr's `ui.toast.delay_seconds`, alerts for an agent that
    /// needs you (blocked) or finished, if it is still in that state (as
    /// herdr does). A newer change for the same pane replaces a pending one.
    fn agent_toast(
        &mut self,
        machine: &MachineId,
        change: &StatusChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = toast_key(machine, &change.pane_id);
        self.pending_alerts.remove(&key);
        if !matches!(change.status, AgentStatus::Blocked | AgentStatus::Done) {
            return;
        }
        let delay = Duration::from_secs(crate::herdr_prefs::get(cx).toast.delay_seconds);
        if delay.is_zero() {
            self.agent_alert(machine, change, window, cx);
            return;
        }
        let (machine, change) = (machine.clone(), change.clone());
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            // The finished task stays in `pending_alerts` until the pane's
            // next change replaces it; dropping it from inside would cancel it.
            this.update_in(cx, |this, window, cx| {
                let still = this
                    .state
                    .read(cx)
                    .machine(&machine, cx)
                    .and_then(|m| find_agent(m.snapshot(), &change.pane_id))
                    .is_some_and(|agent| agent.agent_status == change.status);
                if still {
                    this.agent_alert(&machine, &change, window, cx);
                }
            })
            .ok();
        });
        self.pending_alerts.insert(key, task);
    }

    /// A toast (bottom right) and a sound for an agent that needs you
    /// (blocked) or finished, as herdr's config says: no sound for the
    /// workspace in view, no toast for the tab in view, toasts delivered
    /// per `ui.toast.delivery` (`herdr_prefs::alert`).
    fn agent_alert(
        &mut self,
        machine: &MachineId,
        change: &StatusChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let default_message = match change.status {
            AgentStatus::Blocked => "needs your input",
            AgentStatus::Done => "finished",
            _ => return,
        };
        // A file tab hides the terminals: nothing of herdr's is in view.
        let terminals_shown = crate::workspace::get(window, cx)
            .is_none_or(|ws| ws.read(cx).selected() == crate::workspace::MainTab::Herdr);
        let state = self.state.read(cx);
        let Some(snapshot) = state.machine(machine, cx).map(|m| m.snapshot()) else {
            return;
        };
        let agent = find_agent(snapshot, &change.pane_id);
        // The agent's own words for the state, when it reported some.
        let message = agent
            .and_then(|a| a.state_labels.get(status::word(change.status)))
            .filter(|label| !label.trim().is_empty())
            .map_or_else(|| default_message.to_owned(), Clone::clone);
        let workspace = agent
            .and_then(|agent| snapshot.workspace(&agent.workspace_id))
            .map(|w| format!(" in {}", w.info.label))
            .unwrap_or_default();
        let window_active = window.is_window_active();
        let looking = agent.map_or_else(Default::default, |agent| {
            crate::herdr_prefs::Looking::at(
                agent,
                snapshot,
                terminals_shown && state.selected() == machine,
                window_active,
            )
        });
        let prefs = crate::herdr_prefs::get(cx);
        let alert = crate::herdr_prefs::alert(
            prefs,
            agent.and_then(|a| a.agent.as_deref()),
            looking,
            window_active,
            crate::herdr::config::sound_disabled_by_env(),
        );
        if alert.sound {
            let custom = prefs
                .sound
                .custom_sound(change.status == AgentStatus::Done)
                .map(Path::to_path_buf);
            dock::play_for(change.status, !window_active, custom.as_deref());
        }
        let Some(delivery) = alert.toast else {
            return;
        };
        let target = change.pane_id.clone();
        let target_machine = machine.clone();
        let state = self.state.clone();
        let note = Notification::new()
            .id1::<AgentToast>(toast_key(machine, &change.pane_id))
            .icon(status::icon(change.status, cx))
            .title(format!("{}{workspace}", change.name))
            .message(message)
            .placement(Anchor::BottomRight)
            .delivery(delivery)
            // The toast lives in the overlay window, so it can't dispatch
            // `FocusPane` to the main window's views; it asks herdr directly.
            .on_click(move |_, _, cx| {
                sounds::play(cues::Cue::Navigate, sounds::subtle());
                // `pane.focus` is also what tells herdr the result was seen.
                state.update(cx, |state, cx| {
                    state
                        .focus_pane(&target_machine, &target, cx)
                        .detach_and_log_err(cx);
                });
            });
        self.floating(cx, move |window, cx| window.push_notification(note, cx));
    }

    pub fn toast(
        &mut self,
        message: impl Into<SharedString>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = message.into();
        self.floating(cx, move |window, cx| window.push_notification(message, cx));
    }

    pub fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(window, cx);
        self.refresh_plugin_actions(cx);
        let chrome = cx.entity();
        let state = self.state.clone();
        self.floating(cx, move |window, cx| {
            let palette = palette::open(chrome.clone(), state, window, cx);
            chrome.update(cx, |chrome, _| chrome.palette = Some(palette.downgrade()));
        });
    }

    /// Opens a context menu for `target` at `position` (main-window
    /// coordinates): the target's herdr actions, then `extra`, then matching
    /// plugin actions. It draws in the overlay, above the terminals.
    pub fn context_menu(
        &mut self,
        target: MenuTarget,
        position: Point<Pixels>,
        extra: Vec<MenuEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut entries = self.default_entries(&target, cx);
        if !extra.is_empty() {
            entries.push(MenuEntry::Separator);
            entries.extend(extra);
        }
        let plugins = self.plugin_entries(&target, cx);
        if !plugins.is_empty() {
            entries.push(MenuEntry::Separator);
            entries.extend(plugins);
        }
        self.show_menu(entries, position, window, cx);
        self.refresh_plugin_actions(cx);
    }

    /// Opens a context menu of just `entries` at `position` (main-window
    /// coordinates), for menus about something herdr's snapshot doesn't
    /// hold, such as a saved machine.
    pub fn menu(
        &mut self,
        entries: Vec<MenuEntry>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_menu(entries, position, window, cx);
    }

    fn show_menu(
        &mut self,
        entries: Vec<MenuEntry>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_focus = match self.open_menu.take() {
            Some(open) => {
                remove_layer_later(open.layer, cx);
                open.previous_focus
            }
            None => window.focused(cx),
        };
        let chrome = cx.entity().downgrade();
        let main = self.main_window;
        cx.defer(move |cx| {
            let overlay = overlay_for(main, cx);
            let host = overlay
                .as_ref()
                .map_or(main, |overlay| overlay.read(cx).window_handle());
            let Ok(menu) = cx.update_window(host, |_, window, cx| {
                build_menu(&chrome, entries, window, cx)
            }) else {
                return;
            };
            let layer = overlay.map(|overlay| {
                let view = cx.new(|_| MenuLayer {
                    position,
                    menu: menu.clone(),
                });
                let id = overlay.update(cx, |overlay, cx| {
                    overlay.push_layer(view, Capture::Modal, cx)
                });
                (overlay, id)
            });
            cx.update_window(host, |_, window, cx| {
                menu.focus_handle(cx).focus(window, cx)
            })
            .ok();
            let dismiss = {
                let chrome = chrome.clone();
                cx.subscribe(&menu, move |_, _: &DismissEvent, cx| {
                    update_later(&chrome, cx, |chrome, window, cx| {
                        chrome.close_context_menu(window, cx);
                    });
                })
            };
            let open = OpenMenu {
                position,
                menu,
                layer,
                previous_focus,
                _dismiss: dismiss,
            };
            let stale = chrome
                .update(cx, |this, cx| {
                    cx.notify();
                    this.open_menu.replace(open)
                })
                .ok()
                .flatten();
            if let Some(stale) = stale {
                remove_layer_later(stale.layer, cx);
            }
        });
    }

    /// Closes the open context menu, if any. `window` is the main window.
    /// Anything opening a dialog in the overlay calls this first, so layers
    /// don't pile up.
    pub fn close_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.open_menu.take() else {
            return;
        };
        remove_layer_later(open.layer, cx);
        if let Some(focus) = open.previous_focus {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    /// The cache of `machine`; empty when it's gone.
    fn snapshot_of<'a>(&self, machine: &MachineId, cx: &'a App) -> &'a Snapshot {
        static EMPTY: std::sync::LazyLock<Snapshot> = std::sync::LazyLock::new(Snapshot::default);
        self.state
            .read(cx)
            .machine(machine, cx)
            .map_or(&*EMPTY, |m| m.snapshot())
    }

    /// The herdr actions that make sense for `target`.
    pub fn default_entries(&self, target: &MenuTarget, cx: &App) -> Vec<MenuEntry> {
        use HerdrCommand as C;
        let snapshot = self.snapshot_of(target.machine(), cx);
        match target {
            MenuTarget::Pane { machine, pane_id } => {
                let id = || pane_id.clone();
                let on = || machine.clone();
                vec![
                    MenuEntry::herdr(
                        "Focus Pane",
                        C::FocusPane {
                            machine: on(),
                            pane_id: id(),
                        },
                    ),
                    MenuEntry::herdr(
                        "Split Right",
                        C::SplitPane {
                            machine: on(),
                            pane_id: id(),
                            direction: SplitDirection::Right,
                        },
                    ),
                    MenuEntry::herdr(
                        "Split Down",
                        C::SplitPane {
                            machine: on(),
                            pane_id: id(),
                            direction: SplitDirection::Down,
                        },
                    ),
                    MenuEntry::herdr(
                        "Toggle Zoom",
                        C::ZoomPane {
                            machine: on(),
                            pane_id: id(),
                        },
                    ),
                    MenuEntry::item("Rename Pane…", MenuCommand::Rename(target.clone())),
                    MenuEntry::Separator,
                    MenuEntry::herdr(
                        "Close Pane",
                        C::ClosePane {
                            machine: on(),
                            pane_id: id(),
                        },
                    ),
                ]
            }
            MenuTarget::Tab { machine, tab_id } => {
                let on = || machine.clone();
                let tab = snapshot.tab(tab_id);
                let mut entries = vec![MenuEntry::herdr(
                    "Focus Tab",
                    C::FocusTab {
                        machine: on(),
                        tab_id: tab_id.clone(),
                    },
                )];
                if let Some(tab) = tab {
                    entries.push(MenuEntry::herdr(
                        "New Tab",
                        C::NewTab {
                            machine: on(),
                            workspace_id: tab.info.workspace_id.clone(),
                        },
                    ));
                }
                // A pane alone in its tab has no header to right-click, so
                // the tab offers splitting its focused pane.
                if let Some(pane_id) = tab.and_then(|t| t.focused_pane_id.clone()) {
                    for (label, direction) in [
                        ("Split Right", SplitDirection::Right),
                        ("Split Down", SplitDirection::Down),
                    ] {
                        entries.push(MenuEntry::herdr(
                            label,
                            C::SplitPane {
                                machine: on(),
                                pane_id: pane_id.clone(),
                                direction,
                            },
                        ));
                    }
                }
                entries.extend([
                    MenuEntry::item("Rename Tab…", MenuCommand::Rename(target.clone())),
                    MenuEntry::Separator,
                    MenuEntry::herdr(
                        "Close Tab",
                        C::CloseTab {
                            machine: on(),
                            tab_id: tab_id.clone(),
                        },
                    ),
                ]);
                entries
            }
            MenuTarget::Workspace {
                machine,
                workspace_id,
            } => {
                let id = || workspace_id.clone();
                let on = || machine.clone();
                let placed = snapshot
                    .workspace(workspace_id)
                    .and_then(|w| worktrees::placement(machine, &w.info, cx));
                let mut entries = vec![
                    MenuEntry::herdr(
                        "Focus Workspace",
                        C::FocusWorkspace {
                            machine: on(),
                            workspace_id: id(),
                        },
                    ),
                    MenuEntry::herdr(
                        "New Tab",
                        C::NewTab {
                            machine: on(),
                            workspace_id: id(),
                        },
                    ),
                    MenuEntry::item("Rename Workspace…", MenuCommand::Rename(target.clone())),
                ];
                if placed.is_some() {
                    entries.push(MenuEntry::item(
                        "Open Worktree…",
                        MenuCommand::Worktree(WorktreeCommand::Open {
                            machine: on(),
                            workspace_id: id(),
                        }),
                    ));
                }
                entries.push(MenuEntry::Separator);
                if placed.is_some_and(|p| p.linked) {
                    entries.push(MenuEntry::item(
                        "Remove Worktree…",
                        MenuCommand::Worktree(WorktreeCommand::Remove {
                            machine: on(),
                            workspace_id: id(),
                        }),
                    ));
                }
                entries.push(MenuEntry::herdr(
                    "Close Workspace",
                    C::CloseWorkspace {
                        machine: on(),
                        workspace_id: id(),
                    },
                ));
                if let Some(close_group) = self.group_close(machine, workspace_id, cx) {
                    entries.push(MenuEntry::herdr("Close Worktree Group", close_group));
                }
                entries
            }
        }
    }

    /// Closing the worktree group `workspace_id` heads, as the spaces list
    /// groups it (herdr's info, else the list's own git read); `None` when
    /// it heads none.
    pub fn group_close(
        &self,
        machine: &MachineId,
        workspace_id: &WorkspaceId,
        cx: &App,
    ) -> Option<HerdrCommand> {
        let snapshot = self.snapshot_of(machine, cx);
        let place = |info: &WorkspaceInfo| worktrees::placement(machine, info, cx);
        let members = confirm::group_members(snapshot, workspace_id, place);
        if members.len() < 2 {
            return None;
        }
        let repo_name = snapshot
            .workspace(workspace_id)
            .and_then(|w| place(&w.info))
            .map(|p| p.repo.name)?;
        Some(HerdrCommand::CloseGroup {
            machine: machine.clone(),
            workspace_id: workspace_id.to_owned(),
            repo_name,
            members: members
                .iter()
                .map(|w| w.info.workspace_id.clone())
                .collect(),
        })
    }

    /// Plugin actions declared for the target's context, invoked with it.
    /// They are the selected machine's plugins, so another machine's
    /// workspace gets none.
    fn plugin_entries(&self, target: &MenuTarget, cx: &App) -> Vec<MenuEntry> {
        let state = self.state.read(cx);
        let machine = target.machine();
        if machine != state.selected() {
            return Vec::new();
        }
        let snapshot = state.snapshot(cx);
        let (kind, context) = invocation_context(target, snapshot);
        self.plugin_actions
            .iter()
            .filter(|action| action.contexts.contains(&kind))
            .map(|action| {
                MenuEntry::item(
                    action.title.clone(),
                    MenuCommand::Herdr(HerdrCommand::InvokePluginAction {
                        machine: machine.clone(),
                        action: Box::new(action.clone()),
                        context: Box::new(context.clone()),
                    }),
                )
            })
            .collect()
    }

    pub fn run_menu_command(
        &mut self,
        command: MenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            MenuCommand::Herdr(command) => self.run(command, window, cx),
            MenuCommand::Rename(target) => self.prompt_rename(target, window, cx),
            MenuCommand::Worktree(command) => self.run_worktree_command(command, window, cx),
            MenuCommand::Action(action) => window.dispatch_action(action.boxed_clone(), cx),
            // Deferred out of this update: a callback may reach the chrome
            // itself (opening a dialog does), which would re-enter it.
            MenuCommand::Callback(callback) => {
                let main = window.window_handle();
                cx.defer(move |cx| {
                    cx.update_window(main, |_, window, cx| callback(window, cx))
                        .ok();
                });
            }
        }
    }

    /// Sends a herdr command, asking first when it would close a running
    /// agent or a whole worktree group (herdr closes immediately).
    pub fn run(&mut self, command: HerdrCommand, window: &mut Window, cx: &mut Context<Self>) {
        if let HerdrCommand::NewTab {
            machine,
            workspace_id,
        } = &command
        {
            self.new_tab(machine.clone(), Some(workspace_id.clone()), window, cx);
            return;
        }
        let confirm_close = crate::herdr_prefs::get(cx).confirm_close;
        let confirmation = confirm::needed_for(
            &command,
            self.snapshot_of(command.machine(), cx),
            confirm_close,
        );
        let Some(confirmation) = confirmation else {
            self.send(command, window, cx);
            return;
        };
        let chrome = cx.entity().downgrade();
        let send = move |_: &mut Window, cx: &mut App| {
            let command = command.clone();
            update_later(&chrome, cx, move |chrome, window, cx| {
                chrome.send(command, window, cx);
            });
        };
        self.confirm(confirmation, send, window, cx);
    }

    /// A new tab in `workspace_id` (the focused one when `None`) on
    /// `machine`. With herdr's `ui.prompt_new_tab_name` (on by default) it
    /// asks for the name first; an empty name lets herdr pick one.
    pub fn new_tab(
        &mut self,
        machine: MachineId,
        workspace_id: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let create = move |label: Option<String>, cx: &mut App| {
            state
                .read(cx)
                .on(&machine, cx, |session, cx| {
                    session.new_tab_named(workspace_id.as_ref(), label, cx)
                })
                .detach_and_log_err(cx);
        };
        if !crate::herdr_prefs::get(cx).prompt_new_tab_name {
            create(None, cx);
            return;
        }
        // `ask_name` reaches the chrome itself, so it opens after this update.
        window.defer(cx, move |window, cx| {
            crate::prompt::ask_name(
                "New Tab",
                "Create",
                "",
                move |name, _, cx| {
                    let name = name.trim();
                    create((!name.is_empty()).then(|| name.to_owned()), cx);
                },
                window,
                cx,
            );
        });
    }

    /// Sends without asking; a failure becomes an error toast.
    fn send(&mut self, command: HerdrCommand, _window: &mut Window, cx: &mut Context<Self>) {
        let task = command.send(&self.state, cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                sounds::play(cues::Cue::Error, sounds::subtle());
                let message = error.to_string();
                this.update(cx, |chrome, cx| {
                    chrome.floating(cx, move |window, cx| {
                        window.push_notification(Notification::error(message).title("herdr"), cx);
                    });
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks the user to confirm, then runs `on_confirm`. Cancel does nothing.
    /// The dialog draws in the overlay, so `on_confirm` gets the overlay
    /// window; it should talk to `AppState` (or go through `chrome::get`), not
    /// dispatch actions there. `window` is the main window.
    pub fn confirm(
        &mut self,
        confirmation: Confirmation,
        on_confirm: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(window, cx);
        let on_confirm: Callback = Rc::new(on_confirm);
        self.floating(cx, move |window, cx| {
            confirm::open(confirmation, on_confirm, window, cx);
        });
    }

    fn prompt_rename(&mut self, target: MenuTarget, _window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot_of(target.machine(), cx);
        let (title, current) = match &target {
            MenuTarget::Pane { pane_id, .. } => (
                "Rename Pane",
                snapshot.pane(pane_id).and_then(|p| p.label.clone()),
            ),
            MenuTarget::Tab { tab_id, .. } => (
                "Rename Tab",
                snapshot.tab(tab_id).map(|t| t.info.label.clone()),
            ),
            MenuTarget::Workspace { workspace_id, .. } => (
                "Rename Workspace",
                snapshot
                    .workspace(workspace_id)
                    .map(|w| w.info.label.clone()),
            ),
        };
        let chrome = cx.entity();
        self.floating(cx, move |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Name")
                    .default_value(current.unwrap_or_default())
            });
            let submit: Rc<dyn Fn(&mut App)> = {
                let chrome = chrome.downgrade();
                let input = input.clone();
                Rc::new(move |cx| {
                    let label = input.read(cx).value().trim().to_owned();
                    if label.is_empty() {
                        return;
                    }
                    let command = rename_command(&target, label);
                    update_later(&chrome, cx, move |chrome, window, cx| {
                        chrome.send(command, window, cx);
                    });
                })
            };
            let on_enter = cx.subscribe(&input, {
                let submit = submit.clone();
                let chrome = chrome.downgrade();
                move |_, event: &InputEvent, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        submit(cx);
                        update_later(&chrome, cx, |chrome, _, cx| {
                            chrome.floating(cx, |window, cx| window.close_dialog(cx));
                        });
                    }
                }
            });
            chrome.update(cx, |chrome, _| {
                chrome.rename = Some(on_enter);
                chrome.rename_input = Some(input.downgrade());
            });
            let first_frame = std::cell::Cell::new(true);
            window.open_alert_dialog(cx, move |alert, window, cx| {
                if first_frame.replace(false) {
                    input.focus_handle(cx).focus(window, cx);
                }
                let submit = submit.clone();
                alert
                    .confirm()
                    .title(title)
                    .width(px(420.))
                    .child(Input::new(&input))
                    .ok_text("Rename")
                    .on_ok(move |_, _, cx| {
                        submit(cx);
                        true
                    })
            });
        });
    }
}

/// The overlay of the main window `main`, opening it on first use; `None`
/// when it can't open (no native window, e.g. headless tests).
fn overlay_for(main: AnyWindowHandle, cx: &mut App) -> Option<Entity<Overlay>> {
    cx.update_window(main, |_, window, cx| overlay::get_or_open(window, cx).ok())
        .ok()
        .flatten()
}

/// Removes a menu's overlay layer on the next tick (the overlay window may be
/// mid-update when the menu dismisses itself).
fn remove_layer_later(layer: Option<(Entity<Overlay>, LayerId)>, cx: &mut App) {
    if let Some((overlay, id)) = layer {
        cx.defer(move |cx| overlay.update(cx, |overlay, cx| overlay.remove_layer(id, cx)));
    }
}

fn build_menu(
    chrome: &WeakEntity<Chrome>,
    entries: Vec<MenuEntry>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PopupMenu> {
    let chrome = chrome.clone();
    PopupMenu::build(window, cx, move |menu, window, cx| {
        add_entries(menu, &chrome, &entries, Submenus::Flyout, window, cx).min_w(px(200.))
    })
}

/// Opens a menu of `entries` at `position` through the chrome of `window`
/// (the main window), for views without a `Chrome` handle.
pub fn open_menu(
    entries: Vec<MenuEntry>,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    if entries.is_empty() {
        return;
    }
    if let Some(chrome) = get(window, cx) {
        chrome.update(cx, |chrome, cx| chrome.menu(entries, position, window, cx));
    }
}

/// Fills a Kit popup `menu` with `entries`, run through the chrome of
/// `window` like a context menu's. For menus Kit builds and draws itself in
/// the main window (a dock panel's title-bar menu), so they share the
/// floating menus' entries. A submenu shows inline as a labeled section: a
/// flyout from a main-window menu would open under the terminals.
pub fn fill_popup(
    menu: PopupMenu,
    entries: &[MenuEntry],
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let Some(chrome) = get(window, cx) else {
        return menu;
    };
    add_entries(
        menu,
        &chrome.downgrade(),
        entries,
        Submenus::Inline,
        window,
        cx,
    )
}

/// How [`add_entries`] lays out a [`MenuEntry::Submenu`].
#[derive(Clone, Copy)]
enum Submenus {
    Flyout,
    Inline,
}

fn add_entries(
    mut menu: PopupMenu,
    chrome: &WeakEntity<Chrome>,
    entries: &[MenuEntry],
    submenus: Submenus,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    for entry in entries {
        menu = match entry {
            MenuEntry::Separator => menu.separator(),
            MenuEntry::Submenu { label, entries } => match submenus {
                Submenus::Flyout => {
                    let chrome = chrome.clone();
                    let submenu = PopupMenu::build(window, cx, move |menu, window, cx| {
                        add_entries(menu, &chrome, entries, submenus, window, cx).min_w(px(160.))
                    });
                    menu.item(PopupMenuItem::submenu(label.clone(), submenu))
                }
                Submenus::Inline => {
                    let menu = menu.separator().label(label.clone());
                    add_entries(menu, chrome, entries, submenus, window, cx).separator()
                }
            },
            MenuEntry::Item {
                label,
                command,
                disabled,
            } => {
                let (chrome, command) = (chrome.clone(), command.clone());
                menu.item(
                    PopupMenuItem::new(label.clone())
                        .disabled(*disabled)
                        .on_click(move |_, _, cx| {
                            let command = command.clone();
                            update_later(&chrome, cx, move |chrome, window, cx| {
                                chrome.close_context_menu(window, cx);
                                chrome.run_menu_command(command, window, cx);
                            });
                        }),
                )
            }
        };
    }
    menu
}

/// Runs `f` on the chrome, with the main window, after the current update.
/// Menu, dialog, palette and toast callbacks use this: they fire in the
/// overlay window, or while the chrome is itself being updated (a confirm
/// dispatched from inside it), and a nested update panics.
fn update_later(
    chrome: &WeakEntity<Chrome>,
    cx: &mut App,
    f: impl FnOnce(&mut Chrome, &mut Window, &mut Context<Chrome>) + 'static,
) {
    let chrome = chrome.clone();
    cx.defer(move |cx| {
        let Some(chrome) = chrome.upgrade() else {
            return;
        };
        let main = chrome.read(cx).main_window;
        cx.update_window(main, |_, window, cx| {
            chrome.update(cx, |chrome, cx| f(chrome, window, cx));
        })
        .ok();
    });
}

/// An agent toast's id: one per pane per machine (pane ids repeat across
/// machines).
fn toast_key(machine: &MachineId, pane_id: &PaneId) -> SharedString {
    format!("{machine}/{pane_id}").into()
}

fn rename_command(target: &MenuTarget, label: String) -> HerdrCommand {
    match target {
        MenuTarget::Pane { machine, pane_id } => HerdrCommand::RenamePane {
            machine: machine.clone(),
            pane_id: pane_id.clone(),
            label,
        },
        MenuTarget::Tab { machine, tab_id } => HerdrCommand::RenameTab {
            machine: machine.clone(),
            tab_id: tab_id.clone(),
            label,
        },
        MenuTarget::Workspace {
            machine,
            workspace_id,
        } => HerdrCommand::RenameWorkspace {
            machine: machine.clone(),
            workspace_id: workspace_id.clone(),
            label,
        },
    }
}

/// The context `plugin.action.invoke` gets for a pane: its cwd, agent,
/// status, tab and workspace.
fn pane_context(snapshot: &Snapshot, pane_id: &PaneId, source: &str) -> PluginInvocationContext {
    let pane = snapshot.pane(pane_id);
    let workspace = pane.and_then(|p| snapshot.workspace(&p.workspace_id));
    PluginInvocationContext {
        invocation_source: Some(source.to_owned()),
        focused_pane_id: Some(pane_id.to_owned()),
        focused_pane_cwd: pane.and_then(|p| p.working_dir()).map(str::to_owned),
        focused_pane_agent: pane.and_then(|p| p.agent.clone()),
        focused_pane_status: pane.map(|p| p.agent_status),
        tab_id: pane.map(|p| p.tab_id.clone()),
        tab_label: pane
            .and_then(|p| snapshot.tab(&p.tab_id))
            .map(|t| t.info.label.clone()),
        workspace_id: pane.map(|p| p.workspace_id.clone()),
        workspace_label: workspace.map(|w| w.info.label.clone()),
        ..PluginInvocationContext::default()
    }
}

/// The plugin context kind for `target` and the invocation context for it.
fn invocation_context(
    target: &MenuTarget,
    snapshot: &Snapshot,
) -> (PluginActionContext, PluginInvocationContext) {
    const SOURCE: &str = "context_menu";
    match target {
        MenuTarget::Pane { pane_id, .. } => (
            PluginActionContext::Pane,
            pane_context(snapshot, pane_id, SOURCE),
        ),
        MenuTarget::Tab { tab_id, .. } => {
            let tab = snapshot.tab(tab_id);
            (
                PluginActionContext::Tab,
                PluginInvocationContext {
                    invocation_source: Some(SOURCE.to_owned()),
                    tab_id: Some(tab_id.clone()),
                    tab_label: tab.map(|t| t.info.label.clone()),
                    workspace_id: tab.map(|t| t.info.workspace_id.clone()),
                    ..PluginInvocationContext::default()
                },
            )
        }
        MenuTarget::Workspace { workspace_id, .. } => (
            PluginActionContext::Workspace,
            PluginInvocationContext {
                invocation_source: Some(SOURCE.to_owned()),
                workspace_id: Some(workspace_id.clone()),
                workspace_label: snapshot
                    .workspace(workspace_id)
                    .map(|w| w.info.label.clone()),
                ..PluginInvocationContext::default()
            },
        ),
    }
}

impl Render for Chrome {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Everything floating draws in the overlay. Without one (headless
        // tests) a menu draws here instead.
        let in_window = self.open_menu.as_ref().filter(|open| open.layer.is_none());
        div().when_some(in_window, |this, open| {
            this.child(
                deferred(
                    anchored()
                        .position(open.position)
                        .snap_to_window_with_margin(px(8.))
                        .child(open.menu.clone()),
                )
                .with_priority(1),
            )
        })
    }
}

#[cfg(test)]
mod tests;
