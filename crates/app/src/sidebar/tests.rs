//! The sidebar's dock: a rearranged layout is saved and comes back.

use std::time::Duration;

use gpui_kit::component::dock::{DockPlacement, InsertTarget, PanelId};
use gpui_kit::{AppContext as _, Entity, TestAppContext};

use super::{SAVE_DEBOUNCE, Sidebar};
use crate::state::{AppState, offline_config};

/// The tab group holding `panel`.
fn group_of(sidebar: &Sidebar, panel: PanelId, cx: &gpui_kit::App) -> Option<u64> {
    let dock = sidebar.dock.read(cx);
    let tree = dock.layout(DockPlacement::Center)?;
    tree.find_panel_node(panel).map(|node| node.as_u64())
}

#[gpui::test]
fn agents_stacked_onto_spaces_stay_stacked_after_a_restart(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let file =
        std::env::temp_dir().join(format!("rusted-tools-layout-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));

    let open = |cx: &mut TestAppContext, state: &Entity<AppState>| {
        let (state, file) = (state.clone(), file.clone());
        cx.add_window_view(move |window, cx| {
            Sidebar::with_layout_file(state, Some(file), window, cx)
        })
        .0
    };

    let first = open(cx, &state);
    cx.update(|cx| {
        let window = cx.windows()[0];
        window
            .update(cx, |_, window, cx| {
                let (dock, [spaces, agents, _]) =
                    first.read_with(cx, |s, _| (s.dock.clone(), s.panels));
                let node = dock
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .and_then(|tree| tree.find_panel_node(spaces))
                    .expect("spaces is docked");
                dock.update(cx, |dock, cx| {
                    let target = InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    };
                    dock.move_panel(agents, target, window, cx);
                });
            })
            .unwrap();
    });
    cx.executor()
        .advance_clock(SAVE_DEBOUNCE + Duration::from_millis(100));
    cx.run_until_parked();

    let second = open(cx, &state);
    let stacked = second.read_with(cx, |s, cx| {
        let [spaces, agents, _] = s.panels;
        group_of(s, spaces, cx).is_some() && group_of(s, spaces, cx) == group_of(s, agents, cx)
    });
    let _ = std::fs::remove_file(&file);
    assert!(stacked);
}
