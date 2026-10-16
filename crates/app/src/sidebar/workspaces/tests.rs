//! The workspace list over an injected snapshot. The herdr endpoint is
//! offline, so clicks that would send herdr commands only show up as local
//! effects (expansion, seen results); the live checks run against the dev
//! session (docs/decisions/0009-workspace-list.md).

use std::sync::Arc;

use gpui_kit::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;

use std::collections::HashSet;

use super::agents::ordered;
use super::git::Placement;
use super::groups::{Kind, entries};
use super::{AgentSort, WorkspaceList, rolled_up};
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
    json!({ "repo_key": format!("/r/{repo}/.git"), "repo_name": repo, "repo_root": format!("/r/{repo}"),
            "checkout_path": checkout, "is_linked_worktree": true })
}

fn main_checkout(repo: &str) -> serde_json::Value {
    json!({ "repo_key": format!("/r/{repo}/.git"), "repo_name": repo,
            "repo_root": format!("/r/{repo}"), "checkout_path": format!("/r/{repo}"),
            "is_linked_worktree": false })
}

/// Rows as herdr draws their lead: `id` plain, `>id` a group's parent,
/// `  id` a linked worktree under it.
fn rows(infos: &[WorkspaceInfo], folded: &[&str]) -> Vec<String> {
    let infos: Vec<&WorkspaceInfo> = infos.iter().collect();
    let folded: HashSet<String> = folded.iter().map(|k| format!("/r/{k}/.git")).collect();
    entries(
        &infos,
        |i| i.worktree.as_ref().map(Placement::from_herdr),
        &folded,
    )
    .iter()
    .map(|e| {
        let lead = match e.kind {
            Kind::Plain => "",
            Kind::Parent => ">",
            Kind::Child => "  ",
        };
        format!("{lead}{}", e.info.workspace_id)
    })
    .collect()
}

#[test]
fn group_should_put_main_checkout_before_its_linked_worktrees() {
    let infos = [
        info("w1", 1, linked("alpha", "/wt/one")),
        info("w2", 2, main_checkout("alpha")),
        info("w3", 3, linked("alpha", "/wt/two")),
    ];
    assert_eq!(rows(&infos, &[]), [">w2", "  w1", "  w3"]);
}

#[test]
fn group_should_sit_where_its_first_workspace_sits() {
    let infos = [
        info("w1", 1, json!(null)),
        info("w2", 2, linked("beta", "/wt/b")),
        info("w3", 3, json!(null)),
        info("w4", 4, main_checkout("beta")),
    ];
    assert_eq!(rows(&infos, &[]), ["w1", ">w4", "  w2", "w3"]);
}

#[test]
fn repo_without_linked_worktree_should_be_a_plain_row() {
    let infos = [
        info("w1", 1, main_checkout("alpha")),
        info("w2", 2, json!(null)),
    ];
    assert_eq!(rows(&infos, &[]), ["w1", "w2"]);
}

#[test]
fn linked_worktree_without_main_checkout_should_be_a_plain_row() {
    let infos = [info("w1", 1, linked("alpha", "/wt/a"))];
    assert_eq!(rows(&infos, &[]), ["w1"]);
}

#[test]
fn folded_group_should_keep_parent_and_focused_child() {
    let mut focused = info("w3", 3, linked("alpha", "/wt/b"));
    focused.focused = true;
    let infos = [
        info("w1", 1, main_checkout("alpha")),
        info("w2", 2, linked("alpha", "/wt/a")),
        focused,
    ];
    assert_eq!(rows(&infos, &["alpha"]), [">w1", "  w3"]);
}

#[test]
fn group_should_match_repo_key_with_trailing_slash() {
    let mut slashed = main_checkout("alpha");
    slashed["repo_key"] = json!("/r/alpha/.git/");
    let infos = [
        info("w1", 1, slashed),
        info("w2", 2, linked("alpha", "/wt/a")),
    ];
    assert_eq!(rows(&infos, &[]), [">w1", "  w2"]);
}

#[test]
fn rolled_up_status_should_follow_herdr_priority() {
    let statuses = [AgentStatus::Working, AgentStatus::Done, AgentStatus::Idle];
    assert_eq!(rolled_up(statuses), AgentStatus::Done);
    assert_eq!(
        rolled_up([AgentStatus::Done, AgentStatus::Blocked]),
        AgentStatus::Blocked
    );
}

fn agents_snapshot() -> Snapshot {
    let workspaces = [info("w1", 1, json!(null)), info("w2", 2, json!(null))]
        .into_iter()
        .map(|info| crate::herdr::Workspace { info, tabs: vec![] })
        .collect();
    let mut late_done = agent("c", "w1", AgentStatus::Done);
    late_done.state_change_seq = 9;
    Snapshot {
        workspaces,
        agents: vec![
            agent("a", "w2", AgentStatus::Blocked),
            agent("b", "w1", AgentStatus::Working),
            late_done,
            agent("d", "w1", AgentStatus::Done),
        ],
        ..Snapshot::default()
    }
}

fn panes(agents: Vec<&Agent>) -> Vec<&str> {
    agents.iter().map(|a| a.pane_id.as_str()).collect()
}

#[test]
fn grouped_agents_should_follow_workspaces_then_priority() {
    let snapshot = agents_snapshot();
    assert_eq!(
        panes(ordered(&snapshot, AgentSort::Grouped)),
        ["c", "d", "b", "a"]
    );
}

#[test]
fn priority_agents_should_put_blocked_first_then_newest_change() {
    let snapshot = agents_snapshot();
    assert_eq!(
        panes(ordered(&snapshot, AgentSort::Priority)),
        ["a", "c", "d", "b"]
    );
}

fn snapshot() -> Arc<Snapshot> {
    let workspaces = [
        info("w1", 1, json!(null)),
        info("w2", 2, linked("repo", "/wt/feature")),
        info("w3", 3, main_checkout("repo")),
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
fn every_workspace_and_agent_gets_a_row(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let found: Vec<bool> = ["ws-w1", "ws-w2", "ws-w3", "agent-w2:p1", "agent-w2:p2"]
        .iter()
        .map(|s| cx.debug_bounds(s).is_some())
        .collect();
    assert_eq!(found, [true; 5]);
}

#[gpui::test]
fn folding_the_agents_section_hides_agent_rows(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let toggle = cx.debug_bounds("agents-toggle").unwrap().center();
    cx.simulate_click(toggle, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("agent-w2:p1").is_none());
}

#[gpui::test]
fn sort_toggle_should_switch_to_priority(cx: &mut TestAppContext) {
    let (list, _, cx) = fixture(cx);
    let toggle = cx.debug_bounds("agents-sort").unwrap().center();
    cx.simulate_click(toggle, Modifiers::none());
    cx.run_until_parked();
    let sort = list.read_with(cx, |list, _| list.agent_sort);
    assert_eq!(sort, AgentSort::Priority);
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

#[gpui::test]
fn group_parent_gets_a_fold_toggle(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    assert!(cx.debug_bounds("fold-repo").is_some());
}

#[gpui::test]
fn main_checkout_row_sits_above_linked_worktree_row(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let main = cx.debug_bounds("ws-w3").unwrap().origin.y;
    let linked = cx.debug_bounds("ws-w2").unwrap().origin.y;
    assert!(main < linked);
}

#[gpui::test]
fn folding_a_group_hides_its_linked_worktrees_only(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let toggle = cx.debug_bounds("fold-repo").unwrap().center();
    cx.simulate_click(toggle, Modifiers::none());
    cx.run_until_parked();
    let shown = ["ws-w1", "ws-w2", "ws-w3", "agent-w2:p1"].map(|s| cx.debug_bounds(s).is_some());
    assert_eq!(shown, [true, false, true, true]);
}

#[gpui::test]
fn workspace_in_a_git_folder_shows_its_branch(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("rusted-tools-ws-branch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git2::Repository::init_opts(
        &dir,
        git2::RepositoryInitOptions::new().initial_head("topic"),
    )
    .unwrap();
    let mut worktree = main_checkout("x");
    worktree["checkout_path"] = json!(dir.to_string_lossy());
    let (list, state, cx) = fixture(cx);
    let mut snapshot = (*snapshot()).clone();
    snapshot.workspaces[0].info.worktree = serde_json::from_value(worktree).unwrap();
    state.update(cx, |state, cx| {
        state.replace_snapshot(Arc::new(snapshot), cx)
    });
    cx.run_until_parked();
    let branch = list.read_with(cx, |list, cx| {
        let snapshot = list.state.read(cx).snapshot.clone();
        list.checkout(&snapshot, &snapshot.workspaces[0].info)
            .and_then(|c| c.branch.clone())
    });
    assert_eq!(branch.as_deref(), Some("topic"));
}
