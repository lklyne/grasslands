//! The window's root view: lays out the sidebar, editor and terminal area in
//! resizable panels, and routes cross-module actions to the view that owns
//! them (actions bubble up the focus path to here, not sideways).

use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

use crate::actions::{CommandPalette, FocusPane, OpenContextMenu, OpenFile};
use crate::chrome::Chrome;
use crate::editor::Editor;
use crate::sidebar::Sidebar;
use crate::state::AppState;
use crate::terminal_area::TerminalArea;

pub struct Workspace {
    pub state: Entity<AppState>,
    sidebar: Entity<Sidebar>,
    editor: Entity<Editor>,
    terminal_area: Entity<TerminalArea>,
    chrome: Entity<Chrome>,
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            sidebar: cx.new(|cx| Sidebar::new(state.clone(), window, cx)),
            editor: cx.new(|cx| Editor::new(state.clone(), window, cx)),
            terminal_area: cx.new(|cx| TerminalArea::new(state.clone(), window, cx)),
            chrome: cx.new(|cx| Chrome::new(state.clone(), window, cx)),
            state,
            focus,
        }
    }

    fn open_file(&mut self, action: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.open(&action.path, window, cx));
    }

    fn focus_pane(&mut self, action: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_area
            .update(cx, |area, cx| area.focus_pane(&action.pane_id, window, cx));
    }

    fn command_palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome
            .update(cx, |chrome, cx| chrome.open_command_palette(window, cx));
    }

    fn open_context_menu(
        &mut self,
        action: &OpenContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chrome.update(cx, |chrome, cx| {
            chrome.context_menu(
                action.target.clone(),
                action.position,
                Vec::new(),
                window,
                cx,
            );
        });
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::focus_pane))
            .on_action(cx.listener(Self::command_palette))
            .on_action(cx.listener(Self::open_context_menu))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_resizable("workspace-columns")
                    .child(
                        resizable_panel()
                            .size(px(260.))
                            .size_range(px(160.)..px(600.))
                            .child(self.sidebar.clone()),
                    )
                    .child(resizable_panel().child(self.editor.clone()))
                    .child(resizable_panel().child(self.terminal_area.clone())),
            )
            .child(self.chrome.clone())
    }
}
