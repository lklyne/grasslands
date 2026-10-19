//! An in-memory remote machine's disk for tests: `Fs::Fake`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

#[derive(Default)]
pub struct FakeFs {
    files: Mutex<HashMap<PathBuf, (String, SystemTime)>>,
    clock: AtomicU64,
    pub offline: AtomicBool,
    pub reads: AtomicU64,
    /// Folder listings asked for.
    pub lists: AtomicU64,
}

impl FakeFs {
    fn files(
        &self,
    ) -> io::Result<std::sync::MutexGuard<'_, HashMap<PathBuf, (String, SystemTime)>>> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        Ok(self.files.lock().unwrap())
    }

    fn tick(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(self.clock.fetch_add(1, Ordering::SeqCst))
    }

    pub fn put(&self, path: &Path, text: &str) {
        let modified = self.tick();
        self.files
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), (text.to_owned(), modified));
    }

    pub fn remove(&self, path: &Path) {
        self.files.lock().unwrap().remove(path);
    }

    pub fn get(&self, path: &Path) -> Option<String> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .map(|(text, _)| text.clone())
    }

    pub fn read(&self, path: &Path) -> io::Result<String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let files = self.files()?;
        let (text, _) = files.get(path).ok_or(io::ErrorKind::NotFound)?;
        Ok(text.clone())
    }

    pub fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        let modified = self.tick();
        self.files()?
            .insert(path.to_path_buf(), (text.to_owned(), modified));
        Ok(())
    }

    /// `dir`'s files and the folders implied by deeper ones.
    pub fn read_dir(&self, dir: &Path) -> io::Result<Vec<super::Entry>> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        let files = self.files()?;
        let mut entries: Vec<super::Entry> = Vec::new();
        for path in files.keys() {
            let Some(name) = path
                .strip_prefix(dir)
                .ok()
                .and_then(|rest| rest.components().next())
            else {
                continue;
            };
            let path = dir.join(name);
            if entries.iter().any(|e| e.path == path) {
                continue;
            }
            entries.push(super::Entry {
                is_dir: !files.contains_key(&path),
                name: name.as_os_str().to_string_lossy().into_owned(),
                path,
            });
        }
        super::sort_entries(&mut entries);
        Ok(entries)
    }

    pub fn create_file(&self, path: &Path) -> io::Result<()> {
        let modified = self.tick();
        let mut files = self.files()?;
        if files.contains_key(path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        files.insert(path.to_path_buf(), (String::new(), modified));
        Ok(())
    }

    /// Refuses to overwrite `to`; moves everything below a folder `from`.
    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut files = self.files()?;
        if files.keys().any(|path| path.starts_with(to)) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let moved: Vec<PathBuf> = files
            .keys()
            .filter(|path| path.starts_with(from))
            .cloned()
            .collect();
        if moved.is_empty() {
            return Err(io::ErrorKind::NotFound.into());
        }
        for path in moved {
            let file = files.remove(&path).expect("listed above");
            let rest = path.strip_prefix(from).expect("filtered above");
            files.insert(to.join(rest), file);
        }
        Ok(())
    }

    /// Removes `path` and everything below it.
    pub fn remove_all(&self, path: &Path) -> io::Result<()> {
        let mut files = self.files()?;
        let before = files.len();
        files.retain(|file, _| !file.starts_with(path));
        if files.len() == before {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(())
    }

    pub fn metadata(&self, path: &Path) -> io::Result<super::Metadata> {
        let files = self.files()?;
        let (text, modified) = files.get(path).ok_or(io::ErrorKind::NotFound)?;
        Ok(super::Metadata {
            is_dir: false,
            len: text.len() as u64,
            modified: Some(*modified),
        })
    }
}
