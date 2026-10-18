//! [`RemoteFs`] against a real SSH machine: the throwaway `herdr-remote-test`
//! Docker container (`dockerbox`), whose `~/api` is a git repo on branch
//! `feature` with `src/main.rs` modified, `notes.txt` untracked and `target/`
//! ignored.
//!
//! Ignored by default. Run with
//! `GRASSLANDS_SSH_TEST_TARGET=ssh://lyle@127.0.0.1:2222 scripts/dev-run
//! cargo test -p herdr-client --test remote_fs -- --ignored --nocapture`.
//! It writes under `~/scratch`, stops and starts that container, and starts
//! its herdr server again.

use std::env;
use std::io;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use herdr_client::*;

const CONTAINER: &str = "herdr-remote-test";
const LABEL: &str = "it-rfs";
const TIMEOUT: Duration = Duration::from_secs(60);
const EXEC_TIMEOUT: Duration = Duration::from_secs(10);

fn wait_status(link: &MachineLink, what: &str, pred: impl Fn(&MachineStatus) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let status = link.status();
        if pred(&status) {
            eprintln!("{what}: {status:?}");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; status {status:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn docker(args: &[&str]) {
    let status = Command::new("docker").args(args).status().unwrap();
    assert!(status.success(), "docker {args:?} failed");
}

/// Waits for `Connected`, starting the container's herdr server if needed.
fn connect(link: &MachineLink) {
    wait_status(link, "reachable", |s| {
        matches!(s, MachineStatus::NeedsSetup(_) | MachineStatus::Connected)
    });
    if matches!(link.status(), MachineStatus::NeedsSetup(_)) {
        // The container boots sshd only; the test starts its herdr server.
        docker(&[
            "exec",
            "-d",
            "-u",
            "lyle",
            "-e",
            "HOME=/home/lyle",
            "-w",
            "/home/lyle",
            CONTAINER,
            "/home/lyle/.local/bin/herdr",
            "server",
        ]);
        thread::sleep(Duration::from_secs(1));
        link.reconnect();
    }
    wait_status(link, "connected", |s| *s == MachineStatus::Connected);
}

fn names(fs: &RemoteFs, dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs
        .read_dir(dir)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    names
}

fn mode(fs: &RemoteFs, path: &Path) -> String {
    let path = path.to_str().unwrap();
    let out = fs
        .exec(&["stat", "-c", "%a", path], None, EXEC_TIMEOUT)
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn per_call(what: &str, n: u32, mut call: impl FnMut()) {
    let start = Instant::now();
    for _ in 0..n {
        call();
    }
    let each = start.elapsed() / n;
    eprintln!("{what}: {each:?} per call over {n}");
}

#[test]
#[ignore = "needs GRASSLANDS_SSH_TEST_TARGET and the herdr-remote-test container"]
fn remote_fs_should_read_write_exec_and_survive_a_restart() {
    let Ok(target) = env::var("GRASSLANDS_SSH_TEST_TARGET") else {
        eprintln!("skipping: GRASSLANDS_SSH_TEST_TARGET is not set");
        return;
    };
    // SAFETY: the only test in this binary that touches the environment,
    // set before any thread of ours reads it.
    unsafe { env::set_var(DEV_MACHINES_ENV, LABEL) };
    let profile = MachineProfile {
        id: "4f5e0000feedbeef".into(),
        label: LABEL.into(),
        target,
        session: "default".into(),
        enabled: true,
    };
    let link = MachineLink::start(profile, socket_dir().expect("socket dir"));
    assert!(link.remote_fs().is_none(), "no files before connecting");
    connect(&link);
    let fs = link.remote_fs().expect("remote fs once connected");

    let home = fs.home().unwrap();
    eprintln!("home: {}", home.display());
    let api = home.join("api");

    let listed = names(&fs, &api);
    eprintln!("~/api: {listed:?}");
    for name in [".gitignore", "src", "target"] {
        assert!(listed.iter().any(|n| n == name), "~/api lacks {name}");
    }
    let src = fs
        .read_dir(&api)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "src");
    assert!(src.is_some_and(|e| e.is_dir));
    assert!(!names(&fs, &api).iter().any(|n| n == "." || n == ".."));

    let main = fs.read(&api.join("src/main.rs")).unwrap();
    assert!(!main.is_empty());
    let meta = fs.metadata(&api.join("src/main.rs")).unwrap();
    assert_eq!(meta.len, main.len() as u64);
    assert!(!meta.is_dir && meta.modified.is_some());
    assert_eq!(
        fs.read(&api.join("nope.txt")).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    // Scratch files.
    let scratch = home.join("scratch");
    match fs.create_dir(&scratch) {
        Ok(()) => {}
        Err(err) => assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}"),
    }
    let dir = scratch.join(format!("rfs-{}", std::process::id()));
    fs.create_dir(&dir).unwrap();
    assert_eq!(
        fs.create_dir(&dir).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );

    let file = dir.join("a.txt");
    fs.create_file(&file).unwrap();
    assert_eq!(
        fs.create_file(&file).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(fs.read(&file).unwrap().is_empty());

    // Bigger than one window of chunks, so reads and writes pipeline.
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let start = Instant::now();
    fs.write(&file, &big).unwrap();
    let wrote = start.elapsed();
    let start = Instant::now();
    assert_eq!(fs.read(&file).unwrap(), big);
    eprintln!("3 MB: write {wrote:?}, read {:?}", start.elapsed());

    fs.exec(
        &["chmod", "640", file.to_str().unwrap()],
        None,
        EXEC_TIMEOUT,
    )
    .unwrap();
    fs.write(&file, b"hello\n").unwrap();
    assert_eq!(fs.read(&file).unwrap(), b"hello\n");
    assert_eq!(mode(&fs, &file), "640", "write keeps the mode");
    assert_eq!(names(&fs, &dir), ["a.txt"], "no temp file left behind");

    let new = dir.join("new.txt");
    fs.write(&new, b"fresh").unwrap();
    assert_eq!(fs.read(&new).unwrap(), b"fresh");

    let moved = dir.join("b.txt");
    fs.rename(&file, &moved).unwrap();
    assert_eq!(
        fs.rename(&moved, &new).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs.read(&new).unwrap(), b"fresh", "rename didn't clobber");
    assert_eq!(names(&fs, &dir), ["b.txt", "new.txt"]);

    fs.remove(&moved).unwrap();
    fs.create_dir(&dir.join("sub")).unwrap();
    fs.write(&dir.join("sub/c.txt"), b"c").unwrap();
    fs.remove(&dir).unwrap();
    assert_eq!(
        fs.metadata(&dir).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    // Commands.
    let out = fs
        .exec(&["git", "status", "--porcelain"], Some(&api), EXEC_TIMEOUT)
        .unwrap();
    let porcelain = String::from_utf8_lossy(&out.stdout);
    eprintln!("git status:\n{porcelain}");
    assert!(out.status.success());
    assert!(porcelain.contains(" M src/main.rs"));
    assert!(porcelain.contains("?? notes.txt"));
    assert!(!porcelain.contains("target"));
    let out = fs
        .exec(
            &["printf", "%s|", "a b", "it's", "$HOME"],
            None,
            EXEC_TIMEOUT,
        )
        .unwrap();
    assert_eq!(out.stdout, b"a b|it's|$HOME|");
    let err = fs
        .exec(&["sleep", "5"], None, Duration::from_millis(500))
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);

    per_call("metadata", 100, || {
        fs.metadata(&api).unwrap();
    });
    per_call("read_dir ~/api", 50, || {
        fs.read_dir(&api).unwrap();
    });
    per_call("exec true", 20, || {
        fs.exec(&["true"], None, EXEC_TIMEOUT).unwrap();
    });

    // Offline and back.
    docker(&["stop", "-t", "1", CONTAINER]);
    wait_status(&link, "offline", |s| !matches!(s, MachineStatus::Connected));
    let start = Instant::now();
    assert_eq!(fs.home().unwrap_err().kind(), io::ErrorKind::NotConnected);
    assert_eq!(
        fs.exec(&["true"], None, EXEC_TIMEOUT).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
    assert!(start.elapsed() < Duration::from_millis(100), "fails fast");

    docker(&["start", CONTAINER]);
    connect(&link);
    assert_eq!(fs.home().unwrap(), home);
    assert!(!fs.read(&api.join("src/main.rs")).unwrap().is_empty());
    let same = link.remote_fs().unwrap();
    assert!(std::sync::Arc::ptr_eq(&fs, &same));
}
