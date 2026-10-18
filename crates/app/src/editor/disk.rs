//! Where a buffer's file is read from and saved to. Blocking: call it off the
//! UI thread.
//!
//! TEMPORARY: Local goes straight to `std::fs` until `Fs::Local` is filled
//! in; Remote goes through `crate::fs::Fs`. Once `AppState::fs_for` lands,
//! `Disk` becomes `Fs` and `disk_for` becomes that call.

use std::io;
use std::path::Path;
use std::time::SystemTime;

use crate::fs::Fs;
use crate::state::{AppState, MachineId};

#[derive(Clone)]
pub enum Disk {
    Local,
    Remote(Fs),
    #[cfg(test)]
    Fake(std::sync::Arc<fake::FakeDisk>),
}

/// `machine`'s files; `None` while it isn't connected.
pub fn disk_for(state: &AppState, machine: &MachineId) -> Option<Disk> {
    let MachineId::Remote(_) = machine else {
        return Some(Disk::Local);
    };
    let entry = state.machine(machine)?;
    let files = entry.remote_fs()?;
    Some(Disk::Remote(Fs::Remote {
        machine: machine.clone(),
        label: entry.label().to_owned(),
        files,
    }))
}

impl Disk {
    pub fn read(&self, path: &Path) -> io::Result<String> {
        match self {
            Self::Local => std::fs::read_to_string(path),
            Self::Remote(fs) => fs.read_to_string(path),
            #[cfg(test)]
            Self::Fake(fake) => fake.read(path),
        }
    }

    pub fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        match self {
            Self::Local => std::fs::write(path, text),
            Self::Remote(fs) => fs.write(path, text),
            #[cfg(test)]
            Self::Fake(fake) => fake.write(path, text),
        }
    }

    /// The file's modification time, `None` if the machine doesn't report one.
    pub fn modified(&self, path: &Path) -> io::Result<Option<SystemTime>> {
        match self {
            Self::Local => Ok(std::fs::metadata(path)?.modified().ok()),
            Self::Remote(fs) => Ok(fs.metadata(path)?.modified),
            #[cfg(test)]
            Self::Fake(fake) => fake.modified(path),
        }
    }
}

/// An in-memory remote disk for tests.
#[cfg(test)]
pub mod fake {
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    #[derive(Default)]
    pub struct FakeDisk {
        files: Mutex<HashMap<PathBuf, (String, SystemTime)>>,
        clock: AtomicU64,
        pub offline: AtomicBool,
        pub reads: AtomicU64,
    }

    impl FakeDisk {
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

        pub fn modified(&self, path: &Path) -> io::Result<Option<std::time::SystemTime>> {
            let files = self.files()?;
            let (_, modified) = files.get(path).ok_or(io::ErrorKind::NotFound)?;
            Ok(Some(*modified))
        }
    }
}
