//! What to call agents and panes, in herdr's precedence: the pane's manual
//! name (`pane rename`), the agent's name (`agent rename`), the agent label
//! a hook reported (`pane report-metadata --display-agent`), the agent kind
//! herdr detected, then the terminal's title.

use std::borrow::Cow;
use std::path::Path;

use crate::herdr::{Agent, PaneInfo, Snapshot};

/// The first of `names` with something in it.
fn first<'a>(names: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    names
        .into_iter()
        .flatten()
        .find(|name| !name.trim().is_empty())
}

/// What to call `agent` (in the agent list, toasts, confirmations).
pub fn agent<'a>(snapshot: &'a Snapshot, agent: &'a Agent) -> &'a str {
    let pane = snapshot.pane(&agent.pane_id);
    first([
        pane.and_then(|p| p.label.as_deref()),
        agent.name.as_deref(),
        agent.display_agent.as_deref(),
        agent.agent.as_deref(),
        pane.and_then(|p| p.terminal_title_stripped.as_deref()),
        agent.title.as_deref(),
    ])
    .unwrap_or("agent")
}

/// What to call `pane` when speaking of it ("Close …?"): its agent's name
/// when it runs one, else its header.
pub fn pane<'a>(snapshot: &'a Snapshot, pane: &'a PaneInfo) -> Cow<'a, str> {
    match snapshot.agents.iter().find(|a| a.pane_id == pane.pane_id) {
        Some(found) => Cow::Borrowed(agent(snapshot, found)),
        None => pane_header(pane),
    }
}

/// A pane's header: its manual name, else its terminal's title, as herdr's
/// pane borders show it (agent labels there are opt-in,
/// `ui.show_agent_labels_on_pane_borders`, off by default).
pub fn pane_header(pane: &PaneInfo) -> Cow<'_, str> {
    if let Some(name) = first([
        pane.label.as_deref(),
        pane.terminal_title_stripped.as_deref(),
        pane.title.as_deref(),
    ]) {
        return Cow::Borrowed(name);
    }
    // herdr's pane id means nothing to a person; the folder does.
    pane.working_dir()
        .and_then(|dir| Path::new(dir).file_name())
        .map_or(Cow::Borrowed("Terminal"), |name| {
            Cow::Owned(name.to_string_lossy().into_owned())
        })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{agent, pane, pane_header};
    use crate::herdr::{Agent, PaneInfo, Snapshot};

    fn pane_info(extra: Value) -> PaneInfo {
        let mut value = json!({
            "pane_id": "p1", "terminal_id": "t1", "workspace_id": "w1",
            "tab_id": "w1:t1", "focused": false, "agent_status": "idle", "revision": 0,
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn agent_info(extra: Value) -> Agent {
        let mut value = json!({
            "pane_id": "p1", "terminal_id": "t1", "workspace_id": "w1",
            "tab_id": "w1:t1", "focused": false, "agent_status": "idle", "revision": 0,
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn snapshot(pane: PaneInfo, agent: Option<Agent>) -> Snapshot {
        Snapshot {
            panes: [(pane.pane_id.clone(), pane)].into(),
            agents: agent.into_iter().collect(),
            ..Snapshot::default()
        }
    }

    fn agent_label(pane: Value, agent: Value) -> String {
        let a = agent_info(agent);
        let s = snapshot(pane_info(pane), Some(a.clone()));
        super::agent(&s, &a).to_owned()
    }

    #[test]
    fn manual_pane_name_beats_agent_name() {
        let got = agent_label(json!({ "label": "reviewer" }), json!({ "name": "bob" }));
        assert_eq!(got, "reviewer");
    }

    #[test]
    fn agent_name_beats_display_agent() {
        let got = agent_label(
            json!({}),
            json!({ "name": "bob", "display_agent": "Claude", "agent": "claude" }),
        );
        assert_eq!(got, "bob");
    }

    #[test]
    fn display_agent_beats_agent_kind() {
        let got = agent_label(
            json!({}),
            json!({ "display_agent": "Claude", "agent": "claude" }),
        );
        assert_eq!(got, "Claude");
    }

    #[test]
    fn agent_falls_back_to_terminal_title() {
        let got = agent_label(
            json!({ "terminal_title_stripped": "Claude Code" }),
            json!({}),
        );
        assert_eq!(got, "Claude Code");
    }

    #[test]
    fn blank_names_are_skipped() {
        let got = agent_label(json!({ "label": " " }), json!({ "agent": "codex" }));
        assert_eq!(got, "codex");
    }

    #[test]
    fn agent_with_nothing_is_agent() {
        assert_eq!(agent_label(json!({}), json!({})), "agent");
    }

    #[test]
    fn header_prefers_terminal_title_over_agent_kind() {
        let p = pane_info(json!({ "agent": "claude", "terminal_title_stripped": "Claude Code" }));
        assert_eq!(pane_header(&p), "Claude Code");
    }

    #[test]
    fn header_falls_back_to_folder() {
        let p = pane_info(json!({ "cwd": "/Users/me/grasslands" }));
        assert_eq!(pane_header(&p), "grasslands");
    }

    #[test]
    fn pane_with_agent_is_called_by_its_agent() {
        let s = snapshot(
            pane_info(json!({ "terminal_title_stripped": "zsh" })),
            Some(agent_info(json!({ "name": "bob" }))),
        );
        assert_eq!(pane(&s, s.pane("p1").unwrap()), "bob");
    }

    #[test]
    fn agent_label_ignores_other_panes() {
        let a = agent_info(json!({ "agent": "codex" }));
        let s = snapshot(
            pane_info(json!({ "pane_id": "p2", "label": "x" })),
            Some(a.clone()),
        );
        assert_eq!(agent(&s, &a), "codex");
    }
}
