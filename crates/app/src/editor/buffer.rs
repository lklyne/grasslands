//! One open file: its editor state, what was last read from or written to
//! disk, and any question pending for the user.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use gpui_kit::component::input::EditorState;
use gpui_kit::*;

use super::language;
use super::poll::Stamp;
use crate::state::MachineId;

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
    /// Whose disk the file is on. With `path`, the buffer's identity: the
    /// same path on two machines is two buffers.
    pub machine: MachineId,
    pub path: PathBuf,
    pub state: Entity<EditorState>,
    /// The contents last read from or written to disk; `None` once the file
    /// is deleted on disk.
    saved: Option<String>,
    dirty: bool,
    /// The text of a save still being written; the write it causes isn't a
    /// change from elsewhere.
    saving: Option<String>,
    pub prompt: Option<Prompt>,
    /// Device and inode of the file, to recognise it after a move on disk.
    /// Local only.
    file_id: Option<FileId>,
    /// The modification time and size last seen on a remote machine, for
    /// polling.
    pub stamp: Option<Stamp>,
    /// Bumped whenever what the buffer knows of its disk copy changes (a
    /// save starting or finishing, a reload, a deletion), so a poll that
    /// read the disk before then can tell its result is stale.
    generation: u64,
    _changes: Subscription,
}

type FileId = (u64, u64);

fn file_id(path: &Path) -> Option<FileId> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

impl Buffer {
    pub fn new(
        (machine, path): (MachineId, PathBuf),
        (text, stamp): (String, Option<Stamp>),
        state: Entity<EditorState>,
        changes: Subscription,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        state.update(cx, |state, cx| state.set_value(text.clone(), window, cx));
        Self {
            file_id: machine.is_local().then(|| file_id(&path)).flatten(),
            machine,
            path,
            state,
            saved: Some(text),
            dirty: false,
            saving: None,
            prompt: None,
            stamp,
            generation: 0,
            _changes: changes,
        }
    }

    pub fn is(&self, machine: &MachineId, path: &Path) -> bool {
        &self.machine == machine && self.path == path
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

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// Starts a save: returns the text to write, or `None` if one is already
    /// being written.
    pub fn begin_save(&mut self, cx: &App) -> Option<String> {
        if self.saving.is_some() {
            return None;
        }
        let text = self.state.read(cx).value().to_string();
        self.saving = Some(text.clone());
        self.generation += 1;
        Some(text)
    }

    /// The write started by `begin_save` finished. On success the written
    /// text becomes the saved text; edits made meanwhile stay dirty.
    pub fn finish_save(&mut self, written: std::io::Result<Option<Stamp>>, cx: &App) {
        let text = self.saving.take();
        self.generation += 1;
        let (Ok(stamp), Some(text)) = (written, text) else {
            return;
        };
        self.refresh_file_id();
        self.stamp = stamp;
        self.saved = Some(text);
        self.refresh_dirty(cx);
        if matches!(self.prompt, Some(Prompt::Conflict { .. })) {
            self.prompt = None;
        }
    }

    pub fn classify(&self, disk: std::io::Result<String>) -> std::io::Result<DiskChange> {
        let text = match disk {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DiskChange::Deleted);
            }
            Err(error) => return Err(error),
        };
        let ours = [self.saved.as_deref(), self.saving.as_deref()];
        Ok(if ours.contains(&Some(text.as_str())) {
            DiskChange::Unchanged
        } else if self.dirty {
            DiskChange::Conflict(text)
        } else {
            DiskChange::Reload(text)
        })
    }

    /// Re-reads the file's identity; kept when the file is gone, so a later
    /// `find_moved` can still recognise it.
    pub fn refresh_file_id(&mut self) {
        if !self.machine.is_local() {
            return;
        }
        if let Some(id) = file_id(&self.path) {
            self.file_id = Some(id);
        }
    }

    /// Where the file went, if it vanished from `path` because it was moved:
    /// a path among `hints` (the watcher's batch) or in its old folder that is
    /// the same file (same inode).
    pub fn find_moved(&self, hints: &[PathBuf]) -> Option<PathBuf> {
        let id = self.file_id?;
        let siblings = self
            .path
            .parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path());
        hints
            .iter()
            .cloned()
            .chain(siblings)
            .find(|path| *path != self.path && file_id(path) == Some(id))
    }

    pub fn mark_deleted(&mut self) {
        self.generation += 1;
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
        self.generation += 1;
    }

    /// Keep the unsaved edits over `theirs`: the buffer stays dirty and the
    /// next save overwrites the file.
    pub fn keep_mine(&mut self, theirs: String) {
        self.generation += 1;
        self.saved = Some(theirs);
        self.dirty = true;
        self.prompt = None;
    }
}
