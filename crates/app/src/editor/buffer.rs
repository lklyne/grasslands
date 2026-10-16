//! One open file: its editor state, what was last read from or written to
//! disk, and any question pending for the user.

use std::path::{Path, PathBuf};

use gpui_kit::component::input::EditorState;
use gpui_kit::*;

use super::language;

/// A question shown above the editor until the user answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// The file changed on disk while it had unsaved edits. Holds the new
    /// disk contents.
    Conflict { theirs: String },
    /// The user asked to close the tab with unsaved edits.
    CloseUnsaved,
}

/// What a disk change means for an open buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum DiskChange {
    /// Disk matches what we last saw (often our own save). Nothing to do.
    Unchanged,
    /// No unsaved edits: take the disk contents.
    Reload(String),
    /// Unsaved edits and different disk contents: ask.
    Conflict(String),
    /// The file is gone from disk.
    Deleted,
}

pub struct Buffer {
    pub path: PathBuf,
    pub state: Entity<EditorState>,
    /// The contents last read from or written to disk; `None` once the file
    /// is deleted on disk.
    saved: Option<String>,
    dirty: bool,
    pub prompt: Option<Prompt>,
    _changes: Subscription,
}

impl Buffer {
    pub fn new(
        path: PathBuf,
        text: String,
        state: Entity<EditorState>,
        changes: Subscription,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        state.update(cx, |state, cx| state.set_value(text.clone(), window, cx));
        Self {
            path,
            state,
            saved: Some(text),
            dirty: false,
            prompt: None,
            _changes: changes,
        }
    }

    pub fn new_state(path: &Path, window: &mut Window, cx: &mut App) -> Entity<EditorState> {
        let language = language::for_path(path);
        cx.new(|cx| EditorState::new(window, cx).language(language))
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Recompute the dirty flag after an edit. Undoing back to the saved
    /// text clears it.
    pub fn refresh_dirty(&mut self, cx: &App) {
        self.dirty = self.saved.as_deref() != Some(self.state.read(cx).value().as_ref());
    }

    pub fn save(&mut self, cx: &App) -> std::io::Result<()> {
        let text = self.state.read(cx).value().to_string();
        std::fs::write(&self.path, &text)?;
        self.saved = Some(text);
        self.dirty = false;
        if matches!(self.prompt, Some(Prompt::Conflict { .. })) {
            self.prompt = None;
        }
        Ok(())
    }

    pub fn classify(&self, disk: std::io::Result<String>) -> std::io::Result<DiskChange> {
        let text = match disk {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DiskChange::Deleted);
            }
            Err(error) => return Err(error),
        };
        Ok(if self.saved.as_deref() == Some(text.as_str()) {
            DiskChange::Unchanged
        } else if self.dirty {
            DiskChange::Conflict(text)
        } else {
            DiskChange::Reload(text)
        })
    }

    pub fn mark_deleted(&mut self) {
        self.saved = None;
        self.dirty = true;
    }

    /// Replace the editor contents with `text` from disk, keeping the cursor
    /// where it was as far as the new text allows.
    pub fn take(&mut self, text: String, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            let cursor = state.cursor_position();
            state.set_value(text.clone(), window, cx);
            state.set_cursor_position(cursor, window, cx);
        });
        self.saved = Some(text);
        self.dirty = false;
        self.prompt = None;
    }

    /// Keep the unsaved edits over `theirs`: the buffer stays dirty and the
    /// next save overwrites the file.
    pub fn keep_mine(&mut self, theirs: String) {
        self.saved = Some(theirs);
        self.dirty = true;
        self.prompt = None;
    }
}
