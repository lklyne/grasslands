//! A [`MachineLink`] against a real SSH machine: the throwaway
//! `herdr-remote-test` Docker container (`dockerbox`).
//!
//! Ignored by default. Run with
//! `GRASSLANDS_SSH_TEST_TARGET=ssh://lyle@127.0.0.1:2222 scripts/dev-run
//! cargo test -p herdr-client --test machine_link -- --ignored --nocapture`.
//! It stops and starts that container, and starts its herdr server again.
//! The agent test also needs `GRASSLANDS_TEST_AGENT`: the socket of a
//! throwaway `ssh-agent` holding a throwaway key.
//!
//! herdr's own check runs before each connection, so the profiles here are
//! saved for the herdr CLI in a private `XDG_STATE_HOME` (see `support`).

use std::env;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use herdr_client::*;

mod support;

const CONTAINER: &str = "herdr-remote-test";
const LABEL: &str = "it-ssh";
const TIMEOUT: Duration = Duration::from_secs(60);

fn profile(target: String, enabled: bool) -> MachineProfile {
    MachineProfile {
        id: "17e5700dfeedbeef0000000000000000".into(),
        label: LABEL.into(),
        target,
        session: "default".into(),
        enabled,
    }
}

/// Every profile these tests connect, saved where herdr's check finds them.
fn known(target: &str) -> Vec<MachineProfile> {
    let host_port = target
        .trim_start_matches("ssh://")
        .rsplit_once('@')
        .map_or(target, |(_, host_port)| host_port);
    vec![
        profile(target.to_owned(), true),
        MachineProfile {
            id: WRONG_USER.into(),
            ..profile(format!("ssh://nobody@{host_port}"), true)
        },
        MachineProfile {
            id: NO_SERVER.into(),
            session: "grasslands-it-none".into(),
            ..profile(target.to_owned(), true)
        },
        MachineProfile {
            id: REFUSED.into(),
            ..profile("ssh://lyle@127.0.0.1:1".into(), true)
        },
        MachineProfile {
            id: OWNER_CHILD.into(),
            ..profile(target.to_owned(), true)
        },
        MachineProfile {
            id: AGENT.into(),
            ..profile(target.to_owned(), true)
        },
        MachineProfile {
            id: AGENT_RESTART.into(),
            session: AGENT_SESSION.into(),
            ..profile(target.to_owned(), true)
        },
    ]
}

const WRONG_USER: &str = "5167a1a0000000000000000000000000";
const NO_SERVER: &str = "5e7a9000000000000000000000000000";
const REFUSED: &str = "0ff1100e000000000000000000000000";
const OWNER_CHILD: &str = "0e1ec41d000000000000000000000000";
const AGENT: &str = "a6e47000000000000000000000000000";
const AGENT_RESTART: &str = "a6e47001000000000000000000000000";

fn by_id(profiles: &[MachineProfile], id: &str) -> MachineProfile {
    profiles.iter().find(|p| p.id == id).unwrap().clone()
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
    support::herdr_knows(&known(&target));
    let dir = socket_dir().expect("socket dir");

    let link = MachineLink::start(profile(target, true), dir, support::herdr_bin());
    let statuses = link.statuses();
    wait_status(&link, "connected", |s| *s == MachineStatus::Connected);
    let endpoint = link.endpoint();
    assert!(endpoint.socket_path.exists());
    let attach = link.attach_command("term_x", false).unwrap();
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
    let link = MachineLink::start(
        profile("ssh://nobody@127.0.0.1:1".into(), false),
        dir,
        support::herdr_bin(),
    );
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
    let profiles = known(&target);
    support::herdr_knows(&profiles);
    let dir = socket_dir().expect("socket dir");
    let start = |id| MachineLink::start(by_id(&profiles, id), dir.clone(), support::herdr_bin());

    let link = start(WRONG_USER);
    wait_status(&link, "sign-in", |s| {
        matches!(s, MachineStatus::NeedsSignIn(_))
    });
    drop(link);

    // A session nobody started: reachable, but no server to forward.
    // herdr's check says so, with its own fix.
    let link = start(NO_SERVER);
    wait_status(
        &link,
        "setup",
        |s| matches!(s, MachineStatus::NeedsSetup(reason) if reason.contains("herdr --remote")),
    );
    drop(link);

    let link = start(REFUSED);
    wait_status(&link, "offline", |s| matches!(s, MachineStatus::Offline(_)));
}

/// A throwaway `ssh-agent` socket with one throwaway key, never the user's.
const AGENT_ENV: &str = "GRASSLANDS_TEST_AGENT";

/// `ssh-add -l` against `SSH_AUTH_SOCK=agent`, here.
fn agent_keys(agent: &str) -> String {
    let out = Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", agent)
        .output()
        .unwrap();
    assert!(out.status.success(), "the test agent has no keys");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Waits for `pane_id`'s recent output to contain `needle`.
fn wait_output(client: &Client, pane_id: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let read = client
            .call(
                "pane.read",
                &serde_json::json!({ "pane_id": pane_id, "source": "recent", "lines": 40 }),
            )
            .unwrap()
            .to_string();
        if read.contains(needle) || Instant::now() >= deadline {
            return read;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Whether a new pane on `client`'s server lists the key `fingerprint`
/// with `ssh-add -l`, and what it printed. The pane's workspace is closed
/// again.
fn pane_sees_agent(client: &Client, fingerprint: &str) -> (bool, String) {
    let created = client
        .workspace_create(&NewWorkspace {
            label: Some("grasslands-it-agent".into()),
            ..NewWorkspace::default()
        })
        .unwrap();
    let pane = &created.root_pane.pane_id;
    client
        .call(
            "pane.send_input",
            &serde_json::json!({
                "pane_id": pane, "text": "ssh-add -l; echo AGENT_RC=$?", "keys": ["Enter"],
            }),
        )
        .unwrap();
    let read = wait_output(client, pane, "AGENT_RC=");
    let _ = client.workspace_close(&created.workspace.workspace_id, false);
    (
        read.contains(fingerprint) && read.contains("AGENT_RC=0"),
        read,
    )
}

/// Forwards the throwaway agent from here on: `GRASSLANDS_TEST_AGENT`'s
/// first key fingerprint, or `None` (skip) without one.
fn forward_test_agent() -> Option<String> {
    let agent = env::var(AGENT_ENV).ok()?;
    let keys = agent_keys(&agent);
    // SAFETY: set before any thread of ours reads them. Other tests in this
    // binary may forward the throwaway agent too, which is harmless.
    unsafe {
        env::set_var(DEV_MACHINES_ENV, LABEL);
        env::set_var("SSH_AUTH_SOCK", &agent);
        env::set_var(FORWARD_AGENT_ENV, "1");
    }
    keys.split_whitespace().nth(1).map(str::to_owned)
}

#[test]
#[ignore = "needs GRASSLANDS_SSH_TEST_TARGET, GRASSLANDS_TEST_AGENT and the herdr-remote-test container"]
fn link_should_lend_a_forwarded_agent_to_remote_panes() {
    let (Ok(target), Some(fingerprint)) =
        (env::var("GRASSLANDS_SSH_TEST_TARGET"), forward_test_agent())
    else {
        eprintln!("skipping: GRASSLANDS_SSH_TEST_TARGET or {AGENT_ENV} is not set");
        return;
    };
    let profiles = known(&target);
    support::herdr_knows(&profiles);
    let link = MachineLink::start(
        by_id(&profiles, AGENT),
        socket_dir().expect("socket dir"),
        support::herdr_bin(),
    );
    wait_status(&link, "connected", |s| *s == MachineStatus::Connected);
    let (lent, read) = pane_sees_agent(&Client::new(link.endpoint()), &fingerprint);
    assert!(lent, "the remote pane didn't see the agent:\n{read}");
}

/// `herdr --session <AGENT_SESSION> <args>` on the machine, over plain ssh.
fn remote_herdr(target: &str, args: &str) {
    let host_port = target.trim_start_matches("ssh://");
    let (user_host, port) = host_port.rsplit_once(':').unwrap();
    let status = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-p", port, user_host])
        .arg(format!(
            "~/.local/bin/herdr --session {AGENT_SESSION} {args}"
        ))
        .stdin(Stdio::null())
        .status()
        .unwrap();
    eprintln!("remote herdr {args}: {status}");
}

/// A session of its own on the machine, so restarting it disturbs nobody.
const AGENT_SESSION: &str = "grasslands-it-agent";

/// Starts `AGENT_SESSION`'s server in the container, outside any SSH
/// session: herdr's check only accepts a detached daemon
/// (`detached_server_daemon`), which a server started from an SSH shell
/// isn't.
fn start_agent_server() {
    docker(&[
        "exec",
        "-d",
        "-u",
        "lyle",
        "-w",
        "/home/lyle",
        "-e",
        "HOME=/home/lyle",
        CONTAINER,
        "/home/lyle/.local/bin/herdr",
        "--session",
        AGENT_SESSION,
        "server",
    ]);
    thread::sleep(Duration::from_secs(2));
}

#[test]
#[ignore = "needs GRASSLANDS_SSH_TEST_TARGET, GRASSLANDS_TEST_AGENT and the herdr-remote-test container"]
fn link_should_register_the_agent_again_after_a_server_restart() {
    let (Ok(target), Some(fingerprint)) =
        (env::var("GRASSLANDS_SSH_TEST_TARGET"), forward_test_agent())
    else {
        eprintln!("skipping: GRASSLANDS_SSH_TEST_TARGET or {AGENT_ENV} is not set");
        return;
    };
    start_agent_server();
    let profiles = known(&target);
    support::herdr_knows(&profiles);
    let link = MachineLink::start(
        by_id(&profiles, AGENT_RESTART),
        socket_dir().expect("socket dir"),
        support::herdr_bin(),
    );
    wait_status(&link, "connected", |s| *s == MachineStatus::Connected);
    let client = Client::new(link.endpoint());
    let (before, read_before) = pane_sees_agent(&client, &fingerprint);

    remote_herdr(&target, "server stop");
    start_agent_server();
    // The link keeps its master; it notices the closed registration and
    // registers again within its retry interval.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (mut after, mut read_after) = (false, String::new());
    while !after && Instant::now() < deadline {
        thread::sleep(Duration::from_secs(1));
        if client.ping().is_ok() {
            (after, read_after) = pane_sees_agent(&client, &fingerprint);
        }
    }

    drop(link);
    remote_herdr(&target, "server stop");
    remote_herdr(&target, &format!("session delete {AGENT_SESSION}"));
    assert!(before, "no agent before the restart:\n{read_before}");
    assert!(after, "no agent after the restart:\n{read_after}");
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
    let profiles = known(&target);
    support::herdr_knows(&profiles);
    let link = MachineLink::start(
        by_id(&profiles, OWNER_CHILD),
        socket_dir().expect("socket dir"),
        support::herdr_bin(),
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
