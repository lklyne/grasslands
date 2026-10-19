//! Confirmations. herdr's close calls act at once, so the app asks before a
//! close would end a running agent, and, as herdr's `ui.confirm_close` says
//! (on by default), before closing any workspace or worktree group.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::*;

use super::Callback;
use super::HerdrCommand as Command;
use crate::attention::find_agent;
use crate::herdr::{Snapshot, TabId, Workspace, WorkspaceId, WorkspaceInfo};
use crate::worktree::Placement;
use crate::{label, status};

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
    // OK plays the confirmed action's own cue.
    crate::sounds::play(cues::Cue::Open, cues::PlayOptions::default());
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
            .on_cancel(|_, _, _| {
                crate::sounds::play(cues::Cue::Close, crate::sounds::subtle());
                true
            })
    });
}

/// The workspaces a group close takes with `workspace_id`, as `place` puts
/// them in repos: herdr's rule for `workspace.close { close_group: true }`
/// over the spaces list's placements. Only a main checkout heads a group:
/// it plus every linked worktree of the same repo. On a linked worktree (or
/// outside a repo) this is empty.
pub(super) fn group_members<'a>(
    snapshot: &'a Snapshot,
    workspace_id: &WorkspaceId,
    place: impl Fn(&WorkspaceInfo) -> Option<Placement>,
) -> Vec<&'a Workspace> {
    let Some(source) = snapshot
        .workspace(workspace_id)
        .and_then(|w| place(&w.info))
        .filter(|p| !p.linked)
    else {
        return Vec::new();
    };
    snapshot
        .workspaces
        .iter()
        .filter(|w| {
            &w.info.workspace_id == workspace_id
                || place(&w.info).is_some_and(|p| p.linked && p.repo.key == source.repo.key)
        })
        .collect()
}

/// Whether herdr's `close_group` closes exactly `listed`, so herdr can do
/// the close; otherwise each listed workspace is closed on its own.
pub(super) fn herdr_closes_exactly(listed: &[WorkspaceId], herdr_members: &[&Workspace]) -> bool {
    listed.len() == herdr_members.len()
        && herdr_members
            .iter()
            .all(|w| listed.contains(&w.info.workspace_id))
}

fn agents_where(snapshot: &Snapshot, keep: impl Fn(&TabId, &WorkspaceId) -> bool) -> Vec<&str> {
    snapshot
        .agents
        .iter()
        .filter(|a| keep(&a.tab_id, &a.workspace_id))
        .map(|a| label::agent(snapshot, a))
        .collect()
}

/// The agents running in any of `workspace_ids`.
pub(super) fn agents_in<'a>(
    snapshot: &'a Snapshot,
    workspace_ids: &[&WorkspaceId],
) -> Vec<&'a str> {
    agents_where(snapshot, |_, ws| workspace_ids.contains(&ws))
}

pub(super) fn agent_summary(names: &[&str]) -> String {
    match names {
        [one] => format!("The agent {one} is running there and will be stopped."),
        many => format!(
            "{} agents are running there and will be stopped: {}.",
            many.len(),
            many.join(", ")
        ),
    }
}

fn group_confirmation(
    snapshot: &Snapshot,
    repo_name: &str,
    members: &[WorkspaceId],
) -> Confirmation {
    let labels: Vec<&str> = members
        .iter()
        .map(|id| {
            snapshot
                .workspace(id)
                .map_or(id.as_str(), |w| w.info.label.as_str())
        })
        .collect();
    let ids: Vec<&WorkspaceId> = members.iter().collect();
    let names = agents_in(snapshot, &ids);
    let mut message = format!(
        "This closes {} workspaces of {repo_name}: {}.",
        labels.len(),
        labels.join(", ")
    );
    if !names.is_empty() {
        message.push(' ');
        message.push_str(&agent_summary(&names));
    }
    Confirmation {
        title: "Close the worktree group?".into(),
        message,
        confirm_label: "Close Group".into(),
    }
}

/// The confirmation `command` needs, or `None` when it can run straight away.
/// `confirm_close` is herdr's `ui.confirm_close`: every workspace close asks
/// when it's on, none does when it's off (as in herdr). Closing a pane or a
/// tab asks only when it would stop an agent.
pub(super) fn needed_for(
    command: &Command,
    snapshot: &Snapshot,
    confirm_close: bool,
) -> Option<Confirmation> {
    match command {
        Command::ClosePane { pane_id, .. } => {
            let agent = find_agent(snapshot, pane_id)?;
            let name = label::agent(snapshot, agent);
            Some(Confirmation {
                title: format!("Close {name}?"),
                message: format!(
                    "This pane runs {name} ({}). Closing it stops the agent.",
                    status::text(agent.agent_status, &agent.state_labels)
                ),
                confirm_label: "Close Pane".into(),
            })
        }
        Command::CloseTab { tab_id, .. } => {
            let names = agents_where(snapshot, |tab, _| tab == tab_id);
            (!names.is_empty()).then(|| Confirmation {
                title: "Close tab?".into(),
                message: agent_summary(&names),
                confirm_label: "Close Tab".into(),
            })
        }
        Command::CloseGroup {
            repo_name, members, ..
        } => confirm_close.then(|| group_confirmation(snapshot, repo_name, members)),
        Command::CloseWorkspace { workspace_id, .. } => {
            if !confirm_close {
                return None;
            }
            let names = agents_in(snapshot, &[workspace_id]);
            let label = snapshot
                .workspace(workspace_id)
                .map_or("workspace", |w| w.info.label.as_str());
            let message = if names.is_empty() {
                "This closes the workspace and every terminal in it.".to_owned()
            } else {
                agent_summary(&names)
            };
            Some(Confirmation {
                title: format!("Close {label}?"),
                message,
                confirm_label: "Close Workspace".into(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, group_members, herdr_closes_exactly, needed_for};
    use crate::chrome::worktrees::herdr_placement;
    use crate::herdr::WorkspaceId;
    use crate::herdr::{
        Agent, AgentStatus, Snapshot, Workspace, WorkspaceInfo, WorkspaceWorktreeInfo,
    };
    use crate::worktree::{Placement, Repo};

    fn workspace(id: &str, repo: Option<&str>, linked: bool) -> Workspace {
        Workspace {
            info: WorkspaceInfo {
                workspace_id: id.into(),
                number: 1,
                label: format!("ws-{id}"),
                focused: false,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: format!("{id}:t1").into(),
                agent_status: AgentStatus::Idle,
                worktree: repo.map(|repo| WorkspaceWorktreeInfo {
                    repo_key: repo.into(),
                    repo_name: repo.into(),
                    repo_root: String::new(),
                    checkout_path: String::new(),
                    is_linked_worktree: linked,
                }),
                tokens: Default::default(),
            },
            tabs: Vec::new(),
        }
    }

    fn agent(pane: &str, tab: &str, ws: &str) -> Agent {
        Agent {
            pane_id: pane.into(),
            terminal_id: "".into(),
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
                workspace("w1", Some("repo"), false),
                workspace("w2", Some("repo"), true),
                workspace("w3", None, false),
            ],
            agents: vec![agent("w1:p1", "w1:t1", "w1")],
            ..Snapshot::default()
        }
    }

    fn group_close(members: &[&str]) -> Command {
        Command::CloseGroup {
            machine: crate::state::MachineId::Local,
            workspace_id: "w1".into(),
            repo_name: "repo".into(),
            members: members.iter().map(|m| (*m).into()).collect(),
        }
    }

    /// herdr's placement, plus git's for `w3`: a linked worktree of `repo`
    /// that herdr knows nothing about.
    fn with_inferred(info: &WorkspaceInfo) -> Option<Placement> {
        herdr_placement(info).or_else(|| {
            (info.workspace_id == "w3").then(|| Placement {
                repo: Repo {
                    key: "repo".into(),
                    name: "repo".into(),
                    root: String::new(),
                },
                linked: true,
            })
        })
    }

    fn ids(members: &[&Workspace]) -> Vec<WorkspaceId> {
        members
            .iter()
            .map(|w| w.info.workspace_id.clone())
            .collect()
    }

    #[test]
    fn closing_an_agent_pane_needs_confirmation() {
        let command = Command::ClosePane {
            machine: crate::state::MachineId::Local,
            pane_id: "w1:p1".into(),
        };
        assert!(needed_for(&command, &snapshot(), true).is_some());
    }

    #[test]
    fn closing_a_plain_pane_runs_straight_away() {
        let command = Command::ClosePane {
            machine: crate::state::MachineId::Local,
            pane_id: "w1:p2".into(),
        };
        assert_eq!(needed_for(&command, &snapshot(), true), None);
    }

    #[test]
    fn closing_a_tab_without_agents_runs_straight_away() {
        let command = Command::CloseTab {
            machine: crate::state::MachineId::Local,
            tab_id: "w1:t2".into(),
        };
        assert_eq!(needed_for(&command, &snapshot(), true), None);
    }

    #[test]
    fn closing_a_worktree_group_always_needs_confirmation() {
        let confirmation =
            needed_for(&group_close(&["w1", "w2"]), &snapshot(), true).map(|c| c.message);
        assert_eq!(
            confirmation.as_deref(),
            Some(
                "This closes 2 workspaces of repo: ws-w1, ws-w2. \
                 The agent agent-w1:p1 is running there and will be stopped."
            )
        );
    }

    #[test]
    fn inferred_linked_worktrees_join_the_group() {
        assert_eq!(
            ids(&group_members(&snapshot(), &"w1".into(), with_inferred)),
            ["w1", "w2", "w3"]
        );
    }

    #[test]
    fn group_confirmation_lists_every_member_it_closes() {
        let confirmation = needed_for(&group_close(&["w1", "w2", "w3"]), &snapshot(), true);
        assert_eq!(
            confirmation.map(|c| c.message).as_deref(),
            Some(
                "This closes 3 workspaces of repo: ws-w1, ws-w2, ws-w3. \
                 The agent agent-w1:p1 is running there and will be stopped."
            )
        );
    }

    #[test]
    fn herdr_closes_the_group_when_it_sees_the_same_members() {
        let snapshot = snapshot();
        let herdr = group_members(&snapshot, &"w1".into(), herdr_placement);
        assert!(herdr_closes_exactly(&["w1".into(), "w2".into()], &herdr));
    }

    #[test]
    fn an_inferred_member_closes_each_workspace_on_its_own() {
        let snapshot = snapshot();
        let herdr = group_members(&snapshot, &"w1".into(), herdr_placement);
        let listed = ids(&group_members(&snapshot, &"w1".into(), with_inferred));
        assert!(!herdr_closes_exactly(&listed, &herdr));
    }

    #[test]
    fn herdr_has_no_group_where_only_git_found_one() {
        let mut snapshot = snapshot();
        snapshot.workspaces[0].info.worktree = None;
        snapshot.workspaces[1].info.worktree = None;
        assert!(group_members(&snapshot, &"w1".into(), herdr_placement).is_empty());
    }

    #[test]
    fn linked_worktree_has_no_group() {
        assert!(group_members(&snapshot(), &"w2".into(), herdr_placement).is_empty());
    }

    #[test]
    fn closing_a_linked_worktree_without_agents_runs_straight_away() {
        let command = Command::CloseWorkspace {
            machine: crate::state::MachineId::Local,
            workspace_id: "w2".into(),
        };
        let title = needed_for(&command, &snapshot(), true).map(|c| c.title);
        assert_eq!(title.as_deref(), Some("Close ws-w2?"));
    }

    #[test]
    fn closing_an_empty_workspace_asks_when_confirm_close_is_on() {
        let command = Command::CloseWorkspace {
            machine: crate::state::MachineId::Local,
            workspace_id: "w3".into(),
        };
        assert!(needed_for(&command, &snapshot(), true).is_some());
    }

    #[test]
    fn confirm_close_off_closes_workspaces_straight_away() {
        let command = Command::CloseWorkspace {
            machine: crate::state::MachineId::Local,
            workspace_id: "w1".into(),
        };
        assert_eq!(needed_for(&command, &snapshot(), false), None);
        assert_eq!(
            needed_for(&group_close(&["w1", "w2"]), &snapshot(), false),
            None
        );
    }

    #[test]
    fn confirm_close_off_still_asks_before_stopping_an_agent_pane() {
        let command = Command::ClosePane {
            machine: crate::state::MachineId::Local,
            pane_id: "w1:p1".into(),
        };
        assert!(needed_for(&command, &snapshot(), false).is_some());
    }

    #[test]
    fn group_members_is_empty_outside_a_repo() {
        assert!(group_members(&snapshot(), &"w3".into(), herdr_placement).is_empty());
    }
}
