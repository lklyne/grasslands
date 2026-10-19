//! herdr's tab bar and the active tab's split tree of Ghostty terminals.
//!
//! herdr is the source of truth: the area renders the focused workspace's
//! active tab from `AppState::snapshot` and turns every user action into a
//! herdr call, then re-renders from the snapshot that follows. The only local
//! state is a divider's ratio while it is being dragged (sent on release) and
//! until herdr's layout reflects it. See
//! docs/decisions/0008-terminal-area-and-overlay.md.

pub mod command_terminal;
#[cfg(debug_assertions)]
pub mod hooks;
mod layout;
mod terminals;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::slice;
use std::time::{Duration, Instant};

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::OpenContextMenu;
use crate::chrome::{Confirmation, MenuTarget};
use crate::herdr::{
    AgentStatus, PaneDirection, PaneId, PaneMoveDestination, RightClickTarget, Snapshot,
    SplitDirection, SplitTree, Tab, TabId, TerminalId, ZoomMode,
};
use crate::overlay;
use crate::setup::{Setup, Stage};
use crate::sidebar::files::DraggedPath;
use crate::state::{AppEvent, AppState, MachineId, MachineTask, On, Status};
use crate::{label, status};

use self::command_terminal::CommandTerminal;
use self::terminals::{AttachCommands, Launch, Terminals};

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
    pane_id: PaneId,
    label: SharedString,
}

/// A tab being dragged along the tab bar to a new place (`tab.move`).
#[derive(Clone)]
struct DraggedTab {
    tab_id: TabId,
    /// Its place in the bar, to show which side of a target it will land on.
    index: usize,
    label: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.label.clone())
    }
}

/// herdr's `tab.move` index for dropping the tab at `from` on the tab at
/// `to`, so it lands where `to` was. herdr counts the gaps of the bar before
/// the move, so a tab moving right skips past its target.
fn tab_insert_index(from: usize, to: usize) -> usize {
    if to > from { to + 1 } else { to }
}

/// GPUI's own drag preview, left empty: the main window draws under the
/// terminals, so the visible preview is a [`DragChip`] in the overlay.
struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The dragged pane's label, following the pointer in the overlay. It does
/// not occlude, so the overlay keeps letting the drag's mouse events through
/// to the main window, where the drop targets are.
struct DragChip {
    label: SharedString,
    position: Point<Pixels>,
}

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .left(self.position.x + px(12.))
            .top(self.position.y + px(10.))
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .bg(cx.theme().secondary)
            .text_color(cx.theme().secondary_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .child(self.label.clone())
    }
}

struct PaneDragPreview {
    overlay: Entity<overlay::Overlay>,
    layer: overlay::LayerId,
    chip: Entity<DragChip>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SplitKey {
    tab_id: TabId,
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
    overrides: HashMap<SplitKey, RatioOverride>,
    drag: Option<DividerDrag>,
    pane_drag: Option<PaneDragPreview>,
    drawn: Rc<RefCell<Drawn>>,
    /// Terminal ids whose attach process was running at the last check.
    attached: HashSet<TerminalId>,
    /// Panes already told to forward right-clicks to their program.
    right_click_set: HashSet<On<PaneId>>,
    /// False while the window shows something else in its place (a file
    /// tab); every native view is hidden then.
    visible: bool,
    /// The tab on screen at the last sync, to notice tab switches.
    shown_tab: Option<TabId>,
    /// A tab the user picked from the tab bar: its pane takes keyboard focus
    /// once herdr shows it. The bar is outside the area, so a click there
    /// takes focus out of it.
    focus_on_show: Option<TabId>,
    /// The dragged tab and the tab the drag last ticked over, so each new
    /// tab ticks once.
    tab_drag_tick: Option<(usize, usize)>,
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
            overrides: HashMap::new(),
            drag: None,
            pane_drag: None,
            drawn: Rc::default(),
            attached: HashSet::new(),
            right_click_set: HashSet::new(),
            visible: true,
            shown_tab: None,
            focus_on_show: None,
            tab_drag_tick: None,
            setup_terminal,
            machine_terminal,
            _subscriptions: subscriptions,
            _liveness: Self::watch_liveness(cx),
        };
        this.sync_with_snapshot(window, cx);
        this.sync_command_terminals(cx);
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

    /// An attach process can end without any herdr event (another client
    /// took the terminal over), so check once a second and redraw to show
    /// the pane as detached.
    fn watch_liveness(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    let attached: HashSet<TerminalId> = this
                        .terminals
                        .iter()
                        .filter(|(id, _)| this.terminals.is_attached(id, cx))
                        .map(|(id, _)| id.clone())
                        .collect();
                    if attached != this.attached {
                        this.attached = attached;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
    }

    fn start_pane_drag(
        &mut self,
        label: SharedString,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_pane_drag(cx);
        let overlay = match overlay::get_or_open(window, cx) {
            Ok(overlay) => overlay,
            Err(error) => {
                tracing::error!("drag preview: {error:#}");
                return;
            }
        };
        let chip = cx.new(|_| DragChip { label, position });
        let layer = overlay.update(cx, |overlay, cx| {
            overlay.push_layer(chip.clone(), overlay::Capture::Passive, cx)
        });
        self.pane_drag = Some(PaneDragPreview {
            overlay,
            layer,
            chip,
        });
    }

    fn move_pane_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(preview) = &self.pane_drag {
            preview.chip.update(cx, |chip, cx| {
                chip.position = position;
                cx.notify();
            });
        }
    }

    fn end_pane_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.pane_drag.take() {
            preview
                .overlay
                .update(cx, |overlay, cx| overlay.remove_layer(preview.layer, cx));
        }
    }

    fn snapshot(&self, cx: &App) -> std::sync::Arc<Snapshot> {
        self.state.read(cx).snapshot().clone()
    }

    /// The machine on screen, which the area's commands act on.
    fn machine(&self, cx: &App) -> MachineId {
        self.state.read(cx).selected().clone()
    }

    /// How to attach `terminal_id` on `machine`: `None` while a remote
    /// machine's herdr isn't found yet.
    ///
    /// Only the person's own Reattach passes `takeover`. Attaching on its
    /// own, the app never detaches another client (herdr refuses, and the
    /// pane shows Detached); see docs/decisions/0019-remote-machines.md.
    fn attach_command(
        &self,
        machine: &MachineId,
        terminal_id: &TerminalId,
        takeover: bool,
        cx: &App,
    ) -> Option<Result<String, String>> {
        match machine {
            MachineId::Local => Some(
                self.launch
                    .as_ref()
                    .map(|launch| launch.attach_command(terminal_id, takeover))
                    .ok_or_else(|| "herdr is disconnected".to_owned()),
            ),
            MachineId::Remote(_) => self
                .state
                .read(cx)
                .machine(machine)?
                .attach_command(terminal_id, takeover)
                .map(Ok),
        }
    }

    /// Commands for `machine`'s terminals that have no view yet.
    fn attach_commands(
        &self,
        machine: &MachineId,
        snapshot: &Snapshot,
        cx: &App,
    ) -> AttachCommands {
        snapshot
            .panes
            .values()
            .filter(|pane| !self.terminals.has(machine, &pane.terminal_id))
            .filter_map(|pane| {
                let command = self.attach_command(machine, &pane.terminal_id, false, cx)?;
                Some((pane.terminal_id.clone(), command))
            })
            .collect()
    }

    /// Attaches again every view on `machine` whose attach ended, which is
    /// what a dropped connection does to all of them. Panes that went away
    /// meanwhile are dropped by the next snapshot instead. Without takeover:
    /// a terminal another client took meanwhile stays with it.
    fn reattach_machine(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self
            .state
            .read(cx)
            .machine(machine)
            .map(|m| m.snapshot().clone())
        else {
            return;
        };
        let detached = self.terminals.detached_on(machine, cx);
        if detached.is_empty() {
            return;
        }
        tracing::info!("terminals on {machine}: re-attaching {detached:?}");
        for terminal_id in detached {
            let Some(pane) = snapshot
                .panes
                .values()
                .find(|pane| pane.terminal_id == terminal_id)
                .cloned()
            else {
                continue;
            };
            let Some(command) = self.attach_command(machine, &terminal_id, false, cx) else {
                continue;
            };
            self.terminals.reattach(machine, &pane, command, window, cx);
        }
        cx.notify();
    }

    /// Drops views on other machines (or just `only`) whose panes closed,
    /// and every view on a machine that's gone from the list. Their live
    /// views stay attached, so switching back doesn't re-attach them.
    fn sync_other_machines(
        &mut self,
        only: Option<&MachineId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let machines = self.terminals.other_machines();
        for machine in machines
            .into_iter()
            .filter(|m| only.is_none_or(|only| only == m))
        {
            let snapshot = self
                .state
                .read(cx)
                .machine(&machine)
                .map(|m| m.snapshot().clone());
            match snapshot {
                None => self.terminals.forget_machine(&machine, cx),
                // An empty cache is a sync that hasn't read yet, not a
                // machine without panes.
                Some(snapshot) if snapshot.generation == 0 => {}
                Some(snapshot) => {
                    self.terminals
                        .reconcile(&machine, &snapshot, None, window, cx);
                }
            }
        }
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
        let machine = self.state.read(cx).selected().clone();
        if self.terminals.selected() != &machine {
            // Divider overrides and the shown tab are by herdr id, which
            // another machine reuses.
            self.terminals.select(&machine, cx);
            // The setup terminal is Local's; it isn't drawn on another machine.
            self.sync_command_terminals(cx);
            self.overrides.clear();
            self.drag = None;
            self.shown_tab = None;
            self.focus_on_show = None;
        }
        let commands = self.attach_commands(&machine, &snapshot, cx);
        let changes = self
            .terminals
            .reconcile(&machine, &snapshot, Some(&commands), window, cx);
        if !changes.spawned.is_empty() || !changes.dropped.is_empty() {
            tracing::info!(
                "terminals: spawned {:?}, dropped {:?}, total {}",
                changes.spawned,
                changes.dropped,
                self.terminals.len()
            );
        }

        // `right_click: "pane"`: programs in the pane get right-clicks; our
        // context menu is on the pane header. This is the only place that
        // sets it, for every pane however it was made (split, new tab, an
        // agent's `pane.split`).
        self.right_click_set
            .retain(|on| on.machine != machine || snapshot.panes.contains_key(&on.id));
        for pane_id in snapshot.panes.keys() {
            if self
                .right_click_set
                .insert(On::new(machine.clone(), pane_id.clone()))
            {
                self.state
                    .read(cx)
                    .set_right_click(&machine, pane_id, RightClickTarget::Pane, cx)
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

        // A tab switch hides the terminal holding keyboard focus; the focus
        // moves to the new tab's pane, unless it is elsewhere (the editor).
        let shown_tab = Self::active_tab(&snapshot).map(|tab| tab.info.tab_id.clone());
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

    fn close_tab_confirmed(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let busy = snapshot
            .panes
            .values()
            .filter(|pane| pane.tab_id == tab_id && status::is_busy(pane.agent_status))
            .count();
        let state = self.state.clone();
        let machine = self.machine(cx);
        if busy == 0 {
            state
                .read(cx)
                .close_tab(&machine, &tab_id, cx)
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
                state
                    .read(cx)
                    .close_tab(&machine, &tab_id, cx)
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
                let task = cx.update(|cx| state.read(cx).close_tab(&machine, &tab_id, cx));
                if let Err(error) = task.await {
                    tracing::error!("close tab {tab_id}: {error}");
                }
            }
        })
        .detach();
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
        let machine = self.machine(cx);
        self.state
            .update(cx, |state, cx| state.focus_tab(&machine, &tab_id, cx))
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
        self.overrides.insert(
            key.clone(),
            RatioOverride {
                ratio,
                since: Instant::now(),
            },
        );
        let machine = self.machine(cx);
        self.state
            .read(cx)
            .set_split_ratio(&machine, &key.tab_id, key.path, ratio, cx)
            .detach_and_log_err(cx);
        cx.notify();
    }

    /// `root` with locally dragged ratios applied.
    fn with_overrides(&self, tab_id: &TabId, root: &SplitTree) -> SplitTree {
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
        // Keep the keyboard where it was (a terminal): without this the
        // right-click's mouse-down focuses the terminal area itself, and
        // that is what the menu gives focus back to when it closes.
        window.prevent_default();
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
        let snapshot = &*self.snapshot(cx);
        let theme = cx.theme();
        let active = Self::active_tab(snapshot);
        let workspace = active.and_then(|tab| snapshot.workspace(&tab.info.workspace_id));
        let workspace_id = workspace.map(|w| w.info.workspace_id.clone());
        let tabs: Vec<_> = workspace
            .map(|w| w.tabs.iter().map(|tab| tab.info.clone()).collect())
            .unwrap_or_default();
        let active_id = active.map(|tab| tab.info.tab_id.clone());
        let order: Rc<[TabId]> = tabs.iter().map(|tab| tab.tab_id.clone()).collect();
        let area = cx.entity().downgrade();
        // The strip wears the panel headers' background; the selected tab
        // takes a neutral fill.
        let (strip_bg, active_bg, muted, border, drop) = (
            theme.background,
            theme.secondary,
            theme.muted_foreground,
            theme.border,
            theme.drop_target,
        );
        let drag_border = theme.drag_border;

        h_flex()
            // The dock's 30px title bar plus its panels' 1px top border, so
            // the strip's bottom line meets theirs.
            .h(px(31.))
            .flex_shrink_0()
            .bg(strip_bg)
            .border_b_1()
            .border_color(border)
            .text_sm()
            .children(tabs.into_iter().enumerate().map(|(index, tab)| {
                let is_active = herdr_selected && active_id.as_deref() == Some(tab.tab_id.as_str());
                let (focus_id, close_id, drop_id, menu_id) = (
                    tab.tab_id.clone(),
                    tab.tab_id.clone(),
                    tab.tab_id.clone(),
                    tab.tab_id.clone(),
                );
                // herdr's TUI numbers tabs by place; `number` stays put
                // when a tab moves.
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
                let accept_id = tab.tab_id.clone();
                let (area, order) = (area.clone(), order.clone());
                h_flex()
                    .id(SharedString::from(format!("tab-{}", tab.tab_id)))
                    .relative()
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
                                    machine: this.state.read(cx).selected().clone(),
                                    tab_id: menu_id.clone(),
                                },
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                    .on_drag(dragged, move |dragged, _, _, cx| {
                        area.update(cx, |this, _| this.tab_drag_tick = None).ok();
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
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                            if event.bounds.contains(&event.event.position) {
                                this.tab_drag_tick_over(event.drag(cx).index, index);
                            }
                        },
                    ))
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                        this.tab_drag_tick = None;
                        // Where the tab is now: tabs may have come or gone,
                        // or the bar shows another workspace, since the drag began.
                        let Some(from) = order.iter().position(|id| *id == dragged.tab_id) else {
                            return;
                        };
                        let machine = this.machine(cx);
                        this.state
                            .read(cx)
                            .move_tab(&machine, &dragged.tab_id, tab_insert_index(from, index), cx)
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
                        let machine = this.machine(cx);
                        this.state
                            .read(cx)
                            .move_pane(&machine, &dragged.pane_id, destination, cx)
                            .detach_and_log_err(cx);
                    }))
            }))
            .children(files)
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
                            let machine = this.machine(cx);
                            this.state
                                .read(cx)
                                .new_tab(&machine, workspace_id.as_ref(), cx)
                                .detach_and_log_err(cx);
                        })
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedPane, _, cx| {
                        let destination = PaneMoveDestination::NewTab {
                            workspace_id: workspace_id.clone(),
                            label: None,
                        };
                        let machine = this.machine(cx);
                        this.state
                            .read(cx)
                            .move_pane(&machine, &dragged.pane_id, destination, cx)
                            .detach_and_log_err(cx);
                    })),
            )
            .into_any_element()
    }

    /// Ticks as a drag from tab `from` enters tab `over`, a semitone higher
    /// for each tab further right than where it started (lower going left).
    /// The first move only notes where the drag is.
    fn tab_drag_tick_over(&mut self, from: usize, over: usize) {
        let last = match self.tab_drag_tick {
            Some((origin, last)) if origin == from => last,
            _ => {
                self.tab_drag_tick = Some((from, over));
                return;
            }
        };
        if last == over {
            return;
        }
        self.tab_drag_tick = Some((from, over));
        let tabs = over as f64 - from as f64;
        let options = cues::PlayOptions::default().pitch(2f64.powf(tabs / 12.0));
        crate::sounds::play(cues::Cue::Select, options);
    }

    fn render_node(
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

    fn render_pane(
        &self,
        pane_id: &PaneId,
        with_header: bool,
        frame: &mut Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let snapshot = &frame.snapshot;
        let theme = cx.theme();
        let Some(pane) = snapshot.pane(pane_id) else {
            return div().into_any_element();
        };
        let focused = snapshot.focused_pane_id.as_deref() == Some(pane_id);
        let label: SharedString = label::pane_header(pane).into_owned().into();
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
                    .failure(&terminal_id)
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
                                let machine = this.terminals.selected().clone();
                                let command = this
                                    .attach_command(&machine, &reattach_pane.terminal_id, true, cx)
                                    .unwrap_or_else(|| Err(format!("{machine} isn't connected")));
                                this.terminals.reattach(
                                    &machine,
                                    &reattach_pane,
                                    command,
                                    window,
                                    cx,
                                );
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
        let (paste_pane, paste_file_pane) = (pane_id.to_owned(), pane_id.to_owned());
        let drawn = self.drawn.clone();
        let bounds_pane = pane_id.to_owned();
        let area = cx.entity().downgrade();
        let dragged = DraggedPane {
            pane_id: pane_id.to_owned(),
            label: label.clone(),
        };

        let header = h_flex()
            .id(SharedString::from(format!("pane-header-{header_pane}")))
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
            .bg(if focused { header_active } else { header_bg })
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
                this.swap(&dragged.pane_id, &drop_header, cx);
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
            );

        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .when(with_header, |el| el.child(header))
            .child(body)
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

    fn swap(&mut self, source: &PaneId, target: &PaneId, cx: &mut Context<Self>) {
        if source == target {
            return;
        }
        let snapshot = self.snapshot(cx);
        let same_tab = snapshot
            .pane(source)
            .zip(snapshot.pane(target))
            .is_some_and(|(a, b)| a.tab_id == b.tab_id);
        let machine = self.machine(cx);
        let state = self.state.read(cx);
        if same_tab {
            state
                .swap_panes(&machine, source, target, cx)
                .detach_and_log_err(cx);
        } else if let Some(target_pane) = snapshot.pane(target) {
            // From another tab: move it next to the target instead.
            let destination = PaneMoveDestination::Tab {
                tab_id: target_pane.tab_id.clone(),
                split: SplitDirection::Right,
                target_pane_id: Some(target.to_owned()),
                ratio: None,
            };
            state
                .move_pane(&machine, source, destination, cx)
                .detach_and_log_err(cx);
        }
    }

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
    visible: HashSet<TerminalId>,
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
            drawn.headers.clear();
            drawn.reattach.clear();
        }

        // The setup screen and its terminal are about Local's herdr.
        let local = self.state.read(cx).selected().is_local();
        let setup_terminal = self
            .setup_terminal
            .read(cx)
            .terminal()
            .filter(|_| self.visible && local)
            .map(|terminal| terminal.clone().into_any_element());
        let setup = self.render_machine_run(cx).or_else(|| {
            if local {
                let state = self.state.read(cx);
                crate::setup::render(state.setup(), state, setup_terminal, cx)
            } else {
                None
            }
        });
        let setup = setup.or_else(|| self.render_machine_problem(cx));
        let body = match (setup, Self::active_tab(&snapshot)) {
            (Some(setup), _) => setup,
            (None, None) => {
                let state = self.state.read(cx);
                let machine = state.selected_machine();
                let message = if !machine.id().is_local() {
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
                };
                self.render_empty(message, cx)
            }
            (None, Some(tab)) => match tab.root.as_ref() {
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
        let this = cx.entity().downgrade();
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
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                let snapshot = this.snapshot(cx);
                let workspace = Self::active_tab(&snapshot).map(|t| t.info.workspace_id.clone());
                let machine = this.state.read(cx).selected().clone();
                // Through the chrome, which asks for a name when herdr's
                // `ui.prompt_new_tab_name` says to.
                match crate::chrome::get(window, cx) {
                    Some(chrome) => chrome.update(cx, |chrome, cx| {
                        chrome.new_tab(machine, workspace, window, cx);
                    }),
                    None => this
                        .state
                        .read(cx)
                        .new_tab(&machine, workspace.as_ref(), cx)
                        .detach_and_log_err(cx),
                }
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
            .child(div().flex_1().min_h_0().p(GAP).child(body))
            .child(
                // Window-wide pointer tracking for divider and pane drags.
                // (The workspace tells the overlay where the pointer is.)
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
                                    if this.drag.is_some() {
                                        this.drag_divider_to(event.position, cx);
                                    }
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
                .size_0(),
            )
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

impl Focusable for TerminalArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
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
