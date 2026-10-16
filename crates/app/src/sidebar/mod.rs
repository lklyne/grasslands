//! The sidebar: the workspace list (with each workspace's agents) above the
//! file tree, in a vertical split.

pub mod files;
pub mod workspaces;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::*;

use crate::state::AppState;

pub fn init(cx: &mut App) {
    workspaces::init(cx);
    files::init(cx);
}

pub struct Sidebar {
    workspaces: Entity<workspaces::WorkspaceList>,
    files: Entity<files::FileTree>,
}

impl Sidebar {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            workspaces: cx.new(|cx| workspaces::WorkspaceList::new(state.clone(), window, cx)),
            files: cx.new(|cx| files::FileTree::new(state, window, cx)),
        }
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                v_resizable("sidebar-split")
                    .child(
                        resizable_panel()
                            .size(px(400.))
                            .size_range(px(80.)..px(800.))
                            .child(self.workspaces.clone()),
                    )
                    .child(resizable_panel().child(self.files.clone())),
            )
    }
}
