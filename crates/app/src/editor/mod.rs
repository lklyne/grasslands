//! The code editor: GPUI Kit's code editor over a list of open files.
//!
//! Open files are grouped per worktree; when the active worktree changes the
//! visible group swaps. The tabs themselves are drawn by the window's
//! titlebar (`workspace`), from `tabs` and `active_index`. Open files are watched, and a file
//! that changes on disk reloads unless it has unsaved edits, in which case
//! the user picks between their edits and the disk contents.

mod buffer;
mod language;
mod titles;
mod watcher;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor as CodeEditor, InputEvent, Position};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::buffer::{Buffer, DiskChange, Prompt};
use self::watcher::FileWatcher;
use crate::state::{AppEvent, AppState};

actions!(editor, [Save, CloseTab]);

const CONTEXT: &str = "Editor";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-s", Save, Some(CONTEXT)),
        KeyBinding::new("cmd-w", CloseTab, Some(CONTEXT)),
    ]);
}

/// The open tabs of one worktree.
#[derive(Default)]
struct Group {
    buffers: Vec<Buffer>,
    active: usize,
}

impl Group {
    fn active(&self) -> Option<&Buffer> {
        self.buffers.get(self.active)
    }

    fn titles(&self) -> Vec<String> {
        let paths: Vec<&Path> = self
            .buffers
            .iter()
            .map(|buffer| buffer.path.as_path())
            .collect();
        titles::titles(&paths)
    }
}

/// One open file as the titlebar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTab {
    pub path: PathBuf,
    pub title: String,
    pub dirty: bool,
}

/// Emitted when a file tab becomes the active one: opened, picked, or
/// brought forward to ask about unsaved edits. The workspace then shows the
/// editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    Activated,
}

impl EventEmitter<EditorEvent> for Editor {}

pub struct Editor {
    state: Entity<AppState>,
    /// Tab groups keyed by worktree root (`None`: no worktree).
    groups: HashMap<Option<PathBuf>, Group>,
    worktree: Option<PathBuf>,
    watcher: Option<FileWatcher>,
    _subscription: Subscription,
    _disk_changes: Task<()>,
}

impl Editor {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event, cx| match event {
            AppEvent::ActiveWorktreeChanged(root) => {
                this.worktree.clone_from(root);
                cx.notify();
            }
            AppEvent::FileMoved { from, to } => this.follow_move(from, to, cx),
            AppEvent::FileTrashed(path) => this.follow_trash(path, cx),
            _ => {}
        });
        let (sender, receiver) = async_channel::unbounded::<Vec<PathBuf>>();
        // GPUI's test scheduler rejects wakeups from the watcher's thread;
        // tests feed `on_disk_change` directly and test the watcher alone.
        let watcher = (!cfg!(test))
            .then(|| FileWatcher::new(sender))
            .transpose()
            .inspect_err(|error| tracing::warn!("editor: file watching is off: {error}"))
            .ok()
            .flatten();
        let disk_changes = cx.spawn_in(window, async move |this, cx| {
            while let Ok(paths) = receiver.recv().await {
                let updated = this.update_in(cx, |this, window, cx| {
                    this.on_disk_change(&paths, window, cx);
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        Self {
            worktree: state.read(cx).active_worktree().cloned(),
            state,
            groups: HashMap::new(),
            watcher,
            _subscription: subscription,
            _disk_changes: disk_changes,
        }
    }

    /// The visible group's tabs, in order.
    pub fn tabs(&self) -> Vec<FileTab> {
        let Some(group) = self.group() else {
            return Vec::new();
        };
        group
            .buffers
            .iter()
            .zip(group.titles())
            .map(|(buffer, title)| FileTab {
                path: buffer.path.clone(),
                title,
                dirty: buffer.is_dirty(),
            })
            .collect()
    }

    /// The selected tab of the visible group, if it has any.
    pub fn active_index(&self) -> Option<usize> {
        self.group()
            .filter(|group| !group.buffers.is_empty())
            .map(|group| group.active)
    }

    /// Gives keyboard focus to the selected file.
    pub fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(buffer) = self.group().and_then(Group::active) {
            buffer.state.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    fn group(&self) -> Option<&Group> {
        self.groups.get(&self.worktree)
    }

    fn group_mut(&mut self) -> &mut Group {
        self.groups.entry(self.worktree.clone()).or_default()
    }

    fn active_buffer_mut(&mut self) -> Option<&mut Buffer> {
        let group = self.groups.get_mut(&self.worktree)?;
        group.buffers.get_mut(group.active)
    }

    /// Open `path` in the active worktree's group, or switch to its tab.
    pub fn open(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        // Watch events report canonical paths (/private/tmp, not /tmp).
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                notify_error(
                    format!("Cannot open {}: {error}", path.display()),
                    window,
                    cx,
                );
                return;
            }
        };
        if let Some(index) = self
            .group()
            .and_then(|group| group.buffers.iter().position(|buffer| buffer.path == path))
        {
            self.activate(index, window, cx);
            return;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                notify_error(
                    format!("Cannot open {}: {error}", path.display()),
                    window,
                    cx,
                );
                return;
            }
        };
        let state = Buffer::new_state(&path, window, cx);
        let changes = cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_buffer_edited(&state, cx);
            }
        });
        let buffer = Buffer::new(path, text, state, changes, window, cx);
        if let Some(watcher) = &mut self.watcher {
            watcher.add(&buffer.path);
        }
        let group = self.group_mut();
        group.buffers.push(buffer);
        let index = group.buffers.len() - 1;
        self.activate(index, window, cx);
    }

    /// Open `path` with the cursor at the start of `line` (0-based).
    pub fn open_at(&mut self, path: &Path, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.open(path, window, cx);
        let Ok(path) = path.canonicalize() else {
            return;
        };
        if let Some(buffer) = self.group().and_then(Group::active)
            && buffer.path == path
        {
            buffer.state.update(cx, |state, cx| {
                state.set_cursor_position(Position::new(line, 0), window, cx)
            });
        }
    }

    /// Selects tab `index` of the visible group and focuses it.
    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group_mut();
        let Some(buffer) = group.buffers.get(index) else {
            return;
        };
        group.active = index;
        buffer.state.update(cx, |state, cx| state.focus(window, cx));
        cx.emit(EditorEvent::Activated);
        cx.notify();
    }

    fn on_buffer_edited(
        &mut self,
        state: &Entity<gpui_kit::component::input::EditorState>,
        cx: &mut Context<Self>,
    ) {
        // The edited buffer may be in a group other than the visible one.
        let buffer = self
            .groups
            .values_mut()
            .flat_map(|group| group.buffers.iter_mut())
            .find(|buffer| &buffer.state == state);
        if let Some(buffer) = buffer {
            let was_dirty = buffer.is_dirty();
            buffer.refresh_dirty(cx);
            if was_dirty != buffer.is_dirty() {
                cx.notify();
            }
        }
    }

    fn on_disk_change(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let mut moved = Vec::new();
        for buffer in self
            .groups
            .values_mut()
            .flat_map(|group| group.buffers.iter_mut())
            .filter(|buffer| paths.contains(&buffer.path))
        {
            buffer.refresh_file_id();
            match buffer.classify(std::fs::read_to_string(&buffer.path)) {
                Ok(DiskChange::Unchanged) => {}
                Ok(DiskChange::Reload(text)) => buffer.take(text, window, cx),
                Ok(DiskChange::Conflict(theirs)) => {
                    buffer.prompt = Some(Prompt::Conflict { theirs })
                }
                Ok(DiskChange::Deleted) => match buffer.find_moved(paths) {
                    Some(to) => moved.push((buffer.path.clone(), to)),
                    None => buffer.mark_deleted(),
                },
                Err(error) => {
                    tracing::warn!("editor: cannot reload {}: {error}", buffer.path.display())
                }
            }
        }
        for (from, to) in moved {
            self.follow_move(&from, &to, cx);
        }
        cx.notify();
    }

    /// Points tabs at or under `from` (a file or folder) at their new place.
    fn follow_move(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        let watcher = &mut self.watcher;
        for buffer in self
            .groups
            .values_mut()
            .flat_map(|group| group.buffers.iter_mut())
        {
            let Ok(rest) = buffer.path.strip_prefix(from) else {
                continue;
            };
            // `join("")` would add a trailing slash.
            let new = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            if let Some(watcher) = watcher {
                watcher.remove(&buffer.path);
                watcher.add(&new);
            }
            buffer.path = new;
            buffer.refresh_file_id();
        }
        cx.notify();
    }

    /// The user trashed `path` (a file or folder) from the tree: clean tabs at
    /// or under it close; tabs with unsaved edits stay, marked deleted.
    fn follow_trash(&mut self, path: &Path, cx: &mut Context<Self>) {
        for group in self.groups.values_mut() {
            let active = group.active().map(|buffer| buffer.path.clone());
            let watcher = &mut self.watcher;
            group.buffers.retain_mut(|buffer| {
                if !buffer.path.starts_with(path) {
                    return true;
                }
                if buffer.is_dirty() {
                    buffer.mark_deleted();
                    return true;
                }
                if let Some(watcher) = watcher {
                    watcher.remove(&buffer.path);
                }
                false
            });
            group.active = active
                .and_then(|active| group.buffers.iter().position(|b| b.path == active))
                .unwrap_or(group.active)
                .min(group.buffers.len().saturating_sub(1));
        }
        cx.notify();
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.active_buffer_mut() else {
            return;
        };
        if let Err(error) = buffer.save(cx) {
            let message = format!("Cannot save {}: {error}", buffer.path.display());
            notify_error(message, window, cx);
        }
        cx.notify();
    }

    fn close_active(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.group().map(|group| group.active) {
            self.request_close(index, window, cx);
        }
    }

    /// Close tab `index`, asking first if it has unsaved edits.
    pub fn request_close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.group_mut().buffers.get_mut(index) else {
            return;
        };
        if buffer.is_dirty() {
            buffer.prompt = Some(Prompt::CloseUnsaved);
            self.activate(index, window, cx);
        } else {
            self.close(index, window, cx);
        }
    }

    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group_mut();
        if index >= group.buffers.len() {
            return;
        }
        let buffer = group.buffers.remove(index);
        if index < group.active || group.active >= group.buffers.len() {
            group.active = group.active.saturating_sub(1);
        }
        let active = group.active;
        let empty = group.buffers.is_empty();
        if let Some(watcher) = &mut self.watcher {
            watcher.remove(&buffer.path);
        }
        if empty {
            cx.notify();
        } else {
            self.activate(active, window, cx);
        }
    }

    /// Answer the active buffer's prompt. `choice` is the button pressed.
    pub fn answer(&mut self, choice: Answer, window: &mut Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.active_buffer_mut() else {
            return;
        };
        let close = match (buffer.prompt.take(), choice) {
            (Some(Prompt::Conflict { theirs }), Answer::KeepMine) => {
                buffer.keep_mine(theirs);
                false
            }
            (Some(Prompt::Conflict { theirs }), Answer::TakeTheirs) => {
                buffer.take(theirs, window, cx);
                false
            }
            (Some(Prompt::CloseUnsaved), Answer::Save) => match buffer.save(cx) {
                Ok(()) => true,
                Err(error) => {
                    let message = format!("Cannot save {}: {error}", buffer.path.display());
                    notify_error(message, window, cx);
                    false
                }
            },
            (Some(Prompt::CloseUnsaved), Answer::Discard) => true,
            (prompt, _) => {
                buffer.prompt = prompt;
                false
            }
        };
        if close && let Some(index) = self.group().map(|group| group.active) {
            self.close(index, window, cx);
            return;
        }
        if let Some(buffer) = self.active_buffer_mut() {
            buffer.refresh_dirty(cx);
        }
        cx.notify();
    }

    fn render_prompt(&self, prompt: &Prompt, cx: &mut Context<Self>) -> impl IntoElement {
        let (message, buttons): (&str, &[(Answer, &str)]) = match prompt {
            Prompt::Conflict { .. } => (
                "This file changed on disk and you have unsaved edits.",
                &[
                    (Answer::KeepMine, "Keep mine"),
                    (Answer::TakeTheirs, "Take theirs"),
                ],
            ),
            Prompt::CloseUnsaved => (
                "Save changes before closing?",
                &[
                    (Answer::Save, "Save"),
                    (Answer::Discard, "Don't save"),
                    (Answer::Cancel, "Cancel"),
                ],
            ),
        };
        h_flex()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .bg(cx.theme().warning.opacity(0.15))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().text_sm().child(message))
            .children(buttons.iter().enumerate().map(|(index, &(answer, label))| {
                let button = Button::new(("prompt", index)).small().label(label);
                let button = if index == 0 { button.primary() } else { button };
                button.on_click(
                    cx.listener(move |this, _, window, cx| this.answer(answer, window, cx)),
                )
            }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    KeepMine,
    TakeTheirs,
    Save,
    Discard,
    Cancel,
}

fn notify_error(message: String, window: &mut Window, cx: &mut App) {
    tracing::error!("editor: {message}");
    window.push_notification(Notification::error(message), cx);
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .id("editor")
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close_active))
            .size_full()
            .overflow_hidden();
        // No empty state: with no files the workspace shows herdr instead.
        let Some(buffer) = self.group().and_then(Group::active) else {
            return root;
        };
        root.when_some(buffer.prompt.as_ref(), |this, prompt| {
            this.child(self.render_prompt(prompt, cx))
        })
        .child(
            div().flex_1().min_h_0().child(
                CodeEditor::new(&buffer.state)
                    .bordered(false)
                    .h(relative(1.)),
            ),
        )
    }
}
