//! herdr's saved SSH machines, and one SSH connection to each.
//!
//! herdr keeps the machine list (`herdr machine list --json`); this module
//! never edits it. Before each connection, herdr itself checks the machine
//! (`herdr machine status <id> --json`), and its verdict decides sign-in,
//! setup and offline. A [`MachineLink`] then runs one `ssh` control master
//! per machine and forwards the remote herdr socket to a local socket file,
//! so [`Endpoint`] and `SessionSync` work against it unchanged. A supervisor
//! thread reopens the master with backoff at the same socket path whenever it
//! drops. When the master forwards an SSH agent (the user's ssh_config
//! `ForwardAgent`, as for `herdr --remote`), the link registers it with the
//! remote herdr, so the machine's panes can use it. See
//! `docs/decisions/0019-remote-machines.md`.
//!
//! The master's stdin is a pipe from this process and its remote command
//! waits for EOF on it, so the master ends with the app however the app
//! exits. Socket files are named per process and per link, so app instances
//! never share them; files of processes that are gone are removed once.
//!
//! The remote herdr server is only ever read: never installed, started,
//! restarted or upgraded from here.

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, Once, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::client::{Client, SshAgentRegistration};
use crate::endpoint::{DEV_ROOT, Endpoint};
use crate::error::{Error, Result};
use crate::ids::{ProfileId, TerminalId};
use crate::remote_fs::RemoteFs;
use crate::types::ServerCapabilities;

/// Comma-separated machine labels a debug build may connect to.
pub const DEV_MACHINES_ENV: &str = "GRASSLANDS_DEV_MACHINES";
/// Debug builds: `1` adds `-o ForwardAgent=yes` to every master, so agent
/// forwarding can be tested without editing `~/.ssh/config`.
pub const FORWARD_AGENT_ENV: &str = "RT_FORWARD_AGENT";

/// `sun_path` on macOS, including the terminating NUL.
const SUN_PATH_MAX: usize = 104;
/// ssh binds a control socket at `<ControlPath>.<16 random chars>` first,
/// then renames it into place.
const CONTROL_TEMP_SUFFIX: usize = 17;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Retry interval while sign-in or setup is needed: the fix happens in a
/// terminal, and [`MachineLink::reconnect`] picks it up sooner.
const SLOW_RETRY: Duration = Duration::from_secs(30);
/// Longer than ssh's own `ConnectTimeout`, which normally ends the wait first.
const MASTER_READY_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// `herdr machine status` uses ssh's 10 s connect timeout, then checks herdr.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for the master to say where its forwarded agent is.
const AGENT_LINE_TIMEOUT: Duration = Duration::from_secs(5);
/// Retry interval for an agent registration herdr refused or dropped.
const AGENT_RETRY: Duration = Duration::from_secs(5);
/// How long a master gets to exit after SIGTERM before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(100);

/// herdr's usual install locations, for non-interactive SSH shells whose
/// `PATH` lacks them. `herdr machine status` has already found a matching
/// herdr there, but doesn't say where or which socket its server uses, and
/// the attach command and the socket forward need both. Double quotes only:
/// the script is single-quoted for the remote login shell, which may be fish.
const FIND_HERDR: &str = "command -v herdr || for p in \"$HOME/.local/bin/herdr\" \
    /opt/homebrew/bin/herdr /usr/local/bin/herdr /home/linuxbrew/.linuxbrew/bin/herdr \
    \"$HOME/.local/share/mise/shims/herdr\" \"$HOME/.nix-profile/bin/herdr\"; \
    do if [ -x \"$p\" ]; then echo \"$p\"; exit 0; fi; done; exit 1";

/// The master's remote command: prints the session's `SSH_AUTH_SOCK` (an
/// empty line when ssh forwards no agent), then waits until the app's end
/// of its stdin closes. Works under sh, fish and csh.
const MASTER_COMMAND: &str = "printenv SSH_AUTH_SOCK; echo; cat >/dev/null";

/// Numbers the links of this process, so a link being torn down never
/// shares files with its replacement.
static NEXT_LINK: AtomicU32 = AtomicU32::new(0);
/// Stale files from earlier processes are cleaned once per process.
static CLEANED: Once = Once::new();

/// A saved SSH machine, from `herdr machine list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MachineProfile {
    pub id: ProfileId,
    pub label: String,
    /// e.g. "ssh://lyle@127.0.0.1:2222" or an ssh_config alias.
    pub target: String,
    /// "default" means no `--session`.
    #[serde(default = "default_session")]
    pub session: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_session() -> String {
    "default".to_owned()
}

fn default_enabled() -> bool {
    true
}

impl MachineProfile {
    /// The `--session` to pass to the remote herdr, if any.
    fn remote_session(&self) -> Option<&str> {
        Some(self.session.as_str()).filter(|s| !s.is_empty() && *s != "default")
    }
}

/// Runs `<herdr_bin> machine list --json`.
pub fn list_machines(herdr_bin: &Path) -> Result<Vec<MachineProfile>> {
    let output = Command::new(herdr_bin)
        .args(["machine", "list", "--json"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Error::Protocol(format!(
            "herdr machine list: {} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    parse_machines(&output.stdout)
}

fn parse_machines(json: &[u8]) -> Result<Vec<MachineProfile>> {
    Ok(serde_json::from_slice(json)?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStatus {
    Connecting,
    Connected,
    /// SSH needs a password, passphrase or host-key approval (BatchMode failed).
    NeedsSignIn(String),
    /// herdr on the machine is missing, not running, too old or incompatible.
    NeedsSetup(String),
    /// Couldn't reach it, or the connection dropped; retrying with backoff.
    Offline(String),
    /// Disabled in herdr, or not in GRASSLANDS_DEV_MACHINES in a debug build.
    Disabled(String),
}

/// `$HOME/.local/share/grasslands-herdr/m` in debug builds,
/// `$HOME/Library/Caches/grasslands/m` in release; created 0700.
pub fn socket_dir() -> Option<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME").filter(|h| !h.is_empty())?);
    let dir = if cfg!(debug_assertions) {
        home.join(DEV_ROOT).join("m")
    } else {
        home.join("Library/Caches/grasslands/m")
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .and_then(|()| fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)))
        .inspect_err(|err| log::warn!("machine socket dir {}: {err}", dir.display()))
        .ok()?;
    Some(dir)
}

/// Debug builds: whether `label` is listed in GRASSLANDS_DEV_MACHINES
/// (comma-separated). Release builds: always true.
pub fn dev_allowed(label: &str) -> bool {
    !cfg!(debug_assertions) || listed(env::var(DEV_MACHINES_ENV).ok().as_deref(), label)
}

fn listed(list: Option<&str>, label: &str) -> bool {
    list.is_some_and(|list| list.split(',').any(|item| item.trim() == label))
}

/// Where ssh should connect: the `[user@]host` (or alias) argument and a port.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SshTarget {
    host: String,
    port: Option<u16>,
}

impl SshTarget {
    /// Reads `ssh://[user@]host[:port]`, `user@host` or an ssh_config alias.
    fn parse(target: &str) -> Option<Self> {
        let target = target.trim();
        let Some(rest) = target.strip_prefix("ssh://") else {
            return Self::checked(target.to_owned(), None);
        };
        let rest = rest.trim_end_matches('/');
        let (user, hostport) = match rest.rsplit_once('@') {
            Some((user, hostport)) => (Some(user), hostport),
            None => (None, rest),
        };
        let (host, port) = if let Some(bracketed) = hostport.strip_prefix('[') {
            // IPv6: [::1]:22
            let (host, after) = bracketed.split_once(']')?;
            let port = match after.strip_prefix(':') {
                Some(port) => Some(port.parse().ok()?),
                None if after.is_empty() => None,
                None => return None,
            };
            (host, port)
        } else {
            match hostport.rsplit_once(':') {
                Some((host, port)) => (host, Some(port.parse().ok()?)),
                None => (hostport, None),
            }
        };
        let host = match user {
            Some(user) if !user.is_empty() => format!("{user}@{host}"),
            _ => host.to_owned(),
        };
        Self::checked(host, port)
    }

    /// Refuses anything ssh could read as an option, or that isn't one word.
    fn checked(host: String, port: Option<u16>) -> Option<Self> {
        let bad = host.is_empty()
            || host.starts_with('-')
            || host.ends_with('@')
            || host.chars().any(|c| c.is_whitespace() || c.is_control());
        (!bad).then_some(Self { host, port })
    }

    fn placeholder() -> Self {
        Self {
            host: "grasslands".to_owned(),
            port: None,
        }
    }

    /// `-p <port>`, when there is one.
    fn port_args(&self) -> Vec<String> {
        self.port
            .map(|port| vec!["-p".to_owned(), port.to_string()])
            .unwrap_or_default()
    }
}

/// The control socket and forwarded herdr socket of one machine.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Paths {
    control: PathBuf,
    socket: PathBuf,
}

impl Paths {
    /// `<pid>-<link>-<id8>.{ctl,sock}`.
    fn new(dir: &Path, pid: u32, link: u32, id: &str) -> Self {
        let id8: String = id.chars().take(8).collect();
        let stem = format!("{pid}-{link}-{id8}");
        Self {
            control: dir.join(format!("{stem}.ctl")),
            socket: dir.join(format!("{stem}.sock")),
        }
    }

    /// Whether both sockets can be bound (`sun_path` limit).
    fn fit(&self) -> bool {
        let len = |path: &Path| path.as_os_str().len();
        len(&self.socket) < SUN_PATH_MAX && len(&self.control) + CONTROL_TEMP_SUFFIX < SUN_PATH_MAX
    }

    fn remove(&self) {
        remove_file(&self.socket);
        remove_file(&self.control);
    }

    /// The forwarded socket as an endpoint that never starts a server.
    fn endpoint(&self) -> Endpoint {
        Endpoint {
            bin: PathBuf::from("herdr"),
            session: None,
            socket_path: self.socket.clone(),
            auto_start: false,
            server_log: None,
        }
    }
}

/// The process that owns a socket file name, if it is one of ours. ssh's
/// temporary `<ctl>.<random>` names count too.
fn owner(file_name: &str) -> Option<u32> {
    let (stem, _) = file_name.split_once('.')?;
    let (pid, rest) = stem.split_once('-')?;
    (!rest.is_empty()).then_some(())?;
    pid.parse().ok()
}

fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Removes the files of processes that are gone, first asking any master
/// still listening on a leftover control socket to exit. Files of live
/// processes, and names that aren't ours, are left alone.
fn clean_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(owner) else {
            continue;
        };
        if pid == me || alive(pid) {
            continue;
        }
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "ctl") {
            exit_master(&path);
        }
        log::info!("removing stale {}", path.display());
        remove_file(&path);
    }
}

/// `ssh -S <ctl> -O exit`, for a master this link didn't start. ssh needs a
/// host argument but doesn't use it here.
fn exit_master(control: &Path) {
    let args = control_args(&SshTarget::placeholder(), control, "exit", &[]);
    let _ = run(ssh(&args), Duration::from_secs(2));
}

fn remove_file(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        log::warn!("removing {}: {err}", path.display());
    }
}

/// Debug builds: whether [`FORWARD_AGENT_ENV`] asks for agent forwarding.
fn forced_agent_forwarding() -> bool {
    cfg!(debug_assertions) && env::var(FORWARD_AGENT_ENV).is_ok_and(|v| v == "1")
}

/// `ssh -M -T … <target> <MASTER_COMMAND>`: the control master, which holds
/// the connection open until its stdin closes. Agent forwarding follows the
/// user's ssh_config unless `forward_agent` forces it on.
fn master_args(target: &SshTarget, control: &Path, forward_agent: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "-M",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ControlPersist=no",
        "-o",
        "StreamLocalBindUnlink=yes",
        "-o",
    ]
    .map(str::to_owned)
    .into();
    args.push(format!("ControlPath={}", control.display()));
    if forward_agent {
        args.extend(["-o".to_owned(), "ForwardAgent=yes".to_owned()]);
    }
    args.extend(target.port_args());
    args.push(target.host.clone());
    args.push(MASTER_COMMAND.to_owned());
    args
}

/// `ssh -S <ctl> -O <op> [extra…] <target>`: a request to the master.
fn control_args(target: &SshTarget, control: &Path, op: &str, extra: &[String]) -> Vec<String> {
    let mut args = vec![
        "-S".to_owned(),
        control.display().to_string(),
        "-O".to_owned(),
        op.to_owned(),
    ];
    args.extend_from_slice(extra);
    args.push(target.host.clone());
    args
}

/// `ssh -S <ctl> -o ControlMaster=no [-t] [-p port] <target> <command>`: a
/// command on the machine, through the master.
fn remote_args(target: &SshTarget, control: &Path, tty: bool, command: &str) -> Vec<String> {
    let mut args = Vec::new();
    if tty {
        args.push("-t".to_owned());
    }
    args.extend([
        "-S".to_owned(),
        control.display().to_string(),
        "-o".to_owned(),
        "ControlMaster=no".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
    ]);
    args.extend(target.port_args());
    args.push(target.host.clone());
    args.push(command.to_owned());
    args
}

/// `<herdr> [--session S] <args…>`, quoted for the remote shell.
fn herdr_command(herdr: &str, session: Option<&str>, args: &[&str]) -> String {
    let mut words = vec![shell_quote(herdr)];
    if let Some(session) = session {
        words.extend(["--session".to_owned(), shell_quote(session)]);
    }
    words.extend(args.iter().map(|arg| shell_quote(arg)));
    words.join(" ")
}

fn attach_command_line(
    target: &SshTarget,
    control: &Path,
    herdr: &str,
    session: Option<&str>,
    terminal_id: &str,
    takeover: bool,
) -> String {
    let mut attach = vec!["terminal", "attach", terminal_id];
    if takeover {
        attach.push("--takeover");
    }
    let remote = herdr_command(herdr, session, &attach);
    std::iter::once("ssh".to_owned())
        .chain(remote_args(target, control, true, &remote))
        .map(|arg| shell_quote(&arg))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:@=".contains(c))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

/// What a failed ssh said: its last line that isn't a warning. herdr's
/// check has already passed by the time the link's own ssh runs, so a
/// failure there is the connection dropping.
fn ssh_failure(stderr: &str) -> MachineStatus {
    let line = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("Warning:"))
        .unwrap_or("connection closed");
    MachineStatus::Offline(line.strip_prefix("ssh: ").unwrap_or(line).to_owned())
}

/// One machine in `herdr machine status --json`:
/// `[{"id", "label", "status", "error"}]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct MachineCheck {
    id: String,
    /// "reachable", "auth required", "error" or "disabled" in herdr 0.9.5.
    status: String,
    #[serde(default)]
    error: Option<String>,
}

/// The entry for `id` in `herdr machine status --json` output. herdr exits
/// 1 when any machine isn't reachable, so the exit status says nothing.
fn parse_check(json: &[u8], id: &str) -> Option<MachineCheck> {
    let checks: Vec<MachineCheck> = serde_json::from_slice(json).ok()?;
    checks.into_iter().find(|check| check.id == id)
}

/// herdr's verdict as a status; `None` when the machine is reachable.
///
/// herdr says "auth required" only when ssh refused the key, and "error" for
/// everything else, with a reason. The reason tells the fix apart: a host
/// key needs `herdr machine reconnect` (sign-in), herdr's own fix command
/// (`herdr --remote …`) means setup, and anything else is the network.
fn check_status(check: &MachineCheck, label: &str) -> Option<MachineStatus> {
    let reason = check.error.as_deref().map(check_reason).unwrap_or_default();
    let reason_or = |fallback: String| {
        if reason.is_empty() {
            fallback
        } else {
            reason.clone()
        }
    };
    match check.status.as_str() {
        "reachable" => None,
        "disabled" => Some(MachineStatus::Disabled("disabled in herdr".to_owned())),
        "auth required" => Some(MachineStatus::NeedsSignIn(reason_or(format!(
            "SSH to {label} needs you to sign in"
        )))),
        status => {
            let lower = reason.to_ascii_lowercase();
            Some(if lower.contains("host key") {
                MachineStatus::NeedsSignIn(reason)
            } else if lower.contains("herdr --remote") {
                MachineStatus::NeedsSetup(reason)
            } else {
                MachineStatus::Offline(reason_or(format!("herdr says {label}: {status}")))
            })
        }
    }
}

/// herdr's reason on one line, without its "remote platform detection
/// failed: ssh: " lead-in.
fn check_reason(error: &str) -> String {
    let line = error
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let line = line
        .strip_prefix("remote platform detection failed: ")
        .unwrap_or(&line);
    line.strip_prefix("ssh: ").unwrap_or(line).to_owned()
}

/// The parts of the remote `herdr status --json` the link reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RemoteStatus {
    server: RemoteServer,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RemoteServer {
    running: bool,
    socket: Option<String>,
    capabilities: Option<ServerCapabilities>,
}

/// What the link needs from the remote server.
#[derive(Debug, PartialEq, Eq)]
struct ServerInfo {
    socket: String,
    /// Whether it takes `server.ssh_agent.register`.
    agent_registration: bool,
}

/// The remote server's socket, or why it can't serve the app. herdr's
/// check has vetted the version and compatibility; this only catches a
/// server that stopped since.
fn remote_server(json: &[u8], label: &str) -> Result<ServerInfo, String> {
    let status: RemoteStatus = serde_json::from_slice(json)
        .map_err(|err| format!("couldn't read herdr status on {label}: {err}"))?;
    let server = status.server;
    if !server.running {
        return Err(format!("herdr isn't running on {label}"));
    }
    let socket = server
        .socket
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("herdr on {label} didn't report its socket"))?;
    // `:` would split the `-L local:remote` forward spec.
    if socket.contains(':') {
        return Err(format!(
            "herdr on {label} uses a socket path with ':', which can't be forwarded: {socket}"
        ));
    }
    Ok(ServerInfo {
        socket,
        agent_registration: server
            .capabilities
            .is_some_and(|caps| caps.ssh_agent_registration),
    })
}

/// The remote end of the master's forwarded SSH agent, registered with the
/// remote herdr while the link is up. herdr drops a registration when its
/// connection closes (a server restart, or the tunnel dropping), so it is
/// renewed whenever it's found closed.
struct Agent {
    /// The agent socket on the machine, from the master's session.
    socket: String,
    registration: Option<SshAgentRegistration>,
    next_try: Instant,
    /// The last attempt failed (logged once, not on every retry).
    failing: bool,
}

impl Agent {
    fn new(socket: String) -> Self {
        Self {
            socket,
            registration: None,
            next_try: Instant::now(),
            failing: false,
        }
    }

    /// Registers the agent unless a registration is still open.
    fn keep(&mut self, endpoint: &Endpoint, label: &str) {
        if self
            .registration
            .as_ref()
            .is_some_and(SshAgentRegistration::is_open)
        {
            return;
        }
        if self.registration.take().is_some() {
            log::info!("machine {label}: SSH agent registration closed; registering again");
        }
        let now = Instant::now();
        if now < self.next_try {
            return;
        }
        match Client::new(endpoint.clone()).ssh_agent_register(&self.socket) {
            Ok(registration) => {
                log::info!("machine {label}: panes use the forwarded SSH agent");
                self.registration = Some(registration);
                self.failing = false;
            }
            Err(err) => {
                if !self.failing {
                    log::warn!("machine {label}: registering the SSH agent: {err}");
                }
                self.failing = true;
                self.next_try = now + AGENT_RETRY;
            }
        }
    }
}

enum Msg {
    Reconnect,
    Stop,
}

#[derive(Debug)]
struct Shared {
    status: Mutex<MachineStatus>,
    /// The remote herdr binary, once found.
    herdr: Mutex<Option<String>>,
    /// The running control master. `stop` signals it from here, so a
    /// supervisor busy in a remote command still stops at once.
    master: Mutex<Option<Child>>,
    stopped: AtomicBool,
    /// Counts connections: bumped each time the link becomes `Connected`.
    connection: AtomicU64,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One machine's SSH connection, kept open by a supervisor thread.
pub struct MachineLink {
    profile: MachineProfile,
    target: Option<SshTarget>,
    paths: Paths,
    shared: Arc<Shared>,
    statuses: async_channel::Receiver<MachineStatus>,
    inbox: mpsc::Sender<Msg>,
    fs: OnceLock<Arc<RemoteFs>>,
}

impl MachineLink {
    /// Starts the supervisor and returns at once, status `Connecting`
    /// (or `Disabled` without starting anything). `herdr_bin` is the local
    /// herdr, which checks the machine before each connection.
    pub fn start(profile: MachineProfile, socket_dir: PathBuf, herdr_bin: PathBuf) -> Self {
        let link = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
        let paths = Paths::new(&socket_dir, std::process::id(), link, &profile.id);
        let target = SshTarget::parse(&profile.target);
        let blocked = if !profile.enabled {
            Some(MachineStatus::Disabled("disabled in herdr".to_owned()))
        } else if !dev_allowed(&profile.label) {
            Some(MachineStatus::Disabled(format!(
                "debug builds only connect to machines in {DEV_MACHINES_ENV}"
            )))
        } else if target.is_none() {
            Some(MachineStatus::Offline(format!(
                "{:?} isn't an SSH target",
                profile.target
            )))
        } else if !paths.fit() {
            Some(MachineStatus::Offline(format!(
                "socket path {} is too long",
                paths.socket.display()
            )))
        } else {
            None
        };
        let initial = blocked.clone().unwrap_or(MachineStatus::Connecting);
        let shared = Arc::new(Shared {
            status: Mutex::new(initial.clone()),
            herdr: Mutex::new(None),
            master: Mutex::new(None),
            stopped: AtomicBool::new(false),
            connection: AtomicU64::new(0),
        });
        let (status_tx, statuses) = async_channel::unbounded();
        // Unbounded: never full, and the receiver is held by `statuses`.
        let _ = status_tx.try_send(initial);
        let (inbox, inbox_rx) = mpsc::channel();
        if let (None, Some(target)) = (blocked, &target) {
            let supervisor = Supervisor {
                profile: profile.clone(),
                herdr_bin,
                target: target.clone(),
                paths: paths.clone(),
                shared: Arc::clone(&shared),
                statuses: status_tx,
                inbox: inbox_rx,
            };
            // Detached: `stop` never waits for it.
            let _ = thread::Builder::new()
                .name(format!("machine-{}", profile.label))
                .spawn(move || supervisor.run())
                .inspect_err(|err| log::error!("machine supervisor: {err}"));
        }
        Self {
            profile,
            target,
            paths,
            shared,
            statuses,
            inbox,
            fs: OnceLock::new(),
        }
    }

    pub fn profile(&self) -> &MachineProfile {
        &self.profile
    }

    pub fn status(&self) -> MachineStatus {
        lock(&self.shared.status).clone()
    }

    /// Every status change, latest last.
    pub fn statuses(&self) -> async_channel::Receiver<MachineStatus> {
        self.statuses.clone()
    }

    /// The forwarded socket; fixed for the link's life. `auto_start: false`,
    /// `server_log: None`, `session: None`. `SessionSync::start(endpoint)`
    /// against it reconnects by itself whenever the tunnel comes back.
    pub fn endpoint(&self) -> Endpoint {
        self.paths.endpoint()
    }

    /// Shell command for a terminal surface in a remote pane, like
    /// `Launch::attach_command`. `None` until the remote herdr is found.
    /// Without `takeover`, herdr refuses a terminal another client has.
    pub fn attach_command(&self, terminal_id: &TerminalId, takeover: bool) -> Option<String> {
        let target = self.target.as_ref()?;
        let herdr = lock(&self.shared.herdr).clone()?;
        Some(attach_command_line(
            target,
            &self.paths.control,
            &herdr,
            self.profile.remote_session(),
            terminal_id,
            takeover,
        ))
    }

    /// The machine's files over this link's master; `None` until it first
    /// connects, then the same one for the link's life, failing with
    /// `NotConnected` while the link is down.
    pub fn remote_fs(&self) -> Option<Arc<RemoteFs>> {
        let target = self.target.as_ref()?;
        if self.shared.connection.load(Ordering::SeqCst) == 0 {
            return None;
        }
        let fs = self.fs.get_or_init(|| {
            Arc::new(RemoteFs::new(LinkAccess {
                target: target.clone(),
                control: self.paths.control.clone(),
                shared: Arc::clone(&self.shared),
            }))
        });
        Some(Arc::clone(fs))
    }

    /// Retry now, skipping the backoff wait.
    pub fn reconnect(&self) {
        let _ = self.inbox.send(Msg::Reconnect);
    }

    /// Ends the connection and removes the socket files. Returns at once:
    /// the master gets SIGTERM and the supervisor reaps it on its own
    /// thread. Safe to call more than once; Drop calls it too.
    pub fn stop(&self) {
        if self.shared.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.inbox.send(Msg::Stop);
        if let Some(master) = lock(&self.shared.master).as_mut() {
            terminate(master);
        }
        self.paths.remove();
    }
}

impl Drop for MachineLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a [`RemoteFs`] needs from its link: the master, and whether it's up.
#[derive(Debug)]
pub(crate) struct LinkAccess {
    target: SshTarget,
    control: PathBuf,
    shared: Arc<Shared>,
}

impl LinkAccess {
    /// The current connection's number, while connected.
    pub(crate) fn connection(&self) -> Option<u64> {
        let up = !self.shared.stopped.load(Ordering::SeqCst)
            && *lock(&self.shared.status) == MachineStatus::Connected;
        up.then(|| self.shared.connection.load(Ordering::SeqCst))
    }

    /// `ssh -S <ctl> -o ControlMaster=no [-p port] -s <target> sftp`.
    pub(crate) fn sftp(&self) -> Command {
        let mut args = remote_args(&self.target, &self.control, false, "sftp");
        let host = args.len() - 2;
        args.insert(host, "-s".to_owned());
        ssh(&args)
    }

    /// `command` on the machine through the master, killed after `timeout`.
    pub(crate) fn run(&self, command: &str, timeout: Duration) -> io::Result<Output> {
        run(
            ssh(&remote_args(&self.target, &self.control, false, command)),
            timeout,
        )
    }
}

/// SIGTERM, which lets ssh remove its control socket. Nothing once reaped,
/// so the pid can't belong to anyone else.
fn terminate(master: &mut Child) {
    if !matches!(master.try_wait(), Ok(None)) {
        return;
    }
    if let Ok(pid) = libc::pid_t::try_from(master.id()) {
        // SAFETY: plain kill(2) on our own unreaped child.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// Ends the master and reaps it: SIGTERM, then SIGKILL after a grace period.
fn stop_master(mut master: Child) {
    terminate(&mut master);
    let deadline = Instant::now() + TERM_GRACE;
    while matches!(master.try_wait(), Ok(None)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let _ = master.kill();
    let _ = master.wait();
}

fn ssh(args: &[String]) -> Command {
    let mut command = Command::new("ssh");
    command.args(args);
    command
}

/// Runs `command` to completion, killing it after `timeout`.
fn run(mut command: Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let collect = |reader: Option<JoinHandle<Vec<u8>>>| {
        reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// A running master's output.
struct Master {
    stderr: Arc<Mutex<Vec<u8>>>,
    /// The first line it prints: its session's `SSH_AUTH_SOCK`, if any.
    agent_line: mpsc::Receiver<String>,
}

/// How one connection attempt ended.
enum End {
    Stop,
    /// Start again at once (a reconnect while connected).
    Restart,
    Failed {
        status: MachineStatus,
        was_connected: bool,
    },
}

struct Supervisor {
    profile: MachineProfile,
    herdr_bin: PathBuf,
    target: SshTarget,
    paths: Paths,
    shared: Arc<Shared>,
    statuses: async_channel::Sender<MachineStatus>,
    inbox: mpsc::Receiver<Msg>,
}

impl Supervisor {
    fn run(self) {
        if let Some(dir) = self.paths.control.parent() {
            CLEANED.call_once(|| clean_stale(dir));
        }
        let mut backoff = BACKOFF_MIN;
        loop {
            let (status, was_connected) = match self.connect() {
                End::Stop => break,
                End::Restart => {
                    self.set(MachineStatus::Connecting);
                    continue;
                }
                End::Failed {
                    status,
                    was_connected,
                } => (status, was_connected),
            };
            if was_connected {
                backoff = BACKOFF_MIN;
            }
            let delay = match status {
                MachineStatus::Offline(_) => {
                    let delay = backoff;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                    delay
                }
                _ => SLOW_RETRY,
            };
            log::info!(
                "machine {}: {status:?}; retrying in {delay:?}",
                self.profile.label
            );
            self.set(status);
            match self.inbox.recv_timeout(delay) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Msg::Reconnect) => {
                    backoff = BACKOFF_MIN;
                    self.set(MachineStatus::Connecting);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        self.teardown();
    }

    fn set(&self, status: MachineStatus) {
        let mut current = lock(&self.shared.status);
        if *current != status {
            *current = status.clone();
            let _ = self.statuses.try_send(status);
        }
    }

    fn stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::SeqCst)
    }

    /// One connection: herdr's check, then the master, until it ends.
    fn connect(&self) -> End {
        let failed = |status| End::Failed {
            status,
            was_connected: false,
        };
        if let Some(status) = self.check() {
            return if self.stopped() {
                End::Stop
            } else {
                failed(status)
            };
        }
        let master = match self.spawn_master() {
            Ok(Some(master)) => master,
            Ok(None) => return End::Stop,
            Err(err) => return failed(MachineStatus::Offline(format!("running ssh: {err}"))),
        };
        if let Some(end) = self.wait_ready(&master.stderr) {
            self.teardown();
            return end;
        }
        let server = match self.find_server() {
            Ok(server) => server,
            Err(status) => {
                self.teardown();
                return if self.stopped() {
                    End::Stop
                } else {
                    failed(status)
                };
            }
        };
        if let Err(status) = self.forward(&server.socket) {
            self.teardown();
            return failed(status);
        }
        log::info!(
            "machine {}: forwarding {} to {}",
            self.profile.label,
            self.paths.socket.display(),
            server.socket
        );
        let mut agent = self.forwarded_agent(&master, &server);
        if let Some(agent) = &mut agent {
            agent.keep(&self.paths.endpoint(), &self.profile.label);
        }
        self.shared.connection.fetch_add(1, Ordering::SeqCst);
        self.set(MachineStatus::Connected);
        let end = self.watch(&master.stderr, agent.as_mut());
        self.teardown();
        end
    }

    /// herdr's own check of the machine (`herdr machine status <id>
    /// --json`): fresh non-interactive SSH, then whether a matching herdr
    /// is installed and its server running. `None` when it's reachable, or
    /// when the check can't run or doesn't know the machine; the connection
    /// then goes ahead and its own failure says what's wrong.
    fn check(&self) -> Option<MachineStatus> {
        let mut command = Command::new(&self.herdr_bin);
        command.args(["machine", "status", &self.profile.id, "--json"]);
        let label = &self.profile.label;
        let out = match run(command, CHECK_TIMEOUT) {
            Ok(out) => out,
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                return Some(MachineStatus::Offline(format!(
                    "herdr's check of {label} timed out"
                )));
            }
            Err(err) => {
                log::warn!("machine {label}: herdr machine status: {err}");
                return None;
            }
        };
        let Some(check) = parse_check(&out.stdout, &self.profile.id) else {
            log::warn!(
                "machine {label}: herdr machine status said nothing about it: {} {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return None;
        };
        check_status(&check, label)
    }

    /// Starts the master, keeping its stderr and the agent line it prints.
    /// `None` once stopped.
    fn spawn_master(&self) -> io::Result<Option<Master>> {
        if self.paths.control.exists() {
            exit_master(&self.paths.control);
            remove_file(&self.paths.control);
        }
        let mut slot = lock(&self.shared.master);
        if self.stopped() {
            return Ok(None);
        }
        // The pipe's write end stays in `Child::stdin`, never written to.
        // It closes when the process exits, Drop or not, and then the
        // remote `MASTER_COMMAND` ends and the master with it.
        let args = master_args(&self.target, &self.paths.control, forced_agent_forwarding());
        let mut child = ssh(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let stderr = Arc::clone(&stderr);
            thread::spawn(move || {
                let mut buf = [0; 1024];
                while let Ok(n @ 1..) = pipe.read(&mut buf) {
                    lock(&stderr).extend_from_slice(&buf[..n]);
                }
            });
        }
        let (agent_tx, agent_line) = mpsc::channel();
        if let Some(pipe) = child.stdout.take() {
            thread::spawn(move || {
                let mut lines = BufReader::new(pipe).lines();
                if let Some(Ok(line)) = lines.next() {
                    let _ = agent_tx.send(line);
                }
                // Nothing more comes; keep the pipe open until it ends.
                lines.for_each(drop);
            });
        }
        *slot = Some(child);
        Ok(Some(Master { stderr, agent_line }))
    }

    /// The agent the master forwards, when there is one and the remote
    /// herdr can take it.
    fn forwarded_agent(&self, master: &Master, server: &ServerInfo) -> Option<Agent> {
        let socket = master
            .agent_line
            .recv_timeout(AGENT_LINE_TIMEOUT)
            .ok()
            .map(|line| line.trim().to_owned())
            .filter(|line| line.starts_with('/'))?;
        if !server.agent_registration {
            log::info!(
                "machine {}: herdr there can't take a forwarded SSH agent",
                self.profile.label
            );
            return None;
        }
        Some(Agent::new(socket))
    }

    /// Whether the master has exited. Gone counts as exited (Drop took it).
    fn master_exited(&self) -> bool {
        lock(&self.shared.master)
            .as_mut()
            .is_none_or(|master| !matches!(master.try_wait(), Ok(None)))
    }

    /// Waits until the master accepts requests. `None` when it does.
    fn wait_ready(&self, stderr: &Mutex<Vec<u8>>) -> Option<End> {
        let deadline = Instant::now() + MASTER_READY_TIMEOUT;
        loop {
            if self.master_exited() {
                // Let the reader catch the last of stderr.
                thread::sleep(Duration::from_millis(50));
                return Some(self.master_failed(stderr, false));
            }
            let check = ssh(&control_args(
                &self.target,
                &self.paths.control,
                "check",
                &[],
            ));
            if run(check, COMMAND_TIMEOUT).is_ok_and(|out| out.status.success()) {
                return None;
            }
            if Instant::now() >= deadline {
                return Some(End::Failed {
                    status: MachineStatus::Offline("timed out connecting".to_owned()),
                    was_connected: false,
                });
            }
            match self.inbox.recv_timeout(POLL) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return Some(End::Stop),
                Ok(Msg::Reconnect) | Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn master_failed(&self, stderr: &Mutex<Vec<u8>>, was_connected: bool) -> End {
        if self.stopped() {
            return End::Stop;
        }
        let text = String::from_utf8_lossy(&lock(stderr)).into_owned();
        End::Failed {
            status: ssh_failure(&text),
            was_connected,
        }
    }

    /// Finds the remote herdr and its server.
    fn find_server(&self) -> Result<ServerInfo, MachineStatus> {
        let label = &self.profile.label;
        let script = format!("sh -c {}", shell_quote(FIND_HERDR));
        let out = self.remote(&script)?;
        let herdr = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with('/'))
            .map(str::to_owned);
        let Some(herdr) = herdr.filter(|_| out.status.success()) else {
            return Err(MachineStatus::NeedsSetup(format!(
                "couldn't find herdr on {label}"
            )));
        };
        *lock(&self.shared.herdr) = Some(herdr.clone());

        let status = herdr_command(&herdr, self.profile.remote_session(), &["status", "--json"]);
        let out = self.remote(&status)?;
        if !out.status.success() {
            return Err(MachineStatus::NeedsSetup(format!(
                "herdr on {label} couldn't report its status: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        remote_server(&out.stdout, label).map_err(MachineStatus::NeedsSetup)
    }

    /// Runs `command` on the machine through the master.
    fn remote(&self, command: &str) -> Result<Output, MachineStatus> {
        let args = remote_args(&self.target, &self.paths.control, false, command);
        let out = run(ssh(&args), COMMAND_TIMEOUT)
            .map_err(|err| MachineStatus::Offline(format!("ssh: {err}")))?;
        // 255 is ssh's own failure, not the command's.
        if out.status.code() == Some(255) {
            return Err(ssh_failure(&String::from_utf8_lossy(&out.stderr)));
        }
        Ok(out)
    }

    fn forward(&self, remote_socket: &str) -> Result<(), MachineStatus> {
        remove_file(&self.paths.socket);
        let spec = format!("{}:{remote_socket}", self.paths.socket.display());
        let args = control_args(
            &self.target,
            &self.paths.control,
            "forward",
            &["-L".to_owned(), spec],
        );
        let out = run(ssh(&args), COMMAND_TIMEOUT)
            .map_err(|err| MachineStatus::Offline(format!("ssh forward: {err}")))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(ssh_failure(&String::from_utf8_lossy(&out.stderr)))
        }
    }

    /// Waits while connected, until the master exits or a message arrives,
    /// keeping the agent registered meanwhile.
    fn watch(&self, stderr: &Mutex<Vec<u8>>, mut agent: Option<&mut Agent>) -> End {
        let start = lock(stderr).len();
        let endpoint = self.paths.endpoint();
        loop {
            match self.inbox.recv_timeout(Duration::from_millis(250)) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return End::Stop,
                Ok(Msg::Reconnect) => return End::Restart,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if self.master_exited() {
                thread::sleep(Duration::from_millis(50));
                // Only what it said since connecting, not earlier warnings.
                let tail = lock(stderr).split_off(start);
                return self.master_failed(&Mutex::new(tail), true);
            }
            if let Some(agent) = agent.as_deref_mut() {
                agent.keep(&endpoint, &self.profile.label);
            }
        }
    }

    /// Ends the master (if this thread still owns it) and removes the files.
    fn teardown(&self) {
        let master = lock(&self.shared.master).take();
        if let Some(master) = master {
            stop_master(master);
        }
        self.paths.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(text: &str) -> SshTarget {
        SshTarget::parse(text).unwrap()
    }

    #[test]
    fn target_should_read_ssh_url_with_user_and_port() {
        assert_eq!(
            target("ssh://lyle@127.0.0.1:2222"),
            SshTarget {
                host: "lyle@127.0.0.1".into(),
                port: Some(2222)
            }
        );
    }

    #[test]
    fn target_should_read_ssh_url_without_port() {
        assert_eq!(
            target("ssh://box.local"),
            SshTarget {
                host: "box.local".into(),
                port: None
            }
        );
    }

    #[test]
    fn target_should_read_bracketed_ipv6() {
        assert_eq!(
            target("ssh://me@[::1]:22"),
            SshTarget {
                host: "me@::1".into(),
                port: Some(22)
            }
        );
    }

    #[test]
    fn target_should_keep_user_at_host() {
        assert_eq!(target("lyle@devbox").host, "lyle@devbox");
    }

    #[test]
    fn target_should_keep_alias() {
        assert_eq!(target("dockerbox").port, None);
    }

    #[test]
    fn target_should_refuse_option_lookalike() {
        assert_eq!(SshTarget::parse("-oProxyCommand=evil"), None);
    }

    #[test]
    fn target_should_refuse_bad_port() {
        assert_eq!(SshTarget::parse("ssh://host:http"), None);
    }

    #[test]
    fn master_args_should_name_port_before_host() {
        let args = master_args(
            &target("ssh://u@h:2222"),
            Path::new("/m/abcd1234.ctl"),
            false,
        );
        assert_eq!(
            args[args.len() - 4..args.len() - 1],
            ["-p".to_owned(), "2222".into(), "u@h".into()]
        );
    }

    #[test]
    fn master_args_should_end_with_the_master_command() {
        let args = master_args(&target("h"), Path::new("/m/x.ctl"), false);
        assert_eq!(args.last().map(String::as_str), Some(MASTER_COMMAND));
    }

    #[test]
    fn master_command_should_wait_on_stdin_last() {
        assert!(MASTER_COMMAND.ends_with("cat >/dev/null"));
    }

    #[test]
    fn master_command_should_have_no_quotes_or_dollars() {
        // It goes to the login shell as is; sh, fish and csh must agree.
        assert!(!MASTER_COMMAND.contains(['\'', '"', '$']));
    }

    #[test]
    fn master_args_should_not_use_n() {
        let args = master_args(&target("h"), Path::new("/m/x.ctl"), false);
        assert!(!args.iter().any(|arg| arg == "-N"));
    }

    #[test]
    fn master_args_should_leave_agent_forwarding_to_ssh_config() {
        let args = master_args(&target("h"), Path::new("/m/x.ctl"), false);
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("ForwardAgent") || arg == "-A")
        );
    }

    #[test]
    fn master_args_should_force_agent_forwarding_when_asked() {
        let args = master_args(&target("h"), Path::new("/m/x.ctl"), true).join(" ");
        assert!(args.contains("-o ForwardAgent=yes h "));
    }

    #[test]
    fn master_args_should_use_batch_mode_and_control_path() {
        let args = master_args(&target("h"), Path::new("/m/abcd1234.ctl"), false).join(" ");
        assert!(
            args.starts_with("-M -T -o BatchMode=yes")
                && args.contains("-o ControlPath=/m/abcd1234.ctl")
                && args.contains("ControlPersist=no")
        );
    }

    #[test]
    fn control_args_should_put_forward_spec_before_host() {
        let args = control_args(
            &target("ssh://u@h:22"),
            Path::new("/m/x.ctl"),
            "forward",
            &["-L".into(), "/m/x.sock:/r/herdr.sock".into()],
        );
        assert_eq!(
            args.join(" "),
            "-S /m/x.ctl -O forward -L /m/x.sock:/r/herdr.sock u@h"
        );
    }

    #[test]
    fn attach_command_should_reuse_master_and_name_session() {
        let line = attach_command_line(
            &target("ssh://lyle@127.0.0.1:2222"),
            Path::new("/m/b87d0c96.ctl"),
            "/home/lyle/.local/bin/herdr",
            Some("work"),
            "term_1",
            false,
        );
        assert_eq!(
            line,
            "ssh -t -S /m/b87d0c96.ctl -o ControlMaster=no -o BatchMode=yes -p 2222 \
             lyle@127.0.0.1 '/home/lyle/.local/bin/herdr --session work terminal attach \
             term_1'"
        );
    }

    #[test]
    fn attach_command_should_take_over_only_when_asked() {
        let line = attach_command_line(&target("h"), Path::new("/m/x.ctl"), "/h", None, "t", true);
        assert!(line.ends_with(" h '/h terminal attach t --takeover'"));
    }

    #[test]
    fn attach_command_should_quote_remote_path_with_spaces_twice() {
        let line = attach_command_line(
            &target("h"),
            Path::new("/m/x.ctl"),
            "/opt/my tools/herdr",
            None,
            "t",
            false,
        );
        assert!(line.ends_with(r#" h ''\''/opt/my tools/herdr'\'' terminal attach t'"#));
    }

    #[test]
    fn find_herdr_script_should_have_no_single_quotes() {
        assert!(!FIND_HERDR.contains('\''));
    }

    #[test]
    fn ssh_failure_should_strip_the_ssh_prefix() {
        assert_eq!(
            ssh_failure("ssh: connect to host 127.0.0.1 port 2222: Connection refused\n"),
            MachineStatus::Offline(
                "connect to host 127.0.0.1 port 2222: Connection refused".into()
            )
        );
    }

    #[test]
    fn ssh_failure_should_skip_warnings() {
        assert_eq!(
            ssh_failure("Warning: Permanently added 'h' to the list of known hosts.\n"),
            MachineStatus::Offline("connection closed".into())
        );
    }

    /// `herdr machine status --json` as herdr 0.9.5 printed it for a
    /// reachable machine, a refused port, a refused key, a disabled machine,
    /// an unknown host key, a missing herdr and a stopped session.
    const CHECKS: &str = r#"[
      {"id":"ok","label":"dockerbox","status":"reachable","error":null},
      {"id":"refused","label":"refused","status":"error","error":"remote platform detection failed: ssh: connect to host 127.0.0.1 port 2299: Connection refused"},
      {"id":"key","label":"baduser","status":"auth required","error":"remote platform detection failed: nobodyx@127.0.0.1: Permission denied (publickey)."},
      {"id":"off","label":"disabledbox","status":"disabled","error":null},
      {"id":"hostkey","label":"hostkey","status":"error","error":"remote platform detection failed: No ED25519 host key is known for [localhost]:2222 and you have requested strict checking.\r\nHost key verification failed."},
      {"id":"noherdr","label":"noherdr","status":"error","error":"matching Herdr is not ready on ssh://gltest@127.0.0.1:2222; run `herdr --remote ssh://gltest@127.0.0.1:2222` interactively to install or update it"},
      {"id":"stopped","label":"othersession","status":"error","error":"remote Herdr server is stopped or incompatible; run `herdr --remote ssh://lyle@127.0.0.1:2222 --session nosuch`"}
    ]"#;

    fn checked(id: &str) -> Option<MachineStatus> {
        let check = parse_check(CHECKS.as_bytes(), id).unwrap();
        check_status(&check, "box")
    }

    #[test]
    fn check_should_pass_a_reachable_machine() {
        assert_eq!(checked("ok"), None);
    }

    #[test]
    fn check_should_treat_a_refused_port_as_offline() {
        assert_eq!(
            checked("refused"),
            Some(MachineStatus::Offline(
                "connect to host 127.0.0.1 port 2299: Connection refused".into()
            ))
        );
    }

    #[test]
    fn check_should_treat_auth_required_as_sign_in() {
        assert_eq!(
            checked("key"),
            Some(MachineStatus::NeedsSignIn(
                "nobodyx@127.0.0.1: Permission denied (publickey).".into()
            ))
        );
    }

    #[test]
    fn check_should_treat_an_unknown_host_key_as_sign_in() {
        assert_eq!(
            checked("hostkey"),
            Some(MachineStatus::NeedsSignIn(
                "No ED25519 host key is known for [localhost]:2222 and you have requested \
                 strict checking. Host key verification failed."
                    .into()
            ))
        );
    }

    #[test]
    fn check_should_treat_a_missing_herdr_as_setup() {
        assert!(
            matches!(checked("noherdr"), Some(MachineStatus::NeedsSetup(reason))
            if reason.starts_with("matching Herdr is not ready"))
        );
    }

    #[test]
    fn check_should_treat_a_stopped_server_as_setup() {
        assert!(matches!(
            checked("stopped"),
            Some(MachineStatus::NeedsSetup(_))
        ));
    }

    #[test]
    fn check_should_treat_disabled_as_disabled() {
        assert!(matches!(checked("off"), Some(MachineStatus::Disabled(_))));
    }

    #[test]
    fn check_should_word_an_unknown_status_without_a_reason() {
        let check = MachineCheck {
            id: "x".into(),
            status: "unplugged".into(),
            error: None,
        };
        assert_eq!(
            check_status(&check, "box"),
            Some(MachineStatus::Offline("herdr says box: unplugged".into()))
        );
    }

    #[test]
    fn parse_check_should_skip_other_machines() {
        assert_eq!(parse_check(CHECKS.as_bytes(), "nope"), None);
    }

    #[test]
    fn parse_check_should_refuse_non_json() {
        assert_eq!(parse_check(b"error: no machines", "ok"), None);
    }

    const STATUS: &str = r#"{"client":{"version":"0.9.5","binary":"/home/lyle/.local/bin/herdr"},
        "server":{"status":"running","running":true,"version":"0.9.5","compatible":true,
        "socket":"/home/lyle/.config/herdr/herdr.sock","extra":1,
        "capabilities":{"live_handoff":true,"ssh_agent_registration":true}}}"#;

    #[test]
    fn remote_server_should_give_socket_and_agent_capability() {
        assert_eq!(
            remote_server(STATUS.as_bytes(), "box"),
            Ok(ServerInfo {
                socket: "/home/lyle/.config/herdr/herdr.sock".to_owned(),
                agent_registration: true,
            })
        );
    }

    #[test]
    fn remote_server_should_assume_no_agent_capability_when_unlisted() {
        let json = r#"{"server":{"running":true,"socket":"/s"}}"#;
        assert!(remote_server(json.as_bytes(), "box").is_ok_and(|s| !s.agent_registration));
    }

    #[test]
    fn remote_server_should_need_setup_when_not_running() {
        let json = r#"{"client":{"version":"0.9.5"},"server":{"running":false}}"#;
        assert_eq!(
            remote_server(json.as_bytes(), "box"),
            Err("herdr isn't running on box".to_owned())
        );
    }

    #[test]
    fn remote_server_should_refuse_socket_with_colon() {
        let json = r#"{"server":{"running":true,"version":"0.9.5","socket":"/s:x/herdr.sock"}}"#;
        assert!(remote_server(json.as_bytes(), "box").is_err_and(|e| e.contains("':'")));
    }

    #[test]
    fn parse_machines_should_ignore_unknown_fields() {
        let json = br#"[{"id":"b87d0c96779caefd39b0583c9200208c","label":"dockerbox",
            "target":"ssh://lyle@127.0.0.1:2222","session":"default","enabled":true,
            "selected":false}]"#;
        let machines = parse_machines(json).unwrap();
        assert_eq!(machines[0].label, "dockerbox");
    }

    #[test]
    fn profile_should_pass_no_session_for_default() {
        let machines = parse_machines(br#"[{"id":"a","label":"l","target":"t"}]"#).unwrap();
        assert_eq!(machines[0].remote_session(), None);
    }

    #[test]
    fn paths_should_name_pid_link_and_first_eight_chars_of_id() {
        let paths = Paths::new(Path::new("/m"), 4321, 7, "b87d0c96779caefd39b0583c9200208c");
        assert_eq!(paths.socket, PathBuf::from("/m/4321-7-b87d0c96.sock"));
    }

    #[test]
    fn paths_should_fit_in_a_typical_home_with_a_large_pid() {
        let dir = Path::new("/Users/someone-long-name/.local/share/grasslands-herdr/m");
        assert!(Paths::new(dir, 9_999_999, 9999, "b87d0c96779caefd").fit());
    }

    #[test]
    fn paths_should_not_fit_in_a_very_deep_dir() {
        let dir = PathBuf::from("/").join("d".repeat(90));
        assert!(!Paths::new(&dir, 1, 0, "b87d0c96").fit());
    }

    #[test]
    fn owner_should_read_pid_of_socket_and_temp_control() {
        assert_eq!(
            [
                owner("4321-0-b87d0c96.sock"),
                owner("4321-0-b87d0c96.ctl.AbCdEf0123456789")
            ],
            [Some(4321), Some(4321)]
        );
    }

    #[test]
    fn owner_should_skip_old_style_and_foreign_names() {
        assert_eq!(
            [
                owner("b87d0c96.ctl"),
                owner("notes.txt"),
                owner("4321-.sock")
            ],
            [None, None, None]
        );
    }

    #[test]
    fn alive_should_see_this_process() {
        assert!(alive(std::process::id()));
    }

    #[test]
    fn clean_stale_should_remove_only_dead_pids_files() {
        let dir = env::temp_dir().join(format!("grasslands-clean-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut dead = Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait().unwrap();
        let names = [
            format!("{dead_pid}-0-aaaaaaaa.sock"),
            format!("{}-0-bbbbbbbb.sock", std::process::id()),
            "cccccccc.ctl".to_owned(),
        ];
        for name in &names {
            fs::write(dir.join(name), b"").unwrap();
        }
        clean_stale(&dir);
        let left: Vec<bool> = names.iter().map(|n| dir.join(n).exists()).collect();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(left, [false, true, true]);
    }

    #[test]
    fn listed_should_match_trimmed_labels() {
        assert!(listed(Some("prod, dockerbox"), "dockerbox"));
    }

    #[test]
    fn listed_should_refuse_when_unset() {
        assert!(!listed(None, "dockerbox"));
    }

    #[test]
    fn listed_should_not_match_substrings() {
        assert!(!listed(Some("dockerbox2"), "dockerbox"));
    }
}
