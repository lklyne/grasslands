//! herdr commands the chrome offers in menus and the palette, as data, so an
//! entry can be confirmed before it runs. Each maps onto an `AppState` method.
//! Every command names the machine it was built for: herdr ids repeat across
//! machines, and the selection can move while a confirmation is open.

use gpui_kit::*;

use super::confirm::{group_members, herdr_closes_exactly};
use super::worktrees::herdr_placement;
use crate::herdr::{
    self, PaneId, PluginActionInfo, PluginInvocationContext, SplitDirection, TabId, WorkspaceId,
};
use crate::state::{AppState, MachineId};

#[derive(Debug, Clone)]
pub enum HerdrCommand {
    /// Selects the machine first when it isn't selected.
    FocusPane {
        machine: MachineId,
        pane_id: PaneId,
    },
    SplitPane {
        machine: MachineId,
        pane_id: PaneId,
        direction: SplitDirection,
    },
    ZoomPane {
        machine: MachineId,
        pane_id: PaneId,
    },
    RenamePane {
        machine: MachineId,
        pane_id: PaneId,
        label: String,
    },
    ClosePane {
        machine: MachineId,
        pane_id: PaneId,
    },
    NewTab {
        machine: MachineId,
        workspace_id: WorkspaceId,
    },
    /// Selects the machine first when it isn't selected.
    FocusTab {
        machine: MachineId,
        tab_id: TabId,
    },
    RenameTab {
        machine: MachineId,
        tab_id: TabId,
        label: String,
    },
    CloseTab {
        machine: MachineId,
        tab_id: TabId,
    },
    /// Selects the machine first when it isn't selected.
    FocusWorkspace {
        machine: MachineId,
        workspace_id: WorkspaceId,
    },
    RenameWorkspace {
        machine: MachineId,
        workspace_id: WorkspaceId,
        label: String,
    },
    CloseWorkspace {
        machine: MachineId,
        workspace_id: WorkspaceId,
    },
    /// Closes the worktree group `workspace_id` heads: exactly `members`
    /// (the ones the confirmation lists), with herdr's `close_group` when
    /// herdr's own worktree info makes the same group, else one by one.
    CloseGroup {
        machine: MachineId,
        workspace_id: WorkspaceId,
        repo_name: String,
        members: Vec<WorkspaceId>,
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
            | Self::CloseGroup { machine, .. }
            | Self::InvokePluginAction { machine, .. } => machine,
        }
    }

    /// Sends the command; the cache follows herdr's events, not this call.
    pub fn send(self, state: &Entity<AppState>, cx: &mut App) -> Task<herdr::Result<()>> {
        match self {
            Self::FocusPane { machine, pane_id } => {
                state.update(cx, |state, cx| state.focus_pane(&machine, &pane_id, cx))
            }
            Self::FocusTab { machine, tab_id } => {
                state.update(cx, |state, cx| state.focus_tab(&machine, &tab_id, cx))
            }
            Self::FocusWorkspace {
                machine,
                workspace_id,
            } => state.update(cx, |state, cx| {
                state.focus_workspace(&machine, &workspace_id, cx)
            }),
            Self::SplitPane {
                machine,
                pane_id,
                direction,
            } => {
                let task = state
                    .read(cx)
                    .split_pane(&machine, Some(&pane_id), direction, cx);
                cx.background_executor()
                    .spawn(async move { task.await.map(drop) })
            }
            Self::ZoomPane { machine, pane_id } => state.read(cx).call(&machine, cx, move |c| {
                c.pane_zoom(&pane_id, "toggle").map(drop)
            }),
            Self::RenamePane {
                machine,
                pane_id,
                label,
            } => state
                .read(cx)
                .call(&machine, cx, move |c| c.pane_rename(&pane_id, Some(&label))),
            Self::ClosePane { machine, pane_id } => {
                state.read(cx).close_pane(&machine, &pane_id, cx)
            }
            Self::NewTab {
                machine,
                workspace_id,
            } => {
                let task = state.read(cx).new_tab(&machine, Some(&workspace_id), cx);
                cx.background_executor()
                    .spawn(async move { task.await.map(drop) })
            }
            Self::RenameTab {
                machine,
                tab_id,
                label,
            } => state.read(cx).rename_tab(&machine, &tab_id, &label, cx),
            Self::CloseTab { machine, tab_id } => state.read(cx).close_tab(&machine, &tab_id, cx),
            Self::RenameWorkspace {
                machine,
                workspace_id,
                label,
            } => state
                .read(cx)
                .rename_workspace(&machine, &workspace_id, &label, cx),
            Self::CloseWorkspace {
                machine,
                workspace_id,
            } => state
                .read(cx)
                .close_workspace(&machine, &workspace_id, false, cx),
            Self::CloseGroup {
                machine,
                workspace_id,
                mut members,
                ..
            } => {
                let state = state.read(cx);
                let herdr_group = state.machine(&machine).is_some_and(|m| {
                    let herdr = group_members(m.snapshot(), &workspace_id, herdr_placement);
                    herdr_closes_exactly(&members, &herdr)
                });
                if herdr_group {
                    state.close_workspace(&machine, &workspace_id, true, cx)
                } else {
                    // The head last, as herdr closes a group.
                    members.retain(|id| id != &workspace_id);
                    members.push(workspace_id);
                    state.close_workspaces(&machine, members, cx)
                }
            }
            Self::InvokePluginAction {
                machine,
                action,
                context,
            } => state
                .read(cx)
                .invoke_plugin_action(&machine, &action, *context, cx),
        }
    }
}
