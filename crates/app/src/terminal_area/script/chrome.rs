//! `RT_SCRIPT=chrome`: chrome's floating UI over real terminals. Right-clicks
//! on a pane header and on a tab (GPUI mouse events, as the app gets them),
//! the terminal area's close confirmation, an agent toast and the palette,
//! each checked to be in the overlay window and screenshotted with it.
//!
//! ```sh
//! RT_SCRIPT=chrome scripts/dev-run cargo run -p app
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use serde_json::json;

use super::{Cx, Run};
use crate::actions::CommandPalette;
use crate::herdr::{SplitDirection, SplitTree};
use crate::overlay;
use crate::terminal_area::{HEADER_HEIGHT, TerminalArea};

impl Run {
    pub(super) async fn chrome(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: chrome");
        let connected = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                area.snapshot(cx).generation > 0
            })
            .await;
        self.check(connected, "connected to herdr");
        if !connected {
            anyhow::bail!("no herdr");
        }

        // A workspace of our own with two terminals.
        let task = self.read(cx, |area, cx| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            area.state
                .read(cx)
                .new_workspace(home, Some("chrome-script".to_owned()), cx)
        })?;
        let workspace_id = task.await?.workspace_id;
        let task = self.read(cx, |area, cx| {
            area.state.read(cx).focus_workspace(&workspace_id, cx)
        })?;
        task.await?;
        let ours = {
            let workspace_id = workspace_id.clone();
            move |area: &TerminalArea, cx: &App| {
                TerminalArea::active_tab(&area.snapshot(cx))
                    .is_some_and(|tab| tab.info.workspace_id == workspace_id)
            }
        };
        let shown = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                ours(area, cx) && area.drawn.borrow().panes.len() == 1
            })
            .await;
        self.check(shown, "own workspace shows one terminal");
        let task = self.read(cx, |area, cx| {
            area.state
                .read(cx)
                .split_pane(None, SplitDirection::Right, cx)
        })?;
        task.await?;
        let two = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn.borrow().panes.len() == 2
            })
            .await;
        self.check(two, "split into two terminals");
        let Some((tab_id, tree)) = self.tab(cx)? else {
            anyhow::bail!("no tab");
        };
        let panes: Vec<String> = match &tree {
            SplitTree::Split { .. } => tree.pane_ids().into_iter().map(str::to_owned).collect(),
            SplitTree::Pane(pane) => vec![pane.clone()],
        };
        let [first, second] = panes.as_slice() else {
            anyhow::bail!("expected two panes, got {panes:?}");
        };
        let (first, second) = (first.clone(), second.clone());

        // An agent on the first pane, so closing it needs a confirmation.
        self.report_agent(cx, &first, "working").await?;
        let agent = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).agents.iter().any(|a| a.pane_id == first)
            })
            .await;
        self.check(agent, "agent reported on the first pane");
        Self::sleep(cx, 800).await;
        Self::settle(cx).await;

        // Right-click the first pane's header: the pane menu, in the overlay.
        let body = self.read(cx, |area, _| area.drawn.borrow().panes.get(&first).copied())?;
        let Some(body) = body else {
            anyhow::bail!("first pane not drawn");
        };
        let header = point(body.origin.x + px(60.), body.origin.y - HEADER_HEIGHT / 2.);
        self.right_click(cx, header);
        let open = self.until_chrome(cx, |menu, _| menu == Some(true)).await;
        self.check(
            open,
            "right-click on a pane header opens the pane menu in the overlay",
        );
        let modal = self.overlay_capturing(cx);
        self.check(
            modal,
            "the open menu makes the overlay take clicks and keys",
        );
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "pane-menu-over-terminal");
        self.overlay_action(cx, Box::new(Cancel));
        let closed = self.until_chrome(cx, |menu, _| menu.is_none()).await;
        self.check(closed, "Escape closes the menu");
        Self::sleep(cx, 300).await;
        let released = !self.overlay_capturing(cx);
        self.check(
            released,
            "with the menu closed the overlay lets clicks through",
        );

        // Right-click the tab: the tab menu.
        let tab = self.read(cx, |area, _| area.drawn.borrow().tabs.get(&tab_id).copied())?;
        if let Some(tab) = tab {
            self.right_click(cx, tab.center());
            let open = self.until_chrome(cx, |menu, _| menu == Some(true)).await;
            self.check(
                open,
                "right-click on a tab opens the tab menu in the overlay",
            );
            Self::settle(cx).await;
            self.shoot_with_overlay(cx, "tab-menu");
            self.overlay_action(cx, Box::new(Cancel));
            let closed = self.until_chrome(cx, |menu, _| menu.is_none()).await;
            self.check(closed, "Escape closes the tab menu");
        } else {
            self.check(false, "tab drawn");
        }

        // The terminal area's close confirmation is chrome's, in the overlay.
        cx.update(|window, cx| {
            self.area.update(cx, |area, cx| {
                area.close_pane_confirmed(first.clone(), window, cx);
            });
        })
        .ok();
        let asked = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(
            asked,
            "closing a pane with a running agent asks in the overlay",
        );
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "close-confirm-over-terminal");
        self.overlay_action(cx, Box::new(Confirm { secondary: false }));
        let gone = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).pane(&first).is_none()
            })
            .await;
        self.check(gone, "confirming closes the pane in herdr");

        // An agent turning blocked raises a toast in the overlay.
        self.report_agent(cx, &second, "working").await?;
        let working = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).agents.iter().any(|a| a.pane_id == second)
            })
            .await;
        self.check(working, "agent reported on the second pane");
        self.report_agent(cx, &second, "blocked").await?;
        let toast = self
            .until_overlay(cx, |window, cx| !window.notifications(cx).is_empty())
            .await;
        self.check(toast, "a blocked agent raises a toast in the overlay");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "toast-over-terminal");

        // The palette opens in the overlay.
        self.dispatch(cx, Box::new(CommandPalette));
        let palette = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(palette, "the command palette opens in the overlay");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "palette-over-terminal");
        self.overlay_action(cx, Box::new(Cancel));
        let shut = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(shut, "Escape closes the palette");

        let task = self.read(cx, |area, cx| {
            area.state
                .read(cx)
                .close_workspace(&workspace_id, false, cx)
        })?;
        task.await?;
        Ok(())
    }

    async fn report_agent(&self, cx: &mut Cx, pane_id: &str, status: &str) -> anyhow::Result<()> {
        let params = json!({
            "pane_id": pane_id,
            "source": "chrome-script",
            "agent": "claude",
            "state": status,
        });
        let task = self.read(cx, |area, cx| {
            area.state
                .read(cx)
                .call(cx, move |client| client.call("pane.report_agent", &params))
        })?;
        task.await?;
        Ok(())
    }

    fn right_click(&self, cx: &mut Cx, position: Point<Pixels>) {
        self.mouse(
            cx,
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
        );
        self.mouse(
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Right,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
        );
        self.mouse(
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Right,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
        );
    }

    fn overlay_handle(cx: &mut Cx) -> Option<AnyWindowHandle> {
        cx.update(|window, cx| overlay::get(window, cx).map(|o| o.read(cx).window_handle()))
            .ok()
            .flatten()
    }

    fn overlay_capturing(&self, cx: &mut Cx) -> bool {
        cx.update(|window, cx| overlay::get(window, cx).is_some_and(|o| o.read(cx).is_capturing()))
            .unwrap_or(false)
    }

    /// Dispatches `action` in the overlay window (where the menu or dialog
    /// has the keyboard), like a key press there.
    fn overlay_action(&self, cx: &mut Cx, action: Box<dyn Action>) {
        if let Some(handle) = Self::overlay_handle(cx) {
            cx.update_window(handle, |_, window, cx| window.dispatch_action(action, cx))
                .ok();
        }
    }

    /// Polls a check on the chrome's menu state (`Chrome::menu_open`).
    async fn until_chrome(
        &self,
        cx: &mut Cx,
        mut test: impl FnMut(Option<bool>, &App) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            Self::settle(cx).await;
            let done = cx
                .update(|window, cx| {
                    let menu = crate::chrome::get(window, cx).and_then(|c| c.read(cx).menu_open());
                    test(menu, cx)
                })
                .unwrap_or(false);
            if done {
                return true;
            }
            if start.elapsed() > Duration::from_secs(5) {
                return false;
            }
            Self::sleep(cx, 100).await;
        }
    }

    /// Polls a check run inside the overlay window.
    async fn until_overlay(
        &self,
        cx: &mut Cx,
        mut test: impl FnMut(&mut Window, &mut App) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            Self::settle(cx).await;
            let done = Self::overlay_handle(cx).is_some_and(|handle| {
                cx.update_window(handle, |_, window, cx| test(window, cx))
                    .unwrap_or(false)
            });
            if done {
                return true;
            }
            if start.elapsed() > Duration::from_secs(5) {
                return false;
            }
            Self::sleep(cx, 100).await;
        }
    }
}
