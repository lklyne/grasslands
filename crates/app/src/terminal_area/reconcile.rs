//! Keeps the Ghostty views ([`Terminals`](super::terminals::Terminals)) and
//! per-pane herdr settings in step with each machine's snapshot: spawning
//! and dropping views, re-attaching after a dropped connection, noticing
//! attaches that ended, and the right-click policy.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::*;

use crate::herdr::{PaneId, PaneInfo, RightClickTarget, Snapshot, TerminalId};
use crate::state::{MachineId, On};

use super::TerminalArea;
use super::terminals::AttachCommands;

/// Panes already told to forward right-clicks to their program, on every
/// machine.
#[derive(Debug, Default)]
pub(super) struct RightClicks(HashSet<On<PaneId>>);

impl RightClicks {
    /// Forgets `machine`'s panes missing from `live`, and returns those in
    /// `live` not yet told.
    fn sync<'a>(
        &mut self,
        machine: &MachineId,
        live: impl Iterator<Item = &'a PaneId> + Clone,
    ) -> Vec<PaneId> {
        let still: HashSet<&PaneId> = live.clone().collect();
        self.0
            .retain(|on| &on.machine != machine || still.contains(&on.id));
        live.filter(|pane_id| self.0.insert(On::new(machine.clone(), (*pane_id).clone())))
            .cloned()
            .collect()
    }
}

impl TerminalArea {
    /// How to attach `terminal_id` on `machine`: `None` while a remote
    /// machine's herdr isn't found yet.
    ///
    /// A terminal's first view takes it over, like a newly launched client
    /// should (a previous app instance may still hold it). Re-attaching on
    /// its own after that, the app never takes a terminal back from a
    /// client that took it (herdr refuses, and the pane shows Detached);
    /// only the person's Reattach does. See
    /// docs/decisions/0019-remote-machines.md.
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

    /// Commands for `machine`'s terminals that have no view yet. A first
    /// view takes the terminal over.
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
                let command = self.attach_command(machine, &pane.terminal_id, true, cx)?;
                Some((pane.terminal_id.clone(), command))
            })
            .collect()
    }

    /// The person's Reattach on a detached pane of the selected machine:
    /// takes the terminal over.
    pub(super) fn reattach_pane(
        &mut self,
        pane: &PaneInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let machine = self.terminals.selected().clone();
        let command = self
            .attach_command(&machine, &pane.terminal_id, true, cx)
            .unwrap_or_else(|| Err(format!("{machine} isn't connected")));
        self.terminals.reattach(&machine, pane, command, window, cx);
        cx.notify();
    }

    /// Attaches again every view on `machine` whose attach ended, which is
    /// what a dropped connection does to all of them. Panes that went away
    /// meanwhile are dropped by the next snapshot instead. Without takeover:
    /// a terminal another client took meanwhile stays with it.
    pub(super) fn reattach_machine(
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
    pub(super) fn sync_other_machines(
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

    /// Spawns views for the selected machine's new terminals and drops
    /// those whose panes closed.
    pub(super) fn reconcile_terminals(
        &mut self,
        machine: &MachineId,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let commands = self.attach_commands(machine, snapshot, cx);
        let changes = self
            .terminals
            .reconcile(machine, snapshot, Some(&commands), window, cx);
        if !changes.spawned.is_empty() || !changes.dropped.is_empty() {
            tracing::info!(
                "terminals: spawned {:?}, dropped {:?}, total {}",
                changes.spawned,
                changes.dropped,
                self.terminals.len()
            );
        }
    }

    /// `right_click: "pane"`: programs in the pane get right-clicks; our
    /// context menu is on the pane header. This is the only place that sets
    /// it, for every pane however it was made (split, new tab, an agent's
    /// `pane.split`).
    pub(super) fn forward_right_clicks(
        &mut self,
        machine: &MachineId,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) {
        for pane_id in self.right_clicks.sync(machine, snapshot.panes.keys()) {
            self.state
                .read(cx)
                .set_right_click(machine, &pane_id, RightClickTarget::Pane, cx)
                .detach_and_log_err(cx);
        }
    }

    /// An attach process can end without any herdr event (another client
    /// took the terminal over), so check once a second and redraw to show
    /// the pane as detached.
    pub(super) fn watch_liveness(cx: &mut Context<Self>) -> Task<()> {
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
}

#[cfg(test)]
mod tests {
    use super::RightClicks;
    use crate::herdr::PaneId;
    use crate::state::MachineId;

    fn panes(ids: &[&str]) -> Vec<PaneId> {
        ids.iter().map(|id| PaneId::from(*id)).collect()
    }

    #[test]
    fn right_clicks_tells_each_pane_once() {
        let mut sent = RightClicks::default();
        let live = panes(&["p1", "p2"]);
        let mut first = sent.sync(&MachineId::Local, live.iter());
        first.sort();
        assert_eq!(first, live);
        assert!(sent.sync(&MachineId::Local, live.iter()).is_empty());
    }

    #[test]
    fn right_clicks_tells_a_reused_id_again_after_it_closed() {
        let mut sent = RightClicks::default();
        sent.sync(&MachineId::Local, panes(&["p1"]).iter());
        sent.sync(&MachineId::Local, panes(&[]).iter());
        assert_eq!(
            sent.sync(&MachineId::Local, panes(&["p1"]).iter()),
            panes(&["p1"])
        );
    }

    #[test]
    fn right_clicks_keeps_machines_apart() {
        let mut sent = RightClicks::default();
        let remote = MachineId::Remote("box".into());
        sent.sync(&MachineId::Local, panes(&["p1"]).iter());
        assert_eq!(sent.sync(&remote, panes(&["p1"]).iter()), panes(&["p1"]));
        // Another machine's sync leaves Local's panes alone.
        sent.sync(&remote, panes(&[]).iter());
        assert!(
            sent.sync(&MachineId::Local, panes(&["p1"]).iter())
                .is_empty()
        );
    }
}
