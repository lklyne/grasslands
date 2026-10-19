//! The active tab's split tree: splits with a draggable divider between
//! their sides, and panes with a header and the terminal's slot. The native
//! Ghostty view is placed over its slot (decision 0010); a pane whose attach
//! ended shows why, with Reattach.

use std::collections::HashSet;
use std::path::PathBuf;
use std::slice;
use std::sync::Arc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chrome::MenuTarget;
use crate::herdr::{PaneId, PaneInfo, Snapshot, SplitDirection, SplitTree, TabId, TerminalId};
use crate::sidebar::files::DraggedPath;
use crate::{label, status};

use super::drag::{DraggedPane, NoPreview, PaneDrop, SplitKey, pane_drop};
use super::{GAP, HEADER_HEIGHT, TerminalArea, record_bounds};

/// Per-render bookkeeping.
pub(super) struct Frame {
    pub snapshot: Arc<Snapshot>,
    /// Terminal ids laid out this render.
    pub visible: HashSet<TerminalId>,
}

impl TerminalArea {
    pub(super) fn render_node(
        &self,
        tab_id: &TabId,
        node: &SplitTree,
        path: &mut Vec<bool>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            // A pane alone in its tab (or zoomed) needs no header: the tab
            // already shows its status, label and close button.
            SplitTree::Pane(pane_id) => self.render_pane(pane_id, !path.is_empty(), frame, cx),
            SplitTree::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let key = SplitKey {
                    tab_id: tab_id.to_owned(),
                    path: path.clone(),
                };
                path.push(false);
                let first = self.render_node(tab_id, first, path, frame, cx);
                path.pop();
                path.push(true);
                let second = self.render_node(tab_id, second, path, frame, cx);
                path.pop();
                self.render_split(key, *direction, *ratio, first, second, cx)
            }
        }
    }

    fn render_split(
        &self,
        key: SplitKey,
        direction: SplitDirection,
        ratio: f32,
        first: AnyElement,
        second: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let horizontal = direction == SplitDirection::Right;
        let side = |grow: f32, child: AnyElement| {
            let mut side = div()
                .relative()
                .min_w_0()
                .min_h_0()
                .when(horizontal, |side| side.h_full())
                .when(!horizontal, |side| side.w_full())
                .child(child);
            let style = side.style();
            style.flex_grow = Some(grow);
            style.flex_shrink = Some(1.);
            style.flex_basis = Some(px(0.).into());
            side
        };
        let dragging = self.overrides.is_dragging(&key);
        let theme = cx.theme();
        let (idle, hot) = (theme.border, theme.primary);
        let bounds = record_bounds(&self.drawn, key.clone(), |d| &mut d.splits);
        let divider = div()
            .id(key.element_id())
            .flex_none()
            .when(horizontal, |d| d.w(GAP).h_full().cursor_col_resize())
            .when(!horizontal, |d| d.h(GAP).w_full().cursor_row_resize())
            .child(
                // A hairline in the middle of the gap; the whole gap is the handle.
                div()
                    .absolute()
                    .when(horizontal, |d| d.left(px(2.)).w(px(1.)).h_full())
                    .when(!horizontal, |d| d.top(px(2.)).h(px(1.)).w_full())
                    .bg(if dragging { hot } else { idle }),
            )
            .relative()
            .hover(move |style| style.bg(hot.opacity(0.35)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.overrides.begin_drag(key.clone(), direction);
                    cx.notify();
                }),
            );
        // Plain flex, not h_flex/v_flex: those center their items, which
        // would collapse the sides to their content size.
        div()
            .flex()
            .when(horizontal, |c| c.flex_row())
            .when(!horizontal, |c| c.flex_col())
            .size_full()
            .relative()
            .child(bounds)
            .child(side(ratio, first))
            .child(divider)
            .child(side(1. - ratio, second))
            .into_any_element()
    }

    fn render_pane(
        &self,
        pane_id: &PaneId,
        with_header: bool,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let snapshot = frame.snapshot.clone();
        let Some(pane) = snapshot.pane(pane_id) else {
            return div().into_any_element();
        };
        let focused = snapshot.focused_pane_id.as_deref() == Some(pane_id);
        let terminal_id = &pane.terminal_id;
        let attached = self.terminals.is_attached(terminal_id, cx);
        if attached {
            frame.visible.insert(terminal_id.clone());
        }
        let terminal = match (self.terminals.get(terminal_id), attached) {
            (Some(view), true) => view.terminal.clone().into_any_element(),
            _ => self.render_detached(pane, cx),
        };
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .when(with_header, |el| {
                el.child(self.render_pane_header(pane, focused, cx))
            })
            .child(self.render_pane_body(pane_id, focused, terminal, cx))
            .into_any_element()
    }

    /// In place of a terminal whose attach ended, or never started.
    fn render_detached(&self, pane: &PaneInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let pane_id = &pane.pane_id;
        let message = self
            .terminals
            .failure(&pane.terminal_id)
            .cloned()
            .unwrap_or_else(|| "Detached: another client has this terminal.".to_owned());
        let reattach_pane = pane.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(message)
            .child(
                div()
                    .id(SharedString::from(format!("reattach-{pane_id}")))
                    .relative()
                    .child(record_bounds(&self.drawn, pane_id.to_owned(), |d| {
                        &mut d.reattach
                    }))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .child("Reattach")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reattach_pane(&reattach_pane, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// Status, label and close button; drag it to move the pane, right-click
    /// it for the pane's menu.
    fn render_pane_header(
        &self,
        pane: &PaneInfo,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let pane_id = &pane.pane_id;
        let label: SharedString = label::pane_header(pane).into_owned().into();
        let (drop, muted) = (theme.drop_target, theme.muted_foreground);
        let (close_pane, drop_pane, menu_pane) =
            (pane_id.to_owned(), pane_id.to_owned(), pane_id.to_owned());
        let area = cx.entity().downgrade();
        let dragged = DraggedPane {
            pane_id: pane_id.to_owned(),
            label: label.clone(),
        };
        h_flex()
            .id(SharedString::from(format!("pane-header-{pane_id}")))
            .relative()
            .child(record_bounds(&self.drawn, pane_id.to_owned(), |d| {
                &mut d.headers
            }))
            .h(HEADER_HEIGHT)
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .text_sm()
            .bg(if focused {
                theme.tab_active
            } else {
                theme.tab_bar
            })
            .text_color(if focused { theme.foreground } else { muted })
            .child(status::indicator(pane.agent_status, cx))
            .child(div().flex_1().min_w_0().overflow_hidden().child(label))
            // herdr couldn't resume the pane's agent after a restart.
            .children(pane.restore_error.as_ref().map(|error| {
                div()
                    .id(SharedString::from(format!("restore-error-{pane_id}")))
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.danger)
                    .child("Not restored")
                    .tooltip({
                        let error = SharedString::from(error.clone());
                        move |window, cx| Tooltip::new(error.clone()).build(window, cx)
                    })
            }))
            .child(
                div()
                    .id(SharedString::from(format!("pane-close-{pane_id}")))
                    .px_1()
                    .child("×")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_pane_confirmed(close_pane.clone(), window, cx);
                    })),
            )
            .on_drag(dragged, move |dragged, _, window, cx| {
                crate::sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                crate::workspace::keep_terminals_during_drag(cx);
                let label = dragged.label.clone();
                let position = window.mouse_position();
                area.update(cx, |area, cx| {
                    area.start_pane_drag(label, position, window, cx)
                })
                .ok();
                cx.new(|_| NoPreview)
            })
            .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                this.drop_pane(&dragged.pane_id, &drop_pane, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.context_menu(
                        MenuTarget::Pane {
                            machine: this.state.read(cx).selected().clone(),
                            pane_id: menu_pane.clone(),
                        },
                        event.position,
                        window,
                        cx,
                    );
                }),
            )
            .into_any_element()
    }

    /// The terminal's slot, ringed when focused: a drop target for panes
    /// and files, and a click focuses the pane in herdr.
    fn render_pane_body(
        &self,
        pane_id: &PaneId,
        focused: bool,
        terminal: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (ring, drop) = (theme.ring, theme.drop_target);
        let (drop_pane, click_pane) = (pane_id.to_owned(), pane_id.to_owned());
        let (paste_pane, paste_file_pane) = (pane_id.to_owned(), pane_id.to_owned());
        div()
            .id(SharedString::from(format!("pane-body-{pane_id}")))
            .flex_1()
            .min_h_0()
            .relative()
            // The ring sits in the 1px the native view leaves uncovered.
            .border_1()
            .border_color(if focused { ring } else { transparent_black() })
            .child(record_bounds(&self.drawn, pane_id.to_owned(), |d| {
                &mut d.panes
            }))
            .child(terminal)
            .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                this.drop_pane(&dragged.pane_id, &drop_pane, cx);
            }))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                this.paste_paths(&paste_pane, paths.paths(), window, cx);
            }))
            .on_drop(cx.listener(move |this, dragged: &DraggedPath, window, cx| {
                this.paste_paths(&paste_file_pane, slice::from_ref(&dragged.path), window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    let focused = this.snapshot(cx).focused_pane_id.clone();
                    if focused.as_deref() != Some(click_pane.as_str()) {
                        let machine = this.machine(cx);
                        this.state
                            .update(cx, |state, cx| state.focus_pane(&machine, &click_pane, cx))
                            .detach_and_log_err(cx);
                    }
                }),
            )
            .into_any_element()
    }

    /// Types dropped files' paths into the pane's terminal as one paste,
    /// shell-escaped and space-separated like Ghostty does, then focuses it.
    fn paste_paths(
        &mut self,
        pane_id: &PaneId,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.state.read(cx).selected_machine();
        if !selected.id().is_local() {
            let message = format!("Can't drop files into {} terminals yet", selected.label());
            crate::sounds::play(cues::Cue::Error, crate::sounds::subtle());
            if let Some(chrome) = crate::chrome::get(window, cx) {
                chrome.update(cx, |chrome, cx| chrome.toast(message, window, cx));
            }
            return;
        }
        crate::sounds::play(cues::Cue::Tap, cues::PlayOptions::default());
        let snapshot = self.snapshot(cx);
        let Some(pane) = snapshot.pane(pane_id) else {
            return;
        };
        let Some(view) = self.terminals.get(&pane.terminal_id) else {
            return;
        };
        let text = paths
            .iter()
            .map(|path| shell_escape(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        view.terminal.update(cx, |terminal, cx| {
            terminal.paste_text(&text);
            terminal.focus(window, cx);
        });
        if snapshot.focused_pane_id.as_deref() != Some(pane_id) {
            let machine = self.machine(cx);
            self.state
                .update(cx, |state, cx| state.focus_pane(&machine, pane_id, cx))
                .detach_and_log_err(cx);
        }
    }

    /// Pane `source` dropped on pane `target`.
    fn drop_pane(&mut self, source: &PaneId, target: &PaneId, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let tab_of = |id: &PaneId| snapshot.pane(id).map(|pane| &pane.tab_id);
        let Some(drop) = pane_drop((source, tab_of(source)), (target, tab_of(target))) else {
            return;
        };
        let machine = self.machine(cx);
        let state = self.state.read(cx);
        match drop {
            PaneDrop::Swap => state.swap_panes(&machine, source, target, cx),
            PaneDrop::Move(destination) => state.move_pane(&machine, source, destination, cx),
        }
        .detach_and_log_err(cx);
    }
}

/// Backslash-escapes the characters a shell would split or expand, matching
/// what Ghostty and Terminal.app paste for a dropped file.
fn shell_escape(path: &str) -> String {
    const SPECIAL: &str = "\\ ()[]{}<>\"'`!#$&;|*?\t";
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        if SPECIAL.contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::shell_escape;

    #[test]
    fn shell_escape_backslashes_shell_specials() {
        assert_eq!(shell_escape("/tmp/plain.png"), "/tmp/plain.png");
        assert_eq!(
            shell_escape("/Users/me/Screenshot 2026-10-17 at 9.41 (2).png"),
            r"/Users/me/Screenshot\ 2026-10-17\ at\ 9.41\ \(2\).png"
        );
        assert_eq!(shell_escape("it's$x"), r"it\'s\$x");
    }
}
