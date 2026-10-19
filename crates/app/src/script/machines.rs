//! `RT_SCRIPT=machines`: a live remote machine. Connects to `dockerbox` (the
//! `herdr-remote-test` container), attaches one of its terminals, switches to
//! Local and back, then stops and starts the container and checks the
//! offline state, the cues, and that the remote terminal attaches again.
//!
//! ```sh
//! GRASSLANDS_DEV_MACHINES=dockerbox RT_SCRIPT=machines scripts/dev-run cargo run -p app
//! ```
//!
//! Stopping the container ends whatever else is connected to it.

use std::time::Duration;

use cues::Cue;
use gpui_kit::*;

use super::{Cx, Run};
use crate::herdr::{PaneId, WorkspaceId};
use crate::sounds;
use crate::state::{AppState, MachineId, Status};
use crate::terminal_area::hooks::Hooks;

const LABEL: &str = "dockerbox";
const CONTAINER: &str = "herdr-remote-test";
const LOCAL_WORKSPACE: &str = "machines";

impl Run {
    pub(super) async fn machines(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: machines");
        let state = self.read(cx, |area, _| area.state().clone())?;
        let local_up = self
            .until(cx, Duration::from_secs(30), |area, _, cx| {
                area.snapshot(cx).generation > 0
            })
            .await;
        self.check(local_up, "Local connected");
        let created = self
            .read(cx, |area, cx| {
                area.state().read(cx).new_workspace(
                    &MachineId::Local,
                    None,
                    Some(LOCAL_WORKSPACE.into()),
                    cx,
                )
            })?
            .await;
        let Ok(local_workspace) = created.map(|w| w.workspace_id) else {
            self.fail("created a Local workspace");
            return Ok(());
        };

        let connected = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                remote(area, cx)
                    .is_some_and(|m| m.status().is_usable() && m.snapshot().generation > 0)
            })
            .await;
        let status = self.read(cx, |area, cx| remote(area, cx).map(|m| m.status().clone()))?;
        self.check(connected, &format!("{LABEL} connected ({status:?})"));
        if !connected {
            return Ok(());
        }
        let found = self.read(cx, |area, cx| {
            let machine = remote(area, cx)?;
            let snapshot = machine.snapshot();
            let labels: Vec<String> = snapshot
                .workspaces
                .iter()
                .map(|w| w.info.label.clone())
                .collect();
            let api = snapshot
                .workspaces
                .iter()
                .find(|w| w.info.label == "api")
                .map(|w| w.info.workspace_id.clone());
            Some((machine.id().clone(), labels, api))
        })?;
        let Some((id, labels, api)) = found else {
            return Ok(());
        };
        self.check(
            labels.iter().any(|l| l == "api") && labels.iter().any(|l| l == "infra"),
            &format!("{LABEL} has api and infra ({labels:?})"),
        );
        let Some(api) = api else {
            return Ok(());
        };
        self.shoot(cx, "machines-connected");

        // On dockerbox's api workspace, a terminal attaches.
        focus(cx, &state, &id, &api)?;
        let attached = self
            .until(cx, Duration::from_secs(30), |area, _, cx| {
                shows(area, cx, &id, &api) && all_attached(area, cx)
            })
            .await;
        self.check(attached, "api on dockerbox shows an attached terminal");
        Self::sleep(cx, 2000).await;
        let (pane, view) = self.read(cx, first_view)?.unzip();
        let still = self.read(cx, all_attached)?;
        self.check(
            still && view.is_some(),
            "the remote terminal stays attached",
        );
        if let Some(pane) = &pane {
            let typed = self
                .type_in(cx, pane, "remote-$((40+2))", "remote-42")
                .await;
            self.check(typed, "keys typed reach the remote pane");
        }
        self.shoot(cx, "machines-remote-api");

        // Back on Local: its terminals are attached, dockerbox's stay alive.
        focus(cx, &state, &MachineId::Local, &local_workspace)?;
        let local = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                shows(area, cx, &MachineId::Local, &local_workspace) && all_attached(area, cx)
            })
            .await;
        self.check(local, "Local again shows attached terminals");
        let kept = self.read(cx, |area, cx| {
            area.terminals().detached_on(&id, cx).is_empty()
                && area.terminals().other_machines().contains(&id)
        })?;
        self.check(
            kept,
            "dockerbox's terminal stays attached in the background",
        );
        self.shoot(cx, "machines-local-again");

        focus(cx, &state, &id, &api)?;
        let back = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                shows(area, cx, &id, &api) && all_attached(area, cx)
            })
            .await;
        let same = self.read(cx, |area, cx| first_view(area, cx).map(|(_, v)| v))?;
        self.check(
            back && same.is_some() && same == view,
            "switching back to dockerbox reuses its terminal (no re-attach)",
        );

        // The connection drops.
        let cues_before = sounds::played::mark();
        docker(cx, &["stop", "-t", "1", CONTAINER]).await;
        let offline = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                remote(area, cx).is_some_and(|m| matches!(m.status(), Status::Offline { .. }))
            })
            .await;
        let status = self.read(cx, |area, cx| remote(area, cx).map(|m| m.status().clone()))?;
        self.check(
            offline,
            &format!("docker stop: {LABEL} goes offline ({status:?})"),
        );
        self.check(
            sounds::played::since(cues_before).contains(&Cue::Warning),
            "going offline plays the warning cue",
        );
        Self::settle(cx).await;
        self.shoot(cx, "machines-offline");

        // It comes back.
        let cues_before = sounds::played::mark();
        docker(cx, &["start", CONTAINER]).await;
        let reachable = self
            .until(cx, Duration::from_secs(90), |area, _, cx| {
                remote(area, cx).is_some_and(|m| {
                    matches!(m.status(), Status::Connected | Status::NeedsSetup(_))
                })
            })
            .await;
        self.check(reachable, "docker start: dockerbox is reachable again");
        let needs_server = self.read(cx, |area, cx| {
            remote(area, cx).is_some_and(|m| matches!(m.status(), Status::NeedsSetup(_)))
        })?;
        if needs_server {
            // The container boots sshd only; grasslands never starts a remote
            // server, so the script does.
            eprintln!("script: starting dockerbox's herdr server");
            docker(
                cx,
                &[
                    "exec",
                    "-d",
                    "-u",
                    "lyle",
                    "-e",
                    "HOME=/home/lyle",
                    "-w",
                    "/home/lyle",
                    CONTAINER,
                    "/home/lyle/.local/bin/herdr",
                    "server",
                ],
            )
            .await;
            Self::sleep(cx, 1000).await;
            cx.update(|_, cx| state.update(cx, |state, cx| state.reconnect_machine(&id, cx)))?;
        }
        let reconnected = self
            .until(cx, Duration::from_secs(90), |area, _, cx| {
                remote(area, cx).is_some_and(|m| m.status().is_usable())
            })
            .await;
        self.check(reconnected, "dockerbox connects again");
        self.check(
            sounds::played::since(cues_before).contains(&Cue::Success),
            "reconnecting plays the success cue",
        );
        let recovered = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                area.state().read(cx).selected() == &id
                    && !area.drawn().panes.is_empty()
                    && all_attached(area, cx)
            })
            .await;
        self.check(recovered, "the remote terminal attaches again on its own");
        let pane = self.read(cx, |area, cx| first_view(area, cx).map(|(pane, _)| pane))?;
        if let Some(pane) = &pane {
            let typed = self.type_in(cx, pane, "back-$((40+3))", "back-43").await;
            self.check(typed, "keys reach the remote pane after reconnecting");
        }
        self.shoot(cx, "machines-reconnected");

        focus(cx, &state, &MachineId::Local, &local_workspace)?;
        Self::settle(cx).await;
        let close = self.read(cx, |area, cx| {
            area.state()
                .read(cx)
                .close_workspace(&MachineId::Local, &local_workspace, false, cx)
        })?;
        if close.await.is_ok() {
            eprintln!("ok   closed the script's Local workspace");
        }
        Ok(())
    }

    /// Focuses `pane`, types `echo <text>`, and waits for `expect` in it.
    async fn type_in(&mut self, cx: &mut Cx, pane: &PaneId, text: &str, expect: &str) -> bool {
        cx.update(|window, cx| {
            self.area.update(cx, |area, cx| {
                let machine = area.hooks().machine(cx);
                area.focus_pane(&machine, pane, window, cx);
            });
        })
        .ok();
        Self::settle(cx).await;
        self.type_text(cx, &format!("echo {text}\n"));
        for _ in 0..40 {
            Self::sleep(cx, 150).await;
            if self.read_pane_text(cx, pane).await.contains(expect) {
                return true;
            }
        }
        false
    }
}

pub(super) fn remote<'a>(area: &Hooks, cx: &'a App) -> Option<&'a crate::state::Machine> {
    area.state()
        .read(cx)
        .machines(cx)
        .find(|m| m.label() == LABEL)
}

pub(super) fn focus(
    cx: &mut Cx,
    state: &Entity<AppState>,
    machine: &MachineId,
    workspace: &WorkspaceId,
) -> anyhow::Result<()> {
    cx.update(|_, cx| {
        state.update(cx, |state, cx| {
            state
                .focus_workspace(machine, workspace, cx)
                .detach_and_log_err(cx);
        });
    })
}

/// Whether the area shows `workspace` on `machine`.
pub(super) fn shows(area: &Hooks, cx: &App, machine: &MachineId, workspace: &str) -> bool {
    area.state().read(cx).selected() == machine
        && Hooks::active_tab(&area.snapshot(cx))
            .is_some_and(|tab| tab.info.workspace_id == workspace)
}

/// Every pane laid out has an attached terminal, and there is one.
pub(super) fn all_attached(area: &Hooks, cx: &App) -> bool {
    let snapshot = area.snapshot(cx);
    let drawn = area.drawn();
    !drawn.panes.is_empty()
        && drawn.reattach.is_empty()
        && drawn.panes.keys().all(|pane| {
            snapshot
                .pane(pane)
                .is_some_and(|p| area.terminals().is_attached(&p.terminal_id, cx))
        })
}

/// The first laid-out pane and its terminal view's entity.
fn first_view(area: &Hooks, cx: &App) -> Option<(PaneId, EntityId)> {
    let snapshot = area.snapshot(cx);
    let pane = area.drawn().panes.keys().next().cloned()?;
    let view = area.terminals().get(&snapshot.pane(&pane)?.terminal_id)?;
    Some((pane, view.terminal.entity_id()))
}

pub(super) async fn docker(cx: &mut Cx, args: &[&str]) {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let status = cx
        .background_executor()
        .spawn(async move {
            std::process::Command::new("docker")
                .args(&args)
                .stdout(std::process::Stdio::null())
                .status()
        })
        .await;
    eprintln!("script: docker exited {status:?}");
}
