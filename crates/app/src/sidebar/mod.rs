//! The sidebar's panels: spaces, agents and files. They live in the window's
//! dock (`workspace::dock`), whose skin draws their title bars; see
//! docs/decisions/0011-sidebar-dock.md.

pub mod files;
pub mod workspaces;

use gpui_kit::*;

use crate::state::AppState;

pub fn init(cx: &mut App) {
    workspaces::init(cx);
    files::init(cx);
}

/// The three sidebar panels.
pub struct SidebarPanels {
    pub spaces: Entity<workspaces::WorkspaceList>,
    pub agents: Entity<workspaces::AgentList>,
    pub files: Entity<files::FileTree>,
}

impl SidebarPanels {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut App) -> Self {
        let spaces = cx.new(|cx| workspaces::WorkspaceList::new(state.clone(), cx));
        let agents = cx.new(|cx| workspaces::AgentList::new(state.clone(), cx));
        let files = cx.new(|cx| files::FileTree::new(state, window, cx));
        workspaces::start_script(spaces.downgrade(), agents.downgrade(), window, cx);
        Self {
            spaces,
            agents,
            files,
        }
    }
}
