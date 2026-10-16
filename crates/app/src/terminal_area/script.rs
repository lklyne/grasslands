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

use super::{ClosePane, NewTab, SplitDown, SplitRight, TerminalArea};
use crate::herdr::{SplitDirection, SplitTree};
use crate::overlay::{self, Capture};

pub fn requested() -> Option<String> {
    std::env::var("RT_SCRIPT").ok().filter(|s| !s.is_empty())
}

pub fn start(scenario: String, area: Entity<TerminalArea>, window: &mut Window, cx: &mut App) {
    if scenario != "terminals" {
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
            if let Err(error) = run.terminals(cx).await {
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

    fn shoot(&mut self, cx: &mut Cx, name: &str) {
        self.shot += 1;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/script-shots");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{:02}-{name}.png", self.shot));
        let Some(number) = cx
            .update(|window, _| overlay::ns_window(window).map(|w| w.windowNumber()))
            .ok()
            .flatten()
        else {
            return;
        };
        // Only this app's main window (-l), never the whole screen.
        let output = std::process::Command::new("screencapture")
            .args(["-x", "-o", "-l", &number.to_string()])
            .arg(&path)
            .output();
        match output {
            Ok(out) if out.status.success() => eprintln!("shot {}", path.display()),
            Ok(out) => eprintln!(
                "shot {name} skipped: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(error) => eprintln!("shot {name} skipped: {error}"),
        }
    }

    /// Captures the main window's rectangle on screen, which includes the
    /// overlay child window above it.
    fn shoot_with_overlay(&mut self, cx: &mut Cx, name: &str) {
        self.shot += 1;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/script-shots");
        let path = dir.join(format!("{:02}-{name}.png", self.shot));
        let Some(region) = cx
            .update(|window, _| {
                let ns = overlay::ns_window(window)?;
                let frame = ns.frame();
                let screen = ns.screen()?.frame();
                let top = screen.size.height - frame.origin.y - frame.size.height;
                Some(format!(
                    "{},{},{},{}",
                    frame.origin.x, top, frame.size.width, frame.size.height
                ))
            })
            .ok()
            .flatten()
        else {
            return;
        };
        cx.update(|window, _| window.activate_window()).ok();
        let output = std::process::Command::new("screencapture")
            .args(["-x", "-R", &region])
            .arg(&path)
            .output();
        match output {
            Ok(out) if out.status.success() => eprintln!("shot {}", path.display()),
            Ok(out) => eprintln!(
                "shot {name} skipped: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(error) => eprintln!("shot {name} skipped: {error}"),
        }
    }

    /// Compares where the layout put each visible pane with the native views
    /// on screen: every visible pane has a shown view at exactly its slot,
    /// and no other view is shown.
    fn views_match_layout(&self, cx: &mut Cx) -> Result<usize, String> {
        cx.update(|window, cx| {
            let area = self.area.read(cx);
            let slots: Vec<Bounds<Pixels>> = area.drawn.borrow().panes.values().copied().collect();
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

    fn dispatch(&self, cx: &mut Cx, action: Box<dyn Action>) {
        cx.update(|window, cx| window.dispatch_action(action, cx))
            .ok();
    }

    fn type_text(&self, cx: &mut Cx, text: &str) {
        cx.update(|window, cx| {
            for ch in text.chars() {
                let (key, key_char) = match ch {
                    ' ' => ("space".to_owned(), Some(" ".to_owned())),
                    '\n' => ("enter".to_owned(), None),
                    ch => (ch.to_string(), Some(ch.to_string())),
                };
                window.dispatch_keystroke(
                    Keystroke {
                        modifiers: Modifiers::default(),
                        key,
                        key_char,
                    },
                    cx,
                );
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
        let overlay_key = overlay.update(cx, |overlay, cx| {
            overlay.update_window(cx, |window, _| window.is_window_active())
        });
        self.check(
            overlay_key == Some(true),
            "a modal layer makes the overlay the key window (keyboard for menus/palette)",
        );
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
        let key = cx
            .update(|window, _| window.is_window_active())
            .unwrap_or(false);
        self.check(key, "main window is key again after the modal layer closed");

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

impl Run {
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
