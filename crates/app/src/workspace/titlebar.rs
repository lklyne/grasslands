//! The titlebar: GPUI Kit's `TitleBar` (transparent, unified with the
//! window, draggable, double-click zooms) showing the focused space's name
//! and branch, between the toggles that collapse and expand the side docks.
//! The main area's tabs (herdr's and the open files) are in the strip above
//! it; see `main_area`.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, TitleBar, h_flex};
use gpui_kit::*;

use super::Workspace;

/// Where each main-area tab and close button was drawn last frame, by index:
/// 0 is the herdr tabs, then the file tabs. Read by the script hooks to click
/// them.
#[derive(Default)]
pub struct DrawnTabs {
    pub tabs: Vec<Bounds<Pixels>>,
    pub closes: Vec<(usize, Bounds<Pixels>)>,
}

/// Window options for a window that draws [`Workspace`]'s titlebar.
pub fn window_options() -> WindowOptions {
    let mut options = TitleBar::window_options();
    if let Some(titlebar) = options.titlebar.as_mut() {
        // Shown in the Window menu and Mission Control, not drawn.
        titlebar.title = Some("grasslands".into());
    }
    options
}

impl Workspace {
    pub fn drawn_tabs(&self) -> std::cell::Ref<'_, DrawnTabs> {
        self.drawn_tabs.borrow()
    }

    pub(super) fn toggle_dock(
        &mut self,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.docks
            .update(cx, |docks, cx| docks.toggle(placement, window, cx));
        cx.notify();
    }

    /// The left dock's toggle, the focused workspace's name and branch
    /// centered in the window (on a remote machine, "— <machine>" after the
    /// name), then the right dock's toggle.
    pub(super) fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let snapshot = state.snapshot(cx);
        let machine = state.selected_machine(cx);
        let focused = snapshot
            .focused_workspace_id
            .as_ref()
            .and_then(|id| snapshot.workspace(id));
        let name = focused.map(|w| w.info.label.clone()).unwrap_or_default();
        let branch = focused.and_then(|w| {
            self.spaces
                .read(cx)
                .branch_of(machine.id(), &w.info.workspace_id, cx)
        });
        let machine = (!machine.id().is_local() && !name.is_empty())
            .then(|| format!("— {}", machine.label()));
        let theme = cx.theme();
        let title = h_flex()
            .min_w_0()
            .gap_1p5()
            .text_sm()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(name),
            )
            .children(machine.map(|machine| {
                div()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(machine)
            }))
            .children(branch.map(|branch| {
                h_flex()
                    .min_w_0()
                    .gap_1()
                    .text_color(theme.muted_foreground.opacity(0.5))
                    .child(Icon::new(assets::IconName::GitBranch).xsmall())
                    .child(div().min_w_0().truncate().child(branch))
            }));
        TitleBar::new().child(
            h_flex()
                .relative()
                .size_full()
                .pr_2()
                // Centered in the window, not between the toggles; plain
                // divs take no hits, so the bar still drags under it.
                .child(
                    h_flex()
                        .absolute()
                        .inset_0()
                        .px(px(120.))
                        .justify_center()
                        .child(title),
                )
                .children(self.dock_toggle(DockPlacement::Left, cx))
                .child(div().flex_1())
                .children(self.dock_toggle(DockPlacement::Right, cx)),
        )
    }

    /// The toggle for the dock at `placement`, if the layout has one there.
    fn dock_toggle(&self, placement: DockPlacement, cx: &mut Context<Self>) -> Option<Button> {
        let docks = self.docks.read(cx);
        if !docks.has_panels(placement, cx) {
            return None;
        }
        let open = docks.is_open(placement, cx);
        let (id, icon, side) = match placement {
            DockPlacement::Left => ("toggle-left-dock", IconName::PanelLeft, "Left"),
            _ => ("toggle-right-dock", IconName::PanelRight, "Right"),
        };
        let tooltip = format!("{} {side} Panel", if open { "Hide" } else { "Show" });
        let button = Button::new(id)
            .icon(icon)
            .small()
            .ghost()
            // Dimmed until hovered; the ghost hover color takes over then.
            .text_color(cx.theme().muted_foreground.opacity(0.5))
            .tab_stop(false)
            .tooltip(tooltip)
            .on_click(
                cx.listener(move |this, _, window, cx| this.toggle_dock(placement, window, cx)),
            );
        Some(button)
    }
}
