//! Chrome workflows in a headless window. The herdr endpoint points at a
//! socket that does not exist, so nothing here can reach a real herdr; the
//! cache is fed with `replace_snapshot` instead.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Point, TestAppContext,
    WindowBounds, WindowOptions, px, size,
};

use super::{Chrome, HerdrCommand, MenuCommand, MenuEntry, MenuTarget};
use crate::herdr::config::{Edit, ToastDelivery};
use crate::herdr::{self, Agent, AgentStatus, Snapshot};
use crate::state::{AppState, MachineId};

struct Fixture {
    window: AnyWindowHandle,
    chrome: Entity<Chrome>,
    state: Entity<AppState>,
}

fn offline() -> herdr::Config {
    herdr::Config {
        bin: "/nonexistent/herdr".into(),
        session: None,
        socket_path: std::env::temp_dir().join("grasslands-chrome-test-no-herdr.sock"),
        auto_start: false,
        server_log: None,
    }
}

fn agent(pane: &str, status: AgentStatus) -> Agent {
    Agent {
        pane_id: pane.into(),
        terminal_id: "".into(),
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

fn snapshot(status: AgentStatus) -> Arc<Snapshot> {
    Arc::new(Snapshot {
        agents: vec![agent("w1:p1", status)],
        ..Snapshot::default()
    })
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let state = cx.new(|cx| AppState::new(Ok(offline()), cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(900.), px(600.)),
            })),
            ..WindowOptions::default()
        };
        let mut built = None;
        let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
            let chrome = cx.new(|cx| Chrome::new(state.clone(), window, cx));
            built = Some(chrome.clone());
            chrome
        })
        .unwrap();
        state.update(cx, |state, cx| {
            state.replace_snapshot(snapshot(AgentStatus::Working), cx);
        });
        Fixture {
            window,
            chrome: built.unwrap(),
            state,
        }
    })
}

fn has_dialog(cx: &mut TestAppContext, f: &Fixture) -> bool {
    cx.run_until_parked();
    cx.update_window(f.window, |_, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

#[gpui_kit::test]
fn closing_an_agent_pane_asks_first(cx: &mut TestAppContext) {
    let f = fixture(cx);
    cx.update_window(f.window, |_, window, cx| {
        f.chrome.update(cx, |chrome, cx| {
            let close = HerdrCommand::ClosePane {
                machine: MachineId::Local,
                pane_id: "w1:p1".into(),
            };
            chrome.run(close, window, cx);
        });
    })
    .unwrap();
    assert!(has_dialog(cx, &f));
}

#[gpui_kit::test]
fn context_menu_opens_with_pane_actions(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let target = MenuTarget::Pane {
        machine: MachineId::Local,
        pane_id: "w1:p1".into(),
    };
    let labels = cx
        .update_window(f.window, |_, window, cx| {
            f.chrome.update(cx, |chrome, cx| {
                chrome.context_menu(target.clone(), Point::default(), Vec::new(), window, cx);
                chrome
                    .default_entries(&target, cx)
                    .into_iter()
                    .filter_map(|entry| match entry {
                        MenuEntry::Item { label, .. } => Some(label.to_string()),
                        MenuEntry::Separator | MenuEntry::Submenu { .. } => None,
                    })
                    .collect::<Vec<_>>()
            })
        })
        .unwrap();
    cx.run_until_parked();
    let open = cx.update(|cx| f.chrome.read(cx).open_menu.is_some());
    assert!(open);
    assert_eq!(
        labels,
        [
            "Focus Pane",
            "Split Right",
            "Split Down",
            "Toggle Zoom",
            "Rename Pane…",
            "Close Pane"
        ]
    );
}

#[gpui_kit::test]
fn closing_the_menu_gives_focus_back(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let owner = cx
        .update_window(f.window, |_, window, cx| {
            let owner = f.state.update(cx, |_, cx| cx.focus_handle());
            owner.focus(window, cx);
            f.chrome.update(cx, |chrome, cx| {
                let target = MenuTarget::Tab {
                    machine: MachineId::Local,
                    tab_id: "w1:t1".into(),
                };
                chrome.context_menu(target, Point::default(), Vec::new(), window, cx);
            });
            owner
        })
        .unwrap();
    cx.run_until_parked();
    let restored = cx
        .update_window(f.window, |_, window, cx| {
            f.chrome.update(cx, |chrome, cx| {
                let menu = chrome.open_menu.as_ref().map(|open| open.menu.clone());
                assert!(menu.is_some_and(|menu| menu.focus_handle(cx).is_focused(window)));
                chrome.close_context_menu(window, cx);
            });
            owner.is_focused(window)
        })
        .unwrap();
    assert!(restored);
}

#[gpui_kit::test]
fn palette_opens_as_a_dialog(cx: &mut TestAppContext) {
    let f = fixture(cx);
    cx.update_window(f.window, |_, window, cx| {
        f.chrome
            .update(cx, |chrome, cx| chrome.open_command_palette(window, cx));
    })
    .unwrap();
    assert!(has_dialog(cx, &f));
}

/// herdr's `[ui.toast]`, in memory (tests never write herdr's config).
fn toasts_from_herdr(cx: &mut TestAppContext, delivery: ToastDelivery, delay_seconds: u64) {
    cx.update(|cx| {
        crate::herdr_prefs::set(Edit::ToastDelivery(delivery), cx);
        crate::herdr_prefs::set(Edit::ToastDelay(delay_seconds), cx);
    });
}

fn set_status(cx: &mut TestAppContext, f: &Fixture, status: AgentStatus) {
    cx.update(|cx| {
        f.state.update(cx, |state, cx| {
            state.replace_snapshot(snapshot(status), cx);
        });
    });
    cx.run_until_parked();
}

fn toast_count(cx: &mut TestAppContext, f: &Fixture) -> usize {
    cx.update_window(f.window, |_, window, cx| window.notifications(cx).len())
        .unwrap()
}

#[gpui_kit::test]
fn blocked_agent_raises_a_toast(cx: &mut TestAppContext) {
    let f = fixture(cx);
    toasts_from_herdr(cx, ToastDelivery::Herdr, 0);
    set_status(cx, &f, AgentStatus::Blocked);
    assert_eq!(toast_count(cx, &f), 1);
}

#[gpui_kit::test]
fn herdrs_default_delivery_raises_no_toast(cx: &mut TestAppContext) {
    let f = fixture(cx);
    set_status(cx, &f, AgentStatus::Blocked);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(toast_count(cx, &f), 0);
}

#[gpui_kit::test]
fn the_toast_waits_out_herdrs_delay(cx: &mut TestAppContext) {
    let f = fixture(cx);
    toasts_from_herdr(cx, ToastDelivery::Herdr, 1);
    set_status(cx, &f, AgentStatus::Blocked);
    let before = toast_count(cx, &f);
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!((before, toast_count(cx, &f)), (0, 1));
}

#[gpui_kit::test]
fn an_agent_that_moves_on_during_the_delay_gets_no_toast(cx: &mut TestAppContext) {
    let f = fixture(cx);
    toasts_from_herdr(cx, ToastDelivery::Herdr, 1);
    set_status(cx, &f, AgentStatus::Blocked);
    set_status(cx, &f, AgentStatus::Working);
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(toast_count(cx, &f), 0);
}

#[gpui_kit::test]
fn unconfirmed_command_is_sent_and_failure_toasts(cx: &mut TestAppContext) {
    let f = fixture(cx);
    cx.update_window(f.window, |_, window, cx| {
        f.chrome.update(cx, |chrome, cx| {
            let focus = HerdrCommand::FocusPane {
                machine: MachineId::Local,
                pane_id: "w1:p1".into(),
            };
            chrome.run(focus, window, cx);
        });
    })
    .unwrap();
    // The endpoint has no server, so the send fails into an error toast.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut toasts = 0;
    while toasts == 0 && std::time::Instant::now() < deadline {
        cx.run_until_parked();
        toasts = cx
            .update_window(f.window, |_, window, cx| window.notifications(cx).len())
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(toasts, 1);
}

#[test]
fn agent_toasts_for_the_same_pane_on_two_machines_differ() {
    let local = super::toast_key(&MachineId::Local, &"w1:p1".into());
    let remote = super::toast_key(&MachineId::Remote("b87d".into()), &"w1:p1".into());
    assert_ne!(local, remote);
}

#[gpui_kit::test]
fn pane_menu_commands_keep_the_machine_they_were_opened_on(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let remote = MachineId::Remote("b87d".into());
    let target = MenuTarget::Pane {
        machine: remote.clone(),
        pane_id: "w1:p1".into(),
    };
    let machines = cx.update(|cx| {
        f.chrome
            .read(cx)
            .default_entries(&target, cx)
            .into_iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item {
                    command: super::MenuCommand::Herdr(command),
                    ..
                } => Some(command.machine().clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert!(!machines.is_empty() && machines.iter().all(|m| m == &remote));
}

fn workspace(id: &str, linked: Option<bool>) -> herdr::Workspace {
    herdr::Workspace {
        info: herdr::WorkspaceInfo {
            workspace_id: id.into(),
            number: 1,
            label: format!("ws-{id}"),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: format!("{id}:t1").into(),
            agent_status: AgentStatus::Idle,
            worktree: linked.map(|linked| herdr::WorkspaceWorktreeInfo {
                repo_key: "/r/.git".into(),
                repo_name: "r".into(),
                repo_root: "/r".into(),
                checkout_path: format!("/w/{id}"),
                is_linked_worktree: linked,
            }),
            tokens: Default::default(),
        },
        tabs: Vec::new(),
    }
}

/// `w1` and `w2` without herdr worktree info, `w3` a linked worktree herdr
/// knows about.
fn worktree_fixture(cx: &mut TestAppContext) -> Fixture {
    let f = fixture(cx);
    cx.update(|cx| {
        f.state.update(cx, |state, cx| {
            let snapshot = Snapshot {
                workspaces: vec![
                    workspace("w1", None),
                    workspace("w2", None),
                    workspace("w3", Some(true)),
                ],
                ..Snapshot::default()
            };
            state.replace_snapshot(Arc::new(snapshot), cx);
        });
    });
    f
}

fn workspace_labels(cx: &mut TestAppContext, f: &Fixture, id: &str) -> Vec<String> {
    let target = MenuTarget::Workspace {
        machine: MachineId::Local,
        workspace_id: id.into(),
    };
    cx.update(|cx| {
        f.chrome
            .read(cx)
            .default_entries(&target, cx)
            .into_iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item { label, .. } => Some(label.to_string()),
                MenuEntry::Separator | MenuEntry::Submenu { .. } => None,
            })
            .collect()
    })
}

/// What the spaces list's git read would say: `w1` is the main checkout and
/// `w2` a linked worktree of herdr's repo.
fn infer_w1_w2(cx: &mut TestAppContext) {
    cx.update(|cx| {
        super::set_placements(
            cx,
            std::rc::Rc::new(|_, info, _| {
                super::worktrees::herdr_placement(info).or_else(|| {
                    let linked = match info.workspace_id.as_str() {
                        "w1" => false,
                        "w2" => true,
                        _ => return None,
                    };
                    Some(crate::worktree::Placement {
                        repo: crate::worktree::Repo {
                            key: "/r/.git".into(),
                            name: "r".into(),
                            root: "/r".into(),
                        },
                        linked,
                    })
                })
            }),
        );
    });
}

#[gpui_kit::test]
fn a_linked_worktree_menu_offers_open_and_remove(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    let labels = workspace_labels(cx, &f, "w3");
    assert!(
        labels.contains(&"Open Worktree…".to_owned())
            && labels.contains(&"Remove Worktree…".to_owned())
    );
}

#[gpui_kit::test]
fn a_workspace_outside_a_repo_offers_no_worktree_entries(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    let labels = workspace_labels(cx, &f, "w1");
    assert!(!labels.iter().any(|l| l.contains("Worktree")));
}

#[gpui_kit::test]
fn an_inferred_linked_worktree_offers_remove(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    infer_w1_w2(cx);
    assert!(workspace_labels(cx, &f, "w2").contains(&"Remove Worktree…".to_owned()));
}

#[gpui_kit::test]
fn an_inferred_group_closes_exactly_its_listed_members(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    infer_w1_w2(cx);
    let close = cx.update(|cx| {
        f.chrome
            .read(cx)
            .group_close(&MachineId::Local, &"w1".into(), cx)
    });
    let members = match close {
        Some(HerdrCommand::CloseGroup { members, .. }) => members,
        _ => Vec::new(),
    };
    // w3 is herdr's own linked worktree of the same repo, so it belongs too.
    assert_eq!(members, ["w1", "w2", "w3"]);
}

/// A callback entry that counts its runs, and the entries around it.
fn counted_entries() -> (Vec<MenuEntry>, Rc<Cell<usize>>) {
    let runs = Rc::new(Cell::new(0));
    let count = runs.clone();
    let callback = MenuCommand::Callback(Rc::new(move |_, _| count.set(count.get() + 1)));
    let entries = vec![
        MenuEntry::item("Run", callback.clone()),
        MenuEntry::Separator,
        MenuEntry::Submenu {
            label: "More".into(),
            entries: vec![MenuEntry::item("Run Too", callback)],
        },
    ];
    (entries, runs)
}

#[gpui_kit::test]
fn a_menu_with_a_submenu_opens(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let (entries, _) = counted_entries();
    assert_eq!(
        super::labels(&entries),
        ["Run", "---", "More › [\"Run Too\"]"]
    );
    cx.update_window(f.window, |_, window, cx| {
        super::open_menu(entries, Point::default(), window, cx);
    })
    .unwrap();
    cx.run_until_parked();
    let open = cx.update(|cx| f.chrome.read(cx).open_menu.is_some());
    assert!(open);
}

#[gpui_kit::test]
fn an_empty_menu_does_not_open(cx: &mut TestAppContext) {
    let f = fixture(cx);
    cx.update_window(f.window, |_, window, cx| {
        super::open_menu(Vec::new(), Point::default(), window, cx);
    })
    .unwrap();
    cx.run_until_parked();
    let open = cx.update(|cx| f.chrome.read(cx).open_menu.is_some());
    assert!(!open);
}

/// A dock panel's title-bar menu gets the same entries through the chrome.
#[gpui_kit::test]
fn fill_popup_adds_the_entries(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let (entries, _) = counted_entries();
    let filled = cx
        .update_window(f.window, |_, window, cx| {
            let menu = PopupMenu::build(window, cx, |menu, window, cx| {
                super::fill_popup(menu, &entries, window, cx)
            });
            !menu.read(cx).is_empty()
        })
        .unwrap();
    assert!(filled);
}

/// A callback entry runs through the chrome like any menu command.
#[gpui_kit::test]
fn a_callback_entry_runs_through_the_chrome(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let (entries, runs) = counted_entries();
    let MenuEntry::Item { command, .. } = entries[0].clone() else {
        panic!("first entry is an item");
    };
    cx.update_window(f.window, |_, window, cx| {
        f.chrome.update(cx, |chrome, cx| {
            chrome.run_menu_command(command, window, cx)
        });
    })
    .unwrap();
    assert_eq!(runs.get(), 1);
}

// The flows the live step list drives (`crate::script::steps`), headless.

/// `menu tab <id>` and `menu workspace <id>`.
#[gpui_kit::test]
fn tab_and_workspace_menus_open(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    for target in [
        MenuTarget::Tab {
            machine: MachineId::Local,
            tab_id: "w1:t1".into(),
        },
        MenuTarget::Workspace {
            machine: MachineId::Local,
            workspace_id: "w1".into(),
        },
    ] {
        cx.update_window(f.window, |_, window, cx| {
            f.chrome.update(cx, |chrome, cx| {
                chrome.close_context_menu(window, cx);
                chrome.context_menu(target.clone(), Point::default(), Vec::new(), window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        // `Some(false)`: drawn in the window, as there is no overlay here.
        let open = cx.update(|cx| f.chrome.read(cx).menu_open());
        assert!(open.is_some(), "{target:?}");
    }
}

fn run_menu_command(cx: &mut TestAppContext, f: &Fixture, command: MenuCommand) {
    cx.update_window(f.window, |_, window, cx| {
        f.chrome.update(cx, |chrome, cx| {
            chrome.run_menu_command(command, window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
}

/// `remove-worktree <workspace>`: nothing is removed before the person
/// confirms.
#[gpui_kit::test]
fn removing_a_worktree_asks_first(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    let remove = super::WorktreeCommand::Remove {
        machine: MachineId::Local,
        workspace_id: "w3".into(),
    };
    run_menu_command(cx, &f, MenuCommand::Worktree(remove));
    assert!(has_dialog(cx, &f));
}

/// `open-worktree <workspace>` then `query <text>`: herdr's list (here a
/// fixture) shows in a picker that the query filters.
#[gpui_kit::test]
fn the_worktree_picker_filters_by_query(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    let list: herdr::WorktreeList = serde_json::from_value(serde_json::json!({
        "source": {
            "repo_key": "/r/.git",
            "repo_name": "r",
            "repo_root": "/r",
            "source_checkout_path": "/r",
        },
        "worktrees": [
            {"path": "/r", "label": "r", "is_bare": false, "is_detached": false,
             "is_prunable": false, "is_linked_worktree": false, "branch": "main"},
            {"path": "/w/feat", "label": "r", "is_bare": false, "is_detached": false,
             "is_prunable": false, "is_linked_worktree": true, "branch": "feat"},
        ],
    }))
    .unwrap();
    cx.update(|cx| {
        f.chrome.update(cx, |chrome, cx| {
            chrome.show_worktree_picker(MachineId::Local, "w3".into(), list, cx)
        });
    });
    assert!(has_dialog(cx, &f));
    let picker = cx.update(|cx| f.chrome.read(cx).palette()).unwrap();
    let all = cx.update(|cx| picker.read(cx).matched_count());
    cx.update_window(f.window, |_, window, cx| {
        picker.update(cx, |picker, cx| picker.set_query("feat", window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    let matched = cx.update(|cx| picker.read(cx).matched_count());
    assert_eq!((all, matched), (2, 1));
}

/// `type <text>` replaces the rename dialog's text, which starts as the
/// current label.
#[gpui_kit::test]
fn the_rename_dialog_starts_from_the_label(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    let target = MenuTarget::Workspace {
        machine: MachineId::Local,
        workspace_id: "w2".into(),
    };
    run_menu_command(cx, &f, MenuCommand::Rename(target));
    assert!(has_dialog(cx, &f));
    let input = cx.update(|cx| f.chrome.read(cx).rename_input()).unwrap();
    let value = cx.update(|cx| input.read(cx).value().to_string());
    assert_eq!(value, "ws-w2");
}

/// `palette` then `query split`: with a focused pane, the split entries
/// match.
#[gpui_kit::test]
fn the_palette_query_finds_the_splits(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let pane: herdr::PaneInfo = serde_json::from_value(serde_json::json!({
        "pane_id": "w1:p1", "terminal_id": "t", "workspace_id": "w1", "tab_id": "w1:t1",
        "focused": true, "agent_status": "idle", "revision": 0,
    }))
    .unwrap();
    let snapshot = Snapshot {
        focused_pane_id: Some(pane.pane_id.clone()),
        panes: [(pane.pane_id.clone(), pane)].into(),
        ..Snapshot::default()
    };
    cx.update(|cx| {
        f.state.update(cx, |state, cx| {
            state.replace_snapshot(Arc::new(snapshot), cx)
        });
    });
    cx.update_window(f.window, |_, window, cx| {
        f.chrome
            .update(cx, |chrome, cx| chrome.open_command_palette(window, cx));
    })
    .unwrap();
    assert!(has_dialog(cx, &f));
    let palette = cx.update(|cx| f.chrome.read(cx).palette()).unwrap();
    cx.update_window(f.window, |_, window, cx| {
        palette.update(cx, |palette, cx| palette.set_query("split", window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    let matched = cx.update(|cx| palette.read(cx).matched_count());
    assert!(matched >= 2, "{matched} matches");
}

/// `run close-group <workspace>`: closing a whole group asks first (herdr's
/// `ui.confirm_close`, on by default).
#[gpui_kit::test]
fn closing_a_group_asks_first(cx: &mut TestAppContext) {
    let f = worktree_fixture(cx);
    infer_w1_w2(cx);
    let close = cx
        .update(|cx| {
            f.chrome
                .read(cx)
                .group_close(&MachineId::Local, &"w1".into(), cx)
        })
        .unwrap();
    cx.update_window(f.window, |_, window, cx| {
        f.chrome
            .update(cx, |chrome, cx| chrome.run(close, window, cx));
    })
    .unwrap();
    assert!(has_dialog(cx, &f));
}
