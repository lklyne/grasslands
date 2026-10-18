//! `RT_SCRIPT=titlebar`: the titlebar's herdr and file tabs. Opens files,
//! clicks tabs and presses the tab keys (GPUI events inside the app), and
//! checks the native Ghostty views hide on a file tab and come back on their
//! slots on the herdr tab, that agent toasts still show on a file tab, and
//! that closing the last file selects herdr. Screenshots both themes.
//!
//! ```sh
//! RT_SCRIPT=titlebar scripts/dev-run cargo run -p app
//! ```

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::*;

use super::{Cx, Run};
use crate::actions::{OpenFile, ToggleTheme};
use crate::editor::Answer;
use crate::herdr::{SplitDirection, SplitTree};
use crate::overlay;
use crate::terminal_area::TerminalArea;
use crate::workspace::{self, MainTab, Workspace};

impl Run {
    pub(super) async fn titlebar(&mut self, cx: &mut Cx) -> anyhow::Result<()> {
        eprintln!("script: titlebar");
        let connected = self
            .until(cx, Duration::from_secs(20), |area, _, cx| {
                area.snapshot(cx).generation > 0
            })
            .await;
        self.check(connected, "connected to herdr");
        if !connected {
            anyhow::bail!("no herdr");
        }
        let starts_on_herdr = self.workspace(cx, |ws, _, _| ws.selected() == MainTab::Herdr)?;
        self.check(starts_on_herdr, "the herdr tab is selected at start");

        // A workspace of our own with two terminals.
        let task = self.read(cx, |area, cx| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            area.state
                .read(cx)
                .new_workspace(home, Some("titlebar-script".to_owned()), cx)
        })?;
        let workspace_id = task.await?.workspace_id;
        let task = self.read(cx, |area, cx| {
            area.state.read(cx).focus_workspace(&workspace_id, cx)
        })?;
        task.await?;
        let shown = self
            .until(cx, Duration::from_secs(10), |area, _, cx| {
                TerminalArea::active_tab(&area.snapshot(cx))
                    .is_some_and(|tab| tab.info.workspace_id == workspace_id)
                    && area.drawn.borrow().panes.len() == 1
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
        Self::sleep(cx, 800).await;
        Self::settle(cx).await;
        self.check_views(cx, "herdr tab");
        self.shoot(cx, "herdr-tab");

        // Files: two with the same name, so their tabs show their folders.
        let dir = std::env::temp_dir().join(format!("rt-titlebar-{}", std::process::id()));
        let files = [
            dir.join("one/mod.rs"),
            dir.join("two/mod.rs"),
            dir.join("notes.md"),
        ];
        for file in &files {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(file, format!("// {}\nfn main() {{}}\n", file.display()))?;
        }
        for file in &files {
            self.dispatch(
                cx,
                Box::new(OpenFile {
                    path: file.clone(),
                    machine: crate::state::MachineId::Local,
                }),
            );
            Self::settle(cx).await;
        }
        let on_file = self.workspace(cx, |ws, _, cx| (ws.selected(), ws.selected_index(cx)))?;
        self.check(
            on_file == (MainTab::Files, 3),
            &format!("opening a file selects its tab (got {on_file:?})"),
        );
        let titles = self.workspace(cx, |ws, _, cx| tab_titles(ws, cx))?;
        self.check(
            titles == ["one/mod.rs", "two/mod.rs", "notes.md"],
            &format!("file tabs are disambiguated: {titles:?}"),
        );
        let drawn = self.workspace(cx, |ws, _, _| ws.drawn_tabs().tabs.len())?;
        self.check(drawn == 4, &format!("titlebar draws 4 tabs (got {drawn})"));
        self.check_hidden(cx, "file tab");
        let full = self.workspace(cx, |ws, _, _| {
            // The editor fills the main area: from the sidebar's right edge
            // to the window's, under the titlebar.
            ws.drawn_tabs().tabs.first().map(|tab| tab.bottom())
        })?;
        self.check(full.is_some(), "titlebar tabs have bounds");
        Self::settle(cx).await;
        self.shoot(cx, "file-tab");

        // Click the herdr tab: views come back on their slots.
        self.click_tab(cx, 0).await?;
        let back = self.workspace(cx, |ws, _, _| ws.selected() == MainTab::Herdr)?;
        self.check(back, "clicking herdr selects it");
        Self::sleep(cx, 300).await;
        Self::settle(cx).await;
        self.check_views(cx, "back on the herdr tab");
        self.shoot(cx, "herdr-restored");

        // Click a file tab, then the keys.
        self.click_tab(cx, 2).await?;
        let second = self.workspace(cx, |ws, _, cx| ws.selected_index(cx))?;
        self.check(
            second == 2,
            &format!("clicking tab 2 selects it (got {second})"),
        );
        self.check_hidden(cx, "after clicking a file tab");
        self.key(cx, "ctrl-tab");
        let third = self.workspace(cx, |ws, _, cx| ws.selected_index(cx))?;
        self.check(
            third == 3,
            &format!("ctrl-tab moves to tab 3 (got {third})"),
        );
        self.key(cx, "ctrl-tab");
        let wrapped = self.workspace(cx, |ws, _, cx| ws.selected_index(cx))?;
        self.check(
            wrapped == 0,
            &format!("ctrl-tab wraps to herdr (got {wrapped})"),
        );
        self.key(cx, "ctrl-shift-tab");
        let last = self.workspace(cx, |ws, _, cx| ws.selected_index(cx))?;
        self.check(
            last == 3,
            &format!("ctrl-shift-tab wraps back (got {last})"),
        );
        self.key(cx, "cmd-shift-[");
        let prev = self.workspace(cx, |ws, _, cx| ws.selected_index(cx))?;
        self.check(
            prev == 2,
            &format!("cmd-shift-[ in the editor goes back (got {prev})"),
        );
        self.key(cx, "cmd-1");
        let herdr = self.workspace(cx, |ws, _, _| ws.selected() == MainTab::Herdr)?;
        self.check(herdr, "cmd-1 selects herdr");
        Self::settle(cx).await;
        self.check_views(cx, "after cmd-1");

        // An agent toast while a file tab is selected.
        self.click_tab(cx, 1).await?;
        let Some((_, tree)) = self.tab(cx)? else {
            anyhow::bail!("no tab");
        };
        let pane = match &tree {
            SplitTree::Pane(pane) => pane.clone(),
            SplitTree::Split { .. } => tree.pane_ids()[0].to_owned(),
        };
        self.report_agent(cx, &pane, "working").await?;
        Self::sleep(cx, 500).await;
        self.report_agent(cx, &pane, "blocked").await?;
        let toast = self
            .until_overlay(cx, |window, cx| !window.notifications(cx).is_empty())
            .await;
        self.check(toast, "a blocked agent raises a toast on a file tab");
        self.check_hidden(cx, "with the toast");
        Self::settle(cx).await;
        self.shoot(cx, "toast-on-file-tab");

        // Themes: the titlebar in light and dark.
        for _ in 0..2 {
            self.dispatch(cx, Box::new(ToggleTheme));
            Self::sleep(cx, 300).await;
            Self::settle(cx).await;
            let dark = cx.update(|_, cx| cx.theme().is_dark()).unwrap_or(false);
            self.shoot(
                cx,
                if dark {
                    "titlebar-dark"
                } else {
                    "titlebar-light"
                },
            );
        }

        // A dirty tab asks before closing; closing every file selects herdr.
        self.type_text(cx, "x");
        Self::settle(cx).await;
        let dirty = self.workspace(cx, |ws, _, cx| {
            ws.editor()
                .read(cx)
                .tabs()
                .iter()
                .map(|t| t.dirty)
                .collect::<Vec<_>>()
        })?;
        self.check(
            dirty == [true, false, false],
            &format!("typing marks tab 1 dirty: {dirty:?}"),
        );
        self.click_tab(cx, 0).await?;
        self.click_close(cx, 1).await?;
        let asks = self.workspace(cx, |ws, _, cx| {
            (ws.selected_index(cx), tab_titles(ws, cx).len())
        })?;
        self.check(
            asks == (1, 3),
            &format!("closing a dirty tab from herdr brings it forward to ask: {asks:?}"),
        );
        Self::settle(cx).await;
        self.shoot(cx, "close-prompt");
        for index in [3, 2] {
            self.click_close(cx, index).await?;
        }
        self.key(cx, "cmd-w");
        let still = self.workspace(cx, |ws, _, cx| tab_titles(ws, cx).len())?;
        self.check(
            still == 1,
            &format!("cmd-w on the dirty tab keeps it (got {still} tabs)"),
        );
        self.workspace(cx, |ws, window, cx| {
            ws.editor()
                .update(cx, |editor, cx| editor.answer(Answer::Discard, window, cx))
        })?;
        Self::settle(cx).await;
        let empty = self.workspace(cx, |ws, _, cx| (ws.selected(), tab_titles(ws, cx).len()))?;
        self.check(
            empty == (MainTab::Herdr, 0),
            &format!("closing the last file selects herdr: {empty:?}"),
        );
        Self::sleep(cx, 300).await;
        Self::settle(cx).await;
        self.check_views(cx, "after closing every file");

        let task = self.read(cx, |area, cx| {
            area.state
                .read(cx)
                .close_workspace(&workspace_id, false, cx)
        })?;
        task.await?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    fn workspace<R>(
        &self,
        cx: &mut Cx,
        f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
    ) -> anyhow::Result<R> {
        cx.update(|window, cx| {
            let workspace =
                workspace::get(window, cx).ok_or_else(|| anyhow::anyhow!("no workspace"))?;
            Ok(workspace.update(cx, |ws, cx| f(ws, window, cx)))
        })?
    }

    async fn click_tab(&mut self, cx: &mut Cx, index: usize) -> anyhow::Result<()> {
        let Some(bounds) =
            self.workspace(cx, |ws, _, _| ws.drawn_tabs().tabs.get(index).copied())?
        else {
            anyhow::bail!("titlebar tab {index} not drawn");
        };
        // Left of centre, clear of a file tab's close button.
        self.click(cx, point(bounds.left() + px(14.), bounds.center().y))
            .await;
        Ok(())
    }

    async fn click_close(&mut self, cx: &mut Cx, index: usize) -> anyhow::Result<()> {
        let bounds = self.workspace(cx, |ws, _, _| {
            ws.drawn_tabs()
                .closes
                .iter()
                .find(|(i, _)| *i == index)
                .map(|(_, bounds)| *bounds)
        })?;
        let Some(bounds) = bounds else {
            anyhow::bail!("close button of tab {index} not drawn");
        };
        self.click(cx, bounds.center()).await;
        Ok(())
    }

    /// No native terminal view is shown.
    fn check_hidden(&mut self, cx: &mut Cx, what: &str) {
        let shown = cx
            .update(|window, _| {
                overlay::native_subviews(window)
                    .into_iter()
                    .filter(|(_, hidden)| !hidden)
                    .count()
            })
            .unwrap_or(usize::MAX);
        let visible = self.read(cx, |area, _| area.is_visible()).unwrap_or(true);
        self.check(
            shown == 0 && !visible,
            &format!("{what}: terminals hidden ({shown} native views shown)"),
        );
    }
}

fn tab_titles(workspace: &Workspace, cx: &App) -> Vec<String> {
    workspace
        .editor()
        .read(cx)
        .tabs()
        .into_iter()
        .map(|tab| tab.title)
        .collect()
}
