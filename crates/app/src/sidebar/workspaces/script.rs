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
//! - `worktree <branch>`: submit the New Worktree prompt with `branch`
//! - `new-worktree-in <repo name>`: a repo row's + (opens the prompt)
//! - `repo-worktree <repo name> <branch>`: submit that prompt with `branch`
//! - `rename <workspace>`: the menu's Rename Workspace… (chrome's prompt)
//! - `close <workspace>`: the row's × (chrome confirms if agents run)
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
use super::groups::Kind;
use super::{AgentList, WorkspaceList, agents, close_workspace, rename_workspace};
use crate::attention::{agent_name, status_label};
use crate::overlay;

pub(super) fn start(
    spaces: WeakEntity<WorkspaceList>,
    agents: WeakEntity<AgentList>,
    window: &mut Window,
    cx: &mut App,
) {
    if !cfg!(debug_assertions) {
        return;
    }
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
                        Some(("agent", pane)) => {
                            let _ = agents
                                .update(cx, |this, cx| this.activate(pane.trim(), window, cx));
                        }
                        _ if step == "sort" => {
                            let _ = agents.update(cx, |this, cx| this.toggle_sort(cx));
                        }
                        _ => {
                            let sort = agents.read_with(cx, |this, _| this.sort).ok();
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
        "focus" => this.activate_workspace(arg, cx),
        "fold" => match repo_named(this, arg, cx) {
            Some(repo) => this.toggle_group(&repo.key, cx),
            None => eprintln!("[sidebar-script] no repo {arg}"),
        },
        "new-workspace" => this.new_workspace(cx),
        "new-worktree" => this.new_worktree(None, window, cx),
        "worktree" => this.create_worktree(arg, None, window, cx),
        "new-worktree-in" => match repo_named(this, arg, cx) {
            Some(repo) => this.new_worktree(Some(repo.root), window, cx),
            None => eprintln!("[sidebar-script] no repo {arg}"),
        },
        "repo-worktree" => {
            let (name, branch) = arg.split_once(' ').unwrap_or((arg, ""));
            match repo_named(this, name, cx) {
                Some(repo) => this.create_worktree(branch, Some(&repo.root), window, cx),
                None => eprintln!("[sidebar-script] no repo {name}"),
            }
        }
        "rename" => rename_workspace(arg, window, cx),
        "close" => close_workspace(arg, window, cx),
        "log" => log(this, arg, sort, cx),
        other => eprintln!("[sidebar-script] unknown step {other}"),
    }
}

fn repo_named(this: &WorkspaceList, name: &str, cx: &App) -> Option<Repo> {
    let snapshot = this.state.read(cx).snapshot.clone();
    this.repos(&snapshot).into_iter().find(|r| r.name == name)
}

fn log(this: &WorkspaceList, label: &str, sort: Option<super::AgentSort>, cx: &App) {
    let snapshot = this.state.read(cx).snapshot.clone();
    eprintln!(
        "[sidebar-script] {label}: focused workspace {:?}, pane {:?}, agents {sort:?}",
        snapshot.focused_workspace_id, snapshot.focused_pane_id,
    );
    for entry in this.entries(&snapshot) {
        let info = entry.info;
        let lead = match entry.kind {
            Kind::Parent
                if entry
                    .repo
                    .as_ref()
                    .is_some_and(|r| this.folded.contains(&r.key)) =>
            {
                "▸ "
            }
            Kind::Parent => "▾ ",
            Kind::Child => "   ",
            Kind::Plain => " ",
        };
        let checkout = this.checkout(&snapshot, info);
        eprintln!(
            "[sidebar-script]   {lead}{} {} [{}] status={}",
            info.label,
            info.workspace_id,
            entry.repo.as_ref().map_or("-", |r| r.name.as_str()),
            status_label(info.agent_status),
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
            agent_name(a),
            status_label(a.agent_status),
            a.workspace_id,
        );
    }
}
