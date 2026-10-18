//! The titlebar: GPUI Kit's `TitleBar` (transparent, unified with the
//! window, draggable, double-click zooms) showing the focused space's name.
//! The main area's tabs (herdr's and the open files) are in the strip above
//! it; see `main_area`.

use gpui_kit::component::{ActiveTheme as _, TitleBar};
use gpui_kit::*;

use super::Workspace;

/// Where each main-area tab and close button was drawn last frame, by index:
/// 0 is the herdr tabs, then the file tabs. Read by the script hooks to click
/// them.
#[derive(Default)]
pub struct DrawnTabs {
    pub tabs: Vec<Bounds<Pixels>>,
    pub closes: Vec<(usize, Bounds<Pixels>)>,
}

/// Window options for a window that draws [`Workspace`]'s titlebar.
pub fn window_options() -> WindowOptions {
    let mut options = TitleBar::window_options();
    if let Some(titlebar) = options.titlebar.as_mut() {
        // Shown in the Window menu and Mission Control, not drawn.
        titlebar.title = Some("grasslands".into());
    }
    options
}

impl Workspace {
    pub fn drawn_tabs(&self) -> std::cell::Ref<'_, DrawnTabs> {
        self.drawn_tabs.borrow()
    }

    pub(super) fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.state.read(cx).snapshot();
        let name = snapshot
            .focused_workspace_id
            .as_deref()
            .and_then(|id| snapshot.workspace(id))
            .map(|w| w.info.label.clone())
            .unwrap_or_default();
        TitleBar::new().child(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().foreground)
                .child(name),
        )
    }
}
