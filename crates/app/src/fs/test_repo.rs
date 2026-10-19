//! Throwaway git repos for tests, built with the git CLI and none of the
//! user's git config.

use std::path::{Path, PathBuf};
use std::process::Command;

/// An empty folder of its own under the temp dir, resolved (`/private/var`).
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("grasslands-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

pub struct TestRepo {
    pub dir: PathBuf,
}

impl TestRepo {
    /// A new repo in `dir` on an unborn `branch`.
    pub fn init(dir: &Path, branch: &str) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", branch]);
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// A repo in `dir` on `main` with one commit holding `a.txt`.
    pub fn on_main(dir: &Path) -> Self {
        let repo = Self::init(dir, "main");
        repo.write("a.txt", "one\ntwo\n");
        repo.commit_all("init");
        repo
    }

    pub fn open(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// Runs git here; its trimmed stdout.
    pub fn git(&self, args: &[&str]) -> String {
        git(&self.dir, args)
    }

    pub fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        let path = self.dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    pub fn remove(&self, path: &str) {
        std::fs::remove_file(self.dir.join(path)).unwrap();
    }

    pub fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
    }
}

/// Runs git in `dir` with no global or system config; its trimmed stdout.
/// Panics if git fails.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
