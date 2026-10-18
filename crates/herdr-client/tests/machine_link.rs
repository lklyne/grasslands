//! A [`MachineLink`] against a real SSH machine: the throwaway
//! `herdr-remote-test` Docker container (`dockerbox`).
//!
//! Ignored by default. Run with
//! `GRASSLANDS_SSH_TEST_TARGET=ssh://lyle@127.0.0.1:2222 scripts/dev-run
//! cargo test -p herdr-client --test machine_link -- --ignored --nocapture`.
//! It stops and starts that container, and starts its herdr server again.

use std::env;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use herdr_client::*;

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

fn wait_sync(sync: &SessionSync, what: &str, pred: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !pred(&sync.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
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
    wait_sync(&sync, "api and infra workspaces", |s| {
        let labels = labels(s);
        labels.iter().any(|l| l == "api") && labels.iter().any(|l| l == "infra")
    });
    eprintln!("workspaces: {:?}", labels(&sync.state()));

    docker(&["stop", "-t", "1", CONTAINER]);
    wait_status(&link, "offline", |s| matches!(s, MachineStatus::Offline(_)));
    wait_sync(&sync, "sync reconnecting", |_| {
        sync.status() != ConnectionStatus::Connected
    });

    let before = sync.state().generation;

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
    wait_sync(&sync, "sync recovered", |s| {
        sync.status() == ConnectionStatus::Connected && s.generation > before
    });
    eprintln!("workspaces after restart: {:?}", labels(&sync.state()));
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
    let host_port = target.trim_start_matches("ssh://").rsplit_once('@').unwrap().1;

    let wrong_user = MachineProfile {
        id: "5167a1a0".into(),
        ..profile(format!("ssh://nobody@{host_port}"), true)
    };
    let link = MachineLink::start(wrong_user, dir.clone());
    wait_status(&link, "sign-in", |s| matches!(s, MachineStatus::NeedsSignIn(_)));
    drop(link);

    // A session nobody started: reachable, but no server to forward.
    let no_server = MachineProfile {
        id: "5e7a9000".into(),
        session: "grasslands-it-none".into(),
        ..profile(target, true)
    };
    let link = MachineLink::start(no_server, dir.clone());
    wait_status(&link, "setup", |s| matches!(s, MachineStatus::NeedsSetup(_)));
    assert!(link.attach_command("t").is_some());
    drop(link);

    let refused = MachineProfile {
        id: "0ff1100e".into(),
        ..profile("ssh://lyle@127.0.0.1:1".into(), true)
    };
    let link = MachineLink::start(refused, dir);
    wait_status(&link, "offline", |s| matches!(s, MachineStatus::Offline(_)));
}
