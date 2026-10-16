//! Which agents need the user: status changes between snapshots (for toasts)
//! and finished agents nobody has looked at yet (for badges).
//!
//! herdr's `done` already means "finished, not yet seen by a herdr client";
//! `seen_done` adds the looks herdr can't know about (focusing the pane from
//! this app, clicking the agent in the list).

use std::collections::{BTreeSet, HashMap};

use crate::herdr::{Agent, AgentStatus, PaneId, Snapshot};

/// One agent's move to a new status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChange {
    pub pane_id: PaneId,
    pub name: String,
    pub previous: AgentStatus,
    pub status: AgentStatus,
}

#[derive(Debug, Default)]
pub struct Attention {
    statuses: HashMap<PaneId, AgentStatus>,
    seen_done: BTreeSet<PaneId>,
}

/// What to call an agent: its name, else the agent kind herdr detected.
pub fn agent_name(agent: &Agent) -> &str {
    agent
        .name
        .as_deref()
        .or(agent.display_agent.as_deref())
        .or(agent.agent.as_deref())
        .unwrap_or("agent")
}

pub fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

pub fn find_agent<'a>(snapshot: &'a Snapshot, pane_id: &str) -> Option<&'a Agent> {
    snapshot.agents.iter().find(|a| a.pane_id == pane_id)
}

impl Attention {
    /// Takes in a new snapshot. Returns the status changes of agents already
    /// known, so a (re)connect does not replay old statuses as news.
    pub fn update(&mut self, snapshot: &Snapshot) -> Vec<StatusChange> {
        let changes: Vec<StatusChange> = snapshot
            .agents
            .iter()
            .filter_map(|agent| {
                let previous = *self.statuses.get(&agent.pane_id)?;
                (previous != agent.agent_status).then(|| StatusChange {
                    pane_id: agent.pane_id.clone(),
                    name: agent_name(agent).to_owned(),
                    previous,
                    status: agent.agent_status,
                })
            })
            .collect();
        self.statuses = snapshot
            .agents
            .iter()
            .map(|a| (a.pane_id.clone(), a.agent_status))
            .collect();
        // A new `done` is a new result to look at.
        for change in &changes {
            self.seen_done.remove(&change.pane_id);
        }
        self.seen_done.retain(|pane_id| {
            find_agent(snapshot, pane_id).is_some_and(|a| a.agent_status == AgentStatus::Done)
        });
        if let Some(focused) = snapshot.focused_pane_id.as_deref() {
            self.mark_seen(snapshot, focused);
        }
        changes
    }

    /// Records that the user looked at `pane_id`. Returns whether that cleared
    /// an unseen result.
    pub fn mark_seen(&mut self, snapshot: &Snapshot, pane_id: &str) -> bool {
        let done =
            find_agent(snapshot, pane_id).is_some_and(|a| a.agent_status == AgentStatus::Done);
        done && self.seen_done.insert(pane_id.to_owned())
    }

    pub fn is_unseen_done(&self, snapshot: &Snapshot, pane_id: &str) -> bool {
        find_agent(snapshot, pane_id).is_some_and(|a| a.agent_status == AgentStatus::Done)
            && !self.seen_done.contains(pane_id)
    }

    /// Blocked agents plus unseen finished ones: the dock badge count.
    pub fn count(&self, snapshot: &Snapshot) -> usize {
        snapshot
            .agents
            .iter()
            .filter(|a| {
                a.agent_status == AgentStatus::Blocked || self.is_unseen_done(snapshot, &a.pane_id)
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::{Attention, StatusChange};
    use crate::herdr::{Agent, AgentStatus, Snapshot};

    fn agent(pane: &str, status: AgentStatus) -> Agent {
        Agent {
            pane_id: pane.into(),
            terminal_id: String::new(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            focused: false,
            agent_status: status,
            revision: 0,
            name: None,
            agent: Some("claude".into()),
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

    fn snapshot(agents: Vec<Agent>, focused: Option<&str>) -> Snapshot {
        Snapshot {
            agents,
            focused_pane_id: focused.map(str::to_owned),
            ..Snapshot::default()
        }
    }

    #[test]
    fn first_snapshot_reports_no_changes() {
        let mut attention = Attention::default();
        let first = snapshot(vec![agent("p1", AgentStatus::Blocked)], None);
        assert!(attention.update(&first).is_empty());
    }

    #[test]
    fn status_flip_is_reported() {
        let mut attention = Attention::default();
        attention.update(&snapshot(vec![agent("p1", AgentStatus::Working)], None));
        let changes = attention.update(&snapshot(vec![agent("p1", AgentStatus::Done)], None));
        assert_eq!(
            changes,
            [StatusChange {
                pane_id: "p1".into(),
                name: "claude".into(),
                previous: AgentStatus::Working,
                status: AgentStatus::Done,
            }]
        );
    }

    #[test]
    fn done_agent_counts_until_seen() {
        let mut attention = Attention::default();
        let done = snapshot(vec![agent("p1", AgentStatus::Done)], None);
        attention.update(&done);
        attention.mark_seen(&done, "p1");
        assert_eq!(attention.count(&done), 0);
    }

    #[test]
    fn focused_done_agent_is_seen() {
        let mut attention = Attention::default();
        let done = snapshot(vec![agent("p1", AgentStatus::Done)], Some("p1"));
        attention.update(&done);
        assert!(!attention.is_unseen_done(&done, "p1"));
    }

    #[test]
    fn finishing_again_is_unseen_again() {
        let mut attention = Attention::default();
        let done = snapshot(vec![agent("p1", AgentStatus::Done)], None);
        attention.update(&done);
        attention.mark_seen(&done, "p1");
        attention.update(&snapshot(vec![agent("p1", AgentStatus::Working)], None));
        let again = snapshot(vec![agent("p1", AgentStatus::Done)], None);
        attention.update(&again);
        assert!(attention.is_unseen_done(&again, "p1"));
    }
}
