//! `RT_SCRIPT=drags`: dragging tabs in the tab bar (GPUI mouse events inside
//! the app). Checks a tab dropped on another lands in its place in herdr's
//! order (`tab.move`), that a drag released off any tab or cancelled changes
//! nothing, and that no drag is left active afterwards. The overlay is drawn
//! at each step of a drag, as its toasts and tooltips make it; after the
//! release nothing is drawn on the script's behalf, and the overlay's last
//! frame must not be one drawn mid-drag (it would still show the preview).
//!
//! ```sh
//! RT_SCRIPT=drags scripts/dev-run cargo run -p app
//! ```

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

use super::{Cx, Run, center};
use crate::overlay;
use crate::terminal_area::TerminalArea;

impl Run {
    pub(super) async fn drags(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: drags");
        let connected = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                area.snapshot(cx).generation > 0
            })
            .await;
        self.check(connected, "connected to herdr");
        if !connected {
            anyhow::bail!("no herdr");
        }

        cx.update(|window, cx| overlay::get_or_open(window, cx).map(|_| ()))??;

        // A workspace of our own with three tabs.
        let task = self.read(cx, |area, cx| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            area.state
                .read(cx)
                .new_workspace(home, Some("drags-script".to_owned()), cx)
        })?;
        let workspace_id = task.await?.workspace_id;
        let task = self.read(cx, |area, cx| {
            area.state.read(cx).focus_workspace(&workspace_id, cx)
        })?;
        task.await?;
        for _ in 0..2 {
            let task = self.read(cx, |area, cx| {
                area.state.read(cx).new_tab(Some(&workspace_id), cx)
            })?;
            task.await?;
        }
        let three = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                let snapshot = area.snapshot(cx);
                TerminalArea::active_tab(&snapshot)
                    .is_some_and(|tab| tab.info.workspace_id == workspace_id)
                    && area.drawn.borrow().tabs.len() == 3
            })
            .await;
        self.check(three, "own workspace shows three tabs");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        let start = self.tab_order(cx, &workspace_id)?;
        eprintln!("script: tabs {start:?}");
        let [a, b, c] = start.as_slice() else {
            anyhow::bail!("expected three tabs, got {start:?}");
        };

        // Rightward: the first tab dropped on the last lands last.
        self.drag_tab(cx, a, Target::Tab(c)).await?;
        let want = vec![b.clone(), c.clone(), a.clone()];
        self.check_order(cx, &workspace_id, &want, "first tab dropped on the last")
            .await;

        // Leftward: the (now) last tab dropped on the first lands first.
        self.drag_tab(cx, a, Target::Tab(b)).await?;
        let want = vec![a.clone(), b.clone(), c.clone()];
        self.check_order(cx, &workspace_id, &want, "last tab dropped on the first")
            .await;

        // Released over a terminal: no tab there, nothing moves.
        self.drag_tab(cx, a, Target::Terminal).await?;
        Self::sleep(cx, 500).await;
        self.check_order(cx, &workspace_id, &want, "a tab released off the bar")
            .await;

        // Released outside the window: GPUI cancels the drag.
        self.drag_tab(cx, a, Target::Outside).await?;
        Self::sleep(cx, 500).await;
        self.check_order(
            cx,
            &workspace_id,
            &want,
            "a tab released outside the window",
        )
        .await;

        // Cancelled mid-drag by the app (`stop_active_drag`), then released.
        self.drag_tab(cx, a, Target::Cancel).await?;
        self.check_order(cx, &workspace_id, &want, "a cancelled tab drag")
            .await;

        let task = self.read(cx, |area, cx| {
            area.state
                .read(cx)
                .close_workspace(&workspace_id, false, cx)
        })?;
        task.await?;
        Ok(())
    }

    /// Drags tab `tab_id` by its middle and releases it over `target`.
    async fn drag_tab(
        &mut self,
        cx: &mut Cx,
        tab_id: &str,
        target: Target<'_>,
    ) -> anyhow::Result<()> {
        let (from, to) = self.drawn(cx, |d| {
            let to = match target {
                Target::Tab(id) => d.tabs.get(id).map(|b| center(*b)),
                Target::Terminal => d.panes.values().next().map(|b| center(*b)),
                Target::Outside => Some(point(px(-200.), px(-200.))),
                Target::Cancel => d
                    .tabs
                    .get(tab_id)
                    .map(|b| center(*b) + point(px(0.), px(120.))),
            };
            (d.tabs.get(tab_id).map(|b| center(*b)), to)
        })?;
        let (Some(from), Some(to)) = (from, to) else {
            anyhow::bail!("tab {tab_id} or its target not drawn");
        };
        self.press(cx, from);
        Self::sleep(cx, 40).await;
        let mut dragging = false;
        const STEPS: usize = 10;
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            self.mouse(
                cx,
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: from + (to - from) * t,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
            );
            dragging |= cx.update(|_, cx| cx.has_active_drag())?;
            draw_overlay(cx);
            Self::sleep(cx, 40).await;
        }
        self.check(dragging, &format!("dragging tab {tab_id} is a GPUI drag"));
        if matches!(target, Target::Cancel) {
            cx.update(|window, cx| cx.stop_active_drag(window))?;
        }
        self.release(cx, to);
        // Frames come on their own from here: none forced.
        Self::sleep(cx, 400).await;
        self.check_no_drag(cx, &format!("dropping tab {tab_id}"));
        Ok(())
    }

    fn check_no_drag(&mut self, cx: &mut Cx, what: &str) {
        let (active, overlay_preview) = cx
            .update(|window, cx| {
                let drew = overlay::get(window, cx).is_some_and(|o| o.read(cx).drew_drag(cx));
                (cx.has_active_drag(), drew)
            })
            .unwrap_or((true, true));
        self.check(!active, &format!("{what}: no drag left active"));
        self.check(
            !overlay_preview,
            &format!("{what}: the overlay's last frame shows no drag preview"),
        );
    }

    fn tab_order(&self, cx: &mut Cx, workspace_id: &str) -> anyhow::Result<Vec<String>> {
        self.read(cx, |area, cx| {
            area.snapshot(cx)
                .workspace(workspace_id)
                .map(|w| w.tabs.iter().map(|t| t.info.tab_id.clone()).collect())
                .unwrap_or_default()
        })
    }

    async fn check_order(&mut self, cx: &mut Cx, workspace_id: &str, want: &[String], what: &str) {
        let workspace_id = workspace_id.to_owned();
        let matched = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx)
                    .workspace(&workspace_id)
                    .is_some_and(|w| w.tabs.iter().map(|t| &t.info.tab_id).eq(want.iter()))
            })
            .await;
        let got = self.tab_order(cx, &workspace_id).unwrap_or_default();
        self.check(
            matched,
            &format!("{what}: herdr order {got:?}, want {want:?}"),
        );
    }
}

#[derive(Clone, Copy)]
enum Target<'a> {
    Tab(&'a str),
    Terminal,
    Outside,
    /// Away from the bar, where the app stops the drag before the release.
    Cancel,
}

/// Draws the overlay window now, as a toast or tooltip changing would.
fn draw_overlay(cx: &mut Cx) {
    let handle = cx
        .update(|window, cx| overlay::get(window, cx).map(|o| o.read(cx).window_handle()))
        .ok()
        .flatten();
    if let Some(handle) = handle {
        cx.update_window(handle, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .ok();
    }
}
