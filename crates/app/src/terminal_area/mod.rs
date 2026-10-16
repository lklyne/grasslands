//! herdr's tab bar and the active tab's split tree of Ghostty terminals.
//!
//! herdr is the source of truth: the area renders the focused workspace's
//! active tab from `AppState::snapshot` and turns every user action into a
//! herdr call, then re-renders from the snapshot that follows. The only local
//! state is a divider's ratio while it is being dragged (sent on release) and
//! until herdr's layout reflects it. See
//! docs/decisions/0008-terminal-area-and-overlay.md.

mod layout;
pub mod script;
mod terminals;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::OpenContextMenu;
use crate::chrome::MenuTarget;
use crate::herdr::{
    AgentStatus, PaneDirection, PaneInfo, PaneMoveDestination, RightClickTarget, Snapshot,
    SplitDirection, SplitTree, Tab,
};
use crate::overlay;
use crate::state::{AppEvent, AppState};

use self::terminals::{Launch, Terminals};

actions!(
    terminal_area,
    [
        SplitRight,
        SplitDown,
        ClosePane,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        ResizeLeft,
        ResizeRight,
        ResizeUp,
        ResizeDown,
        ToggleZoom,
    ]
);

const CONTEXT: &str = "TerminalArea";
/// Space between panes: the divider, and room for the focus ring, which the
/// native terminal views would otherwise cover.
const GAP: Pixels = px(5.);
const HEADER_HEIGHT: Pixels = px(22.);
/// How long a local divider ratio waits for herdr's layout to catch up.
const OVERRIDE_TIMEOUT: Duration = Duration::from_secs(3);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-d", SplitRight, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-d", SplitDown, Some(CONTEXT)),
        KeyBinding::new("cmd-w", ClosePane, Some(CONTEXT)),
        KeyBinding::new("cmd-t", NewTab, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-w", CloseTab, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-]", NextTab, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-[", PrevTab, Some(CONTEXT)),
        KeyBinding::new("cmd-alt-left", FocusLeft, Some(CONTEXT)),
        KeyBinding::new("cmd-alt-right", FocusRight, Some(CONTEXT)),
        KeyBinding::new("cmd-alt-up", FocusUp, Some(CONTEXT)),
        KeyBinding::new("cmd-alt-down", FocusDown, Some(CONTEXT)),
        KeyBinding::new("cmd-ctrl-left", ResizeLeft, Some(CONTEXT)),
        KeyBinding::new("cmd-ctrl-right", ResizeRight, Some(CONTEXT)),
        KeyBinding::new("cmd-ctrl-up", ResizeUp, Some(CONTEXT)),
        KeyBinding::new("cmd-ctrl-down", ResizeDown, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-enter", ToggleZoom, Some(CONTEXT)),
    ]);
}

/// A pane being dragged by its header: drop on another pane to swap, on a
/// tab to move it there, on `+` to move it to a new tab.
#[derive(Clone)]
struct DraggedPane {
    pane_id: String,
    label: SharedString,
}

impl Render for DraggedPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(self.label.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SplitKey {
    tab_id: String,
    path: Vec<bool>,
}

struct DividerDrag {
    key: SplitKey,
    direction: SplitDirection,
}

struct RatioOverride {
    ratio: f32,
    since: Instant,
}

/// Where things were drawn last frame, for divider drags, keyboard focus
/// moves and the script's checks. Written during prepaint.
#[derive(Default)]
struct Drawn {
    splits: HashMap<SplitKey, Bounds<Pixels>>,
    /// Terminal body (the native view's slot) of each visible pane.
    panes: HashMap<String, Bounds<Pixels>>,
}

pub struct TerminalArea {
    state: Entity<AppState>,
    terminals: Terminals,
    focus: FocusHandle,
    overrides: HashMap<SplitKey, RatioOverride>,
    drag: Option<DividerDrag>,
    drawn: Rc<RefCell<Drawn>>,
    /// Panes already told to forward right-clicks to their program.
    right_click_set: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl TerminalArea {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let launch = state
            .read(cx)
            .herdr_config
            .as_ref()
            .map(Launch::from_config);
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&state, window, |this, _, event, window, cx| {
                this.on_app_event(event, window, cx);
            }),
            cx.observe_global_in::<gpui_kit::component::Theme>(window, |this, _, cx| {
                this.terminals.reload(None, cx);
            }),
        ];
        let mut this = Self {
            state,
            terminals: Terminals::new(launch),
            focus: cx.focus_handle(),
            overrides: HashMap::new(),
            drag: None,
            drawn: Rc::default(),
            right_click_set: HashSet::new(),
            _subscriptions: subscriptions,
        };
        this.sync_with_snapshot(window, cx);
        if let Some(scenario) = script::requested() {
            script::start(scenario, cx.entity(), window, cx);
        }
        this
    }

    fn snapshot(&self, cx: &App) -> std::sync::Arc<Snapshot> {
        self.state.read(cx).snapshot.clone()
    }

    fn on_app_event(&mut self, event: &AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AppEvent::SnapshotReplaced => self.sync_with_snapshot(window, cx),
            // Keyboard focus follows herdr's focus, but only if it is already
            // in the terminal area: an agent focusing a pane must not pull
            // typing out of the editor.
            AppEvent::FocusedPaneChanged(Some(pane_id)) if self.owns_keyboard_focus(window, cx) => {
                self.focus_terminal(pane_id, window, cx);
            }
            _ => {}
        }
    }

    fn owns_keyboard_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx) || self.terminals.focused(window, cx).is_some()
    }

    /// Reconciles terminals and per-pane settings with a fresh snapshot.
    fn sync_with_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let changes = self.terminals.reconcile(&snapshot, window, cx);
        if !changes.spawned.is_empty() || !changes.dropped.is_empty() {
            log::info!(
                "terminals: spawned {:?}, dropped {:?}, total {}",
                changes.spawned,
                changes.dropped,
                self.terminals.len()
            );
        }

        // `right_click: "pane"`: programs in the pane get right-clicks; our
        // context menu is on the pane header.
        self.right_click_set
            .retain(|pane_id| snapshot.panes.contains_key(pane_id));
        for pane_id in snapshot.panes.keys() {
            if self.right_click_set.insert(pane_id.clone()) {
                self.state
                    .read(cx)
                    .set_right_click(pane_id, RightClickTarget::Pane, cx)
                    .detach_and_log_err(cx);
            }
        }

        // Drop local divider ratios herdr has caught up with (or never will).
        let dragging = self.drag.as_ref().map(|drag| drag.key.clone());
        self.overrides.retain(|key, local| {
            if dragging.as_ref() == Some(key) {
                return true;
            }
            let herdr_ratio = snapshot
                .tab(&key.tab_id)
                .and_then(|tab| tab.root.as_ref())
                .and_then(|root| root.at(&key.path))
                .and_then(|node| match node {
                    SplitTree::Split { ratio, .. } => Some(*ratio),
                    SplitTree::Pane(_) => None,
                });
            let caught_up = herdr_ratio.is_none_or(|ratio| (ratio - local.ratio).abs() < 0.005);
            !caught_up && local.since.elapsed() < OVERRIDE_TIMEOUT
        });
        cx.notify();
    }

    /// The tab on screen: the focused workspace's active tab.
    fn active_tab(snapshot: &Snapshot) -> Option<&Tab> {
        let workspace = snapshot
            .focused_workspace_id
            .as_deref()
            .and_then(|id| snapshot.workspace(id))
            .or_else(|| snapshot.workspaces.first())?;
        workspace
            .tabs
            .iter()
            .find(|tab| tab.info.tab_id == workspace.info.active_tab_id)
            .or_else(|| workspace.tabs.first())
    }

    /// The pane commands act on: herdr's focused pane if it is on screen,
    /// else the active tab's focused pane.
    fn target_pane(&self, cx: &App) -> Option<String> {
        let snapshot = self.snapshot(cx);
        let tab = Self::active_tab(&snapshot)?;
        snapshot
            .focused_pane_id
            .clone()
            .filter(|id| {
                snapshot
                    .pane(id)
                    .is_some_and(|p| p.tab_id == tab.info.tab_id)
            })
            .or_else(|| tab.focused_pane_id.clone())
    }

    /// Ask herdr to focus `pane_id`, and move keyboard focus to its terminal
    /// now; herdr's `pane.focused` then moves the focus ring.
    pub fn focus_pane(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.mark_seen(pane_id, cx);
            state.focus_pane(pane_id, cx).detach_and_log_err(cx);
        });
        self.focus_terminal(pane_id, window, cx);
    }

    fn focus_terminal(&mut self, pane_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let Some(pane) = snapshot.pane(pane_id) else {
            return;
        };
        if let Some(view) = self.terminals.get(&pane.terminal_id) {
            view.terminal
                .update(cx, |terminal, cx| terminal.focus(window, cx));
        }
    }

    // Commands

    fn split(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        let target = self.target_pane(cx);
        self.state
            .read(cx)
            .split_pane(target.as_deref(), direction, cx)
            .detach_and_log_err(cx);
    }

    fn close_pane_confirmed(
        &mut self,
        pane_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(cx);
        let Some(pane) = snapshot.pane(&pane_id) else {
            return;
        };
        let state = self.state.clone();
        if !is_busy(pane.agent_status) {
            state
                .read(cx)
                .close_pane(&pane_id, cx)
                .detach_and_log_err(cx);
            return;
        }
        // herdr closes at once, so a running agent gets a confirmation. A
        // native alert, so it shows above the terminals.
        let name = pane_label(pane);
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Close {name}?"),
            Some("Its agent is still running; closing the pane stops it."),
            &["Close Pane", "Cancel"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                let task = cx.update(|cx| state.read(cx).close_pane(&pane_id, cx));
                if let Err(error) = task.await {
                    log::error!("close pane {pane_id}: {error}");
                }
            }
        })
        .detach();
    }

    fn close_tab_confirmed(&mut self, tab_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let busy = snapshot
            .panes
            .values()
            .filter(|pane| pane.tab_id == tab_id && is_busy(pane.agent_status))
            .count();
        let state = self.state.clone();
        if busy == 0 {
            state.read(cx).close_tab(&tab_id, cx).detach_and_log_err(cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Close this tab?",
            Some(&format!(
                "{busy} running agent{} in it will stop.",
                if busy == 1 { "" } else { "s" }
            )),
            &["Close Tab", "Cancel"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                let task = cx.update(|cx| state.read(cx).close_tab(&tab_id, cx));
                if let Err(error) = task.await {
                    log::error!("close tab {tab_id}: {error}");
                }
            }
        })
        .detach();
    }

    fn cycle_tab(&mut self, step: isize, cx: &mut Context<Self>) {
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
        self.state
            .read(cx)
            .focus_tab(&tab_id, cx)
            .detach_and_log_err(cx);
    }

    fn move_focus(
        &mut self,
        direction: PaneDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.target_pane(cx) else {
            return;
        };
        let to = layout::neighbor(&from, direction, &self.drawn.borrow().panes).map(str::to_owned);
        if let Some(to) = to {
            self.focus_pane(&to, window, cx);
        }
    }

    fn resize_focused(&mut self, direction: PaneDirection, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let (Some(tab), Some(pane_id)) = (Self::active_tab(&snapshot), self.target_pane(cx)) else {
            return;
        };
        let Some(root) = tab.root.as_ref() else {
            return;
        };
        let tree = self.with_overrides(&tab.info.tab_id, root);
        if let Some((path, ratio)) = layout::resize(&tree, &pane_id, direction) {
            let key = SplitKey {
                tab_id: tab.info.tab_id.clone(),
                path,
            };
            self.commit_ratio(key, ratio, cx);
        }
    }

    fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        let Some(pane_id) = self.target_pane(cx) else {
            return;
        };
        self.state
            .read(cx)
            .call(cx, move |client| {
                client.pane_zoom(&pane_id, "toggle").map(drop)
            })
            .detach_and_log_err(cx);
    }

    /// Records a divider position locally and sends it to herdr.
    fn commit_ratio(&mut self, key: SplitKey, ratio: f32, cx: &mut Context<Self>) {
        self.overrides.insert(
            key.clone(),
            RatioOverride {
                ratio,
                since: Instant::now(),
            },
        );
        self.state
            .read(cx)
            .set_split_ratio(&key.tab_id, key.path, ratio, cx)
            .detach_and_log_err(cx);
        cx.notify();
    }

    /// `root` with locally dragged ratios applied.
    fn with_overrides(&self, tab_id: &str, root: &SplitTree) -> SplitTree {
        fn apply(
            node: &SplitTree,
            path: &mut Vec<bool>,
            lookup: &dyn Fn(&[bool]) -> Option<f32>,
        ) -> SplitTree {
            match node {
                SplitTree::Pane(id) => SplitTree::Pane(id.clone()),
                SplitTree::Split {
                    direction,
                    ratio,
                    first,
                    second,
                } => {
                    let ratio = lookup(path).unwrap_or(*ratio);
                    path.push(false);
                    let first = apply(first, path, lookup);
                    path.pop();
                    path.push(true);
                    let second = apply(second, path, lookup);
                    path.pop();
                    SplitTree::Split {
                        direction: *direction,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }
                }
            }
        }
        let lookup = |path: &[bool]| {
            self.overrides
                .get(&SplitKey {
                    tab_id: tab_id.to_owned(),
                    path: path.to_vec(),
                })
                .map(|local| local.ratio)
        };
        apply(root, &mut Vec::new(), &lookup)
    }

    /// Opens chrome's context menu for a pane or tab (chrome draws it).
    fn context_menu(
        &mut self,
        target: MenuTarget,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.dispatch_action(Box::new(OpenContextMenu { target, position }), cx);
    }

    fn drag_divider_to(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        let Some(bounds) = self.drawn.borrow().splits.get(&drag.key).copied() else {
            return;
        };
        let ratio = layout::ratio_at(drag.direction, bounds, pointer);
        self.overrides.insert(
            drag.key.clone(),
            RatioOverride {
                ratio,
                since: Instant::now(),
            },
        );
        cx.notify();
    }

    fn end_divider_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if let Some(local) = self.overrides.get(&drag.key) {
            let ratio = local.ratio;
            self.commit_ratio(drag.key, ratio, cx);
        }
    }

    // Rendering

    fn render_tab_bar(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let active = Self::active_tab(snapshot);
        let workspace = active.and_then(|tab| snapshot.workspace(&tab.info.workspace_id));
        let workspace_id = workspace.map(|w| w.info.workspace_id.clone());
        let tabs: Vec<_> = workspace
            .map(|w| w.tabs.iter().map(|tab| tab.info.clone()).collect())
            .unwrap_or_default();
        let active_id = active.map(|tab| tab.info.tab_id.clone());
        let (tab_bar, active_bg, muted, border, drop) = (
            theme.tab_bar,
            theme.tab_active,
            theme.muted_foreground,
            theme.border,
            theme.drop_target,
        );

        h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .bg(tab_bar)
            .border_b_1()
            .border_color(border)
            .text_sm()
            .when_some(workspace, |bar, workspace| {
                bar.child(
                    div()
                        .px_3()
                        .text_color(muted)
                        .child(workspace.info.label.clone()),
                )
            })
            .children(tabs.into_iter().map(|tab| {
                let is_active = active_id.as_deref() == Some(tab.tab_id.as_str());
                let (focus_id, close_id, drop_id, menu_id) = (
                    tab.tab_id.clone(),
                    tab.tab_id.clone(),
                    tab.tab_id.clone(),
                    tab.tab_id.clone(),
                );
                h_flex()
                    .id(SharedString::from(format!("tab-{}", tab.tab_id)))
                    .group("tab")
                    .h_full()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_r_1()
                    .border_color(border)
                    .when(is_active, |tab| tab.bg(active_bg))
                    .child(status_dot(tab.agent_status, cx))
                    .child(if tab.label.is_empty() {
                        tab.number.to_string()
                    } else {
                        tab.label.clone()
                    })
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state
                            .read(cx)
                            .focus_tab(&focus_id, cx)
                            .detach_and_log_err(cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.context_menu(
                                MenuTarget::Tab {
                                    tab_id: menu_id.clone(),
                                },
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                    .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
                    .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                        let destination = PaneMoveDestination::Tab {
                            tab_id: drop_id.clone(),
                            split: SplitDirection::Right,
                            target_pane_id: None,
                            ratio: None,
                        };
                        this.state
                            .read(cx)
                            .move_pane(&dragged.pane_id, destination, cx)
                            .detach_and_log_err(cx);
                    }))
            }))
            .child(
                div()
                    .id("new-tab")
                    .px_3()
                    .h_full()
                    .flex()
                    .items_center()
                    .text_color(muted)
                    .child("+")
                    .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
                    .on_click({
                        let workspace_id = workspace_id.clone();
                        cx.listener(move |this, _, _, cx| {
                            this.state
                                .read(cx)
                                .new_tab(workspace_id.as_deref(), cx)
                                .detach_and_log_err(cx);
                        })
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                        let destination = PaneMoveDestination::NewTab {
                            workspace_id: workspace_id.clone(),
                            label: None,
                        };
                        this.state
                            .read(cx)
                            .move_pane(&dragged.pane_id, destination, cx)
                            .detach_and_log_err(cx);
                    })),
            )
    }

    fn render_node(
        &self,
        tab_id: &str,
        node: &SplitTree,
        path: &mut Vec<bool>,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            SplitTree::Pane(pane_id) => self.render_pane(pane_id, frame, cx),
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
        let dragging = self.drag.as_ref().is_some_and(|drag| drag.key == key);
        let theme = cx.theme();
        let (idle, hot) = (theme.border, theme.primary);
        let divider_id = SharedString::from(format!(
            "divider-{}-{}",
            key.tab_id,
            key.path
                .iter()
                .map(|step| if *step { '1' } else { '0' })
                .collect::<String>()
        ));
        let drawn = self.drawn.clone();
        let bounds_key = key.clone();
        let divider = div()
            .id(divider_id)
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
                    this.drag = Some(DividerDrag {
                        key: key.clone(),
                        direction,
                    });
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
            .child(
                canvas(
                    move |bounds, _, _| {
                        drawn.borrow_mut().splits.insert(bounds_key, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(side(ratio, first))
            .child(divider)
            .child(side(1. - ratio, second))
            .into_any_element()
    }

    fn render_pane(&self, pane_id: &str, frame: &mut Frame, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = &frame.snapshot;
        let theme = cx.theme();
        let Some(pane) = snapshot.pane(pane_id) else {
            return div().into_any_element();
        };
        let focused = snapshot.focused_pane_id.as_deref() == Some(pane_id);
        let label: SharedString = pane_label(pane).into();
        let terminal_id = pane.terminal_id.clone();
        let attached = self.terminals.is_attached(&terminal_id, cx);
        if attached {
            frame.visible.insert(terminal_id.clone());
        }
        let (ring, header_bg, header_active, muted, drop) = (
            theme.ring,
            theme.tab_bar,
            theme.tab_active,
            theme.muted_foreground,
            theme.drop_target,
        );

        let body: AnyElement = match (self.terminals.get(&terminal_id), attached) {
            (Some(view), true) => view.terminal.clone().into_any_element(),
            _ => {
                let message = self
                    .terminals
                    .failed
                    .get(&terminal_id)
                    .cloned()
                    .unwrap_or_else(|| "Detached: another client has this terminal.".to_owned());
                let reattach_pane = pane.clone();
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_sm()
                    .text_color(muted)
                    .child(message)
                    .child(
                        div()
                            .id(SharedString::from(format!("reattach-{pane_id}")))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .child("Reattach")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.terminals.reattach(&reattach_pane, window, cx);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
        };

        let (header_pane, drop_header, drop_body, click_pane, close_pane, menu_pane) = (
            pane_id.to_owned(),
            pane_id.to_owned(),
            pane_id.to_owned(),
            pane_id.to_owned(),
            pane_id.to_owned(),
            pane_id.to_owned(),
        );
        let drawn = self.drawn.clone();
        let bounds_pane = pane_id.to_owned();
        let dragged = DraggedPane {
            pane_id: pane_id.to_owned(),
            label: label.clone(),
        };

        let header = h_flex()
            .id(SharedString::from(format!("pane-header-{header_pane}")))
            .h(HEADER_HEIGHT)
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .text_xs()
            .bg(if focused { header_active } else { header_bg })
            .text_color(if focused { theme.foreground } else { muted })
            .child(status_dot(pane.agent_status, cx))
            .child(div().flex_1().min_w_0().overflow_hidden().child(label))
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
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                this.swap(&dragged.pane_id, &drop_header, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.context_menu(
                        MenuTarget::Pane {
                            pane_id: menu_pane.clone(),
                        },
                        event.position,
                        window,
                        cx,
                    );
                }),
            );

        let body = div()
            .id(SharedString::from(format!("pane-body-{pane_id}")))
            .flex_1()
            .min_h_0()
            .relative()
            // The ring sits in the 1px the native view leaves uncovered.
            .border_1()
            .border_color(if focused { ring } else { transparent_black() })
            .child(
                canvas(
                    move |bounds, _, _| {
                        drawn.borrow_mut().panes.insert(bounds_pane, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(body)
            .drag_over::<DraggedPane>(move |style, _, _, _| style.bg(drop))
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                this.swap(&dragged.pane_id, &drop_body, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    let focused = this.snapshot(cx).focused_pane_id.clone();
                    if focused.as_deref() != Some(click_pane.as_str()) {
                        this.state
                            .read(cx)
                            .focus_pane(&click_pane, cx)
                            .detach_and_log_err(cx);
                    }
                }),
            );

        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn swap(&mut self, source: &str, target: &str, cx: &mut Context<Self>) {
        if source == target {
            return;
        }
        let snapshot = self.snapshot(cx);
        let same_tab = snapshot
            .pane(source)
            .zip(snapshot.pane(target))
            .is_some_and(|(a, b)| a.tab_id == b.tab_id);
        let state = self.state.read(cx);
        if same_tab {
            state.swap_panes(source, target, cx).detach_and_log_err(cx);
        } else if let Some(target_pane) = snapshot.pane(target) {
            // From another tab: move it next to the target instead.
            let destination = PaneMoveDestination::Tab {
                tab_id: target_pane.tab_id.clone(),
                split: SplitDirection::Right,
                target_pane_id: Some(target.to_owned()),
                ratio: None,
            };
            state
                .move_pane(source, destination, cx)
                .detach_and_log_err(cx);
        }
    }

    fn render_empty(&self, message: String, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(message)
            .into_any_element()
    }
}

/// Per-render bookkeeping.
struct Frame {
    snapshot: std::sync::Arc<Snapshot>,
    /// Terminal ids laid out this render.
    visible: HashSet<String>,
}

fn is_busy(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Working | AgentStatus::Blocked)
}

fn pane_label(pane: &PaneInfo) -> String {
    pane.label
        .clone()
        .or_else(|| pane.display_agent.clone())
        .or_else(|| pane.agent.clone())
        .or_else(|| pane.terminal_title_stripped.clone())
        .or_else(|| pane.title.clone())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| pane.pane_id.clone())
}

fn status_dot(status: AgentStatus, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let color = match status {
        AgentStatus::Working => theme.info,
        AgentStatus::Blocked => theme.warning,
        AgentStatus::Done => theme.success,
        AgentStatus::Idle | AgentStatus::Unknown => theme.muted_foreground.opacity(0.4),
    };
    div().size(px(7.)).flex_none().rounded_full().bg(color)
}

impl Render for TerminalArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.terminals.report_detached(cx);
        let snapshot = self.snapshot(cx);
        let mut frame = Frame {
            snapshot: snapshot.clone(),
            visible: HashSet::new(),
        };
        {
            let mut drawn = self.drawn.borrow_mut();
            drawn.panes.clear();
            drawn.splits.clear();
        }

        let body = match Self::active_tab(&snapshot) {
            None => {
                let state = self.state.read(cx);
                let message = if let Some(config) = state.herdr_config.as_ref()
                    && snapshot.generation == 0
                {
                    format!(
                        "Connecting to herdr ({})…",
                        config.session.as_deref().unwrap_or("default session")
                    )
                } else if state.herdr_config.is_none() {
                    format!("herdr is disconnected: {:?}", state.connection)
                } else {
                    "No workspaces. Create one from the sidebar.".to_owned()
                };
                self.render_empty(message, cx)
            }
            Some(tab) => match tab.root.as_ref() {
                None => self.render_empty("Reading layout…".to_owned(), cx),
                Some(root) => {
                    let tab_id = tab.info.tab_id.clone();
                    let zoomed = tab.zoomed.then(|| tab.focused_pane_id.clone()).flatten();
                    let tree = match zoomed {
                        Some(pane_id) => SplitTree::Pane(pane_id),
                        None => self.with_overrides(&tab_id, root),
                    };
                    self.render_node(&tab_id, &tree, &mut Vec::new(), &mut frame, cx)
                }
            },
        };

        let visible: HashSet<&str> = frame.visible.iter().map(String::as_str).collect();
        self.terminals.show_only(&visible, cx);

        let this = cx.entity().downgrade();
        let dragging = self.drag.is_some();
        v_flex()
            .id("terminal-area")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .on_action(
                cx.listener(|this, _: &SplitRight, _, cx| this.split(SplitDirection::Right, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SplitDown, _, cx| this.split(SplitDirection::Down, cx)),
            )
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                if let Some(pane_id) = this.target_pane(cx) {
                    this.close_pane_confirmed(pane_id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NewTab, _, cx| {
                let snapshot = this.snapshot(cx);
                let workspace = Self::active_tab(&snapshot).map(|t| t.info.workspace_id.clone());
                this.state
                    .read(cx)
                    .new_tab(workspace.as_deref(), cx)
                    .detach_and_log_err(cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let snapshot = this.snapshot(cx);
                if let Some(tab) = Self::active_tab(&snapshot) {
                    this.close_tab_confirmed(tab.info.tab_id.clone(), window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.cycle_tab(1, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| this.cycle_tab(-1, cx)))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                this.move_focus(PaneDirection::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                this.move_focus(PaneDirection::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
                this.move_focus(PaneDirection::Up, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.move_focus(PaneDirection::Down, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeLeft, _, cx| {
                this.resize_focused(PaneDirection::Left, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeRight, _, cx| {
                this.resize_focused(PaneDirection::Right, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ResizeUp, _, cx| this.resize_focused(PaneDirection::Up, cx)),
            )
            .on_action(cx.listener(|this, _: &ResizeDown, _, cx| {
                this.resize_focused(PaneDirection::Down, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| this.toggle_zoom(cx)))
            .child(self.render_tab_bar(&snapshot, cx))
            .child(div().flex_1().min_h_0().p(GAP).child(body))
            .child(
                // Window-wide pointer tracking: divider drags, and telling the
                // overlay where the pointer is (it can't see it while it lets
                // clicks through).
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let on_move = this.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            overlay::track_pointer(event.position, window, cx);
                            if dragging {
                                on_move
                                    .update(cx, |this, cx| this.drag_divider_to(event.position, cx))
                                    .ok();
                            }
                        });
                        if dragging {
                            let this = this.clone();
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble {
                                    this.update(cx, |this, cx| this.end_divider_drag(cx)).ok();
                                }
                            });
                        }
                    },
                )
                .absolute()
                .size_0(),
            )
    }
}

impl Focusable for TerminalArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
