//! The Changes tab: the active worktree's diff against its parent branch
//! (see `git`) in GPUI Kit's diff viewer. Opened from the file tree's git
//! footer. It follows the active worktree and re-reads while open, keeping
//! its scroll place when the patch hasn't changed.

pub mod git;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use crate::state::{AppEvent, AppState};

const REFRESH: Duration = Duration::from_secs(3);

pub struct Changes {
    state: Entity<AppState>,
    diff: Entity<DiffState>,
    root: Option<PathBuf>,
    /// The patch on screen; `None` before the first read or outside git.
    patch: Option<String>,
    loaded: bool,
    _subscriptions: [Subscription; 2],
    _refresh: Task<()>,
}

impl Changes {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event, cx| {
            if let AppEvent::ActiveWorktreeChanged(root) = event {
                this.root.clone_from(root);
                this.patch = None;
                this.loaded = false;
                this.reload(cx);
                cx.notify();
            }
        });
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        });
        let diff = cx.new(|cx| DiffState::new([], cx));
        // The diff notifies itself after a paint (wrapped rows take the
        // width it just measured); redraw with it, or wrapping stays blank.
        let redraw = cx.observe(&diff, |_, _, cx| cx.notify());
        let mut this = Self {
            state: state.clone(),
            diff,
            root: state.read(cx).active_worktree().cloned(),
            patch: None,
            loaded: false,
            _subscriptions: [subscription, redraw],
            _refresh: refresh,
        };
        this.reload(cx);
        this
    }

    /// Re-reads the patch on the background executor.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let patch = cx
                .background_executor()
                .spawn({
                    let root = root.clone();
                    async move { git::patch(&root) }
                })
                .await;
            let _ = this.update(cx, |this, cx| this.apply(&root, patch, cx));
        })
        .detach();
    }

    fn apply(&mut self, root: &Path, patch: Option<String>, cx: &mut Context<Self>) {
        // A read for a worktree we have since left.
        if self.root.as_deref() != Some(root) {
            return;
        }
        self.loaded = true;
        if self.patch == patch {
            return;
        }
        let files = match patch.as_deref().map(DiffFile::parse) {
            Some(Ok(files)) => files,
            Some(Err(error)) => {
                tracing::warn!("changes: can't parse the patch: {error}");
                Vec::new()
            }
            None => Vec::new(),
        };
        self.diff.update(cx, |diff, cx| diff.set_files(files, cx));
        self.patch = patch;
        cx.notify();
    }

    pub fn file_count(&self, cx: &App) -> usize {
        self.diff.read(cx).files().len()
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.diff.update(cx, |diff, cx| {
            let mode = match diff.mode() {
                DiffMode::Split => DiffMode::Unified,
                _ => DiffMode::Split,
            };
            diff.set_mode(mode, cx);
        });
    }
}

/// The diff's focus, for its scroll keys.
impl Focusable for Changes {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.diff.focus_handle(cx)
    }
}

impl Render for Changes {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Changes reads this Mac's disk, like the file panel.
        let machine = self.state.read(cx).selected_machine();
        if !machine.id().is_local() {
            let label = machine.label().to_owned();
            return div()
                .size_full()
                .bg(cx.theme().background)
                .child(crate::machines::remote_files_empty(&label, cx))
                .into_any_element();
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let diff = self.diff.read(cx);
        let files = diff.files();
        let added: usize = files.iter().map(DiffFile::additions).sum();
        let removed: usize = files.iter().map(DiffFile::deletions).sum();
        let count = files.len();
        let split = diff.mode() == DiffMode::Split;
        let empty = match (self.loaded, &self.patch) {
            (false, _) => Some("Reading changes…"),
            (true, None) => Some("Not a git repository"),
            (true, Some(_)) if count == 0 => Some("No changes"),
            _ => None,
        };
        let header = h_flex()
            .flex_none()
            .gap_3()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .text_color(muted)
            .child(format!(
                "{count} {}",
                if count == 1 { "file" } else { "files" }
            ))
            .child(
                h_flex()
                    .gap_1p5()
                    .child(div().text_color(theme.success).child(format!("+{added}")))
                    .child(div().text_color(theme.danger).child(format!("-{removed}"))),
            )
            .child(div().flex_1())
            .child(
                Button::new("changes-mode")
                    .ghost()
                    .xsmall()
                    .label(if split { "Unified" } else { "Split" })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mode(cx))),
            );
        let body = match empty {
            Some(message) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(message)
                .into_any_element(),
            None => Diff::new(&self.diff)
                .flex_1()
                .min_h_0()
                .soft_wrap(true)
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(header)
            .child(body)
            .into_any_element()
    }
}
