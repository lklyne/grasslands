//! The workspace list over an injected snapshot. The herdr endpoint is
//! offline, so clicks that would send herdr commands only show up as local
//! effects (expansion, the actions they dispatch); the live checks run against the dev
//! session (docs/decisions/0009-workspace-list.md).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::dock::{DockArea, DockLayout, DockSkin, panel_handle};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, Modifiers,
    ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div,
};
use serde_json::json;

use std::collections::HashSet;

use super::agents::ordered;
use super::git::Placement;
use super::groups::{Kind, Slot, entries, reordered};
use super::{AgentList, AgentSort, WorkspaceList};
use crate::actions::FocusPane;
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

/// herdr's order after dropping row `dragged` on row `target`, nothing folded.
fn drop_on(infos: &[WorkspaceInfo], dragged: &str, target: &str) -> Option<Vec<String>> {
    let infos: Vec<&WorkspaceInfo> = infos.iter().collect();
    let rows = entries(
        &infos,
        |i| i.worktree.as_ref().map(Placement::from_herdr),
        &HashSet::new(),
    );
    let slot = |id: &str| {
        rows.iter()
            .find(|e| e.info.workspace_id == id)
            .map(Slot::of)
            .unwrap()
    };
    reordered(&rows, &slot(dragged), &slot(target))
        .map(|ids| ids.iter().map(ToString::to_string).collect())
}

/// w1 plain, then alpha's group (w2 main, w3 and w4 linked), then w5 plain.
fn reorder_fixture() -> [WorkspaceInfo; 5] {
    [
        info("w1", 1, json!(null)),
        info("w2", 2, main_checkout("alpha")),
        info("w3", 3, linked("alpha", "/wt/three")),
        info("w4", 4, linked("alpha", "/wt/four")),
        info("w5", 5, json!(null)),
    ]
}

#[test]
fn dragging_plain_row_down_should_land_after_target() {
    let order = drop_on(&reorder_fixture(), "w1", "w5").unwrap();
    assert_eq!(order, ["w2", "w3", "w4", "w5", "w1"]);
}

#[test]
fn dragging_group_parent_should_move_whole_group() {
    let order = drop_on(&reorder_fixture(), "w2", "w1").unwrap();
    assert_eq!(order, ["w2", "w3", "w4", "w1", "w5"]);
}

#[test]
fn dragging_plain_row_onto_linked_worktree_should_pass_whole_group() {
    let order = drop_on(&reorder_fixture(), "w1", "w4").unwrap();
    assert_eq!(order, ["w2", "w3", "w4", "w1", "w5"]);
}

#[test]
fn dragging_linked_worktree_should_reorder_within_group() {
    let order = drop_on(&reorder_fixture(), "w4", "w3").unwrap();
    assert_eq!(order, ["w1", "w2", "w4", "w3", "w5"]);
}

#[test]
fn linked_worktree_should_not_drop_outside_its_group() {
    assert_eq!(drop_on(&reorder_fixture(), "w3", "w5"), None);
}

#[test]
fn group_parent_should_not_drop_inside_its_own_group() {
    assert_eq!(drop_on(&reorder_fixture(), "w2", "w3"), None);
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

/// Both panels in a dock, as the sidebar holds them.
struct Fixture {
    dock: Entity<DockArea>,
    /// Panes a `FocusPane` asked for, as the workspace would take them.
    focused: Rc<RefCell<Vec<String>>>,
}

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focused.clone();
        div()
            .size_full()
            .on_action(move |action: &FocusPane, _, _| {
                focused.borrow_mut().push(action.pane_id.to_string());
            })
            .child(self.dock.clone())
    }
}

struct Panels {
    spaces: Entity<WorkspaceList>,
    agents: Entity<AgentList>,
    state: Entity<AppState>,
    focused: Rc<RefCell<Vec<String>>>,
}

fn fixture(cx: &mut TestAppContext) -> (Panels, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
    let spaces = cx.new(|cx| WorkspaceList::new(state.clone(), cx));
    let agents = cx.new(|cx| AgentList::new(state.clone(), cx));
    let focused = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view(|window, cx| {
        let (dock, _) = DockSkin::dock_area("test", None, window, cx);
        let layout = DockLayout::v_split()
            .child(
                DockLayout::tabs().panel_view(panel_handle(spaces.clone()), cx),
                None,
            )
            .child(
                DockLayout::tabs().panel_view(panel_handle(agents.clone()), cx),
                None,
            );
        dock.update(cx, |dock, cx| dock.set_center(layout, window, cx));
        Fixture {
            dock,
            focused: focused.clone(),
        }
    });
    state.update(cx, |state, cx| state.replace_snapshot(snapshot(), cx));
    cx.run_until_parked();
    (
        Panels {
            spaces,
            agents,
            state,
            focused,
        },
        cx,
    )
}

#[gpui::test]
fn every_workspace_and_agent_gets_a_row(cx: &mut TestAppContext) {
    let (_, cx) = fixture(cx);
    let found: Vec<bool> = ["ws-w1", "ws-w2", "ws-w3", "agent-w2:p1", "agent-w2:p2"]
        .iter()
        .map(|s| cx.debug_bounds(s).is_some())
        .collect();
    assert_eq!(found, [true; 5]);
}

#[gpui::test]
fn sort_toggle_should_switch_to_priority(cx: &mut TestAppContext) {
    let (panels, cx) = fixture(cx);
    let toggle = cx.debug_bounds("agents-sort").unwrap().center();
    cx.simulate_click(toggle, Modifiers::none());
    cx.run_until_parked();
    let sort = panels.agents.read_with(cx, |list, cx| list.sort(cx));
    assert_eq!(sort, AgentSort::Priority);
}

/// herdr turns `done` into `idle` when the pane is focused, so the click
/// only has to focus it.
#[gpui::test]
fn clicking_an_agent_focuses_its_pane(cx: &mut TestAppContext) {
    let (Panels { focused, .. }, cx) = fixture(cx);
    let row = cx.debug_bounds("agent-w2:p1").unwrap().center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(*focused.borrow(), ["w2:p1"]);
}

#[gpui::test]
fn group_parent_gets_a_fold_toggle(cx: &mut TestAppContext) {
    let (_, cx) = fixture(cx);
    assert!(cx.debug_bounds("fold-repo").is_some());
}

#[gpui::test]
fn main_checkout_row_sits_above_linked_worktree_row(cx: &mut TestAppContext) {
    let (_, cx) = fixture(cx);
    let main = cx.debug_bounds("ws-w3").unwrap().origin.y;
    let linked = cx.debug_bounds("ws-w2").unwrap().origin.y;
    assert!(main < linked);
}

#[gpui::test]
fn folding_a_group_hides_its_linked_worktrees_only(cx: &mut TestAppContext) {
    let (_, cx) = fixture(cx);
    let toggle = cx.debug_bounds("fold-repo").unwrap().center();
    cx.simulate_click(toggle, Modifiers::none());
    cx.run_until_parked();
    let shown = ["ws-w1", "ws-w2", "ws-w3", "agent-w2:p1"].map(|s| cx.debug_bounds(s).is_some());
    assert_eq!(shown, [true, false, true, true]);
}

#[gpui::test]
fn workspace_in_a_git_folder_shows_its_branch(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("grasslands-ws-branch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    crate::fs::test_repo::TestRepo::init(&dir, "topic");
    let mut worktree = main_checkout("x");
    worktree["checkout_path"] = json!(dir.to_string_lossy());
    let (
        Panels {
            spaces: list,
            state,
            ..
        },
        cx,
    ) = fixture(cx);
    let mut snapshot = (*snapshot()).clone();
    snapshot.workspaces[0].info.worktree = serde_json::from_value(worktree).unwrap();
    state.update(cx, |state, cx| {
        state.replace_snapshot(Arc::new(snapshot), cx)
    });
    cx.run_until_parked();
    let branch = list.read_with(cx, |list, cx| {
        let snapshot = list.state.read(cx).snapshot().clone();
        list.checkout(
            &crate::state::MachineId::Local,
            &snapshot,
            &snapshot.workspaces[0].info,
        )
        .and_then(|c| c.branch.clone())
    });
    assert_eq!(branch.as_deref(), Some("topic"));
}

/// The "+" menu and the panel's title-bar menu are both built from these.
#[gpui::test]
fn new_menu_offers_workspace_worktree_and_machine_entries(cx: &mut TestAppContext) {
    let (panels, cx) = fixture(cx);
    let labels = panels.spaces.update(cx, |spaces, cx| {
        crate::chrome::labels(&spaces.new_menu_entries(cx))
    });
    assert_eq!(
        labels,
        [
            "New Workspace",
            "New Worktree…",
            "Open Worktree…",
            "---",
            "Add Machine…"
        ]
    );
}
