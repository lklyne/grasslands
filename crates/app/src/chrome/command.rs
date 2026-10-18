//! herdr commands the chrome offers in menus and the palette, as data, so an
//! entry can be confirmed before it runs. Each maps onto an `AppState` method.
//! Workspace commands name their machine (the sidebar lists every machine's
//! workspaces); pane and tab commands act on the selected machine, the only
//! one the terminal area shows.

use gpui_kit::*;

use crate::herdr::{self, PluginActionInfo, PluginInvocationContext, SplitDirection};
use crate::state::{AppState, MachineId};

#[derive(Debug, Clone)]
pub enum HerdrCommand {
    FocusPane {
        pane_id: String,
    },
    SplitPane {
        pane_id: String,
        direction: SplitDirection,
    },
    ZoomPane {
        pane_id: String,
    },
    RenamePane {
        pane_id: String,
        label: String,
    },
    ClosePane {
        pane_id: String,
    },
    NewTab {
        machine: MachineId,
        workspace_id: String,
    },
    FocusTab {
        tab_id: String,
    },
    RenameTab {
        tab_id: String,
        label: String,
    },
    CloseTab {
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
        action: Box<PluginActionInfo>,
        context: Box<PluginInvocationContext>,
    },
}

impl HerdrCommand {
    /// The machine a workspace command acts on; `None` for the selected one.
    pub fn machine(&self) -> Option<&MachineId> {
        match self {
            Self::NewTab { machine, .. }
            | Self::FocusWorkspace { machine, .. }
            | Self::RenameWorkspace { machine, .. }
            | Self::CloseWorkspace { machine, .. } => Some(machine),
            _ => None,
        }
    }

    /// Sends the command; the cache follows herdr's events, not this call.
    pub fn send(self, state: &Entity<AppState>, cx: &mut App) -> Task<herdr::Result<()>> {
        if let Self::FocusWorkspace { machine, .. } = &self {
            state.update(cx, |state, cx| state.select_machine(machine, cx));
        }
        let state = state.read(cx);
        match self {
            Self::FocusPane { pane_id } => state.focus_pane(&pane_id, cx),
            Self::SplitPane { pane_id, direction } => {
                let task = state.split_pane(Some(&pane_id), direction, cx);
                cx.background_executor()
                    .spawn(async move { task.await.map(drop) })
            }
            Self::ZoomPane { pane_id } => {
                state.call(cx, move |c| c.pane_zoom(&pane_id, "toggle").map(drop))
            }
            Self::RenamePane { pane_id, label } => {
                state.call(cx, move |c| c.pane_rename(&pane_id, Some(&label)))
            }
            Self::ClosePane { pane_id } => state.close_pane(&pane_id, cx),
            Self::NewTab {
                machine,
                workspace_id,
            } => {
                let task = state.new_tab_on(&machine, Some(&workspace_id), cx);
                cx.background_executor()
                    .spawn(async move { task.await.map(drop) })
            }
            Self::FocusTab { tab_id } => state.focus_tab(&tab_id, cx),
            Self::RenameTab { tab_id, label } => state.rename_tab(&tab_id, &label, cx),
            Self::CloseTab { tab_id } => state.close_tab(&tab_id, cx),
            Self::FocusWorkspace { workspace_id, .. } => state.focus_workspace(&workspace_id, cx),
            Self::RenameWorkspace {
                machine,
                workspace_id,
                label,
            } => state.rename_workspace_on(&machine, &workspace_id, &label, cx),
            Self::CloseWorkspace {
                machine,
                workspace_id,
                close_group,
            } => state.close_workspace_on(&machine, &workspace_id, close_group, cx),
            Self::InvokePluginAction { action, context } => {
                state.invoke_plugin_action(&action, *context, cx)
            }
        }
    }
}
