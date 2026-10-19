//! The sidebar's panels: spaces, agents and files. They live in the window's
//! dock (`workspace::dock`), whose skin draws their title bars; see
//! docs/decisions/0011-sidebar-dock.md.

pub mod files;
pub mod workspaces;

use std::rc::Rc;

use gpui_kit::*;

use crate::state::AppState;

pub fn init(cx: &mut App) {
    workspaces::init(cx);
    files::init(cx);
}

/// The spaces panel, for the terminal area's script hooks.
struct Spaces(WeakEntity<workspaces::WorkspaceList>);

impl Global for Spaces {}

/// The spaces panel, if one is open.
pub fn spaces(cx: &App) -> Option<Entity<workspaces::WorkspaceList>> {
    cx.try_global::<Spaces>()?.0.upgrade()
}

/// The files panel, for the terminal area's script hooks.
struct Files(WeakEntity<files::FileTree>);

impl Global for Files {}

/// The files panel, if one is open.
pub fn files(cx: &App) -> Option<Entity<files::FileTree>> {
    cx.try_global::<Files>()?.0.upgrade()
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
        #[cfg(debug_assertions)]
        workspaces::start_script(spaces.downgrade(), agents.downgrade(), window, cx);
        let list = spaces.downgrade();
        crate::chrome::set_placements(
            cx,
            Rc::new(move |machine, info, cx| list.upgrade()?.read(cx).placement_on(machine, info, cx)),
        );
        cx.set_global(Spaces(spaces.downgrade()));
        cx.set_global(Files(files.downgrade()));
        Self {
            spaces,
            agents,
            files,
        }
    }
}
