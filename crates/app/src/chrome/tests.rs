//! Chrome workflows in a headless window. The herdr endpoint points at a
//! socket that does not exist, so nothing here can reach a real herdr; the
//! cache is fed with `replace_snapshot` instead.

use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Point, TestAppContext,
    WindowBounds, WindowOptions, px, size,
};

use super::{Chrome, HerdrCommand, MenuEntry, MenuTarget};
use crate::herdr::{self, Agent, AgentStatus, Snapshot};
use crate::state::AppState;

struct Fixture {
    window: AnyWindowHandle,
    chrome: Entity<Chrome>,
    state: Entity<AppState>,
}

fn offline() -> herdr::Config {
    herdr::Config {
        bin: "/nonexistent/herdr".into(),
        session: None,
        socket_path: std::env::temp_dir().join("rusted-tools-chrome-test-no-herdr.sock"),
        auto_start: false,
    }
}

fn agent(pane: &str, status: AgentStatus) -> Agent {
    Agent {
        pane_id: pane.into(),
        terminal_id: String::new(),
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

#[gpui_kit::test]
fn closing_an_agent_pane_asks_first(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let asked = cx
        .update_window(f.window, |_, window, cx| {
            f.chrome.update(cx, |chrome, cx| {
                let close = HerdrCommand::ClosePane {
                    pane_id: "w1:p1".into(),
                };
                chrome.run(close, window, cx);
            });
            window.has_active_dialog(cx)
        })
        .unwrap();
    assert!(asked);
}

#[gpui_kit::test]
fn context_menu_opens_with_pane_actions(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let labels = cx
        .update_window(f.window, |_, window, cx| {
            f.chrome.update(cx, |chrome, cx| {
                let target = MenuTarget::Pane {
                    pane_id: "w1:p1".into(),
                };
                chrome.context_menu(target.clone(), Point::default(), Vec::new(), window, cx);
                assert!(chrome.open_menu.is_some());
                chrome
                    .default_entries(&target, cx)
                    .into_iter()
                    .filter_map(|entry| match entry {
                        MenuEntry::Item { label, .. } => Some(label.to_string()),
                        MenuEntry::Separator => None,
                    })
                    .collect::<Vec<_>>()
            })
        })
        .unwrap();
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
    let restored = cx
        .update_window(f.window, |_, window, cx| {
            let owner = f.state.update(cx, |_, cx| cx.focus_handle());
            owner.focus(window, cx);
            f.chrome.update(cx, |chrome, cx| {
                let target = MenuTarget::Tab {
                    tab_id: "w1:t1".into(),
                };
                chrome.context_menu(target, Point::default(), Vec::new(), window, cx);
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
    let open = cx
        .update_window(f.window, |_, window, cx| {
            f.chrome
                .update(cx, |chrome, cx| chrome.open_command_palette(window, cx));
            window.has_active_dialog(cx)
        })
        .unwrap();
    assert!(open);
}

#[gpui_kit::test]
fn blocked_agent_raises_a_toast(cx: &mut TestAppContext) {
    let f = fixture(cx);
    cx.update(|cx| {
        f.state.update(cx, |state, cx| {
            state.replace_snapshot(snapshot(AgentStatus::Blocked), cx);
        });
    });
    cx.run_until_parked();
    let toasts = cx
        .update_window(f.window, |_, window, cx| window.notifications(cx).len())
        .unwrap();
    assert_eq!(toasts, 1);
}
