//! Debug builds only: drive the workspace list from `RT_SIDEBAR_SCRIPT`, so
//! its flows can be checked live against the dev session without synthetic
//! OS input. Each step calls the same method the matching click calls.
//! Steps are `;`-separated:
//!
//! - `wait <ms>`
//! - `focus <workspace>`: click a workspace row (`workspace.focus`)
//! - `agent <pane>`: click an agent row
//! - `toggle <workspace>`: click a row's chevron
//! - `new-workspace`: header + → New Workspace
//! - `new-worktree`: header + → New Worktree… (opens the prompt)
//! - `worktree <branch>`: submit the New Worktree prompt with `branch`
//! - `rename <workspace>`: the menu's Rename Workspace… (chrome's prompt)
//! - `close <workspace>`: the row's × (chrome confirms if agents run)
//! - `key enter|escape`: `Confirm` / `Cancel` to the focused dialog
//! - `log <label>`: print workspaces, agents and focus to stderr
//!
//! e.g. `RT_SIDEBAR_SCRIPT='wait 2000; focus w2; wait 800; log after'`.

use std::time::Duration;

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::*;

use super::{WorkspaceList, close_workspace, rename_workspace};
use crate::attention::{agent_name, status_label};

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
            let done = this.update_in(cx, |this, window, cx| run_step(this, &step, window, cx));
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
        "new-workspace" => this.new_workspace(cx),
        "new-worktree" => this.new_worktree(window, cx),
        "worktree" => this.create_worktree(arg, window, cx),
        "rename" => rename_workspace(arg, window, cx),
        "close" => close_workspace(arg, window, cx),
        "key" => match arg {
            "enter" => window.dispatch_action(Box::new(Confirm { secondary: false }), cx),
            "escape" => window.dispatch_action(Box::new(Cancel), cx),
            other => eprintln!("[sidebar-script] unknown key {other}"),
        },
        "log" => log(this, arg, cx),
        other => eprintln!("[sidebar-script] unknown step {other}"),
    }
}

fn log(this: &WorkspaceList, label: &str, cx: &App) {
    let snapshot = &this.state.read(cx).snapshot;
    eprintln!(
        "[sidebar-script] {label}: focused workspace {:?}, pane {:?}",
        snapshot.focused_workspace_id, snapshot.focused_pane_id
    );
    for w in &snapshot.workspaces {
        eprintln!(
            "[sidebar-script]   {} {:?} tabs={} panes={} status={} worktree={:?}",
            w.info.workspace_id,
            w.info.label,
            w.info.tab_count,
            w.info.pane_count,
            status_label(w.info.agent_status),
            w.info
                .worktree
                .as_ref()
                .map(|t| (&t.repo_name, t.is_linked_worktree)),
        );
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
