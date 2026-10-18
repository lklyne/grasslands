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
