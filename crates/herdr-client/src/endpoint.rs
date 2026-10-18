//! Where the herdr server lives and how to talk to it.
//!
//! herdr 0.9.5 answers exactly one request per socket connection, so every
//! request opens a fresh connection. Subscriptions keep theirs open.

use std::env;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Set to `1` to let a debug or test build reach a herdr outside the dev root.
pub const ALLOW_LIVE_ENV: &str = "RT_ALLOW_LIVE_HERDR";

/// Debug and test builds only talk to herdr sessions under `$HOME/` + this.
/// Agent panes inherit `HERDR_SOCKET_PATH` for the user's live herdr, so a
/// bare `cargo run`/`cargo test` would otherwise drive it.
pub const DEV_ROOT: &str = ".local/share/grasslands-herdr";

/// How to reach (and if needed start) a herdr server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// herdr executable used to start the server.
    pub bin: PathBuf,
    /// Named session, passed as `--session` when starting the server.
    pub session: Option<String>,
    /// Socket to connect to.
    pub socket_path: PathBuf,
    /// Start `herdr [--session S] server` when nothing answers on the socket.
    pub auto_start: bool,
    /// Where a server started here writes its output (appended). Without
    /// one, the output is dropped and a server that fails to start says nothing.
    pub server_log: Option<PathBuf>,
}

impl Endpoint {
    /// Reads `HERDR_BIN` (default `herdr`), `HERDR_SESSION` and `HERDR_SOCKET_PATH`.
    ///
    /// Without `HERDR_SOCKET_PATH` the socket follows herdr's own rule:
    /// `$XDG_CONFIG_HOME/herdr[/sessions/<name>]/herdr.sock`, with
    /// `XDG_CONFIG_HOME` defaulting to `~/.config`.
    ///
    /// Debug and test builds refuse a socket outside `$HOME/.local/share/grasslands-herdr`
    /// unless `RT_ALLOW_LIVE_HERDR=1` (see [`check_dev_socket`]). Release builds don't.
    pub fn from_env() -> Result<Self> {
        let var = |name: &str| env::var(name).ok().filter(|v| !v.is_empty());
        let bin = PathBuf::from(var("HERDR_BIN").unwrap_or_else(|| "herdr".to_owned()));
        let session = var("HERDR_SESSION");
        let socket_path = var("HERDR_SOCKET_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let config = var("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(var("HOME").unwrap_or_default()).join(".config")
                    });
                default_socket_path(&config, session.as_deref())
            });
        let endpoint = Self {
            bin,
            session,
            socket_path,
            auto_start: true,
            server_log: None,
        };
        endpoint.check_allowed()?;
        Ok(endpoint)
    }

    /// The dev-root guard for this endpoint, as configured by the environment.
    pub fn check_allowed(&self) -> Result<()> {
        if !guarded() {
            return Ok(());
        }
        let allow = env::var(ALLOW_LIVE_ENV).is_ok_and(|v| v == "1");
        let home = env::var_os("HOME").map(PathBuf::from);
        check_dev_socket(&self.socket_path, home.as_deref(), allow)
    }

    /// Opens a connection, starting the server first if allowed and needed.
    pub fn connect(&self) -> Result<UnixStream> {
        match UnixStream::connect(&self.socket_path) {
            Ok(stream) => Ok(stream),
            Err(err) if self.auto_start && is_not_listening(&err) => {
                self.start_server()?;
                Ok(UnixStream::connect(&self.socket_path)?)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Sends one request and returns its `result` object.
    pub fn request_value<P: Serialize>(&self, method: &str, params: &P) -> Result<Value> {
        let stream = self.connect()?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let id = next_id();
        write_request(&stream, &id, method, params)?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        if line.is_empty() {
            return Err(Error::Closed);
        }
        parse_response(&line, &id)
    }

    /// Sends one request and deserializes its `result` into `R`.
    pub fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R> {
        let value = self.request_value(method, params)?;
        serde_json::from_value(value).map_err(|source| Error::Decode {
            method: method.to_owned(),
            source,
        })
    }

    /// Starts the server in its own process group so it outlives this app, then waits
    /// until it answers `ping`.
    pub fn start_server(&self) -> Result<()> {
        self.check_allowed()?;
        log::info!(
            "starting herdr server: {:?} session={:?}",
            self.bin,
            self.session
        );
        let mut cmd = Command::new(&self.bin);
        if let Some(session) = &self.session {
            cmd.arg("--session").arg(session);
        }
        let (stdout, stderr) = self.server_output();
        cmd.arg("server")
            .env("HERDR_SOCKET_PATH", &self.socket_path)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|err| Error::ServerStart(format!("spawning {:?}: {err}", self.bin)))?;

        let deadline = Instant::now() + SERVER_START_TIMEOUT;
        while Instant::now() < deadline {
            if UnixStream::connect(&self.socket_path).is_ok() {
                // Reap the child in the background; the server itself may daemonize.
                thread::spawn(move || child.wait());
                return Ok(());
            }
            if let Ok(Some(status)) = child.try_wait() {
                // Exited early: another client may have won the race and started it.
                if UnixStream::connect(&self.socket_path).is_ok() {
                    return Ok(());
                }
                return Err(Error::ServerStart(format!(
                    "herdr server exited with {status}"
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(Error::ServerStart(format!(
            "no answer on {} after {SERVER_START_TIMEOUT:?}",
            self.socket_path.display()
        )))
    }
}

impl Endpoint {
    /// stdout and stderr for a server started here: [`server_log`](Self::server_log),
    /// or nowhere. A file, not a pipe, since the server outlives the app.
    fn server_output(&self) -> (Stdio, Stdio) {
        let log = self.server_log.as_ref().and_then(|path| {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .inspect_err(|err| log::warn!("herdr server log {}: {err}", path.display()))
                .ok()?;
            Some((file.try_clone().ok()?, file))
        });
        match log {
            Some((out, err)) => (out.into(), err.into()),
            None => (Stdio::null(), Stdio::null()),
        }
    }
}

/// Whether the dev-root guard applies to this build.
fn guarded() -> bool {
    cfg!(any(debug_assertions, test))
}

/// Refuses `socket` unless it lies under `home`/[`DEV_ROOT`] or `allow` is set.
/// The check is lexical: relative paths and `..` are refused outright.
pub fn check_dev_socket(socket: &Path, home: Option<&Path>, allow: bool) -> Result<()> {
    if allow {
        return Ok(());
    }
    let refuse = |why: &str| {
        Err(Error::Refused(format!(
            "{why}: {} is outside ~/{DEV_ROOT}. Debug builds only talk to dev herdr \
             sessions; run through scripts/dev-run, or set {ALLOW_LIVE_ENV}=1 to override",
            socket.display()
        )))
    };
    let Some(home) = home.filter(|home| home.is_absolute()) else {
        return refuse("HOME is not set");
    };
    if !socket.is_absolute() || socket.components().any(|c| c == Component::ParentDir) {
        return refuse("not a plain absolute path");
    }
    if !socket.starts_with(home.join(DEV_ROOT)) {
        return refuse("refusing herdr socket");
    }
    Ok(())
}

pub(crate) fn default_socket_path(config_home: &std::path::Path, session: Option<&str>) -> PathBuf {
    let base = config_home.join("herdr");
    match session {
        Some(name) => base.join("sessions").join(name).join("herdr.sock"),
        None => base.join("herdr.sock"),
    }
}

fn is_not_listening(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::NotFound | ErrorKind::ConnectionRefused
    )
}

pub(crate) fn next_id() -> String {
    format!("rt_{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn write_request<P: Serialize>(
    mut stream: &UnixStream,
    id: &str,
    method: &str,
    params: &P,
) -> Result<()> {
    #[derive(Serialize)]
    struct Request<'a, P> {
        id: &'a str,
        method: &'a str,
        params: &'a P,
    }
    let mut line = serde_json::to_vec(&Request { id, method, params })?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()?;
    Ok(())
}

/// Parses a response line, returning `result` or the server's error.
pub(crate) fn parse_response(line: &str, id: &str) -> Result<Value> {
    let mut value: Value = serde_json::from_str(line)?;
    let got_id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Err(Error::Server { code, message });
    }
    if got_id != id {
        return Err(Error::Protocol(format!(
            "response id {got_id:?} does not match request {id:?}"
        )));
    }
    value
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| Error::Protocol(format!("response without result: {line}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_path_should_use_sessions_dir_for_named_session() {
        let path = default_socket_path(std::path::Path::new("/c"), Some("dev"));
        assert_eq!(path, PathBuf::from("/c/herdr/sessions/dev/herdr.sock"));
    }

    #[test]
    fn default_socket_path_should_use_root_socket_without_session() {
        let path = default_socket_path(std::path::Path::new("/c"), None);
        assert_eq!(path, PathBuf::from("/c/herdr/herdr.sock"));
    }

    const HOME: &str = "/Users/u";

    fn check(socket: &str, allow: bool) -> Result<()> {
        check_dev_socket(Path::new(socket), Some(Path::new(HOME)), allow)
    }

    #[test]
    fn dev_guard_should_accept_socket_under_dev_root() {
        let socket = "/Users/u/.local/share/grasslands-herdr/herdr/sessions/dev-x/herdr.sock";
        assert!(check(socket, false).is_ok());
    }

    #[test]
    fn dev_guard_should_refuse_live_default_socket() {
        let err = check("/Users/u/.config/herdr/herdr.sock", false).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));
    }

    #[test]
    fn dev_guard_should_refuse_dot_dot_escape() {
        let socket = "/Users/u/.local/share/grasslands-herdr/../../../.config/herdr/herdr.sock";
        assert!(check(socket, false).is_err());
    }

    #[test]
    fn dev_guard_should_refuse_relative_socket() {
        assert!(check("herdr.sock", false).is_err());
    }

    #[test]
    fn dev_guard_should_refuse_lookalike_prefix() {
        let socket = "/Users/u/.local/share/grasslands-herdr-live/herdr.sock";
        assert!(check(socket, false).is_err());
    }

    #[test]
    fn dev_guard_should_refuse_without_home() {
        let socket = "/Users/u/.local/share/grasslands-herdr/herdr.sock";
        assert!(check_dev_socket(Path::new(socket), None, false).is_err());
    }

    #[test]
    fn dev_guard_should_allow_anything_when_overridden() {
        assert!(check("/Users/u/.config/herdr/herdr.sock", true).is_ok());
    }

    #[test]
    fn start_server_should_refuse_outside_dev_root_without_spawning() {
        let endpoint = Endpoint {
            bin: "/nonexistent/herdr-must-not-run".into(),
            session: None,
            socket_path: "/nonexistent/live/herdr.sock".into(),
            auto_start: true,
            server_log: None,
        };
        // Refused before spawning: a spawn attempt would be a ServerStart error.
        // Skipped if the override happens to be set in this environment.
        if env::var(ALLOW_LIVE_ENV).is_ok_and(|v| v == "1") {
            return;
        }
        assert!(matches!(endpoint.start_server(), Err(Error::Refused(_))));
    }

    #[test]
    fn parse_response_should_return_server_error() {
        let err = parse_response(
            r#"{"id":"1","error":{"code":"not_found","message":"pane not found"}}"#,
            "1",
        )
        .unwrap_err();
        assert!(matches!(err, Error::Server { code, .. } if code == "not_found"));
    }

    #[test]
    fn parse_response_should_return_result() {
        let result = parse_response(r#"{"id":"1","result":{"type":"ok"}}"#, "1").unwrap();
        assert_eq!(result["type"], "ok");
    }
}
