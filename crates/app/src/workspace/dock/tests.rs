//! The window's dock: a rearranged layout is saved and comes back.

use std::path::Path;
use std::time::Duration;

use gpui_kit::base::Placement;
use gpui_kit::component::dock::{DockPlacement, InsertTarget, PanelId};
use gpui_kit::{AppContext as _, Context, Entity, TestAppContext, VisualTestContext, Window};

use super::{Docks, MainPanel, SAVE_DEBOUNCE};
use crate::sidebar::SidebarPanels;
use crate::state::{AppState, offline_config};

/// Builds a window's dock over `state`, saving to `file`.
fn docks(
    state: &Entity<AppState>,
    file: &Path,
) -> impl FnOnce(&mut Window, &mut Context<Docks>) -> Docks + 'static {
    let (state, file) = (state.clone(), file.to_path_buf());
    move |window, cx| {
        let main = cx.new(MainPanel::new);
        let sidebar = SidebarPanels::new(state, window, cx);
        Docks::new(main, sidebar, Some(file), window, cx)
    }
}

/// The region `panel` sits in.
fn home(
    docks: &Entity<Docks>,
    panel: PanelId,
    cx: &mut VisualTestContext,
) -> Option<DockPlacement> {
    docks.read_with(cx, |docks, cx| {
        let area = docks.area.read(cx);
        [DockPlacement::Center, DockPlacement::Left]
            .into_iter()
            .find(|&p| {
                area.layout(p)
                    .is_some_and(|t| t.find_panel_node(panel).is_some())
            })
    })
}

#[gpui::test]
fn agents_dropped_beside_the_main_area_stay_there_after_a_restart(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let file = std::env::temp_dir().join(format!("grasslands-dock-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));

    let (first, vcx) = cx.add_window_view(docks(&state, &file));
    let [main, _, agents, _] = first.read_with(vcx, |d, _| d.panels);
    assert_eq!(home(&first, agents, vcx), Some(DockPlacement::Left));
    vcx.update(|window, cx| {
        let area = first.read(cx).area.clone();
        let node = area
            .read(cx)
            .layout(DockPlacement::Center)
            .and_then(|tree| tree.find_panel_node(main))
            .expect("the main area is in the center");
        area.update(cx, |area, cx| {
            let target = InsertTarget::Split {
                node,
                placement: Placement::Right,
                size: None,
            };
            area.move_panel(agents, target, window, cx);
        });
    });
    vcx.executor()
        .advance_clock(SAVE_DEBOUNCE + Duration::from_millis(100));
    vcx.run_until_parked();

    let (second, vcx) = cx.add_window_view(docks(&state, &file));
    let [_, _, agents, _] = second.read_with(vcx, |d, _| d.panels);
    let placed = home(&second, agents, vcx);
    let _ = std::fs::remove_file(&file);
    assert_eq!(placed, Some(DockPlacement::Center));
}
