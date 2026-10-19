//! herdr's tab bar and the active tab's split tree of Ghostty terminals.
//!
//! herdr is the source of truth: the area renders the focused workspace's
//! active tab from `AppState::snapshot` and turns every user action into a
//! herdr call, then re-renders from the snapshot that follows. The only local
//! state is a divider's ratio while it is being dragged (sent on release) and
//! until herdr's layout reflects it. See
//! docs/decisions/0008-terminal-area-and-overlay.md.
//!
//! `TerminalArea` composes the pieces and owns keyboard focus-following:
//! - `tab_bar`: the tab strip and tab commands;
//! - `split_view`: the split tree, its dividers and panes;
//! - `drag`: divider, pane and tab drags, as plain state where it can be;
//! - `reconcile` and `terminals`: the Ghostty views kept in step with
//!   herdr's snapshots, and the right-click policy;
//! - `command_terminal`: the setup and machine-run terminals.

pub mod command_terminal;
mod drag;
mod layout;
mod reconcile;
#[cfg(debug_assertions)]
pub mod script;
mod split_view;
mod tab_bar;
mod terminals;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

use crate::actions::OpenContextMenu;
use crate::chrome::{Confirmation, MenuTarget};
use crate::herdr::{
    PaneDirection, PaneId, Snapshot, SplitDirection, SplitTree, Tab, TabId, TerminalId, ZoomMode,
};
use crate::setup::{Setup, Stage};
use crate::state::{AppEvent, AppState, MachineId, MachineTask, Status};
use crate::{label, status};

use self::command_terminal::CommandTerminal;
use self::drag::{PaneDragPreview, RatioOverrides, SplitKey, TabDragTick};
use self::reconcile::RightClicks;
use self::split_view::Frame;
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

/// Where things were drawn last frame, for divider drags, keyboard focus
/// moves and the script's checks. Written during prepaint.
#[derive(Default)]
struct Drawn {
    splits: HashMap<SplitKey, Bounds<Pixels>>,
    /// Terminal body (the native view's slot) of each visible pane.
    panes: HashMap<PaneId, Bounds<Pixels>>,
    /// Pane headers, tabs and Reattach buttons, for the script's in-app
    /// pointer input.
    headers: HashMap<PaneId, Bounds<Pixels>>,
    tabs: HashMap<TabId, Bounds<Pixels>>,
    /// By pane.
    reattach: HashMap<PaneId, Bounds<Pixels>>,
}

/// A canvas that records its bounds (the parent's, when absolute and full
/// size) into `drawn` during prepaint.
fn record_bounds<K: Eq + std::hash::Hash + 'static>(
    drawn: &Rc<RefCell<Drawn>>,
    key: K,
    slot: fn(&mut Drawn) -> &mut HashMap<K, Bounds<Pixels>>,
) -> impl IntoElement {
    let drawn = drawn.clone();
    canvas(
        move |bounds, _, _| {
            slot(&mut drawn.borrow_mut()).insert(key, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

pub struct TerminalArea {
    state: Entity<AppState>,
    /// How Local's terminals attach; `None` when the app refused its herdr
    /// endpoint, so a guarded build can never reach the wrong session.
    launch: Option<Launch>,
    terminals: Terminals,
    focus: FocusHandle,
    /// Divider ratios herdr hasn't confirmed yet, and the divider being
    /// dragged.
    overrides: RatioOverrides,
    pane_drag: Option<PaneDragPreview>,
    drawn: Rc<RefCell<Drawn>>,
    /// Terminal ids whose attach process was running at the last check.
    attached: HashSet<TerminalId>,
    right_clicks: RightClicks,
    /// False while the window shows something else in its place (a file
    /// tab); every native view is hidden then.
    visible: bool,
    /// The tab on screen at the last sync, to notice tab switches.
    shown_tab: Option<TabId>,
    /// A tab the user picked from the tab bar: its pane takes keyboard focus
    /// once herdr shows it. The bar is outside the area, so a click there
    /// takes focus out of it.
    focus_on_show: Option<TabId>,
    tab_drag_tick: TabDragTick,
    /// The terminal beside the herdr setup screen, running the last fix
    /// (`setup::fix_terminal`).
    setup_terminal: Entity<CommandTerminal>,
    /// The terminal beside a machine's sign-in, setup or add
    /// (`machines::run_terminal`), whichever machine is selected.
    machine_terminal: Entity<CommandTerminal>,
    _subscriptions: Vec<Subscription>,
    _liveness: Task<()>,
}
impl TerminalArea {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let launch = state
            .read(cx)
            .herdr_config
            .as_ref()
            .map(Launch::from_config);
        let setup = state.read(cx).setup().clone();
        let setup_terminal = crate::setup::fix_terminal(&setup, window, cx);
        let machine_terminal = crate::machines::run_terminal(&state, window, cx);
        // What render reads, and what announces each change (it doesn't
        // observe `AppState`, so another machine's snapshot doesn't wake it):
        // - the selected machine's snapshot, id and focused pane:
        //   `SnapshotReplaced` (which follows `MachineSelected`);
        // - the selected machine's label and status, Local's connection
        //   problem: `MachinesChanged`;
        // - the machine run and its machine: `MachineRunChanged`,
        //   `MachinesChanged`;
        // - the setup screen: observing `Setup`; the command terminals:
        //   observing each;
        // - its own state (drags, overrides, attaches): its own `notify`.
        let subscriptions = vec![
            cx.observe(&setup, |_, _, cx| cx.notify()),
            cx.observe(&setup_terminal, |_, _, cx| cx.notify()),
            cx.observe(&machine_terminal, |_, _, cx| cx.notify()),
            // Coming back to the window after fixing herdr elsewhere (a
            // terminal, a Nix rebuild) checks again.
            cx.observe_window_activation(window, |this, window, cx| {
                let setup = this.state.read(cx).setup().clone();
                let needed = matches!(setup.read(cx).stage(), Stage::Needed(_));
                if window.is_window_active() && needed {
                    setup.update(cx, Setup::check_herdr);
                }
            }),
            cx.subscribe_in(&state, window, |this, _, event, window, cx| {
                this.on_app_event(event, window, cx);
            }),
            cx.observe_global_in::<gpui_kit::component::Theme>(window, |this, _, cx| {
                this.terminals.reload_all(cx);
            }),
        ];
        let mut this = Self {
            state,
            launch,
            terminals: Terminals::new(),
            focus: cx.focus_handle(),
            overrides: RatioOverrides::default(),
            pane_drag: None,
            drawn: Rc::default(),
            attached: HashSet::new(),
            right_clicks: RightClicks::default(),
            visible: true,
            shown_tab: None,
            focus_on_show: None,
            tab_drag_tick: TabDragTick::default(),
            setup_terminal,
            machine_terminal,
            _subscriptions: subscriptions,
            _liveness: Self::watch_liveness(cx),
        };
        this.sync_with_snapshot(window, cx);
        this.sync_command_terminals(cx);
        #[cfg(debug_assertions)]
        if let Some(scenario) = script::requested() {
            script::start(scenario, cx.entity(), window, cx);
        }
        this
    }

    /// Shows the command terminals that belong on screen: the setup fix's
    /// on Local while no machine run covers it, a machine run's whenever
    /// the area shows.
    fn sync_command_terminals(&mut self, cx: &mut Context<Self>) {
        let run = self.state.read(cx).machine_run().is_some();
        let setup = self.visible && self.terminals.selected().is_local() && !run;
        let visible = self.visible;
        self.setup_terminal
            .update(cx, |terminal, cx| terminal.set_visible(setup, cx));
        self.machine_terminal
            .update(cx, |terminal, cx| terminal.set_visible(visible, cx));
    }

    fn start_pane_drag(
        &mut self,
        label: SharedString,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_pane_drag(cx);
        self.pane_drag = PaneDragPreview::open(label, position, window, cx);
    }

    fn move_pane_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(preview) = &self.pane_drag {
            preview.move_to(position, cx);
        }
    }

    fn end_pane_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.pane_drag.take() {
            preview.close(cx);
        }
    }

    fn snapshot(&self, cx: &App) -> std::sync::Arc<Snapshot> {
        self.state.read(cx).snapshot().clone()
    }

    /// The machine on screen, which the area's commands act on.
    fn machine(&self, cx: &App) -> MachineId {
        self.state.read(cx).selected().clone()
    }

    fn on_app_event(&mut self, event: &AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AppEvent::SnapshotReplaced => self.sync_with_snapshot(window, cx),
            AppEvent::MachinesChanged => {
                self.sync_other_machines(None, window, cx);
                cx.notify();
            }
            AppEvent::MachineSnapshot(machine) if machine != self.terminals.selected() => {
                self.sync_other_machines(Some(machine), window, cx);
            }
            AppEvent::MachineConnected(machine) => self.reattach_machine(machine, window, cx),
            AppEvent::MachineRestarted(machine) => self.terminals.forget_machine(machine, cx),
            AppEvent::MachineRunChanged => {
                self.sync_command_terminals(cx);
                cx.notify();
            }
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
        let machine = self.machine(cx);
        if self.terminals.selected() != &machine {
            // Divider overrides and the shown tab are by herdr id, which
            // another machine reuses.
            self.terminals.select(&machine, cx);
            // The setup terminal is Local's; it isn't drawn on another machine.
            self.sync_command_terminals(cx);
            self.overrides.clear();
            self.shown_tab = None;
            self.focus_on_show = None;
        }
        self.reconcile_terminals(&machine, &snapshot, window, cx);
        self.forward_right_clicks(&machine, &snapshot, cx);
        // Drop local divider ratios herdr has caught up with (or never will).
        self.overrides
            .retain_unconfirmed(|key| herdr_ratio(&snapshot, key), Instant::now());
        self.follow_tab_switch(&snapshot, window, cx);
        cx.notify();
    }

    /// A tab switch hides the terminal holding keyboard focus; the focus
    /// moves to the new tab's pane, unless it is elsewhere (the editor). A
    /// tab picked from the tab bar takes it either way.
    fn follow_tab_switch(
        &mut self,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown_tab = Self::active_tab(snapshot).map(|tab| tab.info.tab_id.clone());
        let picked = shown_tab.is_some() && self.focus_on_show == shown_tab;
        if picked {
            self.focus_on_show = None;
        }
        if shown_tab != self.shown_tab || picked {
            let follow = picked || self.owns_keyboard_focus(window, cx);
            self.shown_tab = shown_tab;
            if self.visible
                && follow
                && let Some(pane_id) = self.target_pane(cx)
            {
                self.focus_terminal(&pane_id, window, cx);
            }
        }
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
    fn target_pane(&self, cx: &App) -> Option<PaneId> {
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

    /// Shows or hides the terminal area. Ghostty views are native views over
    /// GPUI, so leaving the area out of the element tree would leave them on
    /// screen at their last frames: hiding hides every view, and showing
    /// places them again on the next frame and focuses herdr's focused pane.
    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            match self.target_pane(cx) {
                Some(pane_id) => self.focus_terminal(&pane_id, window, cx),
                None => window.focus(&self.focus, cx),
            }
        } else {
            self.terminals.hide_all(cx);
        }
        self.sync_command_terminals(cx);
        cx.notify();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Ask herdr to focus `pane_id`, and move keyboard focus to its terminal
    /// now; herdr's `pane.focused` then moves the focus ring.
    pub fn focus_pane(
        &mut self,
        machine: &MachineId,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state
            .update(cx, |state, cx| state.focus_pane(machine, pane_id, cx))
            .detach_and_log_err(cx);
        self.focus_terminal(pane_id, window, cx);
    }

    fn focus_terminal(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
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
        let machine = self.machine(cx);
        self.state
            .read(cx)
            .split_pane(&machine, target.as_ref(), direction, cx)
            .detach_and_log_err(cx);
    }

    fn close_pane_confirmed(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(cx);
        let Some(pane) = snapshot.pane(&pane_id) else {
            return;
        };
        let state = self.state.clone();
        let machine = self.machine(cx);
        if !status::is_busy(pane.agent_status) {
            state
                .read(cx)
                .close_pane(&machine, &pane_id, cx)
                .detach_and_log_err(cx);
            return;
        }
        // herdr closes at once, so a running agent gets a confirmation:
        // chrome's dialog (drawn in the overlay, above the terminals), or a
        // native alert when there is no chrome (the terminal area alone).
        let name = label::pane(&snapshot, pane).into_owned();
        if let Some(chrome) = crate::chrome::get(window, cx) {
            let confirmation = Confirmation {
                title: format!("Close {name}?"),
                message: "Its agent is still running; closing the pane stops it.".into(),
                confirm_label: "Close Pane".into(),
            };
            let on_confirm = move |_: &mut Window, cx: &mut App| {
                state
                    .read(cx)
                    .close_pane(&machine, &pane_id, cx)
                    .detach_and_log_err(cx);
            };
            chrome.update(cx, |chrome, cx| {
                chrome.confirm(confirmation, on_confirm, window, cx);
            });
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Close {name}?"),
            Some("Its agent is still running; closing the pane stops it."),
            &["Close Pane", "Cancel"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                let task = cx.update(|cx| state.read(cx).close_pane(&machine, &pane_id, cx));
                if let Err(error) = task.await {
                    tracing::error!("close pane {pane_id}: {error}");
                }
            }
        })
        .detach();
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let workspace = Self::active_tab(&snapshot).map(|t| t.info.workspace_id.clone());
        let machine = self.machine(cx);
        // Through the chrome, which asks for a name when herdr's
        // `ui.prompt_new_tab_name` says to.
        match crate::chrome::get(window, cx) {
            Some(chrome) => chrome.update(cx, |chrome, cx| {
                chrome.new_tab(machine, workspace, window, cx);
            }),
            None => self
                .state
                .read(cx)
                .new_tab(&machine, workspace.as_ref(), cx)
                .detach_and_log_err(cx),
        }
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
        let to = layout::neighbor(&from, direction, &self.drawn.borrow().panes).cloned();
        if let Some(to) = to {
            let machine = self.machine(cx);
            self.focus_pane(&machine, &to, window, cx);
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
        let machine = self.machine(cx);
        self.state
            .read(cx)
            .call(&machine, cx, move |client| {
                client.pane_zoom(&pane_id, ZoomMode::Toggle)
            })
            .detach_and_log_err(cx);
    }

    /// Records a divider position locally and sends it to herdr.
    fn commit_ratio(&mut self, key: SplitKey, ratio: f32, cx: &mut Context<Self>) {
        self.overrides.set(key.clone(), ratio, Instant::now());
        let machine = self.machine(cx);
        self.state
            .read(cx)
            .set_split_ratio(&machine, &key.tab_id, key.path, ratio, cx)
            .detach_and_log_err(cx);
        cx.notify();
    }

    /// `root` with locally dragged ratios applied.
    fn with_overrides(&self, tab_id: &TabId, root: &SplitTree) -> SplitTree {
        self.overrides.apply(tab_id, root)
    }

    /// Opens chrome's context menu for a pane or tab (chrome draws it).
    fn context_menu(
        &mut self,
        target: MenuTarget,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Keep the keyboard where it was (a terminal): without this the
        // right-click's mouse-down focuses the terminal area itself, and
        // that is what the menu gives focus back to when it closes.
        window.prevent_default();
        window.dispatch_action(Box::new(OpenContextMenu { target, position }), cx);
    }

    fn drag_divider_to(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let drawn = self.drawn.borrow();
        let moved = self.overrides.drag_to(
            |key| drawn.splits.get(key).copied(),
            pointer,
            Instant::now(),
        );
        drop(drawn);
        if moved {
            cx.notify();
        }
    }

    fn end_divider_drag(&mut self, cx: &mut Context<Self>) {
        if let Some((key, ratio)) = self.overrides.end_drag() {
            self.commit_ratio(key, ratio, cx);
        }
    }

    // Rendering

    /// The selected remote machine's offline (or sign-in, setup, disabled)
    /// screen, while it isn't connected. Its last snapshot is kept for when
    /// it comes back.
    fn render_machine_problem(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let machine = self.state.read(cx).selected_machine();
        if machine.id().is_local() || machine.status().is_usable() {
            return None;
        }
        let (status, detail) = crate::machines::MachineState::of(machine.status());
        let id = machine.id().clone();
        let state = self.state.clone();
        let on_reconnect: crate::chrome::Callback = Rc::new(move |_, cx| {
            state.update(cx, |state, cx| state.reconnect_machine(&id, cx));
        });
        let on_fix = MachineTask::fix(machine).map(|task| {
            let state = self.state.clone();
            let fix: crate::chrome::Callback = Rc::new(move |_, cx| {
                let task = task.clone();
                state.update(cx, |state, cx| state.start_machine_run(task, cx));
            });
            fix
        });
        Some(crate::machines::offline_screen(
            machine.label(),
            status,
            detail,
            on_reconnect,
            on_fix,
            cx,
        ))
    }

    /// A machine's sign-in, setup or add: the card, with its terminal.
    fn render_machine_run(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.state.read(cx);
        let run = state.machine_run()?;
        let machine = run.machine.as_ref().and_then(|id| state.machine(id));
        let detail = machine.and_then(|m| crate::machines::MachineState::of(m.status()).1);
        let close_state = self.state.clone();
        let retry_state = self.state.clone();
        let card = crate::machines::run_card(
            crate::machines::RunCard {
                task: &run.task,
                stage: &run.stage,
                label: machine.map(|m| m.label()),
                detail,
            },
            Rc::new(move |_, cx| {
                close_state.update(cx, |state, cx| state.finish_machine_run(cx));
            }),
            Rc::new(move |_, cx| {
                retry_state.update(cx, |state, cx| state.retry_machine_run(cx));
            }),
            cx,
        );
        let terminal = self
            .machine_terminal
            .read(cx)
            .terminal()
            .filter(|_| self.visible)
            .map(|terminal| terminal.clone().into_any_element());
        Some(crate::setup::screen("machine-run", card, terminal, cx))
    }

    /// Whatever covers the split tree: a machine run, Local's herdr setup,
    /// or a remote machine's offline screen.
    fn render_cover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The setup screen and its terminal are about Local's herdr.
        let local = self.state.read(cx).selected().is_local();
        let setup_terminal = self
            .setup_terminal
            .read(cx)
            .terminal()
            .filter(|_| self.visible && local)
            .map(|terminal| terminal.clone().into_any_element());
        self.render_machine_run(cx)
            .or_else(|| {
                if local {
                    let state = self.state.read(cx);
                    crate::setup::render(state.setup(), state, setup_terminal, cx)
                } else {
                    None
                }
            })
            .or_else(|| self.render_machine_problem(cx))
    }

    /// What the area says when there is no tab to show.
    fn empty_message(&self, snapshot: &Snapshot, cx: &App) -> String {
        let state = self.state.read(cx);
        let machine = state.selected_machine();
        if !machine.id().is_local() {
            match machine.status() {
                Status::Connected if snapshot.generation != 0 => {
                    format!("No workspaces on {}.", machine.label())
                }
                Status::Connected | Status::Connecting => {
                    format!("Connecting to {}…", machine.label())
                }
                _ => format!("{} is offline", machine.label()),
            }
        } else if let Some(config) = state.herdr_config.as_ref()
            && snapshot.generation == 0
        {
            format!(
                "Connecting to herdr ({})…",
                config.session.as_deref().unwrap_or("default session")
            )
        } else if state.herdr_config.is_none() {
            let reason = machine.status().reason().unwrap_or("not connected");
            format!("herdr is disconnected: {reason}")
        } else {
            "No workspaces. Create one from the sidebar.".to_owned()
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

    /// The cover, or the active tab's split tree, or why there is none.
    fn render_body(&self, frame: &mut Frame, cx: &mut Context<Self>) -> AnyElement {
        if let Some(cover) = self.render_cover(cx) {
            return cover;
        }
        let snapshot = frame.snapshot.clone();
        let Some(tab) = Self::active_tab(&snapshot) else {
            let message = self.empty_message(&snapshot, cx);
            return self.render_empty(message, cx);
        };
        let Some(root) = tab.root.as_ref() else {
            return self.render_empty("Reading layout…".to_owned(), cx);
        };
        let tab_id = &tab.info.tab_id;
        let zoomed = tab.zoomed.then(|| tab.focused_pane_id.clone()).flatten();
        let tree = match zoomed {
            Some(pane_id) => SplitTree::Pane(pane_id),
            None => self.with_overrides(tab_id, root),
        };
        self.render_node(tab_id, &tree, &mut Vec::new(), frame, cx)
    }

    /// Window-wide pointer tracking for divider and pane drags. (The
    /// workspace tells the overlay where the pointer is.)
    fn pointer_tracking(cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let on_move = this.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    on_move
                        .update(cx, |this, cx| {
                            this.drag_divider_to(event.position, cx);
                            this.move_pane_drag(event.position, cx);
                        })
                        .ok();
                });
                let this = this.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        this.update(cx, |this, cx| {
                            this.end_divider_drag(cx);
                            this.end_pane_drag(cx);
                        })
                        .ok();
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }
}

/// herdr's ratio for the split at `key`, if it is still there.
fn herdr_ratio(snapshot: &Snapshot, key: &SplitKey) -> Option<f32> {
    snapshot
        .tab(&key.tab_id)
        .and_then(|tab| tab.root.as_ref())
        .and_then(|root| root.at(&key.path))
        .and_then(|node| match node {
            SplitTree::Split { ratio, .. } => Some(*ratio),
            SplitTree::Pane(_) => None,
        })
}

impl Render for TerminalArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.terminals.report_detached(cx);
        let mut frame = Frame {
            snapshot: self.snapshot(cx),
            visible: HashSet::new(),
        };
        {
            let mut drawn = self.drawn.borrow_mut();
            drawn.panes.clear();
            drawn.splits.clear();
            drawn.headers.clear();
            drawn.reattach.clear();
        }
        let body = self.render_body(&mut frame, cx);

        let visible: HashSet<&TerminalId> = if self.visible {
            frame.visible.iter().collect()
        } else {
            HashSet::new()
        };
        self.terminals.show_only(&visible, cx);

        if self.pane_drag.is_some() && !cx.has_active_drag() {
            // Dropped or cancelled without a mouse-up reaching us.
            self.end_pane_drag(cx);
        }
        v_flex()
            .id("terminal-area")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
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
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
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
            .child(div().flex_1().min_h_0().p(GAP).child(body))
            .child(Self::pointer_tracking(cx))
    }
}

impl Focusable for TerminalArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
