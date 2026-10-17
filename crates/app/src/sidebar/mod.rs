//! The sidebar: the spaces, agents and files panels in a GPUI Kit dock, so
//! each can be dragged to reorder, stacked as tabs or split off. The dock's
//! own skin draws the title bars, in the theme's colors. The layout is saved
//! per herdr session and restored on launch; see
//! docs/decisions/0011-sidebar-dock.md.

pub mod files;
pub mod workspaces;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{
    DockArea, DockAreaState, DockEvent, DockLayout, DockSkin, PanelId, panel_handle, register_panel,
};
use gpui_kit::*;

use crate::state::AppState;

/// Bumped when a saved layout should no longer load.
const LAYOUT_VERSION: usize = 1;
/// A drag emits a layout change on every step; save once it settles.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub fn init(cx: &mut App) {
    workspaces::init(cx);
    files::init(cx);
}

pub struct Sidebar {
    dock: Entity<DockArea>,
    /// Where the layout is saved; `None` keeps it in memory only.
    layout_file: Option<PathBuf>,
    /// Spaces, agents and files, in that order.
    panels: [PanelId; 3],
    _save: Option<Task<()>>,
    _subscription: Subscription,
}

impl Sidebar {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_layout_file(state, layout_path(), window, cx)
    }

    pub fn with_layout_file(
        state: Entity<AppState>,
        layout_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let spaces = cx.new(|cx| workspaces::WorkspaceList::new(state.clone(), cx));
        let agents = cx.new(|cx| workspaces::AgentList::new(state.clone(), cx));
        let files = cx.new(|cx| files::FileTree::new(state, window, cx));
        workspaces::start_script(spaces.downgrade(), agents.downgrade(), window, cx);

        // A saved layout names panels; these hand back the live ones.
        register_panel(cx, "SpacesPanel", {
            let spaces = spaces.clone();
            move |_, _, _| panel_handle(spaces.clone())
        });
        register_panel(cx, "AgentsPanel", {
            let agents = agents.clone();
            move |_, _, _| panel_handle(agents.clone())
        });
        register_panel(cx, "FilesPanel", {
            let files = files.clone();
            move |_, _, _| panel_handle(files.clone())
        });

        let (dock, _skin) = DockSkin::dock_area("sidebar", Some(LAYOUT_VERSION), window, cx);
        let panels = [
            PanelId::from(spaces.entity_id()),
            PanelId::from(agents.entity_id()),
            PanelId::from(files.entity_id()),
        ];
        dock.update(cx, |dock, cx| {
            let restored = layout_file
                .as_deref()
                .and_then(saved_layout)
                .is_some_and(|state| {
                    dock.load(state, window, cx).is_ok()
                        && panels.iter().all(|&id| dock.panel(id).is_some())
                });
            if !restored {
                let layout = DockLayout::v_split()
                    .child(
                        DockLayout::tabs().panel_view(panel_handle(spaces), cx),
                        Some(px(300.)),
                    )
                    .child(
                        DockLayout::tabs().panel_view(panel_handle(agents), cx),
                        Some(px(220.)),
                    )
                    .child(DockLayout::tabs().panel_view(panel_handle(files), cx), None);
                dock.set_center(layout, window, cx);
            }
        });

        let subscription = cx.subscribe(&dock, |this, dock, event, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.schedule_save(dock, cx);
            }
        });
        Self {
            dock,
            layout_file,
            panels,
            _save: None,
            _subscription: subscription,
        }
    }

    /// Saves the layout once changes stop for [`SAVE_DEBOUNCE`]; replacing the
    /// task cancels the previous one.
    fn schedule_save(&mut self, dock: Entity<DockArea>, cx: &mut Context<Self>) {
        let Some(path) = self.layout_file.clone() else {
            return;
        };
        self._save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let state = dock.read_with(cx, |dock, cx| dock.dump(cx));
            if let Err(error) = save_layout(&path, &state) {
                tracing::warn!("sidebar: layout not saved to {}: {error}", path.display());
            }
        }));
    }
}

/// Where this herdr session's sidebar layout lives, so a dev session never
/// overwrites the real one.
fn layout_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let name = match std::env::var("HERDR_SESSION") {
        Ok(session) if !session.is_empty() => format!("sidebar-layout-{session}.json"),
        _ => "sidebar-layout.json".to_owned(),
    };
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/rusted-tools")
            .join(name),
    )
}

fn saved_layout(path: &std::path::Path) -> Option<DockAreaState> {
    let json = std::fs::read_to_string(path).ok()?;
    let state: DockAreaState = serde_json::from_str(&json)
        .inspect_err(|error| tracing::warn!("sidebar: ignoring {}: {error}", path.display()))
        .ok()?;
    (state.version == Some(LAYOUT_VERSION)).then_some(state)
}

fn save_layout(path: &std::path::Path, state: &DockAreaState) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.dock.clone())
    }
}
