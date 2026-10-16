//! The workspace list over an injected snapshot. The herdr endpoint is
//! offline, so clicks that would send herdr commands only show up as local
//! effects (expansion, seen results); the live checks run against the dev
//! session (docs/decisions/0009-workspace-list.md).

use std::sync::Arc;

use gpui_kit::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;

use super::{WorkspaceList, by_attention, counts, worktree_line};
use crate::herdr::{Agent, AgentStatus, Snapshot, WorkspaceInfo};
use crate::state::{AppState, offline_config};

fn agent(pane: &str, workspace: &str, status: AgentStatus) -> Agent {
    serde_json::from_value(json!({
        "pane_id": pane, "terminal_id": "t", "workspace_id": workspace,
        "tab_id": format!("{workspace}:t1"), "focused": false,
        "agent_status": status, "revision": 0, "name": pane,
    }))
    .unwrap()
}

fn info(id: &str, number: u64, worktree: serde_json::Value) -> WorkspaceInfo {
    serde_json::from_value(json!({
        "workspace_id": id, "number": number, "label": format!("ws {id}"),
        "focused": false, "pane_count": 3, "tab_count": 1,
        "active_tab_id": format!("{id}:t1"), "agent_status": "idle",
        "worktree": worktree,
    }))
    .unwrap()
}

fn linked(repo: &str, checkout: &str) -> serde_json::Value {
    json!({ "repo_key": repo, "repo_name": repo, "repo_root": format!("/r/{repo}"),
            "checkout_path": checkout, "is_linked_worktree": true })
}

#[test]
fn blocked_agents_sort_first() {
    let mut agents = [
        agent("a", "w1", AgentStatus::Idle),
        agent("b", "w1", AgentStatus::Done),
        agent("c", "w1", AgentStatus::Blocked),
        agent("d", "w1", AgentStatus::Working),
    ];
    agents.sort_by(by_attention);
    let order: Vec<&str> = agents.iter().map(|a| a.pane_id.as_str()).collect();
    assert_eq!(order, ["c", "d", "b", "a"]);
}

#[test]
fn linked_worktree_line_names_repo_and_checkout() {
    let info = info("w1", 1, linked("rusted-tools", "/wt/herdr-client"));
    assert_eq!(
        worktree_line(&info),
        Some(("rusted-tools · herdr-client".to_owned(), true))
    );
}

#[test]
fn workspace_outside_git_has_no_worktree_line() {
    assert_eq!(worktree_line(&info("w1", 1, json!(null))), None);
}

#[test]
fn counts_use_singular_for_one() {
    assert_eq!(counts(&info("w1", 1, json!(null))), "1 tab · 3 panes");
}

fn snapshot() -> Arc<Snapshot> {
    let workspaces = [
        info("w1", 1, json!(null)),
        info("w2", 2, linked("repo", "/wt/feature")),
    ]
    .into_iter()
    .map(|info| crate::herdr::Workspace { info, tabs: vec![] })
    .collect();
    Arc::new(Snapshot {
        generation: 1,
        focused_workspace_id: Some("w1".into()),
        workspaces,
        agents: vec![
            agent("w2:p1", "w2", AgentStatus::Done),
            agent("w2:p2", "w2", AgentStatus::Blocked),
        ],
        ..Snapshot::default()
    })
}

fn fixture(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceList>,
    Entity<AppState>,
    &mut VisualTestContext,
) {
    cx.update(gpui_kit::init);
    let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
    let (list, cx) = cx.add_window_view(|window, cx| WorkspaceList::new(state.clone(), window, cx));
    state.update(cx, |state, cx| state.replace_snapshot(snapshot(), cx));
    cx.run_until_parked();
    (list, state, cx)
}

#[gpui::test]
fn every_workspace_and_its_agents_get_rows(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let found: Vec<bool> = ["ws-w1", "ws-w2", "agent-w2:p1", "agent-w2:p2"]
        .iter()
        .map(|s| cx.debug_bounds(s).is_some())
        .collect();
    assert_eq!(found, [true; 4]);
}

#[gpui::test]
fn collapsing_a_workspace_hides_its_agents(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let toggle = cx.debug_bounds("ws-toggle-w2").unwrap().center();
    cx.simulate_click(toggle, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-w2:p1").is_none());
}

#[gpui::test]
fn clicking_a_finished_agent_marks_it_seen(cx: &mut TestAppContext) {
    let (_, state, cx) = fixture(cx);
    let row = cx.debug_bounds("agent-w2:p1").unwrap().center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    let unseen = state.read_with(cx, |s, _| s.attention.is_unseen_done(&s.snapshot, "w2:p1"));
    assert!(!unseen);
}
