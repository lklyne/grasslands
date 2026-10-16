//! Pushed events from `events.subscribe`.
//!
//! Lines arrive as `{"event": "<kind>", "data": {...}}`. Lifecycle kinds are
//! snake_case (`pane_created`); subscription-scoped kinds are dotted
//! (`pane.agent_status_changed`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::types::*;

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
pub fn subscriptions<'a>(pane_ids: impl IntoIterator<Item = &'a str>) -> Value {
    let lifecycle = LIFECYCLE_SUBSCRIPTIONS
        .iter()
        .map(|kind| json!({ "type": kind }));
    let status = pane_ids
        .into_iter()
        .map(|pane_id| json!({ "type": "pane.agent_status_changed", "pane_id": pane_id }));
    Value::Array(lifecycle.chain(status).collect())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    WorkspaceCreated {
        workspace: WorkspaceInfo,
    },
    WorkspaceUpdated {
        workspace: WorkspaceInfo,
    },
    WorkspaceClosed {
        workspace_id: WorkspaceId,
    },
    WorkspaceRenamed {
        workspace_id: WorkspaceId,
        label: String,
    },
    WorkspaceFocused {
        workspace_id: WorkspaceId,
    },
    WorktreeCreated {
        workspace: WorkspaceInfo,
        worktree: WorktreeInfo,
    },
    WorktreeOpened {
        workspace: WorkspaceInfo,
        worktree: WorktreeInfo,
    },
    WorktreeRemoved {
        workspace_id: WorkspaceId,
        worktree: WorktreeInfo,
    },
    TabCreated {
        tab: TabInfo,
    },
    TabClosed {
        tab_id: TabId,
        workspace_id: WorkspaceId,
    },
    TabRenamed {
        tab_id: TabId,
        workspace_id: WorkspaceId,
        label: String,
    },
    TabFocused {
        tab_id: TabId,
        workspace_id: WorkspaceId,
    },
    PaneCreated {
        pane: PaneInfo,
    },
    PaneClosed {
        pane_id: PaneId,
        workspace_id: WorkspaceId,
    },
    PaneUpdated {
        pane: PaneInfo,
    },
    PaneFocused {
        pane_id: PaneId,
        workspace_id: WorkspaceId,
    },
    PaneMoved {
        previous_pane_id: PaneId,
        pane: PaneInfo,
    },
    PaneExited {
        pane_id: PaneId,
        workspace_id: WorkspaceId,
    },
    PaneAgentDetected {
        pane_id: PaneId,
        workspace_id: WorkspaceId,
        agent: Option<String>,
    },
    PaneAgentStatusChanged {
        pane_id: PaneId,
        workspace_id: WorkspaceId,
        agent: Option<String>,
        agent_status: AgentStatus,
    },
    LayoutUpdated {
        layout: PaneLayoutSnapshot,
    },
    /// A kind this crate does not model (or whose payload failed to parse),
    /// e.g. `workspace_moved`. Still treated as "state may have changed".
    Other {
        event: String,
        data: Value,
    },
}

impl Event {
    /// Parses one pushed line. Returns `None` for lines that are not events.
    pub fn parse(line: &str) -> Option<Self> {
        #[derive(Deserialize)]
        struct Envelope {
            event: String,
            data: Value,
        }
        let Envelope { event, data } = serde_json::from_str(line).ok()?;
        Some(Self::from_parts(event, data))
    }

    fn from_parts(event: String, data: Value) -> Self {
        fn de<T: for<'de> Deserialize<'de>>(data: &Value) -> Option<T> {
            T::deserialize(data).ok()
        }
        #[derive(Deserialize)]
        struct Ws {
            workspace: WorkspaceInfo,
        }
        #[derive(Deserialize)]
        struct WsId {
            workspace_id: WorkspaceId,
        }
        #[derive(Deserialize)]
        struct WsLabel {
            workspace_id: WorkspaceId,
            label: String,
        }
        #[derive(Deserialize)]
        struct Wt {
            workspace: WorkspaceInfo,
            worktree: WorktreeInfo,
        }
        #[derive(Deserialize)]
        struct WtRemoved {
            workspace_id: WorkspaceId,
            worktree: WorktreeInfo,
        }
        #[derive(Deserialize)]
        struct Tab {
            tab: TabInfo,
        }
        #[derive(Deserialize)]
        struct TabRef {
            tab_id: TabId,
            workspace_id: WorkspaceId,
        }
        #[derive(Deserialize)]
        struct TabLabel {
            tab_id: TabId,
            workspace_id: WorkspaceId,
            label: String,
        }
        #[derive(Deserialize)]
        struct Pane {
            pane: PaneInfo,
        }
        #[derive(Deserialize)]
        struct PaneRef {
            pane_id: PaneId,
            workspace_id: WorkspaceId,
        }
        #[derive(Deserialize)]
        struct Moved {
            previous_pane_id: PaneId,
            pane: PaneInfo,
        }
        #[derive(Deserialize)]
        struct Detected {
            pane_id: PaneId,
            workspace_id: WorkspaceId,
            #[serde(default)]
            agent: Option<String>,
        }
        #[derive(Deserialize)]
        struct Status {
            pane_id: PaneId,
            workspace_id: WorkspaceId,
            #[serde(default)]
            agent: Option<String>,
            agent_status: AgentStatus,
        }
        #[derive(Deserialize)]
        struct Layout {
            layout: PaneLayoutSnapshot,
        }

        let parsed = match event.as_str() {
            "workspace_created" => de::<Ws>(&data).map(|d| Self::WorkspaceCreated {
                workspace: d.workspace,
            }),
            "workspace_updated" => de::<Ws>(&data).map(|d| Self::WorkspaceUpdated {
                workspace: d.workspace,
            }),
            "workspace_closed" => de::<WsId>(&data).map(|d| Self::WorkspaceClosed {
                workspace_id: d.workspace_id,
            }),
            "workspace_renamed" => de::<WsLabel>(&data).map(|d| Self::WorkspaceRenamed {
                workspace_id: d.workspace_id,
                label: d.label,
            }),
            "workspace_focused" => de::<WsId>(&data).map(|d| Self::WorkspaceFocused {
                workspace_id: d.workspace_id,
            }),
            "worktree_created" => de::<Wt>(&data).map(|d| Self::WorktreeCreated {
                workspace: d.workspace,
                worktree: d.worktree,
            }),
            "worktree_opened" => de::<Wt>(&data).map(|d| Self::WorktreeOpened {
                workspace: d.workspace,
                worktree: d.worktree,
            }),
            "worktree_removed" => de::<WtRemoved>(&data).map(|d| Self::WorktreeRemoved {
                workspace_id: d.workspace_id,
                worktree: d.worktree,
            }),
            "tab_created" => de::<Tab>(&data).map(|d| Self::TabCreated { tab: d.tab }),
            "tab_closed" => de::<TabRef>(&data).map(|d| Self::TabClosed {
                tab_id: d.tab_id,
                workspace_id: d.workspace_id,
            }),
            "tab_renamed" => de::<TabLabel>(&data).map(|d| Self::TabRenamed {
                tab_id: d.tab_id,
                workspace_id: d.workspace_id,
                label: d.label,
            }),
            "tab_focused" => de::<TabRef>(&data).map(|d| Self::TabFocused {
                tab_id: d.tab_id,
                workspace_id: d.workspace_id,
            }),
            "pane_created" => de::<Pane>(&data).map(|d| Self::PaneCreated { pane: d.pane }),
            "pane_closed" => de::<PaneRef>(&data).map(|d| Self::PaneClosed {
                pane_id: d.pane_id,
                workspace_id: d.workspace_id,
            }),
            "pane_updated" => de::<Pane>(&data).map(|d| Self::PaneUpdated { pane: d.pane }),
            "pane_focused" => de::<PaneRef>(&data).map(|d| Self::PaneFocused {
                pane_id: d.pane_id,
                workspace_id: d.workspace_id,
            }),
            "pane_moved" => de::<Moved>(&data).map(|d| Self::PaneMoved {
                previous_pane_id: d.previous_pane_id,
                pane: d.pane,
            }),
            "pane_exited" => de::<PaneRef>(&data).map(|d| Self::PaneExited {
                pane_id: d.pane_id,
                workspace_id: d.workspace_id,
            }),
            "pane_agent_detected" => de::<Detected>(&data).map(|d| Self::PaneAgentDetected {
                pane_id: d.pane_id,
                workspace_id: d.workspace_id,
                agent: d.agent,
            }),
            "pane.agent_status_changed" | "pane_agent_status_changed" => {
                de::<Status>(&data).map(|d| Self::PaneAgentStatusChanged {
                    pane_id: d.pane_id,
                    workspace_id: d.workspace_id,
                    agent: d.agent,
                    agent_status: d.agent_status,
                })
            }
            "layout_updated" => {
                de::<Layout>(&data).map(|d| Self::LayoutUpdated { layout: d.layout })
            }
            _ => None,
        };
        parsed.unwrap_or(Self::Other { event, data })
    }

    /// Whether this event can change pane membership, which changes the set of
    /// per-pane agent status subscriptions.
    pub fn changes_pane_set(&self) -> bool {
        matches!(
            self,
            Self::PaneCreated { .. }
                | Self::PaneClosed { .. }
                | Self::PaneMoved { .. }
                | Self::TabClosed { .. }
                | Self::WorkspaceCreated { .. }
                | Self::WorkspaceClosed { .. }
                | Self::WorktreeCreated { .. }
                | Self::WorktreeOpened { .. }
                | Self::WorktreeRemoved { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_should_read_lifecycle_event() {
        let line = r#"{"data":{"pane_id":"w1:p3","type":"pane_closed","workspace_id":"w1"},"event":"pane_closed"}"#;
        assert_eq!(
            Event::parse(line),
            Some(Event::PaneClosed {
                pane_id: "w1:p3".into(),
                workspace_id: "w1".into()
            })
        );
    }

    #[test]
    fn parse_should_read_dotted_agent_status_event() {
        let line = r#"{"data":{"agent":"claude","agent_status":"blocked","pane_id":"w1:p1","workspace_id":"w1"},"event":"pane.agent_status_changed"}"#;
        assert!(matches!(
            Event::parse(line),
            Some(Event::PaneAgentStatusChanged {
                agent_status: AgentStatus::Blocked,
                ..
            })
        ));
    }

    #[test]
    fn parse_should_keep_unknown_kinds() {
        let line = r#"{"data":{"x":1},"event":"pane_teleported"}"#;
        assert!(
            matches!(Event::parse(line), Some(Event::Other { event, .. }) if event == "pane_teleported")
        );
    }

    #[test]
    fn parse_should_tolerate_unknown_fields_and_status_values() {
        let line = r#"{"event":"pane_created","data":{"type":"pane_created","new_field":true,"pane":{"pane_id":"w1:p2","terminal_id":"t","workspace_id":"w1","tab_id":"w1:t1","focused":false,"agent_status":"pondering","revision":0,"future":[1]}}}"#;
        assert!(matches!(
            Event::parse(line),
            Some(Event::PaneCreated { pane }) if pane.agent_status == AgentStatus::Unknown
        ));
    }

    #[test]
    fn subscriptions_should_add_status_entry_per_pane() {
        let subs = subscriptions(["w1:p1", "w1:p2"]);
        let count = subs.as_array().map_or(0, Vec::len);
        assert_eq!(count, LIFECYCLE_SUBSCRIPTIONS.len() + 2);
    }
}
