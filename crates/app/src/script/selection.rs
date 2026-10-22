//! `RT_SCRIPT=selection`: selecting text in a herdr pane, whose attach has
//! the mouse. A plain drag selects in Ghostty, a click clears it, a
//! double-click selects a word, and Open Scrollback shows the pane's whole
//! scrollback, past `pane.read`'s 1000 lines.
//!
//! ```sh
//! RT_SCRIPT=selection scripts/dev-run cargo run -p app
//! ```
//!
//! It creates its own workspace (`script-selection`), works only inside it,
//! and closes it at the end.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

use super::{Cx, Run};
use crate::actions::OpenScrollback;
use crate::herdr::SplitTree;
use crate::terminal_area::hooks::Hooks;

const LAST_LINE: &str = "https://example.com/selection";

impl Run {
    pub(super) async fn selection(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: selection");
        let connected = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                area.snapshot(cx).generation > 0
            })
            .await;
        self.check(connected, "connected to herdr");
        if !connected {
            anyhow::bail!("no herdr");
        }

        let task = self.read(cx, |area, cx| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            area.state()
                .read(cx)
                .on(&area.machine(cx), cx, |session, cx| {
                    session.new_workspace(home, Some("script-selection".to_owned()), cx)
                })
        })?;
        let workspace = task.await?;
        let workspace_id = workspace.workspace_id.clone();
        let task = self.on_state(cx, |state, machine, cx| {
            state.focus_workspace(machine, &workspace_id, cx)
        })?;
        task.await?;
        let shown = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                let snapshot = area.snapshot(cx);
                Hooks::active_tab(&snapshot)
                    .is_some_and(|tab| tab.info.workspace_id == workspace_id)
                    && area.drawn().panes.len() == 1
            })
            .await;
        self.check(shown, "new workspace shows one pane");
        Self::sleep(cx, 800).await;
        let Some((_, SplitTree::Pane(pane))) = self.tab(cx)? else {
            anyhow::bail!("expected a single pane");
        };
        cx.update(|window, cx| {
            self.area.update(cx, |area, cx| {
                let machine = area.hooks().machine(cx);
                area.focus_pane(&machine, &pane, window, cx);
            });
        })
        .ok();
        Self::settle(cx).await;

        self.type_text(
            cx,
            &format!("clear; seq 1 1500 | sed 's/^/row /'; echo {LAST_LINE}\n"),
        );
        let mut printed = false;
        for _ in 0..50 {
            Self::sleep(cx, 150).await;
            if self.read_pane_text(cx, &pane).await.contains(LAST_LINE) {
                printed = true;
                break;
            }
        }
        self.check(printed, "the pane printed 1500 rows");
        Self::sleep(cx, 300).await;

        let terminal = self
            .read(cx, |area, cx| {
                let snapshot = area.snapshot(cx);
                let terminal_id = snapshot.pane(&pane)?.terminal_id.clone();
                area.terminals()
                    .get(&terminal_id)
                    .map(|view| view.terminal.clone())
            })?
            .ok_or_else(|| anyhow::anyhow!("no terminal view for {pane}"))?;
        let selection = |cx: &mut Cx| {
            cx.update(|_, cx| terminal.update(cx, |terminal, _| terminal.selection_text()))
                .ok()
                .flatten()
        };
        let slot = self
            .drawn(cx, |drawn| drawn.panes.get(&pane).copied())?
            .ok_or_else(|| anyhow::anyhow!("pane {pane} isn't drawn"))?;
        let at = |x: f32, y: f32| slot.origin + point(px(x), px(y));

        // A plain drag over rows of output selects them in Ghostty.
        self.drag(cx, at(8., 40.), at(260., 140.), 12, |_, _| {})
            .await;
        let dragged = selection(cx);
        self.check(
            dragged.as_deref().is_some_and(|text| text.contains("row ")),
            &format!("a plain drag selects the rows under it (got {dragged:?})"),
        );

        // A click goes to the pane and clears the selection.
        self.click(cx, at(120., 200.)).await;
        let after_click = selection(cx);
        self.check(
            after_click.is_none(),
            &format!("a click clears the selection (got {after_click:?})"),
        );

        // A double-click selects one word.
        let word = at(20., 80.);
        self.click(cx, word).await;
        self.mouse(
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: word,
                modifiers: Modifiers::default(),
                click_count: 2,
                first_mouse: false,
            }),
        );
        Self::settle(cx).await;
        self.mouse(
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: word,
                modifiers: Modifiers::default(),
                click_count: 2,
            }),
        );
        Self::settle(cx).await;
        let double = selection(cx);
        self.check(
            double
                .as_deref()
                .is_some_and(|text| !text.is_empty() && !text.contains(char::is_whitespace)),
            &format!("a double-click selects one word (got {double:?})"),
        );
        self.click(cx, at(120., 200.)).await;

        // Open Scrollback: the whole pane in a read-only editor tab.
        let machine = self.read(cx, |area, cx| area.machine(cx))?;
        let open = OpenScrollback {
            machine,
            pane_id: pane.clone(),
        };
        self.dispatch(cx, Box::new(open.clone()));
        let mut text = None;
        for _ in 0..50 {
            Self::sleep(cx, 150).await;
            text = self.workspace(cx, |ws, _, cx| ws.editor().read(cx).active_text(cx))?;
            if text.as_deref().is_some_and(|t| t.contains(LAST_LINE)) {
                break;
            }
        }
        let text = text.unwrap_or_default();
        self.check(
            text.lines().any(|line| line == "row 1") && text.contains("row 1500"),
            &format!(
                "the scrollback tab holds all 1500 rows ({} lines)",
                text.lines().count()
            ),
        );
        let scrollback_tabs = |ws: &mut crate::workspace::Workspace, cx: &App| {
            ws.editor()
                .read(cx)
                .tabs(cx)
                .iter()
                .filter(|tab| tab.title.ends_with("scrollback"))
                .count()
        };
        let tabs = self.workspace(cx, |ws, _, cx| scrollback_tabs(ws, cx))?;
        self.check(tabs == 1, &format!("one scrollback tab ({tabs})"));
        self.shoot(cx, "scrollback");

        self.dispatch(cx, Box::new(open));
        Self::sleep(cx, 1000).await;
        let tabs = self.workspace(cx, |ws, _, cx| scrollback_tabs(ws, cx))?;
        self.check(
            tabs == 1,
            &format!("opening it again reuses the tab ({tabs})"),
        );

        // Clean up: the tab, then the script's workspace.
        self.workspace(cx, |ws, window, cx| {
            ws.editor().update(cx, |editor, cx| {
                if let Some(index) = editor.active_index() {
                    editor.request_close(index, window, cx);
                }
            })
        })?;
        let task = self.read(cx, |area, cx| {
            area.state()
                .read(cx)
                .on(&area.machine(cx), cx, |session, cx| {
                    session.close_workspace(&workspace.workspace_id, false, cx)
                })
        })?;
        let closed = task.await;
        self.check(closed.is_ok(), "closed the script workspace");
        Ok(())
    }
}
