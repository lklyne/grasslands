//! Debug builds only: drive the workspace list from `RT_SIDEBAR_SCRIPT`, so
//! its flows can be checked live against the dev session without synthetic
//! OS input. Each step calls the same method the matching click calls.
//! Steps are `;`-separated:
//!
//! - `wait <ms>`
//! - `focus <workspace>`: click a workspace row (`workspace.focus`)
//! - `agent <pane>`: click an agent row
//! - `toggle <workspace>`: click a row's chevron
//! - `toggle-repo <repo name>`: click a repo heading
//! - `new-workspace`: header + → New Workspace
//! - `new-worktree`: header + → New Worktree… (opens the prompt)
//! - `worktree <branch>`: submit the New Worktree prompt with `branch`
//! - `new-worktree-in <repo name>`: a repo heading's + (opens the prompt)
//! - `repo-worktree <repo name> <branch>`: submit that prompt with `branch`
//! - `rename <workspace>`: the menu's Rename Workspace… (chrome's prompt)
//! - `close <workspace>`: the row's × (chrome confirms if agents run)
//! - `key <keystroke>`: one key press (`enter`, `escape`, `down`, …) to the
//!   window that owns the keyboard: the overlay while a prompt or dialog is
//!   open there (`overlay::dispatch_keystroke`)
//! - `keys <text>`: type `text`, one key press per character, the same way
//! - `log <label>`: print the groups, workspaces, branches, agents and focus
//!   to stderr
//!
//! e.g. `RT_SIDEBAR_SCRIPT='wait 2000; focus w2; wait 800; log after'`.

use std::time::Duration;

use gpui_kit::*;

use super::git::Repo;
use super::{WorkspaceList, close_workspace, rename_workspace};
use crate::attention::{agent_name, status_label};
use crate::herdr::WorkspaceInfo;
use crate::overlay;

pub(super) fn start(window: &mut Window, cx: &mut Context<WorkspaceList>) {
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
    cx.spawn_in(window, async move |this, cx| {
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
                None => this.update_in(cx, |this, window, cx| run_step(this, &step, window, cx)),
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
    window: &mut Window,
    cx: &mut Context<WorkspaceList>,
) {
    let (verb, arg) = step.split_once(' ').unwrap_or((step, ""));
    let arg = arg.trim();
    match verb {
        "focus" => this.activate_workspace(arg, cx),
        "agent" => this.activate_agent(arg, window, cx),
        "toggle" => this.toggle(arg, cx),
        "toggle-repo" => match repo_named(this, arg, cx) {
            Some(repo) => this.toggle_repo(&repo.key, cx),
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
        "log" => log(this, arg, cx),
        other => eprintln!("[sidebar-script] unknown step {other}"),
    }
}

fn repo_named(this: &WorkspaceList, name: &str, cx: &App) -> Option<Repo> {
    let snapshot = this.state.read(cx).snapshot.clone();
    this.groups(&snapshot)
        .repos
        .into_iter()
        .map(|g| g.repo)
        .find(|r| r.name == name)
}

fn log(this: &WorkspaceList, label: &str, cx: &App) {
    let snapshot = this.state.read(cx).snapshot.clone();
    eprintln!(
        "[sidebar-script] {label}: focused workspace {:?}, pane {:?}",
        snapshot.focused_workspace_id, snapshot.focused_pane_id
    );
    let line = |info: &WorkspaceInfo, indent: &str| {
        let branch = this
            .checkout(&snapshot, info)
            .and_then(|c| c.branch.clone());
        eprintln!(
            "[sidebar-script]   {indent}{} {:?} branch={branch:?} tabs={} panes={} status={}{}",
            info.workspace_id,
            info.label,
            info.tab_count,
            info.pane_count,
            status_label(info.agent_status),
            if this.collapsed.contains(&info.workspace_id) {
                " (folded)"
            } else {
                ""
            },
        );
    };
    let groups = this.groups(&snapshot);
    for group in &groups.repos {
        let folded = this.collapsed_repos.contains(&group.repo.key);
        eprintln!(
            "[sidebar-script]   repo {} ({}){}",
            group.repo.name,
            group.repo.root,
            if folded { " (folded)" } else { "" }
        );
        for &(info, linked) in &group.workspaces {
            line(info, if linked { "    linked " } else { "  main " });
        }
    }
    eprintln!("[sidebar-script]   no repo:");
    for &info in &groups.ungrouped {
        line(info, "  ");
    }
    for a in &snapshot.agents {
        eprintln!(
            "[sidebar-script]   agent {} {} {}",
            a.pane_id,
            agent_name(a),
            status_label(a.agent_status)
        );
    }
}
