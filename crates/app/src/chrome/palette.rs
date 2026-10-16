//! The command palette: the app's own commands, a jump to every agent, and
//! every plugin action from `plugin.action.list`, run with `plugin.action.invoke`.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::*;

use super::{Chrome, HerdrCommand, MenuCommand, MenuTarget, pane_context, update_later};
use crate::actions::{FocusPane, Quit, ToggleTheme};
use crate::attention::{agent_name, status_label};
use crate::herdr::{PluginActionInfo, PluginInvocationContext, SplitDirection};
use crate::state::AppState;

struct Group {
    label: &'static str,
    entries: Vec<(SharedString, MenuCommand)>,
}

pub(super) fn open(
    chrome: Entity<Chrome>,
    state: Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CommandState> {
    let command_state = cx.new(|cx| CommandState::new(window, cx));
    let palette_state = command_state.clone();
    let first_frame = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first_frame.replace(false) {
            command_state.update(cx, |palette, cx| palette.focus(window, cx));
        }
        let plugin_actions = chrome.read(cx).plugin_actions.clone();
        let groups: Vec<Group> = groups(state.read(cx), &plugin_actions)
            .into_iter()
            .filter(|g| !g.entries.is_empty())
            .collect();
        let mut palette = Command::new(&command_state)
            .bordered(false)
            .max_h(px(420.))
            .placeholder("Run a command, jump to an agent, or invoke a plugin action…");
        let mut commands: Vec<Vec<MenuCommand>> = Vec::with_capacity(groups.len());
        for group in groups {
            let mut items = CommandGroup::new().label(group.label);
            let mut row = Vec::with_capacity(group.entries.len());
            for (label, command) in group.entries {
                items = items.item(CommandItem::new().label(label));
                row.push(command);
            }
            palette = palette.group(items);
            commands.push(row);
        }
        let commands = Rc::new(commands);
        let chrome = chrome.downgrade();
        let palette = palette
            .on_confirm(move |index, window, cx| {
                window.close_dialog(cx);
                let Some(command) = commands
                    .get(index.section)
                    .and_then(|group| group.get(index.row))
                    .cloned()
                else {
                    return;
                };
                update_later(&chrome, cx, move |chrome, window, cx| {
                    chrome.run_menu_command(command, window, cx);
                });
            })
            .on_cancel(|window, cx| window.close_dialog(cx));
        dialog.w(px(600.)).close_button(false).child(palette)
    });
    palette_state
}

fn groups(state: &AppState, plugin_actions: &[PluginActionInfo]) -> Vec<Group> {
    let snapshot = &state.snapshot;
    let focused = snapshot.focused_pane();

    let mut app: Vec<(SharedString, MenuCommand)> = Vec::new();
    if let Some(pane) = focused {
        use HerdrCommand as C;
        let herdr = |label: &'static str, command: C| (label.into(), MenuCommand::Herdr(command));
        let pane_id = || pane.pane_id.clone();
        app.extend([
            herdr(
                "Pane: Split Right",
                C::SplitPane {
                    pane_id: pane_id(),
                    direction: SplitDirection::Right,
                },
            ),
            herdr(
                "Pane: Split Down",
                C::SplitPane {
                    pane_id: pane_id(),
                    direction: SplitDirection::Down,
                },
            ),
            herdr("Pane: Toggle Zoom", C::ZoomPane { pane_id: pane_id() }),
            (
                "Pane: Rename…".into(),
                MenuCommand::Rename(MenuTarget::Pane { pane_id: pane_id() }),
            ),
            herdr("Pane: Close", C::ClosePane { pane_id: pane_id() }),
            herdr(
                "Tab: New",
                C::NewTab {
                    workspace_id: pane.workspace_id.clone(),
                },
            ),
            (
                "Tab: Rename…".into(),
                MenuCommand::Rename(MenuTarget::Tab {
                    tab_id: pane.tab_id.clone(),
                }),
            ),
            herdr(
                "Tab: Close",
                C::CloseTab {
                    tab_id: pane.tab_id.clone(),
                },
            ),
            (
                "Workspace: Rename…".into(),
                MenuCommand::Rename(MenuTarget::Workspace {
                    workspace_id: pane.workspace_id.clone(),
                }),
            ),
            herdr(
                "Workspace: Close",
                C::CloseWorkspace {
                    workspace_id: pane.workspace_id.clone(),
                    close_group: false,
                },
            ),
        ]);
    }
    app.push((
        "View: Toggle Light/Dark".into(),
        MenuCommand::Action(Rc::new(ToggleTheme)),
    ));
    app.push((
        "Quit rusted-tools".into(),
        MenuCommand::Action(Rc::new(Quit)),
    ));

    let agents = snapshot
        .agents
        .iter()
        .map(|agent| {
            let workspace = snapshot
                .workspace(&agent.workspace_id)
                .map_or("", |w| w.info.label.as_str());
            (
                format!(
                    "Go to {} — {workspace} ({})",
                    agent_name(agent),
                    status_label(agent.agent_status)
                )
                .into(),
                MenuCommand::Action(Rc::new(FocusPane {
                    pane_id: agent.pane_id.clone(),
                })),
            )
        })
        .collect();

    const SOURCE: &str = "command_palette";
    let context = focused.map_or_else(
        || PluginInvocationContext {
            invocation_source: Some(SOURCE.to_owned()),
            ..PluginInvocationContext::default()
        },
        |pane| pane_context(snapshot, &pane.pane_id, SOURCE),
    );
    let plugins = plugin_actions
        .iter()
        .map(|action| {
            (
                format!("{}: {}", action.plugin_id, action.title).into(),
                MenuCommand::Herdr(HerdrCommand::InvokePluginAction {
                    action: Box::new(action.clone()),
                    context: Box::new(context.clone()),
                }),
            )
        })
        .collect();

    vec![
        Group {
            label: "Commands",
            entries: app,
        },
        Group {
            label: "Agents",
            entries: agents,
        },
        Group {
            label: "Plugins",
            entries: plugins,
        },
    ]
}
