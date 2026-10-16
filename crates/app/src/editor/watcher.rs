//! Watches the folders of open files and reports which paths changed.
//!
//! Folders, not files: a tool that saves by writing a temp file and renaming
//! it over the original replaces the inode, and a watch on the old file would
//! go quiet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

pub struct FileWatcher {
    watcher: RecommendedWatcher,
    /// Watched folders and how many open files are in each.
    folders: HashMap<PathBuf, usize>,
}

impl FileWatcher {
    /// Changed paths are sent on `changes`, one batch per filesystem event.
    pub fn new(changes: async_channel::Sender<Vec<PathBuf>>) -> notify::Result<Self> {
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!("editor: watch error: {error}");
                    return;
                }
            };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            // The receiver is gone only when the editor is; nothing to do then.
            let _ = changes.send_blocking(event.paths);
        })?;
        Ok(Self {
            watcher,
            folders: HashMap::new(),
        })
    }

    pub fn add(&mut self, file: &Path) {
        let Some(folder) = file.parent() else { return };
        let count = self.folders.entry(folder.to_path_buf()).or_default();
        if *count == 0
            && let Err(error) = self.watcher.watch(folder, RecursiveMode::NonRecursive)
        {
            tracing::warn!("editor: cannot watch {}: {error}", folder.display());
        }
        *count += 1;
    }

    pub fn remove(&mut self, file: &Path) {
        let Some(folder) = file.parent() else { return };
        let Some(count) = self.folders.get_mut(folder) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.folders.remove(folder);
            // Already gone if the folder was deleted; that is fine.
            let _ = self.watcher.unwatch(folder);
        }
    }
}
