//! Debug builds only: drive the spaces and agents panels from
//! `RT_SIDEBAR_SCRIPT`, so their flows can be checked live against the dev session without synthetic
//! OS input. Each step calls the same method the matching click calls.
//! Steps are `;`-separated:
//!
//! - `wait <ms>`
//! - `focus <workspace>`: click a workspace row (`workspace.focus`)
//! - `agent <pane>`: click an agent row
//! - `fold <repo name>`: click a group's ▾/▸
//! - `sort`: click the agents panel's "Grouped" / "Priority"
//! - `new-workspace`: the spaces panel's +
//! - `new-worktree`: the spaces panel's menu → New Worktree… (opens the prompt)
//! - `worktree <branch> [base=<ref>] [label=<text>]`: submit the New
//!   Worktree prompt with `branch` and, if given, its base ref and label
//! - `new-worktree-in <repo name>`: a repo row's + (opens the prompt)
//! - `repo-worktree <repo name> <branch>`: submit that prompt with `branch`
//! - `rename <workspace>`: the menu's Rename Workspace… (chrome's prompt)
//! - `close <workspace>`: the row's × (chrome confirms if agents run)
//! - `move <workspace> <target>`: drop a row on `target`'s row (no pointer)
//! - `drag <workspace> <target>`: drag a row with GPUI mouse events and
//!   release it over `target`: a workspace's row, `off` (the panel below the
//!   rows), `outside` (the window) or `cancel` (the app stops the drag before
//!   the release). The overlay is drawn at each step, as a toast or tooltip
//!   would; after the release nothing is drawn on the script's behalf.
//! - `overlay`: open the overlay window, as the first toast or menu would
//! - `check-drag <label>`: print whether a drag is active and whether the
//!   overlay's last frame was drawn during one (and so shows its preview)
//! - `key <keystroke>`: one key press (`enter`, `escape`, `down`, …) to the
//!   window that owns the keyboard: the overlay while a prompt or dialog is
//!   open there (`overlay::dispatch_keystroke`)
//! - `keys <text>`: type `text`, one key press per character, the same way
//! - `log <label>`: print the rows (as herdr would draw them), agents and
//!   focus to stderr
//!
//! e.g. `RT_SIDEBAR_SCRIPT='wait 2000; focus w2; wait 800; log after'`.

use std::time::Duration;

use gpui_kit::*;

use super::git::Repo;
use super::groups::{Kind, Slot};
use super::{AgentList, WorkspaceList, agents, close_workspace, rename_workspace};
use crate::overlay;
use crate::prompt::NewWorktree;
use crate::state::MachineId;
use crate::{label, status};

pub(super) fn start(
    spaces: WeakEntity<WorkspaceList>,
    agents: WeakEntity<AgentList>,
    window: &mut Window,
    cx: &mut App,
) {
    let Ok(script) = std::env::var("RT_SIDEBAR_SCRIPT") else {
        return;
    };
    let steps: Vec<String> = script
        .split(';')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    window
        .spawn(cx, async move |cx| {
            for step in steps {
                eprintln!("[sidebar-script] {step}");
                if let Some(arg) = step.strip_prefix("drag ") {
                    if drag(&spaces, arg, cx).await.is_err() {
                        return;
                    }
                    continue;
                }
                if let Some(ms) = step.strip_prefix("wait ") {
                    let ms = ms.trim().parse().unwrap_or(500);
                    cx.background_executor()
                        .timer(Duration::from_millis(ms))
                        .await;
                    continue;
                }
                let keys = match step.split_once(' ') {
                    Some(("key", key)) => Some(Keystroke::parse(key.trim()).map(|k| vec![k])),
                    Some(("keys", text)) => Some(Ok(overlay::keystrokes(text))),
                    _ => None,
                };
                // Outside the list's update: a key may land back in it.
                let done = match keys {
                    Some(Ok(keys)) => cx.update(|window, cx| {
                        for key in keys {
                            overlay::dispatch_keystroke(key, window, cx);
                        }
                    }),
                    Some(Err(error)) => {
                        eprintln!("[sidebar-script] {step}: {error}");
                        Ok(())
                    }
                    // The main window by name: `update_in` would use whichever
                    // window the list was last used in (see chrome/script.rs).
                    None => cx.update(|window, cx| match step.split_once(' ') {
                        _ if step == "overlay" => {
                            if let Err(error) = overlay::get_or_open(window, cx) {
                                eprintln!("[sidebar-script] overlay: {error:#}");
                            }
                        }
                        Some(("check-drag", label)) => {
                            let drew = overlay::get(window, cx)
                                .map(|overlay| overlay.read(cx).drew_drag(cx));
                            eprintln!(
                                "[sidebar-script] check {}: drag_active={} overlay_drew_drag={drew:?}",
                                label.trim(),
                                cx.has_active_drag(),
                            );
                        }
                        Some(("agent", pane)) => {
                            let _ = agents
                                .update(cx, |this, cx| this.activate(&pane.trim().into(), window, cx));
                        }
                        _ if step == "sort" => {
                            let _ = agents.update(cx, |this, cx| this.toggle_sort(cx));
                        }
                        _ => {
                            let sort = agents.read_with(cx, |this, cx| this.sort(cx)).ok();
                            let _ = spaces
                                .update(cx, |this, cx| run_step(this, &step, sort, window, cx));
                        }
                    }),
                };
                if done.is_err() {
                    return;
                }
            }
        })
        .detach();
}

fn run_step(
    this: &mut WorkspaceList,
    step: &str,
    sort: Option<super::AgentSort>,
    window: &mut Window,
    cx: &mut Context<WorkspaceList>,
) {
    let (verb, arg) = step.split_once(' ').unwrap_or((step, ""));
    let arg = arg.trim();
    match verb {
        "focus" => this.activate_workspace(&arg.into(), cx),
        "fold" => match repo_named(this, arg, cx) {
            Some(repo) => this.toggle_group(&repo.key, cx),
            None => eprintln!("[sidebar-script] no repo {arg}"),
        },
        "new-workspace" => this.new_workspace(window, cx),
        "new-worktree" => this.new_worktree(None, window, cx),
        "worktree" => this.create_worktree(new_worktree(arg), None, window, cx),
        "new-worktree-in" => match repo_named(this, arg, cx) {
            Some(repo) => this.new_worktree(Some(repo.root), window, cx),
            None => eprintln!("[sidebar-script] no repo {arg}"),
        },
        "repo-worktree" => {
            let (name, branch) = arg.split_once(' ').unwrap_or((arg, ""));
            match repo_named(this, name, cx) {
                Some(repo) => {
                    this.create_worktree(new_worktree(branch), Some(&repo.root), window, cx)
                }
                None => eprintln!("[sidebar-script] no repo {name}"),
            }
        }
        "rename" => rename_workspace(&selected(this, cx), &arg.into(), window, cx),
        "close" => close_workspace(&selected(this, cx), &arg.into(), window, cx),
        "move" => {
            let (dragged, target) = arg.split_once(' ').unwrap_or((arg, ""));
            match (slot(this, dragged, cx), slot(this, target.trim(), cx)) {
                (Some(dragged), Some(target)) => {
                    this.reorder(&selected(this, cx), &dragged, &target, cx)
                }
                _ => eprintln!("[sidebar-script] no row for {arg}"),
            }
        }
        "log" => log(this, arg, sort, cx),
        other => eprintln!("[sidebar-script] unknown step {other}"),
    }
}

/// `<branch> [base=<ref>] [label=<text>]` as the New Worktree prompt's fields.
fn new_worktree(arg: &str) -> NewWorktree {
    let mut words = arg.split_whitespace();
    let branch = words.next().unwrap_or_default();
    let (mut base, mut label) = ("", "");
    for word in words {
        if let Some(value) = word.strip_prefix("base=") {
            base = value;
        } else if let Some(value) = word.strip_prefix("label=") {
            label = value;
        }
    }
    NewWorktree::from_fields(branch, base, label)
}

/// The machine the script works on: the selected one.
fn selected(this: &WorkspaceList, cx: &App) -> MachineId {
    this.state.read(cx).selected().clone()
}

/// The row showing `workspace_id`, as a drag source or drop target.
fn slot(this: &WorkspaceList, workspace_id: &str, cx: &App) -> Option<Slot> {
    let snapshot = this.state.read(cx).snapshot().clone();
    this.entries(&selected(this, cx), &snapshot)
        .iter()
        .find(|e| e.info.workspace_id == workspace_id)
        .map(Slot::of)
}

fn repo_named(this: &WorkspaceList, name: &str, cx: &App) -> Option<Repo> {
    let snapshot = this.state.read(cx).snapshot().clone();
    this.repos(&selected(this, cx), &snapshot)
        .into_iter()
        .find(|r| r.name == name)
}

fn log(this: &WorkspaceList, label: &str, sort: Option<super::AgentSort>, cx: &App) {
    let snapshot = this.state.read(cx).snapshot().clone();
    eprintln!(
        "[sidebar-script] {label}: focused workspace {:?}, pane {:?}, agents {sort:?}",
        snapshot.focused_workspace_id, snapshot.focused_pane_id,
    );
    let machine = selected(this, cx);
    for entry in this.entries(&machine, &snapshot) {
        let info = entry.info;
        let lead = match entry.kind {
            Kind::Parent
                if entry
                    .repo
                    .as_ref()
                    .is_some_and(|r| this.is_folded(&machine, &r.key)) =>
            {
                "▸ "
            }
            Kind::Parent => "▾ ",
            Kind::Child => "   ",
            Kind::Plain => " ",
        };
        let checkout = this.checkout(&machine, &snapshot, info);
        eprintln!(
            "[sidebar-script]   {lead}{} {} [{}] status={}",
            info.label,
            info.workspace_id,
            entry.repo.as_ref().map_or("-", |r| r.name.as_str()),
            status::word(info.agent_status),
        );
        if entry.kind != Kind::Child
            && let Some(branch) = checkout.and_then(|c| c.branch.as_deref())
        {
            eprintln!(
                "[sidebar-script]      {branch} ahead/behind={:?}",
                checkout.and_then(|c| c.ahead_behind)
            );
        }
    }
    for a in agents::ordered(&snapshot, sort.unwrap_or(super::AgentSort::Grouped)) {
        eprintln!(
            "[sidebar-script]   agent {} {} {} in {}",
            a.pane_id,
            label::agent(&snapshot, a),
            status::word(a.agent_status),
            a.workspace_id,
        );
    }
}

/// The `drag` step; see the module docs. Errs only when the window is gone.
async fn drag(
    spaces: &WeakEntity<WorkspaceList>,
    arg: &str,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let (dragged, target) = arg.split_once(' ').unwrap_or((arg, ""));
    let (dragged, target) = (dragged.trim().to_owned(), target.trim().to_owned());
    let rows = spaces.read_with(cx, |this, _| this.drawn_rows.borrow().clone())?;
    // Rows by their text key: the bare id on Local, `<machine>/<id>` elsewhere.
    let row = |key: &str| {
        rows.iter()
            .find(|(on, _)| on.to_string() == key)
            .map(|(_, b)| *b)
    };
    let Some(from) = row(&dragged).map(|b| b.center()) else {
        eprintln!("[sidebar-script] drag: no row for {dragged}");
        return Ok(());
    };
    let lowest = rows.values().map(|b| b.bottom()).fold(from.y, Pixels::max);
    let to = match target.as_str() {
        "off" => point(from.x, lowest + px(60.)),
        "outside" => point(px(-200.), px(-200.)),
        "cancel" => point(from.x, lowest + px(30.)),
        id => match row(id) {
            Some(bounds) => bounds.center(),
            None => {
                eprintln!("[sidebar-script] drag: no row for {id}");
                return Ok(());
            }
        },
    };
    let executor = cx.background_executor().clone();
    let pause = || executor.timer(Duration::from_millis(40));
    cx.update(|window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position: from,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
    })?;
    pause().await;
    const STEPS: usize = 10;
    for step in 1..=STEPS {
        let t = step as f32 / STEPS as f32;
        let position = from + (to - from) * t;
        let handle = cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            overlay::get(window, cx).map(|overlay| overlay.read(cx).window_handle())
        })?;
        if let Some(handle) = handle {
            cx.update_window(handle, |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            })
            .ok();
        }
        pause().await;
    }
    cx.update(|window, cx| {
        if target == "cancel" {
            cx.stop_active_drag(window);
        }
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: to,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    })?;
    // Let frames come on their own: one the app asks for, none forced here.
    executor.timer(Duration::from_millis(400)).await;
    Ok(())
}
