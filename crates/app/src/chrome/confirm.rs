//! Confirmations. herdr's close calls act at once, so the app asks before a
//! close would end a running agent or take a whole worktree group with it.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::*;

use super::Callback;
use super::HerdrCommand as Command;
use crate::attention::{agent_name, find_agent, status_label};
use crate::herdr::{Snapshot, Workspace};

/// The question `Chrome::confirm` asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
}

pub(super) fn open(
    confirmation: Confirmation,
    on_confirm: Callback,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let on_confirm = on_confirm.clone();
        alert
            .confirm()
            .icon(Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning))
            .title(confirmation.title.clone())
            .description(confirmation.message.clone())
            .ok_text(confirmation.confirm_label.clone())
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                on_confirm(window, cx);
                true
            })
    });
}

/// The workspaces that `workspace.close { close_group: true }` would close
/// along with `workspace_id`: every workspace of the same repo.
pub(super) fn group_members<'a>(snapshot: &'a Snapshot, workspace_id: &str) -> Vec<&'a Workspace> {
    let Some(repo_key) = snapshot
        .workspace(workspace_id)
        .and_then(|w| w.info.worktree.as_ref())
        .map(|wt| &wt.repo_key)
    else {
        return Vec::new();
    };
    snapshot
        .workspaces
        .iter()
        .filter(|w| {
            w.info
                .worktree
                .as_ref()
                .is_some_and(|wt| &wt.repo_key == repo_key)
        })
        .collect()
}

fn agents_where(snapshot: &Snapshot, keep: impl Fn(&str, &str) -> bool) -> Vec<&str> {
    snapshot
        .agents
        .iter()
        .filter(|a| keep(&a.tab_id, &a.workspace_id))
        .map(agent_name)
        .collect()
}

fn agent_summary(names: &[&str]) -> String {
    match names {
        [one] => format!("The agent {one} is running there and will be stopped."),
        many => format!(
            "{} agents are running there and will be stopped: {}.",
            many.len(),
            many.join(", ")
        ),
    }
}

/// The confirmation `command` needs, or `None` when it can run straight away.
pub(super) fn needed_for(command: &Command, snapshot: &Snapshot) -> Option<Confirmation> {
    match command {
        Command::ClosePane { pane_id } => {
            let agent = find_agent(snapshot, pane_id)?;
            let name = agent_name(agent);
            Some(Confirmation {
                title: format!("Close {name}?"),
                message: format!(
                    "This pane runs {name} ({}). Closing it stops the agent.",
                    status_label(agent.agent_status)
                ),
                confirm_label: "Close Pane".into(),
            })
        }
        Command::CloseTab { tab_id } => {
            let names = agents_where(snapshot, |tab, _| tab == tab_id);
            (!names.is_empty()).then(|| Confirmation {
                title: "Close tab?".into(),
                message: agent_summary(&names),
                confirm_label: "Close Tab".into(),
            })
        }
        Command::CloseWorkspace {
            workspace_id,
            close_group: false,
        } => {
            let names = agents_where(snapshot, |_, ws| ws == workspace_id);
            let label = snapshot
                .workspace(workspace_id)
                .map_or("workspace", |w| w.info.label.as_str());
            (!names.is_empty()).then(|| Confirmation {
                title: format!("Close {label}?"),
                message: agent_summary(&names),
                confirm_label: "Close Workspace".into(),
            })
        }
        Command::CloseWorkspace {
            workspace_id,
            close_group: true,
        } => {
            let members = group_members(snapshot, workspace_id);
            let labels: Vec<&str> = members.iter().map(|w| w.info.label.as_str()).collect();
            let names = agents_where(snapshot, |_, ws| {
                members.iter().any(|w| w.info.workspace_id == ws)
            });
            let repo = snapshot
                .workspace(workspace_id)
                .and_then(|w| w.info.worktree.as_ref())
                .map_or("this repo", |wt| wt.repo_name.as_str());
            let mut message = format!(
                "This closes {} workspaces of {repo}: {}.",
                labels.len(),
                labels.join(", ")
            );
            if !names.is_empty() {
                message.push(' ');
                message.push_str(&agent_summary(&names));
            }
            Some(Confirmation {
                title: "Close the worktree group?".into(),
                message,
                confirm_label: "Close Group".into(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, group_members, needed_for};
    use crate::herdr::{
        Agent, AgentStatus, Snapshot, Workspace, WorkspaceInfo, WorkspaceWorktreeInfo,
    };

    fn workspace(id: &str, repo: Option<&str>) -> Workspace {
        Workspace {
            info: WorkspaceInfo {
                workspace_id: id.into(),
                number: 1,
                label: format!("ws-{id}"),
                focused: false,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: format!("{id}:t1"),
                agent_status: AgentStatus::Idle,
                worktree: repo.map(|repo| WorkspaceWorktreeInfo {
                    repo_key: repo.into(),
                    repo_name: repo.into(),
                    repo_root: String::new(),
                    checkout_path: String::new(),
                    is_linked_worktree: true,
                }),
                tokens: Default::default(),
            },
            tabs: Vec::new(),
        }
    }

    fn agent(pane: &str, tab: &str, ws: &str) -> Agent {
        Agent {
            pane_id: pane.into(),
            terminal_id: String::new(),
            workspace_id: ws.into(),
            tab_id: tab.into(),
            focused: false,
            agent_status: AgentStatus::Working,
            revision: 0,
            name: Some(format!("agent-{pane}")),
            agent: None,
            display_agent: None,
            cwd: None,
            foreground_cwd: None,
            title: None,
            completion_seq: None,
            state_change_seq: 0,
            interactive_ready: false,
            launch_pending: false,
            state_labels: Default::default(),
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            workspaces: vec![
                workspace("w1", Some("repo")),
                workspace("w2", Some("repo")),
                workspace("w3", None),
            ],
            agents: vec![agent("w1:p1", "w1:t1", "w1")],
            ..Snapshot::default()
        }
    }

    #[test]
    fn closing_an_agent_pane_needs_confirmation() {
        let command = Command::ClosePane {
            pane_id: "w1:p1".into(),
        };
        assert!(needed_for(&command, &snapshot()).is_some());
    }

    #[test]
    fn closing_a_plain_pane_runs_straight_away() {
        let command = Command::ClosePane {
            pane_id: "w1:p2".into(),
        };
        assert_eq!(needed_for(&command, &snapshot()), None);
    }

    #[test]
    fn closing_a_tab_without_agents_runs_straight_away() {
        let command = Command::CloseTab {
            tab_id: "w1:t2".into(),
        };
        assert_eq!(needed_for(&command, &snapshot()), None);
    }

    #[test]
    fn closing_a_worktree_group_always_needs_confirmation() {
        let command = Command::CloseWorkspace {
            workspace_id: "w2".into(),
            close_group: true,
        };
        let confirmation = needed_for(&command, &snapshot()).map(|c| c.message);
        assert_eq!(
            confirmation.as_deref(),
            Some(
                "This closes 2 workspaces of repo: ws-w1, ws-w2. \
                 The agent agent-w1:p1 is running there and will be stopped."
            )
        );
    }

    #[test]
    fn group_members_is_empty_outside_a_repo() {
        assert!(group_members(&snapshot(), "w3").is_empty());
    }
}
