//! The window's GPUI Kit dock: the main area (terminals or the editor) in the
//! center, the sidebar panels in the left dock. Any sidebar panel can be
//! dragged to reorder, stacked as tabs, or dropped beside the main area. The
//! dock's own skin draws the title bars, in the theme's colors. The layout is
//! saved per herdr session and restored on launch; see
//! docs/decisions/0011-sidebar-dock.md.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::dock::{
    BasePanel, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin, Panel,
    PanelControl, PanelEvent, PanelId, panel_handle, register_panel,
};
use gpui_kit::*;

use crate::sidebar::SidebarPanels;

/// Bumped when a saved layout should no longer load.
const LAYOUT_VERSION: usize = 2;
/// A drag emits a layout change on every step; save once it settles.
pub(super) const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const SIDEBAR_WIDTH: Pixels = px(260.);

/// The main area as a dock panel: whatever the workspace shows there, with
/// no title bar of its own.
pub struct MainPanel {
    content: Option<AnyView>,
    focus: FocusHandle,
}

impl MainPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            content: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn set_content(&mut self, content: AnyView, cx: &mut Context<Self>) {
        self.content = Some(content);
        cx.notify();
    }
}

impl EventEmitter<PanelEvent> for MainPanel {}

impl Focusable for MainPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for MainPanel {
    fn panel_name(&self) -> &'static str {
        "MainPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for MainPanel {
    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }

    fn title_bar(&self, _: &App) -> bool {
        false
    }
}

impl Render for MainPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.content.clone())
    }
}

pub struct Docks {
    pub(super) area: Entity<DockArea>,
    /// Where the layout is saved; `None` keeps it in memory only.
    layout_file: Option<PathBuf>,
    /// Main, spaces, agents and files, in that order.
    pub(super) panels: [PanelId; 4],
    _save: Option<Task<()>>,
    _subscription: Subscription,
}

impl Docks {
    pub fn new(
        main: Entity<MainPanel>,
        sidebar: SidebarPanels,
        layout_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let SidebarPanels {
            spaces,
            agents,
            files,
        } = sidebar;
        let panels = [
            PanelId::from(main.entity_id()),
            PanelId::from(spaces.entity_id()),
            PanelId::from(agents.entity_id()),
            PanelId::from(files.entity_id()),
        ];
        let handles = [
            panel_handle(main),
            panel_handle(spaces),
            panel_handle(agents),
            panel_handle(files),
        ];
        // A saved layout names panels; these hand back the live ones.
        for handle in &handles {
            let name = handle.panel_name(cx);
            let handle = handle.clone();
            register_panel(cx, name, move |_, _, _| handle.clone());
        }

        let (area, _skin) = DockSkin::dock_area("main", Some(LAYOUT_VERSION), window, cx);
        area.update(cx, |area, cx| {
            let restored = layout_file
                .as_deref()
                .and_then(saved_layout)
                .is_some_and(|state| {
                    area.load(state, window, cx).is_ok()
                        && panels.iter().all(|&id| area.panel(id).is_some())
                });
            if !restored {
                let [main, spaces, agents, files] = handles;
                area.set_center(DockLayout::tabs().panel_view(main, cx), window, cx);
                let sidebar = DockLayout::v_split()
                    .child(DockLayout::tabs().panel_view(spaces, cx), Some(px(300.)))
                    .child(DockLayout::tabs().panel_view(agents, cx), Some(px(220.)))
                    .child(DockLayout::tabs().panel_view(files, cx), None);
                area.set_dock(DockPlacement::Left, sidebar, window, cx);
                area.set_dock_size(DockPlacement::Left, SIDEBAR_WIDTH, window, cx);
            }
        });

        let subscription = cx.subscribe(&area, |this, area, event, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.schedule_save(area, cx);
            }
        });
        Self {
            area,
            layout_file,
            panels,
            _save: None,
            _subscription: subscription,
        }
    }

    /// Saves the layout once changes stop for [`SAVE_DEBOUNCE`]; replacing the
    /// task cancels the previous one.
    fn schedule_save(&mut self, area: Entity<DockArea>, cx: &mut Context<Self>) {
        let Some(path) = self.layout_file.clone() else {
            return;
        };
        self._save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let state = area.read_with(cx, |area, cx| area.dump(cx));
            if let Err(error) = save_layout(&path, &state) {
                tracing::warn!("dock: layout not saved to {}: {error}", path.display());
            }
        }));
    }
}

impl Render for Docks {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.area.clone())
    }
}

/// Where this herdr session's layout lives, so a dev session never
/// overwrites the real one.
pub fn layout_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let name = match std::env::var("HERDR_SESSION") {
        Ok(session) if !session.is_empty() => format!("dock-layout-{session}.json"),
        _ => "dock-layout.json".to_owned(),
    };
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/rusted-tools")
            .join(name),
    )
}

fn saved_layout(path: &Path) -> Option<DockAreaState> {
    let json = std::fs::read_to_string(path).ok()?;
    let state: DockAreaState = serde_json::from_str(&json)
        .inspect_err(|error| tracing::warn!("dock: ignoring {}: {error}", path.display()))
        .ok()?;
    (state.version == Some(LAYOUT_VERSION)).then_some(state)
}

fn save_layout(path: &Path, state: &DockAreaState) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

#[cfg(test)]
mod tests;
