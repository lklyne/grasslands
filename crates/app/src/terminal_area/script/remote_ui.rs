//! `RT_SCRIPT=remote`: the wired machine UI against `dockerbox` (the
//! `herdr-remote-test` container). The sidebar's Local and dockerbox groups,
//! collapsing one, clicking `api` on dockerbox (attached terminal, title,
//! remote files panel), the machine menu, the "+" menu with "New Workspace
//! on ›", workspace commands routed to dockerbox, the Add Machine dialog, and
//! going offline and back (`docker stop`, `docker start`). Each step is
//! screenshotted to `target/script-shots/`.
//!
//! With `RT_ADD_MACHINE=<ssh target>` and `RT_ALLOW_MACHINE_EDITS=1` it
//! then runs the real add flow for that target (typing `RT_ADD_KEYS` into its
//! terminal, if set) and renames what herdr saved to `RT_ADD_LABEL` (default
//! `dockerbox2`). It doesn't remove it; do that with `herdr machine remove`.
//!
//! ```sh
//! GRASSLANDS_DEV_MACHINES=dockerbox RT_SCRIPT=remote scripts/dev-run cargo run -p app
//! ```
//!
//! Stopping the container ends whatever else is connected to it.

use std::time::Duration;

use gpui_kit::*;

use super::remote::{all_attached, docker, focus, remote, shows};
use super::{Cx, Run};
use crate::chrome::HerdrCommand;
use crate::herdr::MachineStatus;
use crate::sidebar;
use crate::state::{MachineId, MachineTask, RunStage};
use crate::terminal_area::TerminalArea;

const CONTAINER: &str = "herdr-remote-test";
const SCRATCH: &str = "wire-script";
const LOCAL_WORKSPACE: &str = "wire-local";

impl Run {
    pub(super) async fn remote_ui(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: remote");
        let state = self.read(cx, |area, _| area.state.clone())?;
        let connected = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                area.snapshot(cx).generation > 0
                    && remote(area, cx).is_some_and(|m| {
                        m.status() == &MachineStatus::Connected && m.snapshot().generation > 0
                    })
            })
            .await;
        self.check(connected, "Local and dockerbox connected");
        let Some(id) = self.read(cx, |area, cx| remote(area, cx).map(|m| m.id().clone()))? else {
            return Ok(());
        };
        if !connected {
            return Ok(());
        }
        // A Local workspace, so both groups have rows; closed at the end.
        let created = self
            .read(cx, |area, cx| {
                area.state.read(cx).new_workspace(
                    &MachineId::Local,
                    None,
                    Some(LOCAL_WORKSPACE.into()),
                    cx,
                )
            })?
            .await;
        let local_workspace = created.ok().map(|w| w.workspace_id);
        self.check(local_workspace.is_some(), "made a Local workspace");
        let api = self.read(cx, |area, cx| workspace_named(area, cx, &id, "api"))?;
        let Some(api) = api else {
            self.fail("dockerbox has an api workspace");
            return Ok(());
        };

        // Groups: a header for Local and for dockerbox, dockerbox's rows.
        let grouped = self
            .until(cx, Duration::from_secs(5), |_, _, cx| {
                sidebar::spaces(cx).is_some_and(|spaces| {
                    let spaces = spaces.read(cx);
                    spaces.drawn_header(&MachineId::Local).is_some()
                        && spaces.drawn_header(&id).is_some()
                        && spaces.drawn_row(&id, &api).is_some()
                })
            })
            .await;
        self.check(grouped, "the sidebar has Local and dockerbox groups");
        self.shoot(cx, "remote-groups");

        // Collapsing dockerbox hides its rows; opening it shows them again.
        self.spaces(cx, |spaces, _, cx| spaces.toggle_machine(&id, cx))?;
        let hidden = self
            .until(cx, Duration::from_secs(3), |_, _, cx| {
                sidebar::spaces(cx).is_some_and(|s| s.read(cx).drawn_row(&id, &api).is_none())
            })
            .await;
        self.check(hidden, "collapsing dockerbox hides its workspaces");
        self.shoot(cx, "remote-collapsed");
        self.spaces(cx, |spaces, _, cx| spaces.toggle_machine(&id, cx))?;

        // Clicking api on dockerbox selects the machine and attaches.
        Self::settle(cx).await;
        let row = self.spaces(cx, |spaces, _, _| spaces.drawn_row(&id, &api))?;
        let Some(row) = row else {
            self.fail("api's row is drawn");
            return Ok(());
        };
        self.click(cx, point(row.left() + px(40.), row.center().y))
            .await;
        let attached = self
            .until(cx, Duration::from_secs(30), |area, _, cx| {
                shows(area, cx, &id, &api) && all_attached(area, cx)
            })
            .await;
        self.check(
            attached,
            "clicking api on dockerbox shows its attached terminal",
        );
        let remote_selected = self.read(cx, |area, cx| {
            area.state.read(cx).active_worktree().is_none()
                && !area.state.read(cx).selected().is_local()
        })?;
        self.check(
            remote_selected,
            "a remote workspace has no local worktree (files empty)",
        );
        Self::sleep(cx, 1000).await;
        self.shoot(cx, "remote-api");

        // The machine menu.
        let header = self.spaces(cx, |spaces, _, _| spaces.drawn_header(&id))?;
        if let Some(header) = header {
            let at = point(header.right() - px(12.), header.bottom());
            self.spaces(cx, |spaces, window, cx| {
                spaces.open_machine_menu(&id, at, window, cx)
            })?;
            let open = self.menu_is_open(cx, true).await;
            self.check(open, "the machine menu opens");
            self.shoot(cx, "remote-machine-menu");
            self.key(cx, "escape");
            let closed = self.menu_is_open(cx, false).await;
            self.check(closed, "escape closes the machine menu");
        }

        // The "+" menu with "New Workspace on ›" open.
        let local_header =
            self.spaces(cx, |spaces, _, _| spaces.drawn_header(&MachineId::Local))?;
        if let Some(header) = local_header {
            // Under the panel's + button, where a click opens it.
            let at = point(header.right() - px(36.), header.top() - px(14.));
            self.spaces(cx, |spaces, window, cx| {
                spaces.open_new_menu(at, window, cx)
            })?;
            let open = self.menu_is_open(cx, true).await;
            self.check(open, "the + menu opens");
            self.key(cx, "down");
            self.key(cx, "down");
            Self::settle(cx).await;
            self.key(cx, "right");
            Self::sleep(cx, 300).await;
            Self::settle(cx).await;
            self.shoot(cx, "remote-new-menu");
            self.key(cx, "escape");
            self.key(cx, "escape");
            Self::settle(cx).await;
            if self.menu_is_open(cx, true).await {
                self.close_menu(cx)?;
            }
        }

        // New Workspace on › dockerbox, then rename and close it through the
        // chrome's commands, which go to dockerbox.
        self.spaces(cx, |spaces, window, cx| {
            spaces.new_workspace_on(&id, window, cx)
        })?;
        let made = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                remote(area, cx).is_some_and(|m| m.snapshot().workspaces.len() > 3)
                    && area.state.read(cx).selected() == &id
            })
            .await;
        let scratch = self.read(cx, |area, cx| {
            let m = remote(area, cx)?;
            m.snapshot().focused_workspace_id.clone()
        })?;
        self.check(
            made && scratch.is_some(),
            "New Workspace on dockerbox makes one there",
        );
        if let Some(scratch) = scratch {
            self.chrome_run(
                cx,
                HerdrCommand::RenameWorkspace {
                    machine: id.clone(),
                    workspace_id: scratch.clone(),
                    label: SCRATCH.into(),
                },
            )?;
            let renamed = self
                .until(cx, Duration::from_secs(10), |area, _, cx| {
                    workspace_named(area, cx, &id, SCRATCH).as_deref() == Some(scratch.as_str())
                })
                .await;
            self.check(renamed, "renaming a dockerbox workspace renames it there");
            self.shoot(cx, "remote-new-workspace");
            // herdr's `ui.confirm_close` would ask first; this checks the
            // close itself.
            cx.update(|_, cx| {
                crate::herdr_prefs::override_for_script(
                    crate::herdr::config::Edit::ConfirmClose(false),
                    cx,
                );
            })?;
            self.chrome_run(
                cx,
                HerdrCommand::CloseWorkspace {
                    machine: id.clone(),
                    workspace_id: scratch.clone(),
                },
            )?;
            let closed = self
                .until(cx, Duration::from_secs(10), |area, _, cx| {
                    workspace_named(area, cx, &id, SCRATCH).is_none()
                })
                .await;
            self.check(closed, "closing it closes it on dockerbox");
        }

        // The Add Machine dialog.
        focus(cx, &state, &id, &api)?;
        self.spaces(cx, |spaces, window, cx| spaces.add_machine(window, cx))?;
        Self::sleep(cx, 400).await;
        Self::settle(cx).await;
        self.shoot(cx, "remote-add-dialog");
        self.key(cx, "escape");
        self.key(cx, "escape");
        Self::settle(cx).await;

        // Offline and back.
        docker(cx, &["stop", "-t", "1", CONTAINER]).await;
        let offline = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                remote(area, cx).is_some_and(|m| matches!(m.status(), MachineStatus::Offline(_)))
            })
            .await;
        self.check(offline, "docker stop: dockerbox goes offline");
        Self::sleep(cx, 500).await;
        self.shoot(cx, "remote-offline");
        docker(cx, &["start", CONTAINER]).await;
        let reachable = self
            .until(cx, Duration::from_secs(90), |area, _, cx| {
                remote(area, cx).is_some_and(|m| {
                    matches!(
                        m.status(),
                        MachineStatus::Connected | MachineStatus::NeedsSetup(_)
                    )
                })
            })
            .await;
        self.check(reachable, "docker start: dockerbox is reachable again");
        let needs_server = self.read(cx, |area, cx| {
            remote(area, cx).is_some_and(|m| matches!(m.status(), MachineStatus::NeedsSetup(_)))
        })?;
        if needs_server {
            // The container boots sshd only; grasslands never starts a
            // remote server, so the script does.
            self.shoot(cx, "remote-needs-setup");
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
        let back = self
            .until(cx, Duration::from_secs(90), |area, _, cx| {
                remote(area, cx).is_some_and(|m| m.status() == &MachineStatus::Connected)
                    && area.state.read(cx).selected() == &id
                    && !area.drawn.borrow().panes.is_empty()
                    && all_attached(area, cx)
            })
            .await;
        self.check(back, "dockerbox recovers with an attached terminal");
        self.shoot(cx, "remote-recovered");

        // Adding edits the user's saved-machine list, so it takes both.
        if let Ok(target) = std::env::var("RT_ADD_MACHINE")
            && std::env::var_os("RT_ALLOW_MACHINE_EDITS").is_some_and(|v| v == "1")
        {
            let label = std::env::var("RT_ADD_LABEL").unwrap_or_else(|_| "dockerbox2".into());
            self.add_flow(cx, &target, &label).await?;
        }
        if let Some(workspace) = local_workspace {
            let close = self.read(cx, |area, cx| {
                area.state
                    .read(cx)
                    .close_workspace(&MachineId::Local, &workspace, false, cx)
            })?;
            self.check(close.await.is_ok(), "closed the Local workspace");
        }
        Ok(())
    }

    /// Adds `target` through the add run, answers its prompts in the
    /// terminal, waits for it to connect, then renames it `label`.
    async fn add_flow(&mut self, cx: &mut Cx, target: &str, label: &str) -> anyhow::Result<()> {
        let state = self.read(cx, |area, _| area.state.clone())?;
        let host = target.to_owned();
        cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.start_machine_run(MachineTask::Add { host }, cx)
            })
        })?;
        Self::sleep(cx, 1500).await;
        self.shoot(cx, "remote-add-running");
        // Whatever the command asks (`RT_ADD_KEYS`, `\n` for Enter) is
        // answered in its terminal, as a person would type it.
        if let Ok(keys) = std::env::var("RT_ADD_KEYS") {
            Self::sleep(cx, 3000).await;
            self.paste_machine_terminal(cx, &keys.replace("\\n", "\n"))?;
            Self::sleep(cx, 1000).await;
            self.shoot(cx, "remote-add-answered");
        }
        let settled = self
            .until(cx, Duration::from_secs(90), |area, _, cx| {
                area.state
                    .read(cx)
                    .machine_run()
                    .is_some_and(|r| matches!(r.stage, RunStage::Done | RunStage::Failed))
            })
            .await;
        let run = self.read(cx, |area, cx| {
            area.state
                .read(cx)
                .machine_run()
                .map(|r| (r.stage.clone(), r.machine.clone()))
        })?;
        self.check(
            settled && matches!(run, Some((RunStage::Done, Some(_)))),
            &format!("the add run connects the new machine ({run:?})"),
        );
        self.shoot(cx, "remote-add-done");
        let Some((_, Some(added))) = run else {
            return Ok(());
        };
        cx.update(|_, cx| state.update(cx, |state, cx| state.finish_machine_run(cx)))?;
        let rename = cx.update(|_, cx| {
            state.update(cx, |state, cx| state.rename_machine(&added, label, cx))
        })?;
        let renamed = rename.await.is_ok()
            && self
                .until(cx, Duration::from_secs(40), |area, _, cx| {
                    area.state
                        .read(cx)
                        .machine(&added)
                        .is_some_and(|m| m.label() == label)
                })
                .await;
        self.check(renamed, &format!("the new machine is renamed {label}"));
        let connected = self
            .until(cx, Duration::from_secs(60), |area, _, cx| {
                area.state
                    .read(cx)
                    .machine(&added)
                    .is_some_and(|m| m.status() == &MachineStatus::Connected)
            })
            .await;
        self.check(connected, &format!("{label} is connected"));
        Self::sleep(cx, 500).await;
        self.shoot(cx, "remote-added");
        Ok(())
    }

    fn spaces<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(
            &mut sidebar::workspaces::WorkspaceList,
            &mut Window,
            &mut Context<sidebar::workspaces::WorkspaceList>,
        ) -> R,
    ) -> anyhow::Result<R> {
        cx.update(|window, cx| {
            let spaces = sidebar::spaces(cx).ok_or_else(|| anyhow::anyhow!("no spaces panel"))?;
            Ok(spaces.update(cx, |spaces, cx| f(spaces, window, cx)))
        })?
    }

    async fn menu_is_open(&self, cx: &mut Cx, open: bool) -> bool {
        self.until(cx, Duration::from_secs(3), |_, window, cx| {
            crate::chrome::get(window, cx)
                .is_some_and(|chrome| chrome.read(cx).menu_open().is_some() == open)
        })
        .await
    }

    fn close_menu(&self, cx: &mut Cx) -> anyhow::Result<()> {
        cx.update(|window, cx| {
            if let Some(chrome) = crate::chrome::get(window, cx) {
                chrome.update(cx, |chrome, cx| chrome.close_context_menu(window, cx));
            }
        })
    }

    fn chrome_run(&self, cx: &mut Cx, command: HerdrCommand) -> anyhow::Result<()> {
        cx.update(|window, cx| {
            if let Some(chrome) = crate::chrome::get(window, cx) {
                chrome.update(cx, |chrome, cx| chrome.run(command, window, cx));
            }
        })
    }

    fn paste_machine_terminal(&self, cx: &mut Cx, text: &str) -> anyhow::Result<()> {
        let area = self.area.clone();
        cx.update(|_, cx| {
            let terminal = area
                .read(cx)
                .machine_terminal
                .as_ref()
                .map(|t| t.terminal.clone());
            if let Some(terminal) = terminal {
                terminal.update(cx, |terminal, _| terminal.paste_text(text));
            }
        })
    }
}

/// The id of the workspace labelled `label` on `machine`.
fn workspace_named(
    area: &TerminalArea,
    cx: &App,
    machine: &MachineId,
    label: &str,
) -> Option<String> {
    area.state
        .read(cx)
        .machine(machine)?
        .snapshot()
        .workspaces
        .iter()
        .find(|w| w.info.label == label)
        .map(|w| w.info.workspace_id.clone())
}
