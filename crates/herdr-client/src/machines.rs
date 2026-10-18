//! herdr's saved SSH machines, and one SSH connection to each.
//!
//! herdr keeps the machine list (`herdr machine list --json`); this module
//! never edits it. A [`MachineLink`] runs one `ssh` control master per machine
//! and forwards the remote herdr socket to a local socket file, so
//! [`Endpoint`] and `SessionSync` work against it unchanged. A supervisor
//! thread reopens the master with backoff at the same socket path whenever it
//! drops. See `docs/decisions/0019-remote-machines.md`.
//!
//! The remote herdr server is only ever read: never installed, started,
//! restarted or upgraded from here.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::endpoint::{DEV_ROOT, Endpoint};
use crate::error::{Error, Result};
use crate::setup::{MIN_VERSION, Version};

/// Comma-separated machine labels a debug build may connect to.
pub const DEV_MACHINES_ENV: &str = "GRASSLANDS_DEV_MACHINES";

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
const POLL: Duration = Duration::from_millis(100);

/// herdr's usual install locations, for non-interactive SSH shells whose
/// `PATH` lacks them. Double quotes only: the script is single-quoted for the
/// remote login shell, which may be fish.
const FIND_HERDR: &str = "command -v herdr || for p in \"$HOME/.local/bin/herdr\" \
    /opt/homebrew/bin/herdr /usr/local/bin/herdr /home/linuxbrew/.linuxbrew/bin/herdr \
    \"$HOME/.local/share/mise/shims/herdr\" \"$HOME/.nix-profile/bin/herdr\"; \
    do if [ -x \"$p\" ]; then echo \"$p\"; exit 0; fi; done; exit 1";

/// A saved SSH machine, from `herdr machine list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MachineProfile {
    pub id: String,
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
    fn new(dir: &Path, id: &str) -> Self {
        let id8: String = id.chars().take(8).collect();
        Self {
            control: dir.join(format!("{id8}.ctl")),
            socket: dir.join(format!("{id8}.sock")),
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
}

fn remove_file(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        log::warn!("removing {}: {err}", path.display());
    }
}

/// `ssh -M -N …`: the control master, which holds the connection open.
fn master_args(target: &SshTarget, control: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-M",
        "-N",
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
    args.extend(target.port_args());
    args.push(target.host.clone());
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
) -> String {
    let remote = herdr_command(
        herdr,
        session,
        &["terminal", "attach", terminal_id, "--takeover"],
    );
    std::iter::once("ssh".to_owned())
        .chain(remote_args(target, control, true, &remote))
        .map(|arg| shell_quote(&arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
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

/// What a failed ssh said, as a status.
fn classify_ssh_error(stderr: &str) -> MachineStatus {
    let line = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("Warning:"))
        .unwrap_or("connection closed");
    let line = line.strip_prefix("ssh: ").unwrap_or(line).to_owned();
    let lower = stderr.to_ascii_lowercase();
    let sign_in = [
        "permission denied",
        "host key verification failed",
        "remote host identification has changed",
        "no matching host key",
        "too many authentication failures",
    ];
    if sign_in.iter().any(|needle| lower.contains(needle)) {
        MachineStatus::NeedsSignIn(line)
    } else {
        MachineStatus::Offline(line)
    }
}

/// The parts of the remote `herdr status --json` the link reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RemoteStatus {
    client: RemoteClient,
    server: RemoteServer,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RemoteClient {
    version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RemoteServer {
    running: bool,
    compatible: Option<bool>,
    version: Option<String>,
    socket: Option<String>,
}

/// The remote server's socket, or why it can't serve the app.
fn remote_socket(json: &[u8], label: &str) -> Result<String, String> {
    let status: RemoteStatus = serde_json::from_slice(json)
        .map_err(|err| format!("couldn't read herdr status on {label}: {err}"))?;
    let server = status.server;
    if !server.running {
        return Err(format!("herdr isn't running on {label}"));
    }
    let version = server
        .version
        .as_deref()
        .or(status.client.version.as_deref())
        .and_then(Version::parse);
    match version {
        Some(v) if v < MIN_VERSION => {
            return Err(format!("herdr {v} on {label} is older than {MIN_VERSION}"));
        }
        None => return Err(format!("couldn't read the herdr version on {label}")),
        Some(_) => {}
    }
    if server.compatible == Some(false) {
        return Err(format!("herdr on {label} needs a restart to update"));
    }
    server
        .socket
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("herdr on {label} didn't report its socket"))
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
    /// The running control master. Drop kills it from here, so a supervisor
    /// busy in a remote command still stops at once.
    master: Mutex<Option<Child>>,
    stopped: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
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
    thread: Option<JoinHandle<()>>,
}

impl MachineLink {
    /// Starts the supervisor and returns at once, status `Connecting`
    /// (or `Disabled` without starting anything).
    pub fn start(profile: MachineProfile, socket_dir: PathBuf) -> Self {
        let paths = Paths::new(&socket_dir, &profile.id);
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
        });
        let (status_tx, statuses) = async_channel::unbounded();
        // Unbounded: never full, and the receiver is held by `statuses`.
        let _ = status_tx.try_send(initial);
        let (inbox, inbox_rx) = mpsc::channel();
        let thread = match (blocked, &target) {
            (None, Some(target)) => {
                let supervisor = Supervisor {
                    profile: profile.clone(),
                    target: target.clone(),
                    paths: paths.clone(),
                    shared: Arc::clone(&shared),
                    statuses: status_tx,
                    inbox: inbox_rx,
                };
                thread::Builder::new()
                    .name(format!("machine-{}", profile.label))
                    .spawn(move || supervisor.run())
                    .inspect_err(|err| log::error!("machine supervisor: {err}"))
                    .ok()
            }
            _ => None,
        };
        Self {
            profile,
            target,
            paths,
            shared,
            statuses,
            inbox,
            thread,
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
        Endpoint {
            bin: PathBuf::from("herdr"),
            session: None,
            socket_path: self.paths.socket.clone(),
            auto_start: false,
            server_log: None,
        }
    }

    /// Shell command for a terminal surface in a remote pane, like
    /// `Launch::attach_command`. `None` until the remote herdr is found.
    pub fn attach_command(&self, terminal_id: &str) -> Option<String> {
        let target = self.target.as_ref()?;
        let herdr = lock(&self.shared.herdr).clone()?;
        Some(attach_command_line(
            target,
            &self.paths.control,
            &herdr,
            self.profile.remote_session(),
            terminal_id,
        ))
    }

    /// Retry now, skipping the backoff wait.
    pub fn reconnect(&self) {
        let _ = self.inbox.send(Msg::Reconnect);
    }
}

impl Drop for MachineLink {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.stopped.store(true, Ordering::SeqCst);
        let _ = self.inbox.send(Msg::Stop);
        if let (Some(master), Some(target)) = (lock(&self.shared.master).take(), &self.target) {
            stop_master(master, target, &self.paths.control);
        }
        if thread.join().is_err() {
            log::error!("machine supervisor for {} panicked", self.profile.label);
        }
        self.paths.remove();
    }
}

/// Asks the master to exit, then makes sure of it and reaps it.
fn stop_master(mut master: Child, target: &SshTarget, control: &Path) {
    if matches!(master.try_wait(), Ok(None)) {
        let exit = ssh(&control_args(target, control, "exit", &[]));
        let _ = run(exit, Duration::from_secs(2));
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(master.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let _ = master.kill();
    }
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
    target: SshTarget,
    paths: Paths,
    shared: Arc<Shared>,
    statuses: async_channel::Sender<MachineStatus>,
    inbox: mpsc::Receiver<Msg>,
}

impl Supervisor {
    fn run(self) {
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

    /// One connection, from starting the master until it ends.
    fn connect(&self) -> End {
        let failed = |status| End::Failed {
            status,
            was_connected: false,
        };
        let stderr = match self.spawn_master() {
            Ok(Some(stderr)) => stderr,
            Ok(None) => return End::Stop,
            Err(err) => return failed(MachineStatus::Offline(format!("running ssh: {err}"))),
        };
        if let Some(end) = self.wait_ready(&stderr) {
            self.teardown();
            return end;
        }
        let socket = match self.find_server() {
            Ok(socket) => socket,
            Err(status) => {
                self.teardown();
                return if self.stopped() {
                    End::Stop
                } else {
                    failed(status)
                };
            }
        };
        if let Err(status) = self.forward(&socket) {
            self.teardown();
            return failed(status);
        }
        log::info!(
            "machine {}: forwarding {} to {}",
            self.profile.label,
            self.paths.socket.display(),
            socket
        );
        self.set(MachineStatus::Connected);
        let end = self.watch(&stderr);
        self.teardown();
        end
    }

    /// Starts the master, keeping its stderr. `None` once stopped.
    fn spawn_master(&self) -> io::Result<Option<Arc<Mutex<Vec<u8>>>>> {
        let mut slot = lock(&self.shared.master);
        if self.stopped() {
            return Ok(None);
        }
        remove_file(&self.paths.control);
        let mut child = ssh(&master_args(&self.target, &self.paths.control))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
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
        *slot = Some(child);
        Ok(Some(stderr))
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
            status: classify_ssh_error(&text),
            was_connected,
        }
    }

    /// Finds the remote herdr and the socket of its server.
    fn find_server(&self) -> Result<String, MachineStatus> {
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
                "herdr isn't installed on {label}"
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
        remote_socket(&out.stdout, label).map_err(MachineStatus::NeedsSetup)
    }

    /// Runs `command` on the machine through the master.
    fn remote(&self, command: &str) -> Result<Output, MachineStatus> {
        let args = remote_args(&self.target, &self.paths.control, false, command);
        let out = run(ssh(&args), COMMAND_TIMEOUT)
            .map_err(|err| MachineStatus::Offline(format!("ssh: {err}")))?;
        // 255 is ssh's own failure, not the command's.
        if out.status.code() == Some(255) {
            return Err(classify_ssh_error(&String::from_utf8_lossy(&out.stderr)));
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
            Err(classify_ssh_error(&String::from_utf8_lossy(&out.stderr)))
        }
    }

    /// Waits while connected, until the master exits or a message arrives.
    fn watch(&self, stderr: &Mutex<Vec<u8>>) -> End {
        let start = lock(stderr).len();
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
        }
    }

    /// Ends the master (if this thread still owns it) and removes the files.
    fn teardown(&self) {
        let master = lock(&self.shared.master).take();
        if let Some(master) = master {
            stop_master(master, &self.target, &self.paths.control);
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
        let args = master_args(&target("ssh://u@h:2222"), Path::new("/m/abcd1234.ctl"));
        assert_eq!(
            args[args.len() - 3..],
            ["-p".to_owned(), "2222".into(), "u@h".into()]
        );
    }

    #[test]
    fn master_args_should_use_batch_mode_and_control_path() {
        let args = master_args(&target("h"), Path::new("/m/abcd1234.ctl")).join(" ");
        assert!(
            args.starts_with("-M -N -o BatchMode=yes")
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
        );
        assert_eq!(
            line,
            "ssh -t -S /m/b87d0c96.ctl -o ControlMaster=no -o BatchMode=yes -p 2222 \
             lyle@127.0.0.1 '/home/lyle/.local/bin/herdr --session work terminal attach \
             term_1 --takeover'"
        );
    }

    #[test]
    fn attach_command_should_quote_remote_path_with_spaces_twice() {
        let line = attach_command_line(
            &target("h"),
            Path::new("/m/x.ctl"),
            "/opt/my tools/herdr",
            None,
            "t",
        );
        assert!(line.ends_with(r#" h ''\''/opt/my tools/herdr'\'' terminal attach t --takeover'"#));
    }

    #[test]
    fn find_herdr_script_should_have_no_single_quotes() {
        assert!(!FIND_HERDR.contains('\''));
    }

    #[test]
    fn classify_should_treat_permission_denied_as_sign_in() {
        assert_eq!(
            classify_ssh_error("lyle@127.0.0.1: Permission denied (publickey,password).\n"),
            MachineStatus::NeedsSignIn(
                "lyle@127.0.0.1: Permission denied (publickey,password).".into()
            )
        );
    }

    #[test]
    fn classify_should_treat_host_key_failure_as_sign_in() {
        assert!(matches!(
            classify_ssh_error(
                "No ED25519 host key is known for h.\nHost key verification failed.\n"
            ),
            MachineStatus::NeedsSignIn(_)
        ));
    }

    #[test]
    fn classify_should_treat_refused_as_offline() {
        assert_eq!(
            classify_ssh_error("ssh: connect to host 127.0.0.1 port 2222: Connection refused\n"),
            MachineStatus::Offline(
                "connect to host 127.0.0.1 port 2222: Connection refused".into()
            )
        );
    }

    #[test]
    fn classify_should_treat_unresolvable_host_as_offline() {
        assert!(matches!(
            classify_ssh_error(
                "ssh: Could not resolve hostname nope: nodename nor servname provided\n"
            ),
            MachineStatus::Offline(_)
        ));
    }

    #[test]
    fn classify_should_skip_warnings() {
        assert_eq!(
            classify_ssh_error("Warning: Permanently added 'h' to the list of known hosts.\n"),
            MachineStatus::Offline("connection closed".into())
        );
    }

    const STATUS: &str = r#"{"client":{"version":"0.9.5","binary":"/home/lyle/.local/bin/herdr"},
        "server":{"status":"running","running":true,"version":"0.9.5","compatible":true,
        "socket":"/home/lyle/.config/herdr/herdr.sock","extra":1}}"#;

    #[test]
    fn remote_status_should_give_socket_when_ready() {
        assert_eq!(
            remote_socket(STATUS.as_bytes(), "box"),
            Ok("/home/lyle/.config/herdr/herdr.sock".to_owned())
        );
    }

    #[test]
    fn remote_status_should_need_setup_when_not_running() {
        let json = r#"{"client":{"version":"0.9.5"},"server":{"running":false}}"#;
        assert_eq!(
            remote_socket(json.as_bytes(), "box"),
            Err("herdr isn't running on box".to_owned())
        );
    }

    #[test]
    fn remote_status_should_need_setup_when_old() {
        let json =
            r#"{"server":{"running":true,"version":"0.9.4","compatible":true,"socket":"/s"}}"#;
        assert!(remote_socket(json.as_bytes(), "box").is_err_and(|e| e.contains("older")));
    }

    #[test]
    fn remote_status_should_need_setup_when_incompatible() {
        let json =
            r#"{"server":{"running":true,"version":"0.9.5","compatible":false,"socket":"/s"}}"#;
        assert!(remote_socket(json.as_bytes(), "box").is_err());
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
    fn paths_should_use_first_eight_chars_of_id() {
        let paths = Paths::new(Path::new("/m"), "b87d0c96779caefd39b0583c9200208c");
        assert_eq!(paths.socket, PathBuf::from("/m/b87d0c96.sock"));
    }

    #[test]
    fn paths_should_fit_in_a_typical_home() {
        let dir = Path::new("/Users/someone-long-name/.local/share/grasslands-herdr/m");
        assert!(Paths::new(dir, "b87d0c96779caefd").fit());
    }

    #[test]
    fn paths_should_not_fit_in_a_very_deep_dir() {
        let dir = PathBuf::from("/").join("d".repeat(90));
        assert!(!Paths::new(&dir, "b87d0c96").fit());
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
