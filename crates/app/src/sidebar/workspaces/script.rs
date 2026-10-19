//! Debug builds only: the spaces and agents panels' steps for the live step
//! list (`crate::script::steps`, `RT_STEPS` or its older name
//! `RT_SIDEBAR_SCRIPT`). Each calls the same method the matching click
//! calls:
//!
//! - `focus <workspace>`: click a workspace row (`workspace.focus`)
//! - `agent <pane>`: click an agent row
//! - `fold <repo name>`: click a group's ▾/▸
//! - `sort`: click the agents panel's "Grouped" / "Priority"
//! - `new-workspace`: the spaces panel's +
//! - `new-worktree`: the spaces panel's menu → New Worktree… (opens the prompt)
//! - `worktree <branch> [base=<ref>] [label=<text>]`: submit the New
//!   Worktree prompt with `branch` and, if given, its base ref and label
//! - `new-worktree-in <repo name>`: a repo row's + (opens the prompt)
//! - `repo-worktree <repo name> <branch> [base=…] [label=…]`: submit that
//!   prompt with `branch`
//! - `rename <workspace>`: the menu's Rename Workspace… (chrome's prompt)
//! - `close <workspace>`: the row's × (chrome confirms if agents run)
//! - `move <workspace> <target>`: drop a row on `target`'s row (no pointer)
//!
//! `drag` (with the pointer) and `log` are the step list's; they read
//! [`drawn_rows`] and [`log`] here.
//!
//! e.g. `RT_SIDEBAR_SCRIPT='wait 2000; focus w2; wait 800; log after'`.

use std::collections::HashMap;

use gpui_kit::*;

use super::git::Repo;
use super::groups::{Kind, Slot};
use super::{AgentSort, WorkspaceList, agents, close_workspace, rename_workspace};
use crate::prompt::NewWorktree;
use crate::state::MachineId;
use crate::{label, status};

const VERBS: &[&str] = &[
    "focus",
    "agent",
    "fold",
    "sort",
    "new-workspace",
    "new-worktree",
    "worktree",
    "new-worktree-in",
    "repo-worktree",
    "rename",
    "close",
    "move",
];

/// Runs `step` on the sidebar's panels; `None` when it isn't one of their
/// steps.
pub(crate) fn run_step(
    step: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<Result<(), String>> {
    let (verb, arg) = step.split_once(' ').unwrap_or((step, ""));
    if !VERBS.contains(&verb) {
        return None;
    }
    let arg = arg.trim();
    let agents = crate::sidebar::agents(cx);
    let result = match verb {
        "agent" | "sort" => {
            let Some(agents) = agents else {
                return Some(Err("no agents panel".into()));
            };
            agents.update(cx, |this, cx| {
                if verb == "agent" {
                    this.activate(&arg.into(), window, cx);
                } else {
                    this.toggle_sort(cx);
                }
            });
            Ok(())
        }
        _ => {
            let Some(spaces) = crate::sidebar::spaces(cx) else {
                return Some(Err("no spaces panel".into()));
            };
            spaces.update(cx, |this, cx| run(this, verb, arg, window, cx))
        }
    };
    Some(result)
}

fn run(
    this: &mut WorkspaceList,
    verb: &str,
    arg: &str,
    window: &mut Window,
    cx: &mut Context<WorkspaceList>,
) -> Result<(), String> {
    let no_repo = |name: &str| format!("no repo {name}");
    match verb {
        "focus" => this.activate_workspace(&arg.into(), cx),
        "fold" => {
            let repo = repo_named(this, arg, cx).ok_or_else(|| no_repo(arg))?;
            this.toggle_group(&repo.key, cx);
        }
        "new-workspace" => this.new_workspace(window, cx),
        "new-worktree" => this.new_worktree(None, window, cx),
        "worktree" => this.create_worktree(new_worktree(arg), None, window, cx),
        "new-worktree-in" => {
            let repo = repo_named(this, arg, cx).ok_or_else(|| no_repo(arg))?;
            this.new_worktree(Some(repo.root), window, cx);
        }
        "repo-worktree" => {
            let (name, fields) = arg.split_once(' ').unwrap_or((arg, ""));
            let repo = repo_named(this, name, cx).ok_or_else(|| no_repo(name))?;
            this.create_worktree(new_worktree(fields), Some(&repo.root), window, cx);
        }
        "rename" => rename_workspace(&selected(this, cx), &arg.into(), window, cx),
        "close" => close_workspace(&selected(this, cx), &arg.into(), window, cx),
        "move" => {
            let (dragged, target) = arg.split_once(' ').unwrap_or((arg, ""));
            let (Some(dragged), Some(target)) =
                (slot(this, dragged, cx), slot(this, target.trim(), cx))
            else {
                return Err(format!("no row for {arg}"));
            };
            this.reorder(&selected(this, cx), &dragged, &target, cx);
        }
        other => return Err(format!("unknown step {other:?}")),
    }
    Ok(())
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

/// The machine the steps work on: the selected one.
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

/// Where each workspace's row was drawn last frame, by its text key: the
/// bare id on Local, `<machine>/<id>` elsewhere.
pub(crate) fn drawn_rows(cx: &App) -> HashMap<String, Bounds<Pixels>> {
    crate::sidebar::spaces(cx)
        .map(|spaces| {
            spaces
                .read(cx)
                .drawn_rows
                .borrow()
                .iter()
                .map(|(on, bounds)| (on.to_string(), *bounds))
                .collect()
        })
        .unwrap_or_default()
}

/// The `log` step's sidebar half: the rows (as herdr would draw them),
/// agents and focus.
pub(crate) fn log(tag: &str, label: &str, cx: &App) {
    let Some(spaces) = crate::sidebar::spaces(cx) else {
        eprintln!("[{tag}] {label}: no spaces panel");
        return;
    };
    let sort = crate::sidebar::agents(cx).map(|agents| agents.read(cx).sort(cx));
    let this = spaces.read(cx);
    let snapshot = this.state.read(cx).snapshot().clone();
    eprintln!(
        "[{tag}] {label}: focused workspace {:?}, pane {:?}, agents {sort:?}",
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
            "[{tag}]   {lead}{} {} [{}] status={}",
            info.label,
            info.workspace_id,
            entry.repo.as_ref().map_or("-", |r| r.name.as_str()),
            status::word(info.agent_status),
        );
        if entry.kind != Kind::Child
            && let Some(branch) = checkout.and_then(|c| c.branch.as_deref())
        {
            eprintln!(
                "[{tag}]      {branch} ahead/behind={:?}",
                checkout.and_then(|c| c.ahead_behind)
            );
        }
    }
    for a in agents::ordered(&snapshot, sort.unwrap_or(AgentSort::Grouped)) {
        eprintln!(
            "[{tag}]   agent {} {} {} in {}",
            a.pane_id,
            label::agent(&snapshot, a),
            status::word(a.agent_status),
            a.workspace_id,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::new_worktree;
    use crate::prompt::NewWorktree;

    #[test]
    fn worktree_step_reads_base_and_label() {
        assert_eq!(
            new_worktree("feat/x base=origin/main label=Feature"),
            NewWorktree::from_fields("feat/x", "origin/main", "Feature")
        );
        assert_eq!(
            new_worktree("feat/x"),
            NewWorktree::from_fields("feat/x", "", "")
        );
    }
}
