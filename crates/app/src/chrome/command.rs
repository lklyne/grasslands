//! herdr commands the chrome offers in menus and the palette, as data, so an
//! entry can be confirmed before it runs. Each maps onto an `AppState` method.

use gpui_kit::*;

use crate::herdr::{self, PluginActionInfo, PluginInvocationContext, SplitDirection};
use crate::state::AppState;

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
    FocusWorkspace {
        workspace_id: String,
    },
    RenameWorkspace {
        workspace_id: String,
        label: String,
    },
    /// `close_group` closes every workspace of the worktree group.
    CloseWorkspace {
        workspace_id: String,
        close_group: bool,
    },
    InvokePluginAction {
        action: Box<PluginActionInfo>,
        context: Box<PluginInvocationContext>,
    },
}

impl HerdrCommand {
    /// Sends the command; the cache follows herdr's events, not this call.
    pub fn send(self, state: &AppState, cx: &App) -> Task<herdr::Result<()>> {
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
            Self::NewTab { workspace_id } => {
                let task = state.new_tab(Some(&workspace_id), cx);
                cx.background_executor()
                    .spawn(async move { task.await.map(drop) })
            }
            Self::FocusTab { tab_id } => state.focus_tab(&tab_id, cx),
            Self::RenameTab { tab_id, label } => state.rename_tab(&tab_id, &label, cx),
            Self::CloseTab { tab_id } => state.close_tab(&tab_id, cx),
            Self::FocusWorkspace { workspace_id } => state.focus_workspace(&workspace_id, cx),
            Self::RenameWorkspace {
                workspace_id,
                label,
            } => state.rename_workspace(&workspace_id, &label, cx),
            Self::CloseWorkspace {
                workspace_id,
                close_group,
            } => state.close_workspace(&workspace_id, close_group, cx),
            Self::InvokePluginAction { action, context } => {
                state.invoke_plugin_action(&action, *context, cx)
            }
        }
    }
}
