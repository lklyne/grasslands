//! A [`MachineLink`] against a real SSH machine: the throwaway
//! `herdr-remote-test` Docker container (`dockerbox`).
//!
//! Ignored by default. Run with
//! `GRASSLANDS_SSH_TEST_TARGET=ssh://lyle@127.0.0.1:2222 scripts/dev-run
//! cargo test -p herdr-client --test machine_link -- --ignored --nocapture`.
//! It stops and starts that container, and starts its herdr server again.

use std::env;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use herdr_client::*;

mod common;

use common::Watch;

const CONTAINER: &str = "herdr-remote-test";
const LABEL: &str = "it-ssh";
const TIMEOUT: Duration = Duration::from_secs(60);

fn profile(target: String, enabled: bool) -> MachineProfile {
    MachineProfile {
        id: "17e5700dfeedbeef".into(),
        label: LABEL.into(),
        target,
        session: "default".into(),
        enabled,
    }
}

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

fn labels(state: &State) -> Vec<String> {
    state
        .workspaces
        .iter()
        .map(|w| w.info.label.clone())
        .collect()
}

#[test]
#[ignore = "needs GRASSLANDS_SSH_TEST_TARGET and the herdr-remote-test container"]
fn link_should_forward_remote_herdr_and_survive_a_restart() {
    let Ok(target) = env::var("GRASSLANDS_SSH_TEST_TARGET") else {
        eprintln!("skipping: GRASSLANDS_SSH_TEST_TARGET is not set");
        return;
    };
    // SAFETY: the only test in this binary that touches the environment,
    // set before any thread of ours reads it.
    unsafe { env::set_var(DEV_MACHINES_ENV, LABEL) };
    let dir = socket_dir().expect("socket dir");

    let link = MachineLink::start(profile(target, true), dir);
    let statuses = link.statuses();
    wait_status(&link, "connected", |s| *s == MachineStatus::Connected);
    let endpoint = link.endpoint();
    assert!(endpoint.socket_path.exists());
    let attach = link.attach_command("term_x").unwrap();
    eprintln!("attach: {attach}");

    let sync = SessionSync::start(endpoint.clone()).unwrap();
    let mut watch = Watch::new(&sync);
    watch.wait("api and infra workspaces", TIMEOUT, |w| {
        let labels = labels(&w.state);
        labels.iter().any(|l| l == "api") && labels.iter().any(|l| l == "infra")
    });
    eprintln!("workspaces: {:?}", labels(&watch.state));

    docker(&["stop", "-t", "1", CONTAINER]);
    wait_status(&link, "offline", |s| matches!(s, MachineStatus::Offline(_)));
    watch.wait("sync reconnecting", TIMEOUT, |w| {
        w.status() != Some(&ConnectionStatus::Connected)
    });

    let before = watch.state.generation;

    docker(&["start", CONTAINER]);
    wait_status(&link, "reachable again", |s| {
        matches!(s, MachineStatus::NeedsSetup(_) | MachineStatus::Connected)
    });
    if matches!(link.status(), MachineStatus::NeedsSetup(_)) {
        // The container boots sshd only. Its herdr server is started here,
        // by the test; the link only ever reads it.
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
    wait_status(&link, "connected again", |s| *s == MachineStatus::Connected);
    watch.wait("sync recovered", TIMEOUT, |w| {
        w.status() == Some(&ConnectionStatus::Connected) && w.state.generation > before
    });
    eprintln!("workspaces after restart: {:?}", labels(&watch.state));
    drop(sync);

    let seen: Vec<_> = std::iter::from_fn(|| statuses.try_recv().ok()).collect();
    eprintln!("statuses: {seen:?}");

    drop(link);
    assert!(!endpoint.socket_path.exists(), "socket left behind");
    assert!(
        !Path::new(&endpoint.socket_path.with_extension("ctl")).exists(),
        "control socket left behind"
    );
}

#[test]
fn disabled_profile_should_start_nothing() {
    let dir = env::temp_dir();
    let link = MachineLink::start(profile("ssh://nobody@127.0.0.1:1".into(), false), dir);
    assert!(matches!(link.status(), MachineStatus::Disabled(_)));
}

#[test]
#[ignore = "needs GRASSLANDS_SSH_TEST_TARGET and the herdr-remote-test container"]
fn link_should_report_sign_in_setup_and_offline() {
    let Ok(target) = env::var("GRASSLANDS_SSH_TEST_TARGET") else {
        eprintln!("skipping: GRASSLANDS_SSH_TEST_TARGET is not set");
        return;
    };
    // SAFETY: the same value the other test sets; nothing else reads it.
    unsafe { env::set_var(DEV_MACHINES_ENV, LABEL) };
    let dir = socket_dir().expect("socket dir");
    let host_port = target
        .trim_start_matches("ssh://")
        .rsplit_once('@')
        .unwrap()
        .1;

    let wrong_user = MachineProfile {
        id: "5167a1a0".into(),
        ..profile(format!("ssh://nobody@{host_port}"), true)
    };
    let link = MachineLink::start(wrong_user, dir.clone());
    wait_status(&link, "sign-in", |s| {
        matches!(s, MachineStatus::NeedsSignIn(_))
    });
    drop(link);

    // A session nobody started: reachable, but no server to forward.
    let no_server = MachineProfile {
        id: "5e7a9000".into(),
        session: "grasslands-it-none".into(),
        ..profile(target, true)
    };
    let link = MachineLink::start(no_server, dir.clone());
    wait_status(&link, "setup", |s| {
        matches!(s, MachineStatus::NeedsSetup(_))
    });
    assert!(link.attach_command("t").is_some());
    drop(link);

    let refused = MachineProfile {
        id: "0ff1100e".into(),
        ..profile("ssh://lyle@127.0.0.1:1".into(), true)
    };
    let link = MachineLink::start(refused, dir);
    wait_status(&link, "offline", |s| matches!(s, MachineStatus::Offline(_)));
}

const CHILD_ENV: &str = "GRASSLANDS_LINK_CHILD";

/// Run by `master_should_end_when_its_owner_exits_without_drop` in a child
/// process: connects, prints the control socket, then exits without
/// dropping the link, the way GPUI's quit does.
#[test]
#[ignore = "a helper the owner-exit test runs in a child process"]
fn owner_child() {
    let Ok(target) = env::var(CHILD_ENV) else {
        return;
    };
    // SAFETY: set before any thread of ours reads it.
    unsafe { env::set_var(DEV_MACHINES_ENV, LABEL) };
    let link = MachineLink::start(
        MachineProfile {
            id: "0e1ec41d".into(),
            ..profile(target, true)
        },
        socket_dir().expect("socket dir"),
    );
    wait_status(&link, "connected", |s| *s == MachineStatus::Connected);
    let control = link.endpoint().socket_path.with_extension("ctl");
    println!("CTL={}", control.display());
    std::process::exit(0);
}

/// ssh processes whose command line names `control`.
fn masters_on(control: &Path) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-axo", "pid,ppid,command"])
        .output()
        .unwrap();
    let needle = control.display().to_string();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains(&needle) && line.contains("ssh"))
        .map(str::to_owned)
        .collect()
}

#[test]
#[ignore = "needs GRASSLANDS_SSH_TEST_TARGET and the herdr-remote-test container"]
fn master_should_end_when_its_owner_exits_without_drop() {
    let Ok(target) = env::var("GRASSLANDS_SSH_TEST_TARGET") else {
        eprintln!("skipping: GRASSLANDS_SSH_TEST_TARGET is not set");
        return;
    };
    let out = Command::new(env::current_exe().unwrap())
        .args(["--exact", "owner_child", "--ignored", "--nocapture"])
        .env(CHILD_ENV, &target)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let control = stdout
        .lines()
        .find_map(|line| line.strip_prefix("CTL="))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("child didn't connect: {stdout}"));
    eprintln!("child exited {}; control {}", out.status, control.display());

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut left = masters_on(&control);
    while !left.is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
        left = masters_on(&control);
    }
    let _ = std::fs::remove_file(control.with_extension("sock"));
    assert!(left.is_empty(), "master outlived its owner: {left:?}");
    assert!(!control.exists(), "control socket left behind");
}
