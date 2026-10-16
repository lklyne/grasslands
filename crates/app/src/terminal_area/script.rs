//! `RT_SCRIPT=terminals`: drives the terminal area against the dev herdr
//! session and checks it, printing `ok`/`FAIL` lines to stderr and saving
//! screenshots of this app's own window to `target/script-shots/`.
//!
//! ```sh
//! RT_SCRIPT=terminals scripts/dev-run cargo run -p app
//! ```
//!
//! Input is dispatched inside the app (GPUI keystrokes, actions and mouse
//! events), never through the OS. Each check reads state back: herdr's
//! snapshot, the text herdr holds for a pane, and the frames of the native
//! Ghostty views compared with the slots the layout drew. If the screen is
//! locked GPUI stops drawing, so the script forces frames itself; screenshots
//! then fail and are reported as skipped.
//!
//! The script creates its own workspace (`script`), works only inside it,
//! and closes it at the end. It exits the app when done unless
//! `RT_SCRIPT_KEEP=1`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::*;
use serde_json::json;

mod chrome;

use super::{ClosePane, NewTab, SplitDown, SplitRight, TerminalArea};
use crate::herdr::{SplitDirection, SplitTree};
use crate::overlay::{self, Capture};

pub fn requested() -> Option<String> {
    std::env::var("RT_SCRIPT").ok().filter(|s| !s.is_empty())
}

pub fn start(scenario: String, area: Entity<TerminalArea>, window: &mut Window, cx: &mut App) {
    if scenario != "terminals" && scenario != "chrome" {
        eprintln!("script: unknown scenario {scenario:?}");
        return;
    }
    window
        .spawn(cx, async move |cx| {
            let mut run = Run {
                area,
                failures: 0,
                shot: 0,
            };
            let result = if scenario == "chrome" {
                run.chrome(cx).await
            } else {
                run.terminals(cx).await
            };
            if let Err(error) = result {
                run.fail(&format!("script stopped: {error:#}"));
            }
            eprintln!(
                "script: done, {} failure{}",
                run.failures,
                if run.failures == 1 { "" } else { "s" }
            );
            if std::env::var("RT_SCRIPT_KEEP").is_err() {
                cx.update(|_, cx| cx.quit()).ok();
            }
        })
        .detach();
}

struct Run {
    area: Entity<TerminalArea>,
    failures: usize,
    shot: usize,
}

type Cx = AsyncWindowContext;

impl Run {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            eprintln!("ok   {what}");
        } else {
            self.fail(what);
        }
    }

    fn fail(&mut self, what: &str) {
        self.failures += 1;
        eprintln!("FAIL {what}");
    }

    async fn sleep(cx: &mut Cx, millis: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(millis))
            .await;
    }

    /// Lets GPUI lay out and paint. A locked screen stops the display link,
    /// so draw directly as well.
    async fn settle(cx: &mut Cx) {
        for _ in 0..3 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            })
            .ok();
            Self::sleep(cx, 60).await;
        }
    }

    /// Polls `test` until it holds or `timeout` passes.
    async fn until(
        &self,
        cx: &mut Cx,
        timeout: Duration,
        mut test: impl FnMut(&TerminalArea, &Window, &App) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            Self::settle(cx).await;
            let area = self.area.clone();
            let done = cx
                .update(|window, cx| {
                    let area = area.read(cx);
                    test(area, window, cx)
                })
                .unwrap_or(false);
            if done {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
            Self::sleep(cx, 100).await;
        }
    }

    fn read<R>(&self, cx: &mut Cx, f: impl FnOnce(&TerminalArea, &App) -> R) -> anyhow::Result<R> {
        let area = self.area.clone();
        cx.update(|_, cx| f(area.read(cx), cx))
    }

    /// Every screenshot goes through `scripts/capture-windows.swift`: this
    /// app's windows only, whatever else is on screen.
    fn shoot(&mut self, cx: &mut Cx, name: &str) {
        self.shoot_with_overlay(cx, name);
    }

    /// Captures this app's main window composited with its overlay child
    /// window, and nothing else: other apps' windows on top are left out (a
    /// screen-region capture would show whatever is frontmost there).
    fn shoot_with_overlay(&mut self, cx: &mut Cx, name: &str) {
        self.shot += 1;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = root.join("target/script-shots");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{:02}-{name}.png", self.shot));
        let numbers = cx
            .update(|window, cx| {
                let main = overlay::ns_window(window)?.windowNumber();
                let overlay = overlay::get(window, cx).and_then(|overlay| {
                    let handle = overlay.read(cx).window_handle();
                    cx.update_window(handle, |_, window, _| {
                        overlay::ns_window(window).map(|w| w.windowNumber())
                    })
                    .ok()
                    .flatten()
                });
                Some((main, overlay))
            })
            .ok()
            .flatten();
        let Some((main, overlay)) = numbers else {
            eprintln!("shot {name} skipped: no window");
            return;
        };
        let mut command = std::process::Command::new("swift");
        command
            .arg(root.join("scripts/capture-windows.swift"))
            .arg(&path)
            .arg(main.to_string());
        if let Some(overlay) = overlay {
            command.arg(overlay.to_string());
        }
        match capture(command, Duration::from_secs(20)) {
            Ok(()) if path.exists() => eprintln!("shot {}", path.display()),
            Ok(()) => eprintln!("shot {name} skipped: no file written"),
            Err(error) => eprintln!("shot {name} skipped: {error}"),
        }
    }

    /// Compares where the layout put each visible pane with the native views
    /// on screen: every visible pane has a shown view at exactly its slot,
    /// and no other view is shown.
    fn views_match_layout(&self, cx: &mut Cx) -> Result<usize, String> {
        cx.update(|window, cx| {
            let area = self.area.read(cx);
            // Detached panes show a Reattach button instead of a view.
            let drawn = area.drawn.borrow();
            let slots: Vec<Bounds<Pixels>> = drawn
                .panes
                .iter()
                .filter(|(pane, _)| !drawn.reattach.contains_key(*pane))
                .map(|(_, bounds)| *bounds)
                .collect();
            drop(drawn);
            if let Some(tiny) = slots
                .iter()
                .find(|slot| slot.size.width < px(80.) || slot.size.height < px(60.))
            {
                return Err(format!("a pane slot is too small to use: {tiny:?}"));
            }
            let shown: Vec<Bounds<Pixels>> = overlay::native_subviews(window)
                .into_iter()
                .filter(|(_, hidden)| !hidden)
                .map(|(bounds, _)| bounds)
                .collect();
            let near = |a: &Bounds<Pixels>, b: &Bounds<Pixels>| {
                let d = |x: Pixels, y: Pixels| (f32::from(x) - f32::from(y)).abs() < 1.0;
                d(a.origin.x, b.origin.x)
                    && d(a.origin.y, b.origin.y)
                    && d(a.size.width, b.size.width)
                    && d(a.size.height, b.size.height)
            };
            let missing: Vec<_> = slots
                .iter()
                .filter(|slot| !shown.iter().any(|view| near(slot, view)))
                .collect();
            let extra: Vec<_> = shown
                .iter()
                .filter(|view| !slots.iter().any(|slot| near(slot, view)))
                .collect();
            if missing.is_empty() && extra.is_empty() {
                Ok(slots.len())
            } else {
                Err(format!(
                    "slots without a view: {missing:?}; views without a slot: {extra:?}"
                ))
            }
        })
        .map_err(|e| e.to_string())?
    }

    fn check_views(&mut self, cx: &mut Cx, what: &str) {
        match self.views_match_layout(cx) {
            Ok(count) => eprintln!("ok   {what}: {count} views on their slots"),
            Err(error) => self.fail(&format!("{what}: {error}")),
        }
    }

    /// Dispatches `action` where the keyboard is: the overlay while
    /// something modal is open there, else the main window.
    fn dispatch(&self, cx: &mut Cx, action: Box<dyn Action>) {
        cx.update(|window, cx| overlay::dispatch_action(action, window, cx))
            .ok();
    }

    /// Types `text` one key press at a time, where the keyboard is.
    fn type_text(&self, cx: &mut Cx, text: &str) {
        cx.update(|window, cx| {
            for keystroke in overlay::keystrokes(text) {
                overlay::dispatch_keystroke(keystroke, window, cx);
            }
        })
        .ok();
    }

    fn mouse(&self, cx: &mut Cx, event: PlatformInput) {
        cx.update(|window, cx| {
            window.dispatch_event(event, cx);
        })
        .ok();
    }

    /// The tab the script works in and its tree.
    fn tab(&self, cx: &mut Cx) -> anyhow::Result<Option<(String, SplitTree)>> {
        self.read(cx, |area, cx| {
            let snapshot = area.snapshot(cx);
            TerminalArea::active_tab(&snapshot)
                .and_then(|tab| Some((tab.info.tab_id.clone(), tab.root.clone()?)))
        })
    }

    async fn read_pane_text(&self, cx: &mut Cx, pane_id: &str) -> String {
        let pane_id = pane_id.to_owned();
        let Ok(task) = self.read(cx, |area, cx| {
            area.state.read(cx).call(cx, move |client| {
                client.call(
                    "pane.read",
                    &json!({ "pane_id": pane_id, "source": "recent", "lines": 40 }),
                )
            })
        }) else {
            return String::new();
        };
        match task.await {
            Ok(value) => value
                .pointer("/read/text")
                .and_then(|text| text.as_str())
                .unwrap_or_default()
                .to_owned(),
            Err(error) => format!("<{error}>"),
        }
    }

    async fn terminals(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
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
            area.state
                .read(cx)
                .new_workspace(home, Some("script".to_owned()), cx)
        })?;
        let workspace = task.await?;
        let workspace_id = workspace.workspace_id.clone();
        let ours = |area: &TerminalArea, cx: &App| {
            let snapshot = area.snapshot(cx);
            TerminalArea::active_tab(&snapshot)
                .is_some_and(|tab| tab.info.workspace_id == workspace_id)
        };
        let task = self.read(cx, |area, cx| {
            area.state.read(cx).focus_workspace(&workspace_id, cx)
        })?;
        task.await?;
        let shown = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                ours(area, cx) && area.drawn.borrow().panes.len() == 1
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
            self.area
                .update(cx, |area, cx| area.focus_pane(&first, window, cx));
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

        // Split right and down through the app's own actions.
        self.dispatch(cx, Box::new(SplitRight));
        let two = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn.borrow().panes.len() == 2
            })
            .await;
        self.check(two, "cmd-d action splits right (2 panes)");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "after split right");
        self.dispatch(cx, Box::new(SplitDown));
        let three = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn.borrow().panes.len() == 3
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
            .update(|window, cx| self.area.read(cx).terminals.focused(window, cx).is_some())
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
        let key = super::SplitKey {
            tab_id: tab_id.clone(),
            path: Vec::new(),
        };
        let split = self.read(cx, |area, _| area.drawn.borrow().splits.get(&key).copied())?;
        if let Some(bounds) = split {
            let ratio_now = self.read(cx, |area, cx| {
                area.with_overrides(
                    &tab_id,
                    area.snapshot(cx)
                        .tab(&tab_id)
                        .and_then(|t| t.root.as_ref())
                        .unwrap_or(&SplitTree::Pane(String::new())),
                )
            })?;
            let ratio_now = match ratio_now {
                SplitTree::Split { ratio, .. } => ratio,
                SplitTree::Pane(_) => 0.5,
            };
            let x = f32::from(bounds.origin.x)
                + f32::from(bounds.size.width) * ratio_now
                + f32::from(super::GAP) * (0.5 - ratio_now);
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
            let local = self.read(cx, |area, _| area.overrides.get(&key).map(|o| o.ratio))?;
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
        let before: HashSet<String> = self.read(cx, |area, _| {
            area.drawn.borrow().panes.keys().cloned().collect()
        })?;
        self.dispatch(cx, Box::new(NewTab));
        let switched = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                let panes = area.drawn.borrow();
                panes.panes.len() == 1 && !panes.panes.keys().any(|p| before.contains(p))
            })
            .await;
        self.check(switched, "cmd-t opens a new tab with one pane");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "new tab: other tab's views hidden");
        self.shoot(cx, "second-tab");

        // Move the new tab's pane into the first tab: same terminal, same view.
        let Some((_, SplitTree::Pane(moving))) = self.tab(cx)? else {
            anyhow::bail!("expected one pane in the new tab");
        };
        let (terminal_id, view_before) = self.read(cx, |area, cx| {
            let snapshot = area.snapshot(cx);
            let terminal_id = snapshot.pane(&moving).map(|p| p.terminal_id.clone());
            let view = terminal_id
                .as_deref()
                .and_then(|id| area.terminals.get(id))
                .map(|v| v.terminal.entity_id());
            (terminal_id, view)
        })?;
        let task = self.read(cx, |area, cx| {
            area.state.read(cx).move_pane(
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
                    .as_deref()
                    .and_then(|id| area.terminals.get(id))
                    .map(|v| v.terminal.entity_id());
                in_first && view.is_some() && view == view_before
            })
            .await;
        self.check(kept, "moved pane keeps its Ghostty view (no restart)");
        let four = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn.borrow().panes.len() == 4
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
            (area.terminals.len(), terminal)
        })?;
        self.dispatch(cx, Box::new(ClosePane));
        let closed = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.terminals.len() + 1 == count_before
                    && closing_terminal
                        .as_deref()
                        .is_some_and(|id| area.terminals.get(id).is_none())
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
            area.state
                .read(cx)
                .close_workspace(&workspace.workspace_id, false, cx)
        })?;
        let closed = task.await;
        self.check(closed.is_ok(), "closed the script workspace");
        Ok(())
    }
}

/// Runs the capture helper, giving up after `timeout`: ScreenCaptureKit can
/// hang (seen with several captures queued machine-wide), and the script
/// must not stall on a screenshot.
fn capture(mut command: std::process::Command, timeout: Duration) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    let mut child = command
        // Its own process group: `swift` runs the script in a child
        // `swift-frontend`, which a timeout must stop too.
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    std::io::Read::read_to_string(&mut pipe, &mut stderr).ok();
                }
                return Err(stderr.trim().to_owned());
            }
            Ok(None) if start.elapsed() > timeout => {
                // Our own child's group, nothing else.
                std::process::Command::new("kill")
                    .arg("-KILL")
                    .arg(format!("-{}", child.id()))
                    .status()
                    .ok();
                child.wait().ok();
                return Err(format!("capture timed out after {timeout:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn center(bounds: Bounds<Pixels>) -> Point<Pixels> {
    bounds.origin + point(bounds.size.width / 2., bounds.size.height / 2.)
}

impl Run {
    fn press(&self, cx: &mut Cx, at: Point<Pixels>) {
        self.mouse(
            cx,
            PlatformInput::MouseMove(MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
        );
        self.mouse(
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
        );
    }

    fn release(&self, cx: &mut Cx, at: Point<Pixels>) {
        self.mouse(
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
        );
    }

    async fn click(&self, cx: &mut Cx, at: Point<Pixels>) {
        self.press(cx, at);
        Self::settle(cx).await;
        self.release(cx, at);
        Self::settle(cx).await;
    }

    /// Drags with the left button from `from` to `to` in `steps` frames;
    /// `during` runs at the halfway step, while the button is still down.
    async fn drag(
        &mut self,
        cx: &mut Cx,
        from: Point<Pixels>,
        to: Point<Pixels>,
        steps: usize,
        mut during: impl FnMut(&mut Self, &mut Cx),
    ) {
        self.press(cx, from);
        Self::settle(cx).await;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let position = from + point((to.x - from.x) * t, (to.y - from.y) * t);
            self.mouse(
                cx,
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
            );
            Self::settle(cx).await;
            if step == steps / 2 {
                during(self, cx);
            }
        }
        self.release(cx, to);
        Self::settle(cx).await;
    }

    fn drawn<R>(&self, cx: &mut Cx, f: impl FnOnce(&super::Drawn) -> R) -> anyhow::Result<R> {
        self.read(cx, |area, _| f(&area.drawn.borrow()))
    }

    fn focused_pane(&self, cx: &mut Cx) -> Option<String> {
        self.read(cx, |area, cx| area.snapshot(cx).focused_pane_id.clone())
            .ok()
            .flatten()
    }

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
            self.area
                .update(cx, |area, cx| area.focus_pane(&leftmost, window, cx));
        })
        .ok();
        let on_left = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).focused_pane_id.as_deref() == Some(leftmost.as_str())
            })
            .await;
        self.check(on_left, "focus starts on the leftmost pane");
        let expected = self.drawn(cx, |d| {
            super::layout::neighbor(&leftmost, crate::herdr::PaneDirection::Right, &d.panes)
                .map(str::to_owned)
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
                let area = self.area.read(cx);
                let snapshot = area.snapshot(cx);
                let want = expected
                    .as_deref()
                    .and_then(|p| snapshot.pane(p))
                    .map(|p| p.terminal_id.clone());
                area.terminals.focused(window, cx).map(str::to_owned) == want
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

    /// Dragging a pane header: onto another pane swaps them, onto a tab moves
    /// the pane there. The preview is drawn in the overlay, above terminals.
    async fn header_drags(&mut self, cx: &mut Cx, first_tab: &str) -> anyhow::Result<()> {
        let Some((_, tree)) = self.tab(cx)? else {
            anyhow::bail!("no tab for header drags");
        };
        let order_before: Vec<String> = tree.pane_ids().into_iter().map(str::to_owned).collect();
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
                    .read(cx, |area, _| area.pane_drag.is_some())
                    .unwrap_or(false);
                run.shoot_with_overlay(cx, "dragging-header-over-terminal");
            },
        )
        .await;
        self.check(
            chip_shown,
            "the drag preview is an overlay layer while dragging",
        );
        let gone = self.read(cx, |area, _| area.pane_drag.is_none())?;
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
                TerminalArea::active_tab(&snapshot).is_some_and(|t| t.info.tab_id != first_tab)
                    && area.drawn.borrow().panes.len() == 1
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
                .as_deref()
                .and_then(|t| area.terminals.get(t))
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
                    .any(|p| Some(&p.terminal_id) == terminal.as_ref() && p.tab_id == first_tab);
                let view = terminal
                    .as_deref()
                    .and_then(|t| area.terminals.get(t))
                    .map(|v| v.terminal.entity_id());
                in_first && view.is_some() && view == view_before
            })
            .await;
        self.check(
            moved,
            "dropping a header on another tab moves the pane there (pane.move), same view",
        );
        // Show the first tab again if herdr stayed on the (now gone) second.
        let task = self.read(cx, |area, cx| area.state.read(cx).focus_tab(first_tab, cx))?;
        task.await.ok();
        let four = self
            .until(cx, Duration::from_secs(10), |area, _, _| {
                area.drawn.borrow().panes.len() == 4
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
    async fn zoom(&mut self, cx: &mut Cx, tab_id: &str) -> anyhow::Result<()> {
        let panes = self.drawn(cx, |d| d.panes.len())?;
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("cmd-shift-enter").unwrap_or_default(), cx);
        })
        .ok();
        let zoomed = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).tab(tab_id).is_some_and(|t| t.zoomed)
                    && area.drawn.borrow().panes.len() == 1
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
                area.drawn.borrow().panes.len() == panes
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
            let pane = area.drawn.borrow().panes.keys().next().cloned();
            let terminal = pane
                .as_deref()
                .and_then(|p| snapshot.pane(p))
                .map(|p| p.terminal_id.clone());
            (pane, terminal, area.state.read(cx).herdr_config.clone())
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
                !area.terminals.is_attached(&terminal, cx)
                    && area.drawn.borrow().reattach.contains_key(&pane)
            })
            .await;
        self.check(
            detached,
            "a takeover by another client shows the pane as Detached",
        );
        Self::settle(cx).await;
        self.check_views(cx, "detached: its view is hidden");
        self.shoot(cx, "detached");
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
                area.terminals.is_attached(&terminal, cx)
                    && !area.drawn.borrow().reattach.contains_key(&pane)
            })
            .await;
        self.check(back, "clicking Reattach attaches the terminal again");
        Self::sleep(cx, 600).await;
        Self::settle(cx).await;
        self.check_views(cx, "reattached");
        cx.update(|window, cx| {
            self.area
                .update(cx, |area, cx| area.focus_pane(&pane, window, cx));
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
        let read_config = |area: &TerminalArea| {
            area.terminals
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
            let ids: HashSet<String> = area.terminals.iter().map(|(id, _)| id.clone()).collect();
            (ids, area.state.read(cx).herdr_config.clone())
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
                let live: HashSet<&str> = snapshot
                    .panes
                    .values()
                    .map(|p| p.terminal_id.as_str())
                    .collect();
                let views: HashSet<&str> =
                    area.terminals.iter().map(|(id, _)| id.as_str()).collect();
                !live.is_empty()
                    && live.iter().all(|id| !before.contains(*id))
                    && live == views
                    && live.iter().all(|id| area.terminals.is_attached(id, cx))
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
            let task = self.read(cx, |area, cx| {
                area.state.read(cx).focus_workspace(&workspace_id, cx)
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
