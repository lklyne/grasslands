//! The command palette: the app's own commands, a jump to every agent, and
//! every plugin action from `plugin.action.list`, run with `plugin.action.invoke`.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::*;

use super::worktrees::{self, WorktreeCommand};
use super::{Chrome, HerdrCommand, MenuCommand, MenuTarget, pane_context, update_later};
use crate::actions::{FocusPane, OpenChanges, OpenSettings, Quit, ToggleTheme};
use crate::herdr::{PluginActionInfo, PluginInvocationContext, SplitDirection, WorkspaceInfo};
use crate::state::AppState;
use crate::worktree::Placement;
use crate::{label, status};

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
        let state = state.read(cx);
        let machine = state.selected().clone();
        let place = |info: &WorkspaceInfo| worktrees::placement(&machine, info, cx);
        let groups: Vec<Group> = groups(state, &plugin_actions, place, cx)
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
        dialog.w(px(600.)).p_0().close_button(false).child(palette)
    });
    palette_state
}

fn groups(
    state: &AppState,
    plugin_actions: &[PluginActionInfo],
    place: impl Fn(&WorkspaceInfo) -> Option<Placement>,
    cx: &App,
) -> Vec<Group> {
    let snapshot = state.snapshot(cx);
    let machine = || state.selected().clone();
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
                    machine: machine(),
                    pane_id: pane_id(),
                    direction: SplitDirection::Right,
                },
            ),
            herdr(
                "Pane: Split Down",
                C::SplitPane {
                    machine: machine(),
                    pane_id: pane_id(),
                    direction: SplitDirection::Down,
                },
            ),
            herdr(
                "Pane: Toggle Zoom",
                C::ZoomPane {
                    machine: machine(),
                    pane_id: pane_id(),
                },
            ),
            (
                "Pane: Rename…".into(),
                MenuCommand::Rename(MenuTarget::Pane {
                    machine: machine(),
                    pane_id: pane_id(),
                }),
            ),
            herdr(
                "Pane: Close",
                C::ClosePane {
                    machine: machine(),
                    pane_id: pane_id(),
                },
            ),
            herdr(
                "Tab: New",
                C::NewTab {
                    machine: machine(),
                    workspace_id: pane.workspace_id.clone(),
                },
            ),
            (
                "Tab: Rename…".into(),
                MenuCommand::Rename(MenuTarget::Tab {
                    machine: machine(),
                    tab_id: pane.tab_id.clone(),
                }),
            ),
            herdr(
                "Tab: Close",
                C::CloseTab {
                    machine: machine(),
                    tab_id: pane.tab_id.clone(),
                },
            ),
            (
                "Workspace: Rename…".into(),
                MenuCommand::Rename(MenuTarget::Workspace {
                    machine: machine(),
                    workspace_id: pane.workspace_id.clone(),
                }),
            ),
            herdr(
                "Workspace: Close",
                C::CloseWorkspace {
                    machine: machine(),
                    workspace_id: pane.workspace_id.clone(),
                },
            ),
        ]);
        let placed = snapshot
            .workspace(&pane.workspace_id)
            .and_then(|w| place(&w.info));
        let worktree = |label: &'static str, command: fn(_, _) -> WorktreeCommand| {
            (
                label.into(),
                MenuCommand::Worktree(command(machine(), pane.workspace_id.clone())),
            )
        };
        if placed.is_some() {
            app.push(worktree("Worktree: Open…", |machine, workspace_id| {
                WorktreeCommand::Open {
                    machine,
                    workspace_id,
                }
            }));
        }
        if placed.is_some_and(|p| p.linked) {
            app.push(worktree("Worktree: Remove…", |machine, workspace_id| {
                WorktreeCommand::Remove {
                    machine,
                    workspace_id,
                }
            }));
        }
    }
    app.push((
        "View: Show Changes".into(),
        MenuCommand::Action(Rc::new(OpenChanges)),
    ));
    app.push((
        "View: Toggle Light/Dark".into(),
        MenuCommand::Action(Rc::new(ToggleTheme)),
    ));
    app.push((
        "Settings…".into(),
        MenuCommand::Action(Rc::new(OpenSettings)),
    ));
    app.push(("Quit grasslands".into(), MenuCommand::Action(Rc::new(Quit))));

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
                    label::agent(snapshot, agent),
                    status::text(agent.agent_status, &agent.state_labels)
                )
                .into(),
                MenuCommand::Action(Rc::new(FocusPane {
                    machine: machine(),
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
                    machine: machine(),
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
