//! Watches the whole active worktree (recursive; FSEvents on macOS) and
//! reports changed paths. The tree only re-lists folders it has loaded.

use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

pub struct RootWatcher {
    _watcher: RecommendedWatcher,
}

impl RootWatcher {
    /// Changed paths are sent on `changes`, one batch per filesystem event.
    pub fn new(root: &Path, changes: async_channel::Sender<Vec<PathBuf>>) -> notify::Result<Self> {
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!("files: watch error: {error}");
                        return;
                    }
                };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                // The receiver is gone only when the tree is; nothing to do then.
                let _ = changes.send_blocking(event.paths);
            })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn watcher_should_report_new_file_in_subfolder() {
        let dir = std::env::temp_dir().join(format!("grasslands-rootwatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        let dir = dir.canonicalize().unwrap();
        let (sender, receiver) = async_channel::unbounded();
        let _watcher = RootWatcher::new(&dir, sender).unwrap();
        // FSEvents needs a moment before it delivers.
        std::thread::sleep(Duration::from_millis(300));
        let file = dir.join("a/b/new.txt");
        std::fs::write(&file, "x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(paths) = receiver.try_recv()
                && paths.iter().any(|p| p.ends_with("a/b/new.txt"))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no event for {}", file.display());
    }
}
