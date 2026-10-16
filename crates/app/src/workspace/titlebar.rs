//! The titlebar: GPUI Kit's `TitleBar` (transparent, unified with the
//! window, draggable, double-click zooms) holding the main area's tabs. The
//! first tab is herdr and can't be closed; the rest are the editor's open
//! files for the active worktree.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::Workspace;

/// Where each titlebar tab and close button was drawn last frame, by
/// titlebar index (0 is herdr). Read by the script hooks to click them.
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
        titlebar.title = Some("rusted-tools".into());
    }
    options
}

impl Workspace {
    pub fn drawn_tabs(&self) -> std::cell::Ref<'_, DrawnTabs> {
        self.drawn_tabs.borrow()
    }

    pub(super) fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        {
            let mut drawn = self.drawn_tabs.borrow_mut();
            drawn.tabs.clear();
            drawn.closes.clear();
        }
        let selected = self.selected_index(cx);
        let files = self.editor.read(cx).tabs();
        let herdr = self.render_tab(0, selected == 0, cx).child("herdr");
        let file_tabs = files.into_iter().enumerate().map(|(file, tab)| {
            let index = file + 1;
            let drawn = self.drawn_tabs.clone();
            self.render_tab(index, selected == index, cx)
                .child(tab.title)
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .when(tab.dirty, |this| {
                            this.child(div().text_color(cx.theme().warning).child("●"))
                        })
                        .child(
                            div()
                                .relative()
                                .child(
                                    Button::new(("close-file-tab", index))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.editor.update(cx, |editor, cx| {
                                                editor.request_close(file, window, cx)
                                            });
                                        })),
                                )
                                .child(
                                    canvas(
                                        move |bounds, _, _| {
                                            drawn.borrow_mut().closes.push((index, bounds))
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                                ),
                        ),
                )
        });
        TitleBar::new().child(
            h_flex()
                .id("titlebar-tabs")
                .h_full()
                .min_w_0()
                .flex_1()
                .gap_1()
                .items_center()
                .overflow_x_scroll()
                .child(herdr)
                .children(file_tabs),
        )
    }

    /// One tab's frame. Mouse-down is stopped here so pressing a tab doesn't
    /// start a window move, and clicks so a double-click doesn't zoom.
    fn render_tab(&self, index: usize, selected: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let drawn = self.drawn_tabs.clone();
        h_flex()
            .id(("titlebar-tab", index))
            .relative()
            .flex_shrink_0()
            .h(px(26.))
            .pl_3()
            .pr(if index == 0 { px(12.) } else { px(4.) })
            .gap_1()
            .items_center()
            .rounded_md()
            .text_sm()
            .border_1()
            .map(|this| {
                if selected {
                    this.bg(theme.background)
                        .border_color(theme.border)
                        .text_color(theme.foreground)
                } else {
                    this.border_color(transparent_black())
                        .text_color(theme.muted_foreground)
                        .hover(|style| style.bg(theme.secondary_hover))
                }
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.select_index(index, window, cx);
            }))
            .child(
                canvas(
                    move |bounds, _, _| drawn.borrow_mut().tabs.push(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}
