//! Agent status changes between snapshots, for toasts and sounds, and the
//! dock badge count. herdr owns whether a result has been seen: its `done`
//! means "finished, not yet seen", and an API `pane.focus`, `agent.focus` or
//! `workspace.focus` (every way the app shows a pane) turns it into `idle`.
//! A focused pane that finishes goes straight to `idle`.

use std::collections::HashMap;

use crate::herdr::{Agent, AgentStatus, PaneId, Snapshot};
use crate::{label, status};

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
}

pub fn find_agent<'a>(snapshot: &'a Snapshot, pane_id: &PaneId) -> Option<&'a Agent> {
    snapshot.agents.iter().find(|a| &a.pane_id == pane_id)
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
                    name: label::agent(snapshot, agent).to_owned(),
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
        changes
    }
}

/// Blocked agents plus finished ones nobody has looked at: the dock badge
/// count.
pub fn count(snapshot: &Snapshot) -> usize {
    snapshot
        .agents
        .iter()
        .filter(|a| status::wants_user(a.agent_status))
        .count()
}

#[cfg(test)]
mod tests {
    use super::{Attention, StatusChange, count};
    use crate::herdr::{Agent, AgentStatus, Snapshot};

    fn agent(pane: &str, status: AgentStatus) -> Agent {
        serde_json::from_value(serde_json::json!({
            "pane_id": pane, "terminal_id": "", "workspace_id": "w1",
            "tab_id": "w1:t1", "focused": false, "agent_status": status,
            "revision": 0, "agent": "claude",
        }))
        .unwrap()
    }

    fn snapshot(agents: Vec<Agent>) -> Snapshot {
        Snapshot {
            agents,
            ..Snapshot::default()
        }
    }

    #[test]
    fn first_snapshot_reports_no_changes() {
        let mut attention = Attention::default();
        let first = snapshot(vec![agent("p1", AgentStatus::Blocked)]);
        assert!(attention.update(&first).is_empty());
    }

    #[test]
    fn status_flip_is_reported() {
        let mut attention = Attention::default();
        attention.update(&snapshot(vec![agent("p1", AgentStatus::Working)]));
        let changes = attention.update(&snapshot(vec![agent("p1", AgentStatus::Done)]));
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
    fn unchanged_status_is_not_reported() {
        let mut attention = Attention::default();
        attention.update(&snapshot(vec![agent("p1", AgentStatus::Done)]));
        let changes = attention.update(&snapshot(vec![agent("p1", AgentStatus::Done)]));
        assert!(changes.is_empty());
    }

    #[test]
    fn new_agent_is_not_news() {
        let mut attention = Attention::default();
        attention.update(&snapshot(vec![]));
        let changes = attention.update(&snapshot(vec![agent("p2", AgentStatus::Blocked)]));
        assert!(changes.is_empty());
    }

    #[test]
    fn seen_result_is_reported_as_done_to_idle() {
        let mut attention = Attention::default();
        attention.update(&snapshot(vec![agent("p1", AgentStatus::Done)]));
        let changes = attention.update(&snapshot(vec![agent("p1", AgentStatus::Idle)]));
        assert_eq!(changes[0].status, AgentStatus::Idle);
    }

    #[test]
    fn count_is_blocked_plus_done() {
        let all = snapshot(vec![
            agent("p1", AgentStatus::Blocked),
            agent("p2", AgentStatus::Done),
            agent("p3", AgentStatus::Idle),
            agent("p4", AgentStatus::Working),
        ]);
        assert_eq!(count(&all), 2);
    }
}
