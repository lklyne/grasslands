//! `RT_SCRIPT=terminals`: the terminal area against the dev herdr session.
//! Typing, a file drop, splits, a key binding over a focused terminal,
//! divider drags (views checked against their slots on every step), tabs,
//! pane moves by command and by header drag, zoom, a takeover by another
//! client and Reattach, overlay capture, theme reload and a real server
//! restart.
//!
//! ```sh
//! RT_SCRIPT=terminals scripts/dev-run cargo run -p app
//! ```
//!
//! It creates its own workspace (`script`), works only inside it, and
//! closes it at the end.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

use super::{Cx, Run, center, shell_quote};
use crate::herdr::{PaneId, SplitDirection, SplitTree, TabId, TerminalId};
use crate::overlay::{self, Capture};
use crate::state::MachineId;
use crate::terminal_area::hooks::{self, GAP, Hooks, neighbor};
use crate::terminal_area::{ClosePane, NewTab, NextTab, SplitDown, SplitRight};

impl Run {
    pub(super) async fn terminals(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: terminals");
        let connected = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                area.snapshot(cx).generation > 0
            })
            .await;
        self.check(connected, "connected to herdr");
        if !connected {
            anyhow::bail!("no herdr");
        }

        // A workspace of our own, focused, with one pane.
        let task = self.read(cx, |area, cx| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            area.state().read(cx).new_workspace(
                &area.machine(cx),
                home,
                Some("script".to_owned()),
                cx,
            )
        })?;
        let workspace = task.await?;
        let workspace_id = workspace.workspace_id.clone();
        let ours = |area: &Hooks, cx: &App| {
            let snapshot = area.snapshot(cx);
            Hooks::active_tab(&snapshot).is_some_and(|tab| tab.info.workspace_id == workspace_id)
        };
        let task = self.on_state(cx, |state, machine, cx| {
            state.focus_workspace(machine, &workspace_id, cx)
        })?;
        task.await?;
        let shown = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                ours(area, cx) && area.drawn().panes.len() == 1
            })
            .await;
        self.check(shown, "new workspace shows one pane");
        Self::sleep(cx, 800).await;
        Self::settle(cx).await;
        self.check_views(cx, "one pane");
        self.shoot(cx, "one-pane");

        // Typing reaches the shell through the Ghostty view.
        let Some((_, SplitTree::Pane(first))) = self.tab(cx)? else {
            anyhow::bail!("expected a single pane");
        };
        cx.update(|window, cx| {
            self.area.update(cx, |area, cx| {
                let machine = area.hooks().machine(cx);
                area.focus_pane(&machine, &first, window, cx);
            });
        })
        .ok();
        Self::settle(cx).await;
        self.type_text(cx, "echo typed-$((6*7))\n");
        let mut typed = false;
        for _ in 0..30 {
            Self::sleep(cx, 150).await;
            if self.read_pane_text(cx, &first).await.contains("typed-42") {
                typed = true;
                break;
            }
        }
        self.check(
            typed,
            "keys typed in the app reach the pane (herdr reads typed-42)",
        );

        // A file dropped from Finder pastes its shell-escaped path.
        self.type_text(cx, "echo dropped:");
        let center = self.read(cx, |area, _| {
            area.drawn().panes.get(&first).map(|b| b.center())
        })?;
        if let Some(position) = center {
            let paths = ExternalPaths(vec![PathBuf::from("/tmp/drop me (1).png")].into());
            self.mouse(
                cx,
                FileDropEvent::Entered { position, paths }.to_platform_input(),
            );
            Self::settle(cx).await;
            self.mouse(cx, FileDropEvent::Pending { position }.to_platform_input());
            Self::settle(cx).await;
            self.mouse(cx, FileDropEvent::Submit { position }.to_platform_input());
        }
        Self::settle(cx).await;
        self.type_text(cx, "\n");
        let mut dropped = false;
        for _ in 0..30 {
            Self::sleep(cx, 150).await;
            if self
                .read_pane_text(cx, &first)
                .await
                .contains("dropped:/tmp/drop me (1).png")
            {
                dropped = true;
                break;
            }
        }
        self.check(dropped, "a dropped file pastes its escaped path");

        // Split right and down through the app's own actions.
        self.dispatch(cx, Box::new(SplitRight));
        let two = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn().panes.len() == 2
            })
            .await;
        self.check(two, "cmd-d action splits right (2 panes)");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "after split right");
        self.dispatch(cx, Box::new(SplitDown));
        let three = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn().panes.len() == 3
            })
            .await;
        self.check(three, "split down (3 panes)");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "after split down");
        self.shoot(cx, "three-panes");

        // A key binding while a terminal has focus: the app's resize shortcut
        // must win over the terminal.
        let Some((tab_id, tree)) = self.tab(cx)? else {
            anyhow::bail!("lost the tab");
        };
        let root_ratio = match &tree {
            SplitTree::Split { ratio, .. } => *ratio,
            SplitTree::Pane(_) => 0.5,
        };
        let terminal_focused = cx
            .update(|window, cx| {
                self.area
                    .read(cx)
                    .hooks()
                    .terminals()
                    .focused(window, cx)
                    .is_some()
            })
            .unwrap_or(false);
        self.check(terminal_focused, "a terminal has keyboard focus");
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-ctrl-left").unwrap_or_default(), cx);
        })
        .ok();
        let resized = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx)
                    .tab(&tab_id)
                    .and_then(|t| t.root.as_ref())
                    .is_some_and(|root| match root {
                        SplitTree::Split { ratio, .. } => (ratio - root_ratio).abs() > 0.01,
                        SplitTree::Pane(_) => false,
                    })
            })
            .await;
        self.check(
            resized,
            "cmd-ctrl-left over a focused terminal moves a divider in herdr",
        );

        // Drag the root divider with in-app mouse events; check the views
        // follow on every step, then that herdr got the ratio on release.
        let root_split = (tab_id.clone(), Vec::new());
        let split = self.read(cx, |area, _| area.drawn().splits.get(&root_split).copied())?;
        if let Some(bounds) = split {
            let ratio_now = self.read(cx, |area, cx| {
                area.with_overrides(
                    &tab_id,
                    area.snapshot(cx)
                        .tab(&tab_id)
                        .and_then(|t| t.root.as_ref())
                        .unwrap_or(&SplitTree::Pane("".into())),
                )
            })?;
            let ratio_now = match ratio_now {
                SplitTree::Split { ratio, .. } => ratio,
                SplitTree::Pane(_) => 0.5,
            };
            let x = f32::from(bounds.origin.x)
                + f32::from(bounds.size.width) * ratio_now
                + f32::from(GAP) * (0.5 - ratio_now);
            let y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.;
            let start = point(px(x), px(y));
            self.mouse(
                cx,
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: start,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
            );
            Self::settle(cx).await;
            self.mouse(
                cx,
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: start,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
            );
            let mut lockstep = true;
            let target = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.3;
            for step in 1..=10 {
                let t = step as f32 / 10.;
                let position = point(px(x + (target - x) * t), px(y));
                self.mouse(
                    cx,
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    }),
                );
                Self::settle(cx).await;
                if let Err(error) = self.views_match_layout(cx) {
                    lockstep = false;
                    eprintln!("     step {step}: {error}");
                }
                if step == 5 {
                    self.shoot(cx, "mid-drag");
                }
            }
            self.check(lockstep, "views stay on their slots on every drag step");
            let local = self.read(cx, |area, _| area.override_ratio(&tab_id, &[]))?;
            self.check(
                local.is_some_and(|r| (r - 0.3).abs() < 0.03),
                &format!("divider follows the pointer locally (ratio {local:?})"),
            );
            self.mouse(
                cx,
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: point(px(target), px(y)),
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
            );
            let committed = self
                .until(cx, Duration::from_secs(5), |area, _, cx| {
                    area.snapshot(cx)
                        .tab(&tab_id)
                        .and_then(|t| t.root.as_ref())
                        .is_some_and(|root| match root {
                            SplitTree::Split { ratio, .. } => (ratio - 0.3).abs() < 0.03,
                            SplitTree::Pane(_) => false,
                        })
                })
                .await;
            self.check(
                committed,
                "release sends layout.set_split_ratio; herdr has ~0.30",
            );
            self.shoot(cx, "after-drag");
        } else {
            self.fail("root split was not drawn");
        }

        // A second tab: the first tab's views hide, the new one shows.
        let before: HashSet<PaneId> =
            self.read(cx, |area, _| area.drawn().panes.keys().cloned().collect())?;
        self.dispatch(cx, Box::new(NewTab));
        let switched = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                let panes = area.drawn();
                panes.panes.len() == 1 && !panes.panes.keys().any(|p| before.contains(p))
            })
            .await;
        self.check(switched, "cmd-t opens a new tab with one pane");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "new tab: other tab's views hidden");
        self.shoot(cx, "second-tab");
        self.tab_switch_focus(cx, &tab_id).await?;

        // Move the new tab's pane into the first tab: same terminal, same view.
        let Some((_, SplitTree::Pane(moving))) = self.tab(cx)? else {
            anyhow::bail!("expected one pane in the new tab");
        };
        let (terminal_id, view_before) = self.read(cx, |area, cx| {
            let snapshot = area.snapshot(cx);
            let terminal_id = snapshot.pane(&moving).map(|p| p.terminal_id.clone());
            let view = terminal_id
                .as_ref()
                .and_then(|id| area.terminals().get(id))
                .map(|v| v.terminal.entity_id());
            (terminal_id, view)
        })?;
        let task = self.read(cx, |area, cx| {
            area.state().read(cx).move_pane(
                &area.machine(cx),
                &moving,
                crate::herdr::PaneMoveDestination::Tab {
                    tab_id: tab_id.clone(),
                    split: SplitDirection::Right,
                    target_pane_id: None,
                    ratio: None,
                },
                cx,
            )
        })?;
        let moved = task.await;
        self.check(
            moved.is_ok(),
            &format!("pane.move to the first tab: {moved:?}"),
        );
        let kept = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                let snapshot = area.snapshot(cx);
                let in_first = snapshot
                    .panes
                    .values()
                    .any(|p| Some(&p.terminal_id) == terminal_id.as_ref() && p.tab_id == tab_id);
                let view = terminal_id
                    .as_ref()
                    .and_then(|id| area.terminals().get(id))
                    .map(|v| v.terminal.entity_id());
                in_first && view.is_some() && view == view_before
            })
            .await;
        self.check(kept, "moved pane keeps its Ghostty view (no restart)");
        let four = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn().panes.len() == 4
            })
            .await;
        self.check(four, "first tab now shows 4 panes");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "after move");

        // Close one pane with the app's shortcut; its view goes away.
        let (count_before, closing_terminal) = self.read(cx, |area, cx| {
            let snapshot = area.snapshot(cx);
            let terminal = area
                .target_pane(cx)
                .and_then(|p| snapshot.pane(&p).map(|p| p.terminal_id.clone()));
            (area.terminals().len(), terminal)
        })?;
        self.dispatch(cx, Box::new(ClosePane));
        let closed = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.terminals().len() + 1 == count_before
                    && closing_terminal
                        .as_ref()
                        .is_some_and(|id| area.terminals().get(id).is_none())
            })
            .await;
        self.check(closed, "cmd-w closes the focused pane and drops its view");
        Self::sleep(cx, 400).await;
        Self::settle(cx).await;
        self.check_views(cx, "after close");

        self.focus_moves(cx).await?;
        self.header_drags(cx, &tab_id).await?;
        self.zoom(cx, &tab_id).await?;
        self.detach_and_reattach(cx).await?;

        // Overlay: a passive toast takes the pointer only over itself; a
        // modal layer takes everything.
        let overlay = cx.update(overlay::get_or_open)??;
        let toast = cx.new(|_| ScriptToast);
        let toast_id = overlay.update(cx, |overlay, cx| {
            overlay.push_layer(toast, Capture::Passive, cx)
        });
        Self::settle(cx).await;
        Self::sleep(cx, 300).await;
        Self::settle(cx).await;
        // The toast sits over the terminal area's right edge.
        let on_toast = cx.update(|window, _| {
            let viewport = window.viewport_size();
            point(viewport.width - px(150.), px(170.))
        })?;
        cx.update(|window, cx| overlay::track_pointer(on_toast, window, cx))
            .ok();
        let over = overlay.read_with(cx, |overlay, _| overlay.is_capturing());
        self.check(over, "overlay takes the pointer over its toast");
        cx.update(|window, cx| overlay::track_pointer(point(px(500.), px(500.)), window, cx))
            .ok();
        let away = overlay.read_with(cx, |overlay, _| overlay.is_capturing());
        self.check(!away, "overlay lets the pointer through elsewhere");
        self.shoot_with_overlay(cx, "overlay-toast");
        let modal = cx.new(|_| ScriptToast);
        let modal_id = overlay.update(cx, |overlay, cx| {
            overlay.push_layer(modal, Capture::Modal, cx)
        });
        let modal_on = overlay.read_with(cx, |overlay, _| overlay.is_capturing());
        Self::sleep(cx, 300).await;
        // Key status needs the app to be the active app; while the human uses
        // another app no window can be key, so these checks are skipped.
        let frontmost = overlay::app_is_active();
        if frontmost {
            let overlay_key = overlay.update(cx, |overlay, cx| {
                overlay.update_window(cx, |window, _| window.is_window_active())
            });
            self.check(
                overlay_key == Some(true),
                "a modal layer makes the overlay the key window (keyboard for menus/palette)",
            );
        } else {
            eprintln!("skip key-window checks: the app is not frontmost");
        }
        self.check(modal_on, "a modal layer makes the overlay take every click");
        overlay.update(cx, |overlay, cx| {
            overlay.remove_layer(modal_id, cx);
            overlay.remove_layer(toast_id, cx);
        });
        Self::sleep(cx, 400).await;
        Self::settle(cx).await;
        cx.update(|window, cx| overlay::track_pointer(on_toast, window, cx))
            .ok();
        let off = overlay.read_with(cx, |overlay, _| overlay.is_capturing());
        self.check(!off, "overlay is click-through again once layers close");
        if frontmost {
            let key = cx
                .update(|window, _| window.is_window_active())
                .unwrap_or(false);
            self.check(key, "main window is key again after the modal layer closed");
        }

        self.overlay_follows_resize(cx, &overlay).await;

        self.theme_reload(cx).await?;
        self.server_restart(cx, &workspace.label).await?;

        // Clean up: close the script's workspace.
        let task = self.read(cx, |area, cx| {
            area.state().read(cx).close_workspace(
                &area.machine(cx),
                &workspace.workspace_id,
                false,
                cx,
            )
        })?;
        let closed = task.await;
        self.check(closed.is_ok(), "closed the script workspace");
        Ok(())
    }
}

impl Run {
    /// cmd-alt-arrows move herdr's focus to the pane next to the focused one.
    async fn focus_moves(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        // Start from the leftmost pane.
        let leftmost = self.drawn(cx, |d| {
            d.panes
                .iter()
                .min_by(|a, b| f32::from(a.1.origin.x).total_cmp(&f32::from(b.1.origin.x)))
                .map(|(id, _)| id.clone())
        })?;
        let Some(leftmost) = leftmost else {
            self.fail("focus: no panes drawn");
            return Ok(());
        };
        cx.update(|window, cx| {
            self.area.update(cx, |area, cx| {
                let machine = area.hooks().machine(cx);
                area.focus_pane(&machine, &leftmost, window, cx);
            });
        })
        .ok();
        let on_left = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).focused_pane_id.as_deref() == Some(leftmost.as_str())
            })
            .await;
        self.check(on_left, "focus starts on the leftmost pane");
        let expected = self.drawn(cx, |d| {
            neighbor(&leftmost, crate::herdr::PaneDirection::Right, &d.panes).cloned()
        })?;
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-alt-right").unwrap_or_default(), cx);
        })
        .ok();
        let moved = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                expected.is_some() && area.snapshot(cx).focused_pane_id == expected
            })
            .await;
        self.check(
            moved,
            &format!("cmd-alt-right moves herdr's focus {leftmost} -> {expected:?}"),
        );
        let terminal_follows = cx
            .update(|window, cx| {
                let area = self.area.read(cx).hooks();
                let snapshot = area.snapshot(cx);
                let want = expected
                    .as_deref()
                    .and_then(|p| snapshot.pane(p))
                    .map(|p| p.terminal_id.clone());
                area.terminals().focused(window, cx).cloned() == want
            })
            .unwrap_or(false);
        self.check(
            terminal_follows,
            "keyboard focus moved to that pane's terminal",
        );
        self.shoot(cx, "focus-moved");
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-alt-left").unwrap_or_default(), cx);
        })
        .ok();
        let back = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).focused_pane_id.as_deref() == Some(leftmost.as_str())
            })
            .await;
        self.check(back, "cmd-alt-left moves it back");
        Ok(())
    }

    /// Switching tabs moves keyboard focus to the new tab's terminal, so
    /// typing goes there without a click.
    async fn tab_switch_focus(&mut self, cx: &mut Cx, first_tab: &TabId) -> anyhow::Result<()> {
        let typing_in_active_tab = |area: &Hooks, window: &Window, cx: &App| {
            let snapshot = area.snapshot(cx);
            let want = area
                .target_pane(cx)
                .and_then(|p| snapshot.pane(&p).map(|p| p.terminal_id.clone()));
            want.is_some() && area.terminals().focused(window, cx).cloned() == want
        };
        let on_new = self
            .until(cx, Duration::from_secs(5), typing_in_active_tab)
            .await;
        self.check(on_new, "a new tab takes keyboard focus");

        let second_tab = self.tab(cx)?.map(|(id, _)| id);
        let first_strip = self.drawn(cx, |d| d.tabs.get(first_tab).copied())?;
        let Some(first_strip) = first_strip else {
            self.fail("tab switch: first tab not drawn in the strip");
            return Ok(());
        };
        self.click(cx, center(first_strip)).await;
        let clicked = self
            .until(cx, Duration::from_secs(5), |area, window, cx| {
                Hooks::active_tab(&area.snapshot(cx)).is_some_and(|t| &t.info.tab_id == first_tab)
                    && typing_in_active_tab(area, window, cx)
            })
            .await;
        self.check(clicked, "clicking a tab focuses its terminal");

        self.dispatch(cx, Box::new(NextTab));
        let cycled = self
            .until(cx, Duration::from_secs(5), |area, window, cx| {
                Hooks::active_tab(&area.snapshot(cx)).map(|t| &t.info.tab_id) == second_tab.as_ref()
                    && typing_in_active_tab(area, window, cx)
            })
            .await;
        self.check(cycled, "cmd-shift-] focuses the next tab's terminal");
        Ok(())
    }

    /// Dragging a pane header: onto another pane swaps them, onto a tab moves
    /// the pane there. The preview is drawn in the overlay, above terminals.
    async fn header_drags(&mut self, cx: &mut Cx, first_tab: &TabId) -> anyhow::Result<()> {
        let Some((_, tree)) = self.tab(cx)? else {
            anyhow::bail!("no tab for header drags");
        };
        let order_before: Vec<PaneId> = tree.pane_ids().into_iter().map(PaneId::from).collect();
        let (Some(a), Some(b)) = (order_before.first().cloned(), order_before.last().cloned())
        else {
            anyhow::bail!("need two panes to swap");
        };
        let (from, to) = self.drawn(cx, |d| {
            (d.headers.get(&a).copied(), d.headers.get(&b).copied())
        })?;
        let (Some(from), Some(to)) = (from, to) else {
            self.fail("swap: headers were not drawn");
            return Ok(());
        };
        let mut chip_shown = false;
        self.drag(
            cx,
            center(from),
            center(to) + point(px(0.), px(40.)),
            12,
            |run, cx| {
                chip_shown = run
                    .read(cx, |area, _| area.pane_dragging())
                    .unwrap_or(false);
                run.shoot_with_overlay(cx, "dragging-header-over-terminal");
            },
        )
        .await;
        self.check(
            chip_shown,
            "the drag preview is an overlay layer while dragging",
        );
        let gone = self.read(cx, |area, _| !area.pane_dragging())?;
        self.check(gone, "the drag preview is removed on drop");
        let swapped = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                let snapshot = area.snapshot(cx);
                let order: Vec<&str> = snapshot
                    .tab(first_tab)
                    .and_then(|t| t.root.as_ref())
                    .map(|root| root.pane_ids())
                    .unwrap_or_default();
                order.first() == Some(&b.as_str()) && order.last() == Some(&a.as_str())
            })
            .await;
        self.check(
            swapped,
            &format!("dropping {a}'s header on {b} swaps them (pane.swap)"),
        );
        Self::sleep(cx, 400).await;
        Self::settle(cx).await;
        self.check_views(cx, "after swap");
        self.shoot(cx, "after-swap");

        // A second tab, then drag its pane's header onto the first tab.
        self.dispatch(cx, Box::new(NewTab));
        let second = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                let snapshot = area.snapshot(cx);
                Hooks::active_tab(&snapshot).is_some_and(|t| &t.info.tab_id != first_tab)
                    && area.drawn().panes.len() == 1
            })
            .await;
        self.check(second, "a second tab to move from");
        Self::sleep(cx, 500).await;
        Self::settle(cx).await;
        let (moving, header, tab) = self.drawn(cx, |d| {
            let pane = d.panes.keys().next().cloned();
            let header = pane.as_ref().and_then(|p| d.headers.get(p).copied());
            (pane, header, d.tabs.get(first_tab).copied())
        })?;
        let (Some(moving), Some(header), Some(tab)) = (moving, header, tab) else {
            self.fail("move: header or tab not drawn");
            return Ok(());
        };
        let terminal = self.read(cx, |area, cx| {
            area.snapshot(cx)
                .pane(&moving)
                .map(|p| p.terminal_id.clone())
        })?;
        let view_before = self.read(cx, |area, _| {
            terminal
                .as_ref()
                .and_then(|t| area.terminals().get(t))
                .map(|v| v.terminal.entity_id())
        })?;
        self.drag(cx, center(header), center(tab), 10, |_, _| {})
            .await;
        let moved = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                let snapshot = area.snapshot(cx);
                let in_first = snapshot
                    .panes
                    .values()
                    .any(|p| Some(&p.terminal_id) == terminal.as_ref() && &p.tab_id == first_tab);
                let view = terminal
                    .as_ref()
                    .and_then(|t| area.terminals().get(t))
                    .map(|v| v.terminal.entity_id());
                in_first && view.is_some() && view == view_before
            })
            .await;
        self.check(
            moved,
            "dropping a header on another tab moves the pane there (pane.move), same view",
        );
        // Show the first tab again if herdr stayed on the (now gone) second.
        let task = self.on_state(cx, |state, machine, cx| {
            state.focus_tab(machine, first_tab, cx)
        })?;
        task.await.ok();
        let four = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn().panes.len() == 4
            })
            .await;
        self.check(four, "the first tab shows the moved pane (4 panes)");
        Self::sleep(cx, 400).await;
        Self::settle(cx).await;
        self.check_views(cx, "after move by drag");
        self.shoot(cx, "after-move-by-drag");
        Ok(())
    }

    /// cmd-shift-enter zooms the focused pane: only it is shown, full size.
    async fn zoom(&mut self, cx: &mut Cx, tab_id: &TabId) -> anyhow::Result<()> {
        let panes = self.drawn(cx, |d| d.panes.len())?;
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-shift-enter").unwrap_or_default(), cx);
        })
        .ok();
        let zoomed = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).tab(tab_id).is_some_and(|t| t.zoomed)
                    && area.drawn().panes.len() == 1
            })
            .await;
        self.check(zoomed, "cmd-shift-enter zooms the focused pane (pane.zoom)");
        Self::sleep(cx, 400).await;
        Self::settle(cx).await;
        self.check_views(cx, "zoomed: the other views are hidden");
        self.shoot(cx, "zoomed");
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-shift-enter").unwrap_or_default(), cx);
        })
        .ok();
        let unzoomed = self
            .until(cx, Duration::from_secs(5), |area, _, _| {
                area.drawn().panes.len() == panes
            })
            .await;
        self.check(unzoomed, "cmd-shift-enter again restores the split");
        Self::sleep(cx, 400).await;
        Self::settle(cx).await;
        self.check_views(cx, "unzoomed");
        Ok(())
    }

    /// Another client takes a terminal over: the pane shows Detached with a
    /// Reattach button; clicking it takes the terminal back.
    async fn detach_and_reattach(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        let (pane, terminal, config) = self.read(cx, |area, cx| {
            let snapshot = area.snapshot(cx);
            let pane = area.drawn().panes.keys().next().cloned();
            let terminal = pane
                .as_deref()
                .and_then(|p| snapshot.pane(p))
                .map(|p| p.terminal_id.clone());
            (pane, terminal, area.state().read(cx).herdr_config.clone())
        })?;
        let (Some(pane), Some(terminal), Some(config)) = (pane, terminal, config) else {
            self.fail("detach: no pane or herdr config");
            return Ok(());
        };
        let Some(session) = config.session.clone() else {
            self.fail("detach needs HERDR_SESSION");
            return Ok(());
        };
        // `script` gives the other client a pty; herdr refuses a zero-sized
        // one, hence the stty. Killing `script` closes the pty, which ends
        // the attach.
        let attach = format!(
            "stty rows 24 cols 80; exec {} --session {} terminal attach {} --takeover",
            shell_quote(&config.bin.to_string_lossy()),
            shell_quote(&session),
            shell_quote(&terminal),
        );
        let other = std::process::Command::new("/usr/bin/script")
            .args(["-q", "/dev/null", "sh", "-c", &attach])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let mut other = match other {
            Ok(child) => child,
            Err(error) => {
                self.fail(&format!("could not start another client: {error}"));
                return Ok(());
            }
        };
        let detached = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                !area.terminals().is_attached(&terminal, cx)
                    && area.drawn().reattach.contains_key(&pane)
            })
            .await;
        self.check(
            detached,
            "a takeover by another client shows the pane as Detached",
        );
        Self::settle(cx).await;
        self.check_views(cx, "detached: its view is hidden");
        self.shoot(cx, "detached");

        // Attaching on its own (as after a reconnect), the app leaves the
        // terminal with the other client: herdr refuses a plain attach.
        cx.update(|window, cx| {
            self.area.update(cx, |area, cx| {
                hooks::reattach_machine(area, &MachineId::Local, window, cx);
            });
        })
        .ok();
        Self::sleep(cx, 2000).await;
        let still_detached =
            self.read(cx, |area, cx| !area.terminals().is_attached(&terminal, cx))?;
        let other_kept = matches!(other.try_wait(), Ok(None));
        self.check(
            still_detached && other_kept,
            "an automatic re-attach doesn't take the terminal back",
        );
        let _ = other.kill();
        let _ = other.wait();

        let button = self.drawn(cx, |d| d.reattach.get(&pane).copied())?;
        let Some(button) = button else {
            self.fail("reattach: button not drawn");
            return Ok(());
        };
        self.click(cx, center(button)).await;
        let back = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                area.terminals().is_attached(&terminal, cx)
                    && !area.drawn().reattach.contains_key(&pane)
            })
            .await;
        self.check(back, "clicking Reattach attaches the terminal again");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "reattached");
        cx.update(|window, cx| {
            self.area.update(cx, |area, cx| {
                let machine = area.hooks().machine(cx);
                area.focus_pane(&machine, &pane, window, cx);
            });
        })
        .ok();
        Self::settle(cx).await;
        self.type_text(cx, "echo back-$((40+2))\n");
        let mut typed = false;
        for _ in 0..30 {
            Self::sleep(cx, 150).await;
            if self.read_pane_text(cx, &pane).await.contains("back-42") {
                typed = true;
                break;
            }
        }
        self.check(typed, "typing works in the reattached terminal");
        self.shoot(cx, "reattached");
        Ok(())
    }

    /// Toggling light/dark rewrites every terminal's config file and
    /// reloads it in place.
    async fn theme_reload(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        let read_config = |area: &Hooks| {
            area.terminals()
                .iter()
                .next()
                .and_then(|(_, view)| std::fs::read_to_string(view.config_path()).ok())
        };
        let before = self.read(cx, |area, _| read_config(area))?;
        self.dispatch(cx, Box::new(crate::actions::ToggleTheme));
        let changed = self
            .until(cx, Duration::from_secs(5), |area, _, _| {
                read_config(area) != before
            })
            .await;
        self.check(
            changed,
            "theme toggle rewrites and reloads terminal configs",
        );
        Self::sleep(cx, 500).await;
        self.shoot(cx, "other-theme");
        self.dispatch(cx, Box::new(crate::actions::ToggleTheme));
        Self::sleep(cx, 300).await;
        Ok(())
    }

    /// Restarts this worktree's own dev herdr server. Restored panes come
    /// back with new terminal ids, so every view must be replaced by one
    /// attached to the new id.
    async fn server_restart(&mut self, cx: &mut Cx, label: &str) -> anyhow::Result<()> {
        let (before, config) = self.read(cx, |area, cx| {
            let ids: HashSet<TerminalId> =
                area.terminals().iter().map(|(id, _)| id.clone()).collect();
            (ids, area.state().read(cx).herdr_config.clone())
        })?;
        let Some(config) = config else {
            self.fail("server restart: herdr is disconnected");
            return Ok(());
        };
        let Some(session) = config.session.clone() else {
            self.fail("server restart needs HERDR_SESSION (never restart the default server)");
            return Ok(());
        };
        let stopped = std::process::Command::new(&config.bin)
            .args(["--session", &session, "server", "stop"])
            .output();
        self.check(
            stopped.as_ref().is_ok_and(|out| out.status.success()),
            &format!("stopped dev server {session}"),
        );
        let replaced = self
            .until(cx, Duration::from_secs(30), |area, _, cx| {
                let snapshot = area.snapshot(cx);
                let live: HashSet<&TerminalId> =
                    snapshot.panes.values().map(|p| &p.terminal_id).collect();
                let views: HashSet<&TerminalId> =
                    area.terminals().iter().map(|(id, _)| id).collect();
                !live.is_empty()
                    && live.iter().all(|id| !before.contains(*id))
                    && live == views
                    && live.iter().all(|id| area.terminals().is_attached(id, cx))
            })
            .await;
        self.check(
            replaced,
            "after a server restart every view is re-attached to the new terminal ids",
        );
        // Show our workspace again (restore may focus another one).
        let workspace_id = self.read(cx, |area, cx| {
            area.snapshot(cx)
                .workspaces
                .iter()
                .find(|w| w.info.label == label)
                .map(|w| w.info.workspace_id.clone())
        })?;
        if let Some(workspace_id) = workspace_id {
            let task = self.on_state(cx, |state, machine, cx| {
                state.focus_workspace(machine, &workspace_id, cx)
            })?;
            task.await.ok();
        }
        Self::sleep(cx, 800).await;
        Self::settle(cx).await;
        self.check_views(cx, "after server restart");
        self.shoot(cx, "after-restart");
        Ok(())
    }
}

impl Run {
    /// Resizing the main window resizes the overlay with it. (The frame is
    /// applied after GPUI's update ends; applied inline it was dropped.)
    async fn overlay_follows_resize(&mut self, cx: &mut Cx, overlay: &Entity<overlay::Overlay>) {
        let original = cx.update(|window, _| window.bounds().size).ok();
        let mut ok = true;
        for size in [size(px(1180.), px(760.)), size(px(1420.), px(900.))] {
            cx.update(|window, _| window.resize(size)).ok();
            Self::sleep(cx, 400).await;
            Self::settle(cx).await;
            let parent = cx
                .update(|window, _| {
                    overlay::ns_window(window).map(|ns| ns.contentRectForFrameRect(ns.frame()))
                })
                .ok()
                .flatten();
            let child = overlay.update(cx, |overlay, cx| {
                overlay.update_window(cx, |window, _| {
                    overlay::ns_window(window).map(|ns| ns.frame())
                })
            });
            let matches = match (parent, child.flatten()) {
                (Some(p), Some(c)) => {
                    let near = |a: f64, b: f64| (a - b).abs() < 1.0;
                    near(p.origin.x, c.origin.x)
                        && near(p.origin.y, c.origin.y)
                        && near(p.size.width, c.size.width)
                        && near(p.size.height, c.size.height)
                }
                _ => false,
            };
            if !matches {
                ok = false;
                eprintln!("     overlay {child:?} vs parent content {parent:?}");
            }
        }
        self.check(ok, "the overlay follows main-window resizes");
        if let Some(original) = original {
            cx.update(|window, _| window.resize(original)).ok();
            Self::sleep(cx, 300).await;
        }
    }
}

/// A stand-in toast for the overlay checks, at the top-left of the window.
struct ScriptToast;

impl Render for ScriptToast {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::ActiveTheme as _;
        div()
            .absolute()
            .right(px(30.))
            .top(px(140.))
            .w(px(240.))
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .occlude()
            .child("Overlay toast over a terminal")
    }
}
