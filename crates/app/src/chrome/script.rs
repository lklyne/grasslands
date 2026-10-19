//! Debug builds only: the chrome's steps for the live step list
//! (`crate::script::steps`, `RT_STEPS` or its older name
//! `RT_CHROME_SCRIPT`):
//!
//! - `menu pane|tab|workspace <id>`: open that context menu
//! - `run close-pane|close-tab|close-workspace|close-group <id>`: `Chrome::run`
//!   (`close-group` closes the group the spaces list shows, as its menu does)
//! - `open-worktree <workspace>`: the workspace menu's Open Worktree… (the
//!   picker; `query` filters it, `key enter` picks)
//! - `remove-worktree <workspace>`: the menu's Remove Worktree… (asks first;
//!   `key enter` confirms)
//! - `palette`, `query <text>`: open the palette, type into it
//! - `type <text>`: replace the rename dialog's text
//!
//! e.g. `RT_CHROME_SCRIPT='menu pane w1:p1; wait 1500; key escape; log after'`.

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use super::{Chrome, HerdrCommand, MenuCommand, MenuTarget, WorktreeCommand};
use crate::overlay;
use crate::state::MachineId;
use crate::{label, status};

const VERBS: &[&str] = &[
    "menu",
    "run",
    "open-worktree",
    "remove-worktree",
    "palette",
    "query",
    "type",
];

/// Runs `step` on the main window's chrome; `None` when it isn't one of the
/// chrome's steps.
pub(crate) fn run_step(
    step: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<Result<(), String>> {
    let verb = step.split(' ').next().unwrap_or_default();
    if !VERBS.contains(&verb) {
        return None;
    }
    let Some(chrome) = super::get(window, cx) else {
        return Some(Err("no chrome in this window".into()));
    };
    Some(chrome.update(cx, |chrome, cx| run(chrome, step, window, cx)))
}

fn run(
    chrome: &mut Chrome,
    step: &str,
    window: &mut Window,
    cx: &mut Context<Chrome>,
) -> Result<(), String> {
    let mut words = step.splitn(3, ' ');
    let verb = words.next().unwrap_or_default();
    let first = words.next().unwrap_or_default();
    let rest = words.next().unwrap_or_default();
    match verb {
        "menu" => {
            let target = menu_target(first, rest)?;
            chrome.context_menu(target, point(px(320.), px(200.)), Vec::new(), window, cx);
        }
        "run" => {
            let command = match first {
                "close-group" => chrome
                    .group_close(&MachineId::Local, &rest.into(), cx)
                    .ok_or_else(|| format!("{rest} heads no worktree group"))?,
                other => herdr_command(other, rest)?,
            };
            chrome.run(command, window, cx);
        }
        "open-worktree" | "remove-worktree" => {
            let command = worktree_command(verb, first);
            chrome.run_menu_command(MenuCommand::Worktree(command), window, cx);
        }
        "palette" => chrome.open_command_palette(window, cx),
        "query" => {
            let query = step.strip_prefix("query ").unwrap_or_default().to_owned();
            let palette = chrome
                .palette
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .ok_or("no palette open")?;
            palette.update(cx, |palette, cx| palette.set_query(query, window, cx));
        }
        "type" => {
            let text = step.strip_prefix("type ").unwrap_or_default().to_owned();
            let input = chrome
                .rename_input
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .ok_or("no rename dialog open")?;
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        other => return Err(format!("unknown step {other:?}")),
    }
    Ok(())
}

/// `menu <kind> <id>`'s target, on Local.
fn menu_target(kind: &str, id: &str) -> Result<MenuTarget, String> {
    let machine = MachineId::Local;
    Ok(match kind {
        "pane" => MenuTarget::Pane {
            machine,
            pane_id: id.into(),
        },
        "tab" => MenuTarget::Tab {
            machine,
            tab_id: id.into(),
        },
        "workspace" => MenuTarget::Workspace {
            machine,
            workspace_id: id.into(),
        },
        other => return Err(format!("unknown menu target {other:?}")),
    })
}

/// `run <command> <id>`'s command, on Local (`close-group` aside, which
/// needs the chrome).
fn herdr_command(name: &str, id: &str) -> Result<HerdrCommand, String> {
    let machine = MachineId::Local;
    Ok(match name {
        "close-pane" => HerdrCommand::ClosePane {
            machine,
            pane_id: id.into(),
        },
        "close-tab" => HerdrCommand::CloseTab {
            machine,
            tab_id: id.into(),
        },
        "close-workspace" => HerdrCommand::CloseWorkspace {
            machine,
            workspace_id: id.into(),
        },
        other => return Err(format!("unknown command {other:?}")),
    })
}

fn worktree_command(verb: &str, workspace: &str) -> WorktreeCommand {
    let (machine, workspace_id) = (MachineId::Local, workspace.into());
    if verb == "open-worktree" {
        WorktreeCommand::Open {
            machine,
            workspace_id,
        }
    } else {
        WorktreeCommand::Remove {
            machine,
            workspace_id,
        }
    }
}

/// The `log` step's chrome half: what is open, and herdr's workspaces, tabs,
/// panes and agents.
pub(crate) fn log(tag: &str, label: &str, window: &mut Window, cx: &mut App) {
    let Some(chrome) = super::get(window, cx) else {
        eprintln!("[{tag}] log {label}: no chrome");
        return;
    };
    let (host, menu_open, state) = {
        let chrome = chrome.read(cx);
        let host = chrome.floating_window(window, cx);
        (host, chrome.open_menu.is_some(), chrome.state.clone())
    };
    let in_overlay = host != window.window_handle();
    let (dialog, toasts) = if in_overlay {
        cx.update_window(host, |_, window, cx| {
            (window.has_active_dialog(cx), window.notifications(cx).len())
        })
        .unwrap_or_default()
    } else {
        (window.has_active_dialog(cx), window.notifications(cx).len())
    };
    let capturing = overlay::get(window, cx).is_some_and(|o| o.read(cx).is_capturing());
    let snapshot = state.read(cx).snapshot(cx).clone();
    let panes: Vec<&str> = snapshot.panes.keys().map(|id| id.as_str()).collect();
    let agents: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| {
            format!(
                "{}={}:{}",
                a.pane_id,
                label::agent(&snapshot, a),
                status::word(a.agent_status)
            )
        })
        .collect();
    let tabs: Vec<String> = snapshot
        .workspaces
        .iter()
        .flat_map(|w| &w.tabs)
        .map(|t| format!("{}={}", t.info.tab_id, t.info.label))
        .collect();
    let workspaces: Vec<String> = snapshot
        .workspaces
        .iter()
        .map(|w| format!("{}={}", w.info.workspace_id, w.info.label))
        .collect();
    eprintln!(
        "[{tag}] log {label}: in_overlay={in_overlay} capturing={capturing} \
         menu_open={} dialog={} toasts={} focused={:?} \
         workspaces={workspaces:?} tabs={tabs:?} panes={panes:?} agents={agents:?}",
        menu_open, dialog, toasts, snapshot.focused_pane_id,
    );
}

#[cfg(test)]
mod tests {
    use super::{herdr_command, menu_target};
    use crate::chrome::{HerdrCommand, MenuTarget};

    #[test]
    fn menu_steps_name_a_target_on_local() {
        assert!(matches!(
            menu_target("tab", "w1:t2"),
            Ok(MenuTarget::Tab { tab_id, .. }) if tab_id == "w1:t2"
        ));
        assert!(menu_target("window", "w1").is_err());
    }

    #[test]
    fn run_steps_name_a_herdr_command() {
        assert!(matches!(
            herdr_command("close-workspace", "w2"),
            Ok(HerdrCommand::CloseWorkspace { workspace_id, .. }) if workspace_id == "w2"
        ));
        assert!(herdr_command("explode", "w2").is_err());
    }
}
