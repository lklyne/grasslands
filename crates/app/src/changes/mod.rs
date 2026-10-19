//! The Changes tab: the active worktree's diff against its parent branch
//! (see `crate::fs::git`) in GPUI Kit's diff viewer, on whichever machine
//! the worktree is (through `crate::fs::Fs`). Opened from the file tree's git footer. It
//! follows the active worktree and re-reads while open (a saved machine's
//! only while the window is active) and when the window becomes active,
//! keeping its scroll place when the patch hasn't changed.

#[cfg(test)]
mod tests;

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use crate::state::{ActiveRoot, AppEvent, AppState};

const REFRESH: Duration = Duration::from_secs(3);

pub struct Changes {
    state: Entity<AppState>,
    diff: Entity<DiffState>,
    root: Option<ActiveRoot>,
    /// The patch on screen; `None` before the first read or outside git.
    patch: Option<String>,
    loaded: bool,
    /// A read still out; reloads wait for it, so a stalled link doesn't
    /// pile them up. Dropped when the worktree changes.
    reading: Option<Task<()>>,
    _subscriptions: [Subscription; 3],
    _refresh: Task<()>,
}

impl Changes {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, state, event, cx| match event {
            AppEvent::ActiveRootChanged(root) => {
                this.root.clone_from(root);
                this.patch = None;
                this.loaded = false;
                this.reading = None;
                this.reload(cx);
                cx.notify();
            }
            // The remote placeholder names the selected machine.
            AppEvent::MachineSelected(_) => cx.notify(),
            AppEvent::MachinesChanged if !state.read(cx).selected().is_local() => cx.notify(),
            _ => {}
        });
        let refresh = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                let reloaded = this.update_in(cx, |this, window, cx| {
                    let local = this.root.as_ref().is_none_or(|r| r.machine.is_local());
                    if local || window.is_window_active() {
                        this.reload(cx);
                    }
                });
                if reloaded.is_err() {
                    break;
                }
            }
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.reload(cx);
            }
        });
        let diff = cx.new(|cx| DiffState::new([], cx));
        // The diff notifies itself after a paint (wrapped rows take the
        // width it just measured); redraw with it, or wrapping stays blank.
        let redraw = cx.observe(&diff, |_, _, cx| cx.notify());
        let mut this = Self {
            state: state.clone(),
            diff,
            root: state.read(cx).active_root(cx),
            patch: None,
            loaded: false,
            reading: None,
            _subscriptions: [subscription, redraw, activation],
            _refresh: refresh,
        };
        this.reload(cx);
        this
    }

    /// Re-reads the patch on the background executor, unless a read is
    /// still out.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.reading.is_some() {
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let Some(fs) = self.state.read(cx).fs_for(&root.machine, cx) else {
            return;
        };
        self.reading = Some(cx.spawn(async move |this, cx| {
            let patch = cx
                .background_executor()
                .spawn({
                    let path = root.path.clone();
                    async move { fs.git_patch(&path) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.reading = None;
                // A failed read (the machine dropped) keeps what's shown.
                if let Ok(patch) = patch {
                    this.apply(&root, patch, cx);
                }
            });
        }));
    }

    fn apply(&mut self, root: &ActiveRoot, patch: Option<String>, cx: &mut Context<Self>) {
        // A read for a worktree we have since left.
        if self.root.as_ref() != Some(root) {
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

    /// The patch on screen; `None` before the first read or outside git.
    pub fn patch(&self) -> Option<&str> {
        self.patch.as_deref()
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
        // An offline machine's files can't be read, like the file panel's.
        let machine = self.state.read(cx).selected_machine(cx);
        if !machine.id().is_local() && !machine.status().is_usable() {
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
