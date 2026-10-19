//! Pushed events from `events.subscribe`.
//!
//! Lines arrive as `{"event": "<kind>", "data": {...}}`. Lifecycle kinds are
//! snake_case (`pane_created`); subscription-scoped kinds are dotted
//! (`pane.agent_status_changed`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ids::PaneId;

/// Lifecycle subscriptions the session cache uses. `pane.agent_status_changed`
/// needs a `pane_id`, so it is added per pane (see [`subscriptions`]).
pub const LIFECYCLE_SUBSCRIPTIONS: &[&str] = &[
    "workspace.created",
    "workspace.updated",
    "workspace.metadata_updated",
    "workspace.renamed",
    "workspace.moved",
    "workspace.reordered",
    "workspace.closed",
    "workspace.focused",
    "worktree.created",
    "worktree.opened",
    "worktree.removed",
    "tab.created",
    "tab.closed",
    "tab.focused",
    "tab.renamed",
    "tab.moved",
    "pane.created",
    "pane.closed",
    "pane.updated",
    "pane.focused",
    "pane.moved",
    "pane.exited",
    "pane.agent_detected",
    "layout.updated",
];

/// The `subscriptions` array: every lifecycle kind plus agent status for each pane.
pub fn subscriptions<'a>(pane_ids: impl IntoIterator<Item = &'a PaneId>) -> Value {
    let lifecycle = LIFECYCLE_SUBSCRIPTIONS
        .iter()
        .map(|kind| json!({ "type": kind }));
    let status = pane_ids
        .into_iter()
        .map(|pane_id| json!({ "type": "pane.agent_status_changed", "pane_id": pane_id }));
    Value::Array(lifecycle.chain(status).collect())
}

/// The kind of one pushed line (`pane_closed`, `pane.agent_status_changed`).
/// Returns `None` for lines that are not events, such as responses.
pub fn kind(line: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        event: String,
        #[expect(dead_code, reason = "only present to tell events from responses")]
        data: Value,
    }
    serde_json::from_str::<Envelope>(line).ok().map(|e| e.event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_should_read_lifecycle_event() {
        let line = r#"{"data":{"pane_id":"w1:p3","type":"pane_closed","workspace_id":"w1"},"event":"pane_closed"}"#;
        assert_eq!(kind(line).as_deref(), Some("pane_closed"));
    }

    #[test]
    fn kind_should_read_dotted_agent_status_event() {
        let line = r#"{"data":{"agent":"claude","agent_status":"blocked","pane_id":"w1:p1","workspace_id":"w1"},"event":"pane.agent_status_changed"}"#;
        assert_eq!(kind(line).as_deref(), Some("pane.agent_status_changed"));
    }

    #[test]
    fn kind_should_ignore_responses() {
        let line = r#"{"id":"1","result":{"type":"subscription_started"}}"#;
        assert_eq!(kind(line), None);
    }

    #[test]
    fn subscriptions_should_add_status_entry_per_pane() {
        let subs = subscriptions(&[PaneId::from("w1:p1"), PaneId::from("w1:p2")]);
        let count = subs.as_array().map_or(0, Vec::len);
        assert_eq!(count, LIFECYCLE_SUBSCRIPTIONS.len() + 2);
    }
}
