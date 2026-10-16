//! `RT_SCRIPT=chrome`: chrome's floating UI over real terminals. Right-clicks
//! on a pane header and on a tab (GPUI mouse events, as the app gets them),
//! the terminal area's close confirmation, an agent toast, the name prompt,
//! the rename dialog and the palette, each checked to be in the overlay
//! window and screenshotted with it.
//!
//! They are driven by key presses (`overlay::dispatch_keystroke`, which sends
//! them to the window AppKit would make key: the overlay while something
//! modal is open there): Escape, Enter, arrows through the menu and palette,
//! typing into the prompts. After each one closes, keyboard focus must be
//! back on the terminal that had it.
//!
//! ```sh
//! RT_SCRIPT=chrome scripts/dev-run cargo run -p app
//! ```

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use serde_json::json;

use super::{Cx, Run};
use crate::actions::CommandPalette;
use crate::chrome::{MenuCommand, MenuTarget};
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

        // Type into the second terminal, as a person would before opening
        // anything: it has the keyboard, and must get it back each time.
        let bodies = self.read(cx, |area, _| {
            let drawn = area.drawn.borrow();
            (
                drawn.panes.get(&first).copied(),
                drawn.panes.get(&second).copied(),
            )
        })?;
        let (Some(body), Some(second_body)) = bodies else {
            anyhow::bail!("panes not drawn");
        };
        self.click(cx, second_body.center()).await;
        let in_terminal = self
            .until(cx, Duration::from_secs(3), |area, window, cx| {
                area.terminals.focused(window, cx).is_some()
            })
            .await;
        self.check(
            in_terminal,
            "clicking a pane gives its terminal the keyboard",
        );

        // Right-click the first pane's header: the pane menu, in the overlay.
        let header = point(body.origin.x + px(60.), body.origin.y - HEADER_HEIGHT / 2.);
        self.right_click(cx, header);
        let open = self.until_chrome(cx, |menu, _| menu == Some(true)).await;
        self.check(
            open,
            "right-click on a pane header opens the pane menu in the overlay",
        );
        // The main window keeps its focus (a terminal) while the menu, in
        // the overlay, has the keyboard; it must be the same after.
        let typing = self.focused(cx);
        let area = self.area.clone();
        let on_terminal = cx
            .update(|window, cx| area.read(cx).terminals.focused(window, cx).is_some())
            .unwrap_or(false);
        self.check(
            on_terminal,
            "a terminal keeps main-window focus under the menu",
        );
        let modal = self.overlay_capturing(cx);
        self.check(
            modal,
            "the open menu makes the overlay take clicks and keys",
        );
        self.check(
            self.keys_go_to_overlay(cx),
            "keys go to the overlay while the menu is open",
        );
        self.key_window(cx, true, "the open menu").await;
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "pane-menu-over-terminal");
        self.key(cx, "escape");
        let closed = self.until_chrome(cx, |menu, _| menu.is_none()).await;
        self.check(closed, "Escape closes the menu");
        Self::sleep(cx, 300).await;
        let released = !self.overlay_capturing(cx);
        self.check(
            released,
            "with the menu closed the overlay lets clicks through",
        );
        self.key_window(cx, false, "the menu closed").await;
        self.focus_back(cx, &typing, "the menu").await;

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
            self.key(cx, "escape");
            let closed = self.until_chrome(cx, |menu, _| menu.is_none()).await;
            self.check(closed, "Escape closes the tab menu");
            self.focus_back(cx, &typing, "the tab menu").await;
        } else {
            self.check(false, "tab drawn");
        }

        // Arrows through the second pane's menu: down, down is Split Right.
        let second_header = point(
            second_body.origin.x + px(60.),
            second_body.origin.y - HEADER_HEIGHT / 2.,
        );
        self.right_click(cx, second_header);
        let open = self.until_chrome(cx, |menu, _| menu == Some(true)).await;
        self.check(open, "the second pane's menu opens");
        self.key(cx, "down");
        Self::settle(cx).await;
        self.key(cx, "down");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "pane-menu-arrowed");
        self.key(cx, "enter");
        let split = self
            .until(cx, Duration::from_secs(5), |area, _, _| {
                area.drawn.borrow().panes.len() == 3
            })
            .await;
        self.check(split, "down, down, Enter in the pane menu runs Split Right");
        let closed = self.until_chrome(cx, |menu, _| menu.is_none()).await;
        self.check(closed, "picking a menu item closes the menu");
        self.focus_in_terminals(cx, "the arrowed menu").await;

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
        self.key_window(cx, true, "the confirmation").await;
        self.shoot_with_overlay(cx, "close-confirm-over-terminal");
        self.key(cx, "enter");
        let gone = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx).pane(&first).is_none()
            })
            .await;
        self.check(gone, "Enter confirms: the pane is closed in herdr");
        let shut = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(shut, "Enter closes the confirmation");
        self.key_window(cx, false, "the confirmation closed").await;
        self.focus_in_terminals(cx, "the confirmation").await;

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

        self.name_prompt(cx, &workspace_id).await;
        self.rename_dialog(cx, &workspace_id).await;
        self.palette(cx).await;

        let task = self.read(cx, |area, cx| {
            area.state
                .read(cx)
                .close_workspace(&workspace_id, false, cx)
        })?;
        task.await?;
        Ok(())
    }

    pub(super) async fn report_agent(
        &self,
        cx: &mut Cx,
        pane_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
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

    /// One key press, sent where the keyboard is (the overlay while
    /// something modal is open there), never through the OS.
    pub(super) fn key(&self, cx: &mut Cx, key: &str) {
        let Ok(keystroke) = Keystroke::parse(key) else {
            eprintln!("script: bad keystroke {key:?}");
            return;
        };
        cx.update(|window, cx| overlay::dispatch_keystroke(keystroke, window, cx))
            .ok();
    }

    fn keys(&self, cx: &mut Cx, text: &str) {
        cx.update(|window, cx| {
            for keystroke in overlay::keystrokes(text) {
                overlay::dispatch_keystroke(keystroke, window, cx);
            }
        })
        .ok();
    }

    fn keys_go_to_overlay(&self, cx: &mut Cx) -> bool {
        let overlay = Self::overlay_handle(cx);
        overlay.is_some()
            && cx
                .update(|window, cx| Some(overlay::key_target(window, cx)))
                .ok()
                .flatten()
                == overlay
    }

    /// The main window's keyboard focus.
    fn focused(&self, cx: &mut Cx) -> Option<FocusHandle> {
        cx.update(|window, cx| window.focused(cx)).ok().flatten()
    }

    /// After a menu or dialog closes: the main window's focus is what it was
    /// before (`before`), so typing goes on in the same terminal.
    async fn focus_back(&mut self, cx: &mut Cx, before: &Option<FocusHandle>, what: &str) {
        Self::settle(cx).await;
        let now = self.focused(cx);
        let keys_home = cx
            .update(|window, cx| overlay::key_target(window, cx) == window.window_handle())
            .unwrap_or(false);
        self.check(
            before.is_some() && now == *before && keys_home,
            &format!("after {what} the keyboard is back on the terminal that had it"),
        );
    }

    /// After something that changed herdr's focus (a split, a close): the
    /// keyboard is on some terminal again, and keys go to the main window.
    async fn focus_in_terminals(&mut self, cx: &mut Cx, what: &str) {
        let back = self
            .until(cx, Duration::from_secs(3), |area, window, cx| {
                area.terminals.focused(window, cx).is_some()
            })
            .await;
        let keys_home = cx
            .update(|window, cx| overlay::key_target(window, cx) == window.window_handle())
            .unwrap_or(false);
        self.check(
            back && keys_home,
            &format!("after {what} the keyboard is on a terminal again"),
        );
    }

    /// AppKit key status: the overlay is the key window while something
    /// modal is open (`overlay_key`), the main window otherwise. A window can
    /// only be key while the app is active, so this is skipped while a person
    /// is using another app.
    async fn key_window(&mut self, cx: &mut Cx, overlay_key: bool, what: &str) {
        if !overlay::app_is_active() {
            eprintln!("skip key-window check ({what}): the app is not frontmost");
            return;
        }
        let start = Instant::now();
        let mut state = (false, false);
        while start.elapsed() < Duration::from_secs(2) {
            Self::settle(cx).await;
            let main = cx
                .update(|window, _| window.is_window_active())
                .unwrap_or(false);
            let over = Self::overlay_handle(cx).is_some_and(|handle| {
                cx.update_window(handle, |_, window, _| window.is_window_active())
                    .unwrap_or(false)
            });
            state = (main, over);
            if state == (!overlay_key, overlay_key) {
                break;
            }
        }
        let (main, over) = state;
        self.check(
            over == overlay_key && main == !overlay_key,
            &format!(
                "with {what} the {} window is key (main={main}, overlay={over})",
                if overlay_key { "overlay" } else { "main" }
            ),
        );
    }

    /// The shared name prompt (`crate::prompt`): type into it, Escape it
    /// (nothing submitted), then type again and submit with Enter.
    async fn name_prompt(&mut self, cx: &mut Cx, workspace_id: &str) {
        let before = self.focused(cx);
        // Opened from an open context menu (a sidebar menu, then a click on
        // a "New Folder" button), the prompt replaces the menu.
        let target = MenuTarget::Workspace {
            workspace_id: workspace_id.to_owned(),
        };
        cx.update(|window, cx| {
            if let Some(chrome) = crate::chrome::get(window, cx) {
                chrome.update(cx, |chrome, cx| {
                    let at = point(px(320.), px(200.));
                    chrome.context_menu(target, at, Vec::new(), window, cx);
                });
            }
        })
        .ok();
        let menu = self.until_chrome(cx, |menu, _| menu == Some(true)).await;
        self.check(menu, "a workspace menu opens before the prompt");
        self.check(
            self.overlay_capturing(cx),
            "the overlay's NSWindow takes clicks under a menu",
        );
        let submitted: Rc<RefCell<Vec<String>>> = Rc::default();
        let open = |cx: &mut Cx, submitted: &Rc<RefCell<Vec<String>>>| {
            let submitted = submitted.clone();
            cx.update(|window, cx| {
                crate::prompt::ask_name(
                    "Script Name",
                    "Create",
                    "",
                    move |name, _, _| submitted.borrow_mut().push(name.to_owned()),
                    window,
                    cx,
                );
            })
            .ok();
        };

        open(cx, &submitted);
        let shown = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(shown, "the name prompt opens in the overlay");
        let replaced = self.until_chrome(cx, |menu, _| menu.is_none()).await;
        self.check(replaced, "opening the prompt closes the open menu");
        Self::sleep(cx, 300).await;
        let still_modal = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(
            still_modal && self.overlay_capturing(cx),
            "with the menu gone the prompt keeps the overlay's NSWindow taking clicks",
        );
        self.check(
            self.keys_go_to_overlay(cx),
            "keys go to the overlay while the prompt is open",
        );
        self.key_window(cx, true, "the name prompt").await;
        self.typing_reaches(cx, None, "the name prompt").await;
        self.keys(cx, "Escaped");
        Self::settle(cx).await;
        self.shoot_with_overlay(cx, "name-prompt-typed");
        self.native_keys(cx, &["escape"]).await;
        let shut = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(shut, "Escape (AppKit key event) closes the name prompt");
        Self::sleep(cx, 300).await;
        self.check(submitted.borrow().is_empty(), "Escape submits nothing");
        self.key_window(cx, false, "the name prompt closed").await;
        self.focus_back(cx, &before, "the name prompt (Escape)")
            .await;

        open(cx, &submitted);
        let shown = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(shown, "the name prompt opens again");
        self.typing_reaches(cx, None, "the name prompt (again)")
            .await;
        // Real typing: letters, backspace, arrows, Enter, all as AppKit
        // key events. "nativx", ⌫, "e", then six lefts and "a " in front.
        self.native_type(cx, "nativx").await;
        self.native_keys(cx, &["backspace", "e"]).await;
        self.native_keys(cx, &["left"; 6]).await;
        self.native_type(cx, "a ").await;
        self.native_keys(cx, &["enter"]).await;
        let shut = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(shut, "Enter closes the name prompt");
        Self::sleep(cx, 300).await;
        let got = submitted.borrow().clone();
        self.check(
            got == ["a native"],
            &format!(
                "AppKit typing, backspace, left arrows and Enter fill and submit the prompt (got {got:?})"
            ),
        );
        self.focus_back(cx, &before, "the name prompt (Enter)")
            .await;
    }

    /// The chrome's rename dialog: select all, type a new name, Enter; herdr
    /// has the new label.
    async fn rename_dialog(&mut self, cx: &mut Cx, workspace_id: &str) {
        let before = self.focused(cx);
        let target = MenuTarget::Workspace {
            workspace_id: workspace_id.to_owned(),
        };
        cx.update(|window, cx| {
            if let Some(chrome) = crate::chrome::get(window, cx) {
                chrome.update(cx, |chrome, cx| {
                    chrome.run_menu_command(MenuCommand::Rename(target), window, cx);
                });
            }
        })
        .ok();
        let shown = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(shown, "the rename dialog opens in the overlay");
        let field = cx
            .update(|window, cx| {
                crate::chrome::get(window, cx)
                    .and_then(|c| c.read(cx).rename_input())
                    .map(|input| input.focus_handle(cx))
            })
            .ok()
            .flatten();
        self.check(field.is_some(), "the rename dialog's field is reachable");
        self.typing_reaches(cx, field, "the rename dialog").await;
        self.native_keys(cx, &["cmd-a"]).await;
        self.native_type(cx, "renamed by keys").await;
        self.shoot_with_overlay(cx, "rename-typed");
        self.native_keys(cx, &["enter"]).await;
        let renamed = self
            .until(cx, Duration::from_secs(5), |area, _, cx| {
                area.snapshot(cx)
                    .workspace(workspace_id)
                    .is_some_and(|w| w.info.label == "renamed by keys")
            })
            .await;
        self.check(
            renamed,
            "select-all, typing and Enter (AppKit key events) in the rename dialog rename in herdr",
        );
        let shut = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(shut, "Enter closes the rename dialog");
        self.focus_back(cx, &before, "the rename dialog").await;
    }

    /// The palette: type a query, arrow down, Enter runs the selected entry.
    async fn palette(&mut self, cx: &mut Cx) {
        let panes = self
            .read(cx, |area, _| area.drawn.borrow().panes.len())
            .unwrap_or(0);
        cx.update(|window, cx| overlay::dispatch_action(Box::new(CommandPalette), window, cx))
            .ok();
        let shown = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(shown, "the command palette opens in the overlay");
        self.check(
            self.keys_go_to_overlay(cx),
            "keys go to the overlay while the palette is open",
        );
        self.key_window(cx, true, "the palette").await;
        let palette = cx
            .update(|window, cx| crate::chrome::get(window, cx).and_then(|c| c.read(cx).palette()))
            .ok()
            .flatten();
        let Some(palette) = palette else {
            self.check(false, "the palette's state is reachable");
            return;
        };
        let field = palette.read_with(cx, |p, cx| p.focus_handle(cx));
        self.typing_reaches(cx, Some(field), "the palette").await;
        self.native_type(cx, "split").await;
        let (query, matched, first) = palette.read_with(cx, |p, cx| {
            (p.query(cx), p.matched_count(), p.selected_index())
        });
        self.check(
            query == "split" && matched >= 2,
            &format!(
                "AppKit typing goes into the palette's query (got {query:?}, {matched} matches)"
            ),
        );
        self.native_keys(cx, &["down"]).await;
        Self::settle(cx).await;
        let second = palette.read_with(cx, |p, _| p.selected_index());
        self.check(
            first.is_some() && second.is_some() && first != second,
            &format!("down moves the palette selection ({first:?} -> {second:?})"),
        );
        self.shoot_with_overlay(cx, "palette-arrowed");
        self.key(cx, "enter");
        let shut = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(shut, "Enter closes the palette");
        let ran = self
            .until(cx, Duration::from_secs(5), |area, _, _| {
                area.drawn.borrow().panes.len() == panes + 1
            })
            .await;
        self.check(ran, "Enter runs the selected entry (a split)");
        self.key_window(cx, false, "the palette closed").await;
        self.focus_in_terminals(cx, "the palette").await;

        // And Escape closes it without running anything.
        cx.update(|window, cx| overlay::dispatch_action(Box::new(CommandPalette), window, cx))
            .ok();
        let shown = self
            .until_overlay(cx, |window, cx| window.has_active_dialog(cx))
            .await;
        self.check(shown, "the palette opens again");
        let before = self.focused(cx);
        self.key(cx, "escape");
        let shut = self
            .until_overlay(cx, |window, cx| !window.has_active_dialog(cx))
            .await;
        self.check(shut, "Escape closes the palette");
        self.focus_back(cx, &before, "the palette (Escape)").await;
    }

    /// The overlay's AppKit side, for real-typing checks.
    fn native_keyboard(cx: &mut Cx) -> Option<overlay::NativeKeyboard> {
        cx.update(|window, cx| overlay::get(window, cx).map(|o| o.read(cx).native_keyboard()))
            .ok()
            .flatten()
    }

    /// With a text field open in the overlay: GPUI focus is on it (`field`,
    /// when known) in the overlay window, and AppKit would deliver real
    /// typing to it: GPUI's view is the overlay's first responder and the
    /// field registered an input handler. Before the fix the first responder
    /// was the panel itself, and every key press beeped (0008).
    async fn typing_reaches(&mut self, cx: &mut Cx, field: Option<FocusHandle>, what: &str) {
        Self::settle(cx).await;
        let focused = Self::overlay_handle(cx).and_then(|handle| {
            cx.update_window(handle, |_, window, cx| window.focused(cx))
                .ok()
                .flatten()
        });
        let main_focus = self.focused(cx);
        self.check(
            focused.is_some()
                && field
                    .as_ref()
                    .is_none_or(|field| focused.as_ref() == Some(field)),
            &format!(
                "with {what} open its text field has focus in the overlay window ({focused:?})"
            ),
        );
        self.check(
            main_focus.is_some() && main_focus != focused,
            &format!("with {what} open the main window keeps its own focus ({main_focus:?})"),
        );
        let Some(keyboard) = Self::native_keyboard(cx) else {
            self.check(
                false,
                &format!("{what}: the overlay's AppKit side is reachable"),
            );
            return;
        };
        let path = keyboard.path();
        tracing::debug!("script: {what}: overlay {path:?}, focused element {focused:?}");
        eprintln!("script: {what}: overlay {path:?}");
        self.check(
            path.view_first,
            &format!(
                "with {what} open GPUI's view is the overlay's first responder (got {})",
                path.first_responder
            ),
        );
        self.check(
            path.input_handler,
            &format!("with {what} open the overlay has an input handler for typed text"),
        );
        if overlay::app_is_active() {
            self.check(
                path.key,
                &format!("with {what} open the overlay is key in AppKit"),
            );
        }
    }

    /// Key presses handed to the overlay's `NSWindow` as AppKit's event loop
    /// hands them (`sendEvent:` → first responder → `keyDown:` → text
    /// input), the path real typing takes. In-process, never through the OS.
    async fn native_keys(&mut self, cx: &mut Cx, keys: &[&str]) {
        let Some(keyboard) = Self::native_keyboard(cx) else {
            self.check(false, "the overlay's AppKit side is reachable for typing");
            return;
        };
        for key in keys {
            if !keyboard.send_key(key) {
                self.check(false, &format!("AppKit key event for {key:?}"));
            }
            Self::settle(cx).await;
        }
    }

    /// `native_keys` for each character of `text` (lowercase letters and
    /// spaces).
    async fn native_type(&mut self, cx: &mut Cx, text: &str) {
        let keys: Vec<String> = text
            .chars()
            .map(|ch| {
                if ch == ' ' {
                    "space".to_owned()
                } else {
                    ch.to_string()
                }
            })
            .collect();
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        self.native_keys(cx, &keys).await;
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
    pub(super) async fn until_overlay(
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
