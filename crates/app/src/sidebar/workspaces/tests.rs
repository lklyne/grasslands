//! The workspace list over an injected snapshot. The herdr endpoint is
//! offline, so clicks that would send herdr commands only show up as local
//! effects (expansion, seen results); the live checks run against the dev
//! session (docs/decisions/0009-workspace-list.md).

use std::sync::Arc;

use gpui_kit::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;

use super::git::Placement;
use super::groups::group;
use super::{WorkspaceList, by_attention, counts, rolled_up};
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

fn main_checkout(repo: &str) -> serde_json::Value {
    json!({ "repo_key": format!("/r/{repo}/.git"), "repo_name": repo,
            "repo_root": format!("/r/{repo}"), "checkout_path": format!("/r/{repo}"),
            "is_linked_worktree": false })
}

/// Group ids in display order: `repo: ids…`, then `-: ids…` for no repo.
fn grouped(infos: &[WorkspaceInfo]) -> Vec<String> {
    let groups = group(infos, |i| i.worktree.as_ref().map(Placement::from_herdr));
    let ids = |ids: Vec<&str>| ids.join(" ");
    groups
        .repos
        .iter()
        .map(|g| {
            let ws = g.workspaces.iter().map(|(i, _)| i.workspace_id.as_str());
            format!("{}: {}", g.repo.name, ids(ws.collect()))
        })
        .chain((!groups.ungrouped.is_empty()).then(|| {
            let ws = groups.ungrouped.iter().map(|i| i.workspace_id.as_str());
            format!("-: {}", ids(ws.collect()))
        }))
        .collect()
}

#[test]
fn group_should_put_main_checkout_before_its_linked_worktrees() {
    let infos = [
        info("w1", 1, linked("alpha", "/wt/one")),
        info("w2", 2, main_checkout("alpha")),
        info("w3", 3, linked("alpha", "/wt/two")),
    ];
    assert_eq!(grouped(&infos), ["alpha: w2 w1 w3"]);
}

#[test]
fn group_should_order_repos_by_their_first_workspace() {
    let infos = [
        info("w1", 1, main_checkout("beta")),
        info("w2", 2, main_checkout("alpha")),
        info("w3", 3, linked("beta", "/wt/b")),
    ];
    assert_eq!(grouped(&infos), ["beta: w1 w3", "alpha: w2"]);
}

#[test]
fn group_should_put_workspaces_without_repo_last() {
    let infos = [
        info("w1", 1, json!(null)),
        info("w2", 2, main_checkout("alpha")),
        info("w3", 3, json!(null)),
    ];
    assert_eq!(grouped(&infos), ["alpha: w2", "-: w1 w3"]);
}

#[test]
fn group_should_match_repo_key_with_trailing_slash() {
    let mut slashed = main_checkout("alpha");
    slashed["repo_key"] = json!("/r/alpha/.git/");
    let infos = [
        info("w1", 1, slashed),
        info("w2", 2, linked("alpha", "/wt/a")),
    ];
    assert_eq!(grouped(&infos), ["alpha: w1 w2"]);
}

#[test]
fn rolled_up_status_should_prefer_blocked() {
    let statuses = [
        AgentStatus::Done,
        AgentStatus::Blocked,
        AgentStatus::Working,
    ];
    assert_eq!(rolled_up(statuses), AgentStatus::Blocked);
}

#[test]
fn counts_use_singular_for_one() {
    assert_eq!(counts(&info("w1", 1, json!(null))), "1 tab · 3 panes");
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

#[gpui::test]
fn repo_gets_a_heading(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    assert!(cx.debug_bounds("repo-repo").is_some());
}

#[gpui::test]
fn main_checkout_row_sits_above_linked_worktree_row(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let main = cx.debug_bounds("ws-w3").unwrap().origin.y;
    let linked = cx.debug_bounds("ws-w2").unwrap().origin.y;
    assert!(main < linked);
}

#[gpui::test]
fn folding_a_repo_hides_its_workspaces(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let heading = cx.debug_bounds("repo-repo").unwrap().center();
    cx.simulate_click(heading, Modifiers::none());
    cx.run_until_parked();
    let hidden = ["ws-w2", "ws-w3", "agent-w2:p1"].map(|s| cx.debug_bounds(s).is_none());
    assert_eq!(hidden, [true; 3]);
}

#[gpui::test]
fn workspace_without_repo_stays_visible_after_folding_repos(cx: &mut TestAppContext) {
    let (_, _, cx) = fixture(cx);
    let heading = cx.debug_bounds("repo-repo").unwrap().center();
    cx.simulate_click(heading, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("ws-w1").is_some());
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
