//! herdr commands the chrome offers in menus and the palette, as data, so an
//! entry can be confirmed before it runs. Each maps onto an `AppState` method.
//! Every command names the machine it was built for: herdr ids repeat across
//! machines, and the selection can move while a confirmation is open.

use gpui_kit::*;

use crate::herdr::{self, PluginActionInfo, PluginInvocationContext, SplitDirection};
use crate::state::{AppState, MachineId};

#[derive(Debug, Clone)]
pub enum HerdrCommand {
    /// Selects the machine first when it isn't selected.
    FocusPane {
        machine: MachineId,
        pane_id: String,
    },
    SplitPane {
        machine: MachineId,
        pane_id: String,
        direction: SplitDirection,
    },
    ZoomPane {
        machine: MachineId,
        pane_id: String,
    },
    RenamePane {
        machine: MachineId,
        pane_id: String,
        label: String,
    },
    ClosePane {
        machine: MachineId,
        pane_id: String,
    },
    NewTab {
        machine: MachineId,
        workspace_id: String,
    },
    /// Selects the machine first when it isn't selected.
    FocusTab {
        machine: MachineId,
        tab_id: String,
    },
    RenameTab {
        machine: MachineId,
        tab_id: String,
        label: String,
    },
    CloseTab {
        machine: MachineId,
        tab_id: String,
    },
    /// Selects the machine first when it isn't selected.
    FocusWorkspace {
        machine: MachineId,
        workspace_id: String,
    },
    RenameWorkspace {
        machine: MachineId,
        workspace_id: String,
        label: String,
    },
    /// `close_group` closes every workspace of the worktree group.
    CloseWorkspace {
        machine: MachineId,
        workspace_id: String,
        close_group: bool,
    },
    InvokePluginAction {
        machine: MachineId,
        action: Box<PluginActionInfo>,
        context: Box<PluginInvocationContext>,
    },
}

impl HerdrCommand {
    /// The machine the command acts on.
    pub fn machine(&self) -> &MachineId {
        match self {
            Self::FocusPane { machine, .. }
            | Self::SplitPane { machine, .. }
            | Self::ZoomPane { machine, .. }
            | Self::RenamePane { machine, .. }
            | Self::ClosePane { machine, .. }
            | Self::NewTab { machine, .. }
            | Self::FocusTab { machine, .. }
            | Self::RenameTab { machine, .. }
            | Self::CloseTab { machine, .. }
            | Self::FocusWorkspace { machine, .. }
            | Self::RenameWorkspace { machine, .. }
            | Self::CloseWorkspace { machine, .. }
            | Self::InvokePluginAction { machine, .. } => machine,
        }
    }

    /// Sends the command; the cache follows herdr's events, not this call.
    pub fn send(self, state: &Entity<AppState>, cx: &mut App) -> Task<herdr::Result<()>> {
        match self {
            Self::FocusPane { machine, pane_id } => {
                state.update(cx, |state, cx| state.focus_pane_on(&machine, &pane_id, cx))
            }
            Self::FocusTab { machine, tab_id } => {
                state.update(cx, |state, cx| state.focus_tab_on(&machine, &tab_id, cx))
            }
            Self::FocusWorkspace {
                machine,
                workspace_id,
            } => state.update(cx, |state, cx| {
                state.focus_workspace_on(&machine, &workspace_id, cx)
            }),
            Self::SplitPane {
                machine,
                pane_id,
                direction,
            } => {
                let task = state
                    .read(cx)
                    .split_pane_on(&machine, Some(&pane_id), direction, cx);
                cx.background_executor()
                    .spawn(async move { task.await.map(drop) })
            }
            Self::ZoomPane { machine, pane_id } => state.read(cx).call_on(&machine, cx, move |c| {
                c.pane_zoom(&pane_id, "toggle").map(drop)
            }),
            Self::RenamePane {
                machine,
                pane_id,
                label,
            } => state
                .read(cx)
                .call_on(&machine, cx, move |c| c.pane_rename(&pane_id, Some(&label))),
            Self::ClosePane { machine, pane_id } => {
                state.read(cx).close_pane_on(&machine, &pane_id, cx)
            }
            Self::NewTab {
                machine,
                workspace_id,
            } => {
                let task = state.read(cx).new_tab_on(&machine, Some(&workspace_id), cx);
                cx.background_executor()
                    .spawn(async move { task.await.map(drop) })
            }
            Self::RenameTab {
                machine,
                tab_id,
                label,
            } => state.read(cx).rename_tab_on(&machine, &tab_id, &label, cx),
            Self::CloseTab { machine, tab_id } => {
                state.read(cx).close_tab_on(&machine, &tab_id, cx)
            }
            Self::RenameWorkspace {
                machine,
                workspace_id,
                label,
            } => state
                .read(cx)
                .rename_workspace_on(&machine, &workspace_id, &label, cx),
            Self::CloseWorkspace {
                machine,
                workspace_id,
                close_group,
            } => state
                .read(cx)
                .close_workspace_on(&machine, &workspace_id, close_group, cx),
            Self::InvokePluginAction {
                machine,
                action,
                context,
            } => state
                .read(cx)
                .invoke_plugin_action_on(&machine, &action, *context, cx),
        }
    }
}
