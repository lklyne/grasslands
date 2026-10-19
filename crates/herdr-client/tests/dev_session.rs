//! The cache follows layout commands sent to the dev herdr session.

mod common;

use herdr_client::*;
use serde_json::json;

use common::{Watch, dev_endpoint};

fn tree(state: &State, tab_id: &str) -> Option<SplitTree> {
    state.tab(tab_id).and_then(|t| t.root.clone())
}

fn panes_of(state: &State, tab_id: &str) -> Vec<PaneId> {
    tree(state, tab_id)
        .map(|t| t.pane_ids().into_iter().map(PaneId::from).collect())
        .unwrap_or_default()
}

#[test]
fn cache_should_follow_layout_commands() {
    let Some(endpoint) = dev_endpoint() else {
        return;
    };
    let sync = SessionSync::start(endpoint).unwrap();
    let client = sync.client().clone();
    let mut watch = Watch::new(&sync);
    watch.wait_for("first read", |s| s.generation > 0);

    let created = client
        .workspace_create(&NewWorkspace {
            label: Some("it-cache".into()),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
    let ws = created.workspace.workspace_id.clone();
    let t1 = created.tab.tab_id.clone();
    let p1 = created.root_pane.pane_id.clone();
    watch.wait_for("new workspace", |s| {
        tree(s, &t1) == Some(SplitTree::Pane(p1.clone()))
    });

    // Split.
    let p2 = client
        .pane_split(&PaneSplit::new(Some(p1.clone()), SplitDirection::Right))
        .unwrap()
        .pane_id;
    let p3 = client
        .pane_split(&PaneSplit::new(Some(p2.clone()), SplitDirection::Down))
        .unwrap()
        .pane_id;
    let state = watch.wait_for("two splits", |s| {
        panes_of(s, &t1) == [p1.clone(), p2.clone(), p3.clone()]
    });
    let splits = tree(&state, &t1).unwrap().splits();
    assert_eq!(
        splits
            .iter()
            .map(|s| (s.path.clone(), s.direction))
            .collect::<Vec<_>>(),
        vec![
            (vec![], SplitDirection::Right),
            (vec![true], SplitDirection::Down)
        ]
    );
    let terminal = &state.pane(&p3).unwrap().terminal_id;
    assert!(!terminal.is_empty());

    // Set a split ratio.
    client.layout_set_split_ratio(&t1, &[true], 0.25).unwrap();
    watch.wait_for("ratio 0.25", |s| {
        tree(s, &t1)
            .and_then(|t| t.splits().into_iter().find(|r| r.path == [true]))
            .is_some_and(|r| (r.ratio - 0.25).abs() < 0.01)
    });

    // Swap.
    let swap = client.pane_swap(&p1, &p3).unwrap();
    assert!(swap.changed);
    watch.wait_for("swap", |s| {
        panes_of(s, &t1) == [p3.clone(), p2.clone(), p1.clone()]
    });

    // Move to another tab.
    let t2 = client
        .tab_create(&NewTab {
            workspace_id: Some(ws.clone()),
            label: Some("two".into()),
            ..Default::default()
        })
        .unwrap();
    let moved = client
        .pane_move(
            &p2,
            &PaneMoveDestination::Tab {
                tab_id: t2.tab.tab_id.clone(),
                split: SplitDirection::Right,
                target_pane_id: Some(t2.root_pane.pane_id.clone()),
                ratio: None,
            },
            false,
        )
        .unwrap();
    // herdr may renumber a moved pane.
    let p2_moved = moved.pane.pane_id.clone();
    watch.wait_for("move", |s| {
        panes_of(s, &t1) == [p3.clone(), p1.clone()]
            && panes_of(s, &t2.tab.tab_id) == [t2.root_pane.pane_id.clone(), p2_moved.clone()]
    });

    // Close.
    client.pane_close(&p3).unwrap();
    watch.wait_for("close", |s| {
        panes_of(s, &t1) == [p1.clone()] && s.pane(&p3).is_none()
    });

    // Agent status arrives through the per-pane subscription alone.
    client
        .call(
            "pane.report_agent",
            &json!({ "pane_id": p1, "source": "it", "agent": "claude", "state": "blocked" }),
        )
        .unwrap();
    watch.wait_for("agent status", |s| {
        s.pane(&p1)
            .is_some_and(|p| p.agent_status == AgentStatus::Blocked)
    });
    assert!(
        watch
            .events
            .iter()
            .any(|kind| kind == "pane.agent_status_changed"),
        "no pane.agent_status_changed event for {p1}; saw {:?}",
        watch.events
    );

    // Rename and focus round trips.
    client.tab_rename(&t1, "renamed").unwrap();
    watch.wait_for("tab rename", |s| {
        s.tab(&t1).is_some_and(|t| t.info.label == "renamed")
    });
    client.pane_focus(&p1).unwrap();
    watch.wait_for("focus", |s| {
        s.focused_pane_id.as_deref() == Some(p1.as_str())
    });

    client.workspace_close(&ws, false).unwrap();
    watch.wait_for("workspace closed", |s| s.workspace(&ws).is_none());
}

#[test]
fn commands_should_decode_dev_server_results() {
    let Some(endpoint) = dev_endpoint() else {
        return;
    };
    let client = Client::new(endpoint);
    assert_eq!(client.ping().unwrap().version, "0.9.5");
    client.agent_list().unwrap();
    client.plugin_action_list(None).unwrap();
    client.workspace_list().unwrap();
}

#[test]
fn unknown_pane_should_return_server_error() {
    let Some(endpoint) = dev_endpoint() else {
        return;
    };
    let err = Client::new(endpoint)
        .pane_get(&"w999:p999".into())
        .unwrap_err();
    assert!(err.code().is_some(), "{err}");
}
