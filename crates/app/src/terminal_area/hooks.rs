//! Debug builds only: what the live test harness (`crate::script`) may see
//! and do in the terminal area, so it can stay outside this module and
//! the area's fields stay private.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::*;

pub use super::layout::neighbor;
pub use super::terminals::Terminals;
use super::{SplitKey, TerminalArea};
use crate::herdr::{PaneId, Snapshot, SplitTree, Tab, TabId};
use crate::state::{AppState, MachineId};
use crate::terminal_area::command_terminal::CommandTerminal;

/// The space between panes.
pub const GAP: Pixels = super::GAP;
/// A pane header's height, above its terminal.
pub const HEADER_HEIGHT: Pixels = super::HEADER_HEIGHT;

/// Where things were drawn last frame, copied out of the area's record.
#[derive(Default, Clone)]
pub struct Drawn {
    /// Each split's bounds, by tab and path from the root (`false` is the
    /// first child).
    pub splits: HashMap<(TabId, Vec<bool>), Bounds<Pixels>>,
    /// Terminal body (the native view's slot) of each visible pane.
    pub panes: HashMap<PaneId, Bounds<Pixels>>,
    pub headers: HashMap<PaneId, Bounds<Pixels>>,
    pub tabs: HashMap<TabId, Bounds<Pixels>>,
    /// Reattach buttons, by pane.
    pub reattach: HashMap<PaneId, Bounds<Pixels>>,
}

/// A read-only view of a [`TerminalArea`] for checks.
#[derive(Clone, Copy)]
pub struct Hooks<'a>(&'a TerminalArea);

impl TerminalArea {
    pub fn hooks(&self) -> Hooks<'_> {
        Hooks(self)
    }
}

impl<'a> Hooks<'a> {
    pub fn state(&self) -> &'a Entity<AppState> {
        &self.0.state
    }

    pub fn snapshot(&self, cx: &App) -> Arc<Snapshot> {
        self.0.snapshot(cx)
    }

    /// The machine on screen.
    pub fn machine(&self, cx: &App) -> MachineId {
        self.0.machine(cx)
    }

    pub fn terminals(&self) -> &'a Terminals {
        &self.0.terminals
    }

    pub fn setup_terminal(&self) -> &'a Entity<CommandTerminal> {
        &self.0.setup_terminal
    }

    pub fn machine_terminal(&self) -> &'a Entity<CommandTerminal> {
        &self.0.machine_terminal
    }

    pub fn is_visible(&self) -> bool {
        self.0.is_visible()
    }

    pub fn drawn(&self) -> Drawn {
        let drawn = self.0.drawn.borrow();
        Drawn {
            splits: drawn
                .splits
                .iter()
                .map(|(key, bounds)| ((key.tab_id.clone(), key.path.clone()), *bounds))
                .collect(),
            panes: drawn.panes.clone(),
            headers: drawn.headers.clone(),
            tabs: drawn.tabs.clone(),
            reattach: drawn.reattach.clone(),
        }
    }

    /// A divider's local ratio while it is dragged or until herdr's layout
    /// catches up.
    pub fn override_ratio(&self, tab_id: &TabId, path: &[bool]) -> Option<f32> {
        let key = SplitKey {
            tab_id: tab_id.clone(),
            path: path.to_vec(),
        };
        self.0.overrides.get(&key).map(|o| o.ratio)
    }

    /// `root` with the local divider ratios applied, as drawn.
    pub fn with_overrides(&self, tab_id: &TabId, root: &SplitTree) -> SplitTree {
        self.0.with_overrides(tab_id, root)
    }

    /// A pane header is being dragged (its preview is in the overlay).
    pub fn pane_dragging(&self) -> bool {
        self.0.pane_drag.is_some()
    }

    /// The pane commands act on.
    pub fn target_pane(&self, cx: &App) -> Option<PaneId> {
        self.0.target_pane(cx)
    }

    /// The tab on screen: the focused workspace's active tab.
    pub fn active_tab(snapshot: &Snapshot) -> Option<&Tab> {
        TerminalArea::active_tab(snapshot)
    }
}

/// Re-attaches `machine`'s detached terminals, as after a reconnect.
pub fn reattach_machine(
    area: &mut TerminalArea,
    machine: &MachineId,
    window: &mut Window,
    cx: &mut Context<TerminalArea>,
) {
    area.reattach_machine(machine, window, cx);
}

/// Closes `pane_id`, asking first when an agent runs there.
pub fn close_pane_confirmed(
    area: &mut TerminalArea,
    pane_id: PaneId,
    window: &mut Window,
    cx: &mut Context<TerminalArea>,
) {
    area.close_pane_confirmed(pane_id, window, cx);
}
