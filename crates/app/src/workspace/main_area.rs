//! The main area: one tab strip (herdr's tabs for the focused space, then
//! the open files, then Changes, then +) over the terminals, the editor or
//! the diff.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{MainTab, Workspace};

/// The view the dock's main panel shows. It draws through the workspace,
/// which owns the terminal area, the editor and which of them is showing.
pub struct MainArea {
    workspace: WeakEntity<Workspace>,
}

impl MainArea {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self { workspace }
    }
}

impl Render for MainArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        workspace.update(cx, |workspace, cx| workspace.render_main(cx))
    }
}

/// A canvas that hands its parent's bounds to `record` during prepaint.
fn record(record: impl Fn(Bounds<Pixels>) + 'static) -> impl IntoElement {
    canvas(move |bounds, _, _| record(bounds), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

impl Workspace {
    fn render_main(&mut self, cx: &mut Context<Self>) -> AnyElement {
        {
            let mut drawn = self.drawn_tabs.borrow_mut();
            drawn.tabs.clear();
            drawn.closes.clear();
        }
        let herdr = self.selected == MainTab::Herdr;
        let mut files = self.render_file_tabs(cx);
        files.extend(self.render_changes_tab(cx));
        let drawn = self.drawn_tabs.clone();
        let strip = self
            .terminal_area
            .update(cx, |area, cx| area.render_tab_bar(herdr, files, cx));
        let body: AnyView = match (self.selected, &self.changes) {
            (MainTab::Changes, Some(changes)) => changes.clone().into(),
            (MainTab::Files, _) => self.editor.clone().into(),
            _ => self.terminal_area.clone().into(),
        };
        v_flex()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(strip)
                    // Index 0 for the script hooks: the herdr tabs, from the
                    // strip's left edge.
                    .child(
                        canvas(
                            move |bounds, _, _| drawn.borrow_mut().tabs.insert(0, bounds),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(40.))
                        .h_full(),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// The editor's open files as strip tabs; clicking one shows it.
    fn render_file_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let warning = cx.theme().warning;
        self.editor
            .read(cx)
            .tabs(cx)
            .into_iter()
            .enumerate()
            .map(|(file, tab)| {
                let label = h_flex().gap_1().child(tab.title).when(tab.dirty, |this| {
                    this.child(div().text_color(warning).child("●"))
                });
                self.render_tab(file + 1, label, cx, move |this, window, cx| {
                    this.editor
                        .update(cx, |editor, cx| editor.request_close(file, window, cx));
                })
            })
            .collect()
    }

    /// The Changes tab, while it is open.
    fn render_changes_tab(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let index = self.changes_index(cx)?;
        let muted = cx.theme().muted_foreground;
        let label = h_flex()
            .gap_1()
            .child(
                Icon::new(assets::IconName::GitCompare)
                    .xsmall()
                    .text_color(muted),
            )
            .child("Changes");
        Some(self.render_tab(index, label, cx, |this, window, cx| {
            this.close_changes(window, cx)
        }))
    }

    /// One strip tab at titlebar `index`: clicking shows it, its close
    /// button runs `close`.
    fn render_tab(
        &self,
        index: usize,
        label: impl IntoElement,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let selected = self.selected_index(cx) == index;
        let theme = cx.theme();
        let (active_bg, muted, border) = (theme.secondary, theme.muted_foreground, theme.border);
        let (tabs, closes) = (self.drawn_tabs.clone(), self.drawn_tabs.clone());
        h_flex()
            .id(("file-tab", index))
            .relative()
            .child(record(move |bounds| tabs.borrow_mut().tabs.push(bounds)))
            .h_full()
            .pl_3()
            .pr_1()
            .gap_1()
            .items_center()
            .border_r_1()
            .border_color(border)
            .map(|this| {
                if selected {
                    this.bg(active_bg)
                } else {
                    this.text_color(muted)
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_index(index, window, cx);
            }))
            .child(label)
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
                                close(this, window, cx);
                            })),
                    )
                    .child(record(move |bounds| {
                        closes.borrow_mut().closes.push((index, bounds))
                    })),
            )
            .into_any_element()
    }
}
