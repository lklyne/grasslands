//! The main area's tab strip: the focused workspace's herdr tabs, which
//! drag along the strip to reorder and take dropped panes, and the tab
//! commands (pick, cycle, close).

use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chrome::{Confirmation, MenuTarget};
use crate::herdr::{AgentStatus, PaneMoveDestination, SplitDirection, TabId, TabInfo, WorkspaceId};
use crate::status;

use super::drag::{DraggedPane, DraggedTab, tab_insert_index};
use super::{TerminalArea, record_bounds};

impl TerminalArea {
    /// The main area's tab strip: the focused workspace's herdr tabs, then
    /// `files` (the editor's tabs, drawn by the workspace), then +. The
    /// workspace draws it above whichever of the terminals or the editor is
    /// showing; `herdr_selected` says which.
    pub fn render_tab_bar(
        &self,
        herdr_selected: bool,
        files: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.drawn.borrow_mut().tabs.clear();
        let snapshot = self.snapshot(cx);
        let active = Self::active_tab(&snapshot);
        let workspace = active.and_then(|tab| snapshot.workspace(&tab.info.workspace_id));
        let workspace_id = workspace.map(|w| w.info.workspace_id.clone());
        let tabs: Vec<TabInfo> = workspace
            .map(|w| w.tabs.iter().map(|tab| tab.info.clone()).collect())
            .unwrap_or_default();
        let active_id = active
            .filter(|_| herdr_selected)
            .map(|tab| tab.info.tab_id.clone());
        let order: Rc<[TabId]> = tabs.iter().map(|tab| tab.tab_id.clone()).collect();
        let tabs: Vec<_> = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| {
                let is_active = active_id.as_ref() == Some(&tab.tab_id);
                self.render_tab(tab, index, is_active, order.clone(), cx)
            })
            .collect();
        let new_tab = self.render_new_tab(workspace_id, cx);
        let theme = cx.theme();

        h_flex()
            // The dock's 30px title bar plus its panels' 1px top border, so
            // the strip's bottom line meets theirs.
            .h(px(31.))
            .flex_shrink_0()
            // The strip wears the panel headers' background.
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            // Tabs past the strip's width scroll sideways (a vertical wheel
            // scrolls it too); + stays pinned at the end.
            .child(
                h_flex()
                    .id("tab-strip")
                    .h_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .children(tabs)
                    .children(files),
            )
            .child(new_tab)
            .into_any_element()
    }

    /// The tab at `index` in the strip, whose tabs are `order`.
    fn render_tab(
        &self,
        tab: TabInfo,
        index: usize,
        is_active: bool,
        order: Rc<[TabId]>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        // The selected tab takes a neutral fill.
        let (active_bg, muted, border, drop, drag_border) = (
            theme.secondary,
            theme.muted_foreground,
            theme.border,
            theme.drop_target,
            theme.drag_border,
        );
        let (focus_id, close_id, drop_id, menu_id, accept_id) = (
            tab.tab_id.clone(),
            tab.tab_id.clone(),
            tab.tab_id.clone(),
            tab.tab_id.clone(),
            tab.tab_id.clone(),
        );
        // herdr's TUI numbers tabs by place; `number` stays put when a tab
        // moves.
        let label = if tab.label.is_empty() {
            (index + 1).to_string()
        } else {
            tab.label.clone()
        };
        let dragged = DraggedTab {
            tab_id: tab.tab_id.clone(),
            index,
            label: label.clone().into(),
        };
        let area = cx.entity().downgrade();
        h_flex()
            .id(SharedString::from(format!("tab-{}", tab.tab_id)))
            .relative()
            .flex_shrink_0()
            .child(record_bounds(&self.drawn, tab.tab_id.clone(), |d| {
                &mut d.tabs
            }))
            .group("tab")
            .h_full()
            .px_3()
            .gap_2()
            .items_center()
            .border_r_1()
            .border_color(border)
            .map(|el| {
                if is_active {
                    el.bg(active_bg)
                } else {
                    el.text_color(muted)
                }
            })
            // An idle tab shows no dot; its muted label says enough.
            .when(
                !matches!(tab.agent_status, AgentStatus::Idle | AgentStatus::Unknown),
                |el| el.child(status::indicator(tab.agent_status, cx)),
            )
            .child(label)
            .child(
                div()
                    .id(SharedString::from(format!("tab-close-{}", tab.tab_id)))
                    .invisible()
                    .group_hover("tab", |s| s.visible())
                    .text_color(muted)
                    .child("×")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab_confirmed(close_id.clone(), window, cx);
                    })),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                window.dispatch_action(Box::new(crate::workspace::ShowHerdr), cx);
                this.pick_tab(&focus_id, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.context_menu(
                        MenuTarget::Tab {
                            machine: this.machine(cx),
                            tab_id: menu_id.clone(),
                        },
                        event.position,
                        window,
                        cx,
                    );
                }),
            )
            .on_drag(dragged, move |dragged, _, _, cx| {
                area.update(cx, |this, _| this.tab_drag_tick.reset()).ok();
                crate::sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                crate::workspace::keep_terminals_during_drag(cx);
                cx.new(|_| dragged.clone())
            })
            .can_drop(move |dragged, _, _| {
                dragged
                    .downcast_ref::<DraggedTab>()
                    .is_none_or(|d| d.tab_id != accept_id)
            })
            // The line marks the side the tab will land on.
            .drag_over::<DraggedTab>(move |style, dragged, _, _| {
                let style = style.border_color(drag_border);
                if dragged.index < index {
                    style.border_r_2()
                } else {
                    style.border_l_2()
                }
            })
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        this.tab_drag_tick_over(event.drag(cx).index, index);
                    }
                }),
            )
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                this.tab_drag_tick.reset();
                // Where the tab is now: tabs may have come or gone, or the
                // bar shows another workspace, since the drag began.
                let Some(from) = order.iter().position(|id| *id == dragged.tab_id) else {
                    return;
                };
                this.session
                    .read(cx)
                    .move_tab(&dragged.tab_id, tab_insert_index(from, index), cx)
                    .detach_and_log_err(cx);
            }))
            .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                let destination = PaneMoveDestination::Tab {
                    tab_id: drop_id.clone(),
                    split: SplitDirection::Right,
                    target_pane_id: None,
                    ratio: None,
                };
                this.move_pane_to(&dragged.pane_id, destination, cx);
            }))
            .into_any_element()
    }

    /// `+`: a new tab in `workspace_id`; drop a pane on it to move the pane
    /// to a new tab.
    fn render_new_tab(
        &self,
        workspace_id: Option<WorkspaceId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let drop = theme.drop_target;
        div()
            .id("new-tab")
            .flex_shrink_0()
            .px_3()
            .h_full()
            .flex()
            .items_center()
            .text_color(theme.muted_foreground)
            .child("+")
            .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
            .on_click({
                let workspace_id = workspace_id.clone();
                cx.listener(move |this, _, _, cx| {
                    this.session
                        .read(cx)
                        .new_tab(workspace_id.as_ref(), cx)
                        .detach_and_log_err(cx);
                })
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                let destination = PaneMoveDestination::NewTab {
                    workspace_id: workspace_id.clone(),
                    label: None,
                };
                this.move_pane_to(&dragged.pane_id, destination, cx);
            }))
            .into_any_element()
    }

    fn move_pane_to(
        &self,
        pane_id: &crate::herdr::PaneId,
        destination: PaneMoveDestination,
        cx: &mut Context<Self>,
    ) {
        self.session
            .read(cx)
            .move_pane(pane_id, destination, cx)
            .detach_and_log_err(cx);
    }

    /// Ticks as a drag from tab `from` enters tab `over`, a semitone higher
    /// for each tab further right than where it started (lower going left).
    fn tab_drag_tick_over(&mut self, from: usize, over: usize) {
        if let Some(semitones) = self.tab_drag_tick.over(from, over) {
            let options = cues::PlayOptions::default().pitch(2f64.powf(semitones / 12.0));
            crate::sounds::play(cues::Cue::Select, options);
        }
    }

    /// Shows `tab_id` and gives its pane keyboard focus, now if it is
    /// already on screen, else when herdr's snapshot shows it.
    fn pick_tab(&mut self, tab_id: &TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown_tab.as_deref() == Some(tab_id) {
            if let Some(pane_id) = self.target_pane(cx) {
                self.focus_terminal(&pane_id, window, cx);
            }
            return;
        }
        self.focus_on_show = Some(tab_id.to_owned());
        let machine = self.machine(cx);
        self.state
            .update(cx, |state, cx| state.focus_tab(&machine, tab_id, cx))
            .detach_and_log_err(cx);
    }

    pub(super) fn cycle_tab(&mut self, step: isize, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let Some(active) = Self::active_tab(&snapshot) else {
            return;
        };
        let Some(workspace) = snapshot.workspace(&active.info.workspace_id) else {
            return;
        };
        let count = workspace.tabs.len() as isize;
        let Some(index) = workspace
            .tabs
            .iter()
            .position(|tab| tab.info.tab_id == active.info.tab_id)
        else {
            return;
        };
        let next = (index as isize + step).rem_euclid(count) as usize;
        let tab_id = workspace.tabs[next].info.tab_id.clone();
        let machine = self.machine(cx);
        self.state
            .update(cx, |state, cx| state.focus_tab(&machine, &tab_id, cx))
            .detach_and_log_err(cx);
    }

    pub(super) fn close_tab_confirmed(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(cx);
        let busy = snapshot
            .panes
            .values()
            .filter(|pane| pane.tab_id == tab_id && status::is_busy(pane.agent_status))
            .count();
        let session = self.session.clone();
        if busy == 0 {
            session
                .read(cx)
                .close_tab(&tab_id, cx)
                .detach_and_log_err(cx);
            return;
        }
        let message = format!(
            "{busy} running agent{} in it will stop.",
            if busy == 1 { "" } else { "s" }
        );
        if let Some(chrome) = crate::chrome::get(window, cx) {
            let confirmation = Confirmation {
                title: "Close this tab?".into(),
                message,
                confirm_label: "Close Tab".into(),
            };
            let on_confirm = move |_: &mut Window, cx: &mut App| {
                session
                    .read(cx)
                    .close_tab(&tab_id, cx)
                    .detach_and_log_err(cx);
            };
            chrome.update(cx, |chrome, cx| {
                chrome.confirm(confirmation, on_confirm, window, cx);
            });
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Close this tab?",
            Some(&message),
            &["Close Tab", "Cancel"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                let task = cx.update(|cx| session.read(cx).close_tab(&tab_id, cx));
                if let Err(error) = task.await {
                    tracing::error!("close tab {tab_id}: {error}");
                }
            }
        })
        .detach();
    }
}
