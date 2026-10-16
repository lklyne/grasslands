//! App chrome: command palette, context menus, toasts and confirmations.
//!
//! Ghostty's native views sit above GPUI's layer, so anything that must float
//! over a terminal is drawn in a transparent overlay window (being proven by
//! the ghostty-terminals agent). Callers go through `Chrome`'s methods (or the
//! `OpenContextMenu` action) so the choice of in-window vs overlay rendering
//! stays in this module. See docs/decisions/0002-app-structure.md.

mod command;
mod confirm;
mod dock;
mod palette;
#[cfg(debug_assertions)]
mod script;

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::CommandState;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::{Notification, NotificationDelivery, NotificationType};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::FocusPane;
use crate::attention::{StatusChange, find_agent};
use crate::herdr::{
    AgentStatus, PluginActionContext, PluginActionInfo, PluginInvocationContext, Snapshot,
    SplitDirection,
};
use crate::state::{AppEvent, AppState};

pub use command::HerdrCommand;
pub use confirm::Confirmation;

pub fn init(_cx: &mut App) {}

/// What a context menu is about. The terminal area opens one per pane and
/// tab, the sidebar per workspace and agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuTarget {
    Pane { pane_id: String },
    Tab { tab_id: String },
    Workspace { workspace_id: String },
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

struct OpenMenu {
    position: Point<Pixels>,
    menu: Entity<PopupMenu>,
    /// Focus to give back on close, so app key bindings keep working.
    previous_focus: Option<FocusHandle>,
    _dismiss: Subscription,
}

/// Keys agent toasts so a newer status for the same pane replaces the older.
struct AgentToast;

pub struct Chrome {
    state: Entity<AppState>,
    /// Every plugin action, from `plugin.action.list`; refreshed on use.
    plugin_actions: Rc<Vec<PluginActionInfo>>,
    open_menu: Option<OpenMenu>,
    /// The open (or last opened) palette's state.
    palette: Option<WeakEntity<CommandState>>,
    rename: Option<Subscription>,
    /// The rename dialog's input, while one is open.
    rename_input: Option<WeakEntity<InputState>>,
    badge: usize,
    _subscriptions: Vec<Subscription>,
}

impl Chrome {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe_in(&state, window, |this, _, event, window, cx| {
            if let AppEvent::AgentStatusChanged(change) = event {
                this.agent_toast(change, window, cx);
            }
        });
        let badge = cx.observe(&state, |this, state, cx| {
            let state = state.read(cx);
            let count = state.attention.count(&state.snapshot);
            if count != this.badge {
                this.badge = count;
                dock::set_badge(count);
            }
        });
        let mut chrome = Self {
            state,
            plugin_actions: Rc::default(),
            open_menu: None,
            palette: None,
            rename: None,
            rename_input: None,
            badge: 0,
            _subscriptions: vec![events, badge],
        };
        chrome.refresh_plugin_actions(cx);
        #[cfg(debug_assertions)]
        script::start(window, cx);
        chrome
    }

    fn refresh_plugin_actions(&mut self, cx: &mut Context<Self>) {
        let task = self.state.read(cx).plugin_actions(cx);
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

    /// A toast and a sound for an agent that needs you (blocked) or finished.
    /// While the window is in the background the toast also goes to the OS.
    fn agent_toast(&mut self, change: &StatusChange, window: &mut Window, cx: &mut Context<Self>) {
        let (kind, message) = match change.status {
            AgentStatus::Blocked => (NotificationType::Warning, "needs your input"),
            AgentStatus::Done => (NotificationType::Success, "finished"),
            _ => return,
        };
        let snapshot = &self.state.read(cx).snapshot;
        let workspace = find_agent(snapshot, &change.pane_id)
            .and_then(|agent| snapshot.workspace(&agent.workspace_id))
            .map(|w| format!(" in {}", w.info.label))
            .unwrap_or_default();
        let delivery = if window.is_window_active() {
            NotificationDelivery::InApp
        } else {
            NotificationDelivery::InAppAndSystem
        };
        let target = change.pane_id.clone();
        let note = Notification::new()
            .id1::<AgentToast>(SharedString::from(change.pane_id.clone()))
            .with_type(kind)
            .title(format!("{}{workspace}", change.name))
            .message(message)
            .delivery(delivery)
            .on_click(move |_, window, cx| {
                window.dispatch_action(
                    Box::new(FocusPane {
                        pane_id: target.clone(),
                    }),
                    cx,
                );
            });
        window.push_notification(note, cx);
        dock::play_for(change.status);
    }

    pub fn toast(
        &mut self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(message.into(), cx);
    }

    pub fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(window, cx);
        self.refresh_plugin_actions(cx);
        let palette = palette::open(cx.entity(), self.state.clone(), window, cx);
        self.palette = Some(palette.downgrade());
    }

    /// Opens a context menu for `target` at `position` (window coordinates):
    /// the target's herdr actions, then `extra`, then matching plugin actions.
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

    fn show_menu(
        &mut self,
        entries: Vec<MenuEntry>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chrome = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            for entry in entries {
                menu = match entry {
                    MenuEntry::Separator => menu.separator(),
                    MenuEntry::Item {
                        label,
                        command,
                        disabled,
                    } => {
                        let chrome = chrome.clone();
                        menu.item(PopupMenuItem::new(label).disabled(disabled).on_click(
                            move |_, window, cx| {
                                let command = command.clone();
                                update_later(&chrome, window, cx, move |chrome, window, cx| {
                                    chrome.close_context_menu(window, cx);
                                    chrome.run_menu_command(command, window, cx);
                                });
                            },
                        ))
                    }
                };
            }
            menu.min_w(px(200.))
        });
        let dismiss = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
            this.close_context_menu(window, cx);
        });
        let previous_focus = match self.open_menu.take() {
            Some(open) => open.previous_focus,
            None => window.focused(cx),
        };
        menu.focus_handle(cx).focus(window, cx);
        self.open_menu = Some(OpenMenu {
            position,
            menu,
            previous_focus,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    fn close_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.open_menu.take() else {
            return;
        };
        if let Some(focus) = open.previous_focus {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    /// The herdr actions that make sense for `target`.
    pub fn default_entries(&self, target: &MenuTarget, cx: &App) -> Vec<MenuEntry> {
        use HerdrCommand as C;
        let snapshot = &self.state.read(cx).snapshot;
        match target {
            MenuTarget::Pane { pane_id } => {
                let id = || pane_id.clone();
                vec![
                    MenuEntry::herdr("Focus Pane", C::FocusPane { pane_id: id() }),
                    MenuEntry::herdr(
                        "Split Right",
                        C::SplitPane {
                            pane_id: id(),
                            direction: SplitDirection::Right,
                        },
                    ),
                    MenuEntry::herdr(
                        "Split Down",
                        C::SplitPane {
                            pane_id: id(),
                            direction: SplitDirection::Down,
                        },
                    ),
                    MenuEntry::herdr("Toggle Zoom", C::ZoomPane { pane_id: id() }),
                    MenuEntry::item("Rename Pane…", MenuCommand::Rename(target.clone())),
                    MenuEntry::Separator,
                    MenuEntry::herdr("Close Pane", C::ClosePane { pane_id: id() }),
                ]
            }
            MenuTarget::Tab { tab_id } => {
                let workspace_id = snapshot
                    .tab(tab_id)
                    .map(|t| t.info.workspace_id.clone())
                    .unwrap_or_default();
                vec![
                    MenuEntry::herdr(
                        "Focus Tab",
                        C::FocusTab {
                            tab_id: tab_id.clone(),
                        },
                    ),
                    MenuEntry::herdr("New Tab", C::NewTab { workspace_id }),
                    MenuEntry::item("Rename Tab…", MenuCommand::Rename(target.clone())),
                    MenuEntry::Separator,
                    MenuEntry::herdr(
                        "Close Tab",
                        C::CloseTab {
                            tab_id: tab_id.clone(),
                        },
                    ),
                ]
            }
            MenuTarget::Workspace { workspace_id } => {
                let id = || workspace_id.clone();
                let mut entries = vec![
                    MenuEntry::herdr("Focus Workspace", C::FocusWorkspace { workspace_id: id() }),
                    MenuEntry::herdr("New Tab", C::NewTab { workspace_id: id() }),
                    MenuEntry::item("Rename Workspace…", MenuCommand::Rename(target.clone())),
                    MenuEntry::Separator,
                    MenuEntry::herdr(
                        "Close Workspace",
                        C::CloseWorkspace {
                            workspace_id: id(),
                            close_group: false,
                        },
                    ),
                ];
                if confirm::group_members(snapshot, workspace_id).len() > 1 {
                    entries.push(MenuEntry::herdr(
                        "Close Worktree Group",
                        C::CloseWorkspace {
                            workspace_id: id(),
                            close_group: true,
                        },
                    ));
                }
                entries
            }
        }
    }

    /// Plugin actions declared for the target's context, invoked with it.
    fn plugin_entries(&self, target: &MenuTarget, cx: &App) -> Vec<MenuEntry> {
        let snapshot = &self.state.read(cx).snapshot;
        let (kind, context) = invocation_context(target, snapshot);
        self.plugin_actions
            .iter()
            .filter(|action| action.contexts.contains(&kind))
            .map(|action| {
                MenuEntry::item(
                    action.title.clone(),
                    MenuCommand::Herdr(HerdrCommand::InvokePluginAction {
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
            MenuCommand::Action(action) => window.dispatch_action(action.boxed_clone(), cx),
            MenuCommand::Callback(callback) => callback(window, cx),
        }
    }

    /// Sends a herdr command, asking first when it would close a running
    /// agent or a whole worktree group (herdr closes immediately).
    pub fn run(&mut self, command: HerdrCommand, window: &mut Window, cx: &mut Context<Self>) {
        let confirmation = confirm::needed_for(&command, &self.state.read(cx).snapshot);
        let Some(confirmation) = confirmation else {
            self.send(command, window, cx);
            return;
        };
        let chrome = cx.entity().downgrade();
        let send = move |window: &mut Window, cx: &mut App| {
            let command = command.clone();
            update_later(&chrome, window, cx, move |chrome, window, cx| {
                chrome.send(command, window, cx);
            });
        };
        self.confirm(confirmation, send, window, cx);
    }

    /// Sends without asking; a failure becomes an error toast.
    fn send(&mut self, command: HerdrCommand, window: &mut Window, cx: &mut Context<Self>) {
        let task = command.send(self.state.read(cx), cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = task.await {
                cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(error.to_string()).title("herdr"),
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks the user to confirm, then runs `on_confirm`. Cancel does nothing.
    pub fn confirm(
        &mut self,
        confirmation: Confirmation,
        on_confirm: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(window, cx);
        confirm::open(confirmation, Rc::new(on_confirm), window, cx);
    }

    fn prompt_rename(&mut self, target: MenuTarget, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = &self.state.read(cx).snapshot;
        let (title, current) = match &target {
            MenuTarget::Pane { pane_id } => (
                "Rename Pane",
                snapshot.pane(pane_id).and_then(|p| p.label.clone()),
            ),
            MenuTarget::Tab { tab_id } => (
                "Rename Tab",
                snapshot.tab(tab_id).map(|t| t.info.label.clone()),
            ),
            MenuTarget::Workspace { workspace_id } => (
                "Rename Workspace",
                snapshot
                    .workspace(workspace_id)
                    .map(|w| w.info.label.clone()),
            ),
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Name")
                .default_value(current.unwrap_or_default())
        });
        self.rename_input = Some(input.downgrade());
        let submit: Callback = {
            let chrome = cx.entity().downgrade();
            let input = input.clone();
            Rc::new(move |window, cx| {
                let label = input.read(cx).value().trim().to_owned();
                if label.is_empty() {
                    return;
                }
                let command = rename_command(&target, label);
                update_later(&chrome, window, cx, move |chrome, window, cx| {
                    chrome.send(command, window, cx);
                });
            })
        };
        self.rename = Some(cx.subscribe_in(&input, window, {
            let submit = submit.clone();
            move |_, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    submit(window, cx);
                    window.close_dialog(cx);
                }
            }
        }));
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
                .on_ok(move |_, window, cx| {
                    submit(window, cx);
                    true
                })
        });
    }
}

/// Runs `f` on the chrome after the current update. Menu, dialog and palette
/// callbacks use this: they can fire while the chrome is itself being updated
/// (a confirm dispatched from inside it), and a nested update panics.
fn update_later(
    chrome: &WeakEntity<Chrome>,
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut Chrome, &mut Window, &mut Context<Chrome>) + 'static,
) {
    let chrome = chrome.clone();
    window.defer(cx, move |window, cx| {
        chrome.update(cx, |chrome, cx| f(chrome, window, cx)).ok();
    });
}

fn rename_command(target: &MenuTarget, label: String) -> HerdrCommand {
    match target {
        MenuTarget::Pane { pane_id } => HerdrCommand::RenamePane {
            pane_id: pane_id.clone(),
            label,
        },
        MenuTarget::Tab { tab_id } => HerdrCommand::RenameTab {
            tab_id: tab_id.clone(),
            label,
        },
        MenuTarget::Workspace { workspace_id } => HerdrCommand::RenameWorkspace {
            workspace_id: workspace_id.clone(),
            label,
        },
    }
}

/// The context `plugin.action.invoke` gets for a pane: its cwd, agent,
/// status, tab and workspace.
fn pane_context(snapshot: &Snapshot, pane_id: &str, source: &str) -> PluginInvocationContext {
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
        MenuTarget::Pane { pane_id } => (
            PluginActionContext::Pane,
            pane_context(snapshot, pane_id, SOURCE),
        ),
        MenuTarget::Tab { tab_id } => {
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
        MenuTarget::Workspace { workspace_id } => (
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
        // Toasts and dialogs render in GPUI Kit's root layers; the only
        // floating piece drawn here is a programmatically opened menu.
        div().when_some(self.open_menu.as_ref(), |this, open| {
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
