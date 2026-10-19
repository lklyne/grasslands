//! Whether this machine's herdr can serve the app, and the commands that fix
//! it when it can't.
//!
//! The app uses the herdr the user's terminal uses (`HERDR_BIN`, else `herdr`
//! on `PATH`) and never ships its own, so a missing or old herdr is fixed
//! through whatever installed it: Homebrew, mise, or herdr's own installer.
//! `herdr status --json` reports the binary and the running server in one call.

use std::ffi::OsString;
use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::endpoint::{Endpoint, default_socket_path};

/// The oldest herdr whose API the app speaks (`docs/herdr-api-0.9.5.schema.json`).
pub const MIN_VERSION: Version = Version::new(0, 9, 5);

/// herdr's own installer, for machines without Homebrew.
const INSTALL_SCRIPT: &str = "curl -fsSL https://herdr.dev/install.sh | sh";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Reads `0.9.5`, `v0.9.5`, `herdr 0.9.5` or `0.9.5-preview.2`; a
    /// pre-release counts as its release.
    pub fn parse(text: &str) -> Option<Self> {
        let word = text.split_whitespace().find(|word| {
            word.trim_start_matches('v')
                .starts_with(|c: char| c.is_ascii_digit())
        })?;
        let mut parts = word.trim_start_matches('v').splitn(3, '.').map(|part| {
            let digits = part
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(part.len());
            part[..digits].parse::<u32>().ok()
        });
        Some(Self::new(
            parts.next()??,
            parts.next()??,
            parts.next().flatten().unwrap_or(0),
        ))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What installed the herdr binary, which decides how it is updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Homebrew,
    Mise,
    Nix,
    /// herdr's own installer or a manual download; `herdr update` manages it.
    Direct,
}

impl InstallMethod {
    /// Judged from where the binary really lives (symlinks followed).
    pub fn of(binary: &Path) -> Self {
        let real = binary
            .canonicalize()
            .unwrap_or_else(|_| binary.to_path_buf());
        let real = real.to_string_lossy();
        if real.contains("/Cellar/") {
            Self::Homebrew
        } else if real.contains("/mise/") {
            Self::Mise
        } else if real.starts_with("/nix/") {
            Self::Nix
        } else {
            Self::Direct
        }
    }
}

/// The herdr binary the app found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub binary: PathBuf,
    /// `None` when herdr is too old to report it the way the app asks.
    pub version: Option<Version>,
    pub method: InstallMethod,
}

/// The result of [`Endpoint::check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// The binary is new enough, and so is the server if one is running.
    Ready,
    /// No herdr at `HERDR_BIN` or on `PATH`.
    Missing,
    /// herdr is there but couldn't be run.
    Broken { binary: PathBuf, reason: String },
    /// The binary is older than [`MIN_VERSION`].
    Outdated(Installed),
    /// The binary is fine, but a server it can't serve the app from is still
    /// running (an older version, or one herdr calls incompatible).
    ServerOutdated {
        installed: Installed,
        server: Option<Version>,
    },
}

/// A way out of a [`Check`] that isn't `Ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// `brew install herdr`, or herdr's installer without Homebrew.
    Install,
    /// Update the binary through what installed it.
    Update(InstallMethod),
    /// `herdr server stop`. This ends every pane in the session; whoever
    /// connects next starts a server from the current binary.
    StopServer,
}

impl Check {
    /// The fix to offer, if there is one.
    pub fn fix(&self) -> Option<Fix> {
        match self {
            Self::Ready | Self::Broken { .. } => None,
            Self::Missing => Some(Fix::Install),
            Self::Outdated(installed) => Some(Fix::Update(installed.method)),
            Self::ServerOutdated { .. } => Some(Fix::StopServer),
        }
    }
}

/// A command that carries out a [`Fix`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixCommand {
    argv: Vec<OsString>,
    /// Set for commands that talk to the server.
    socket_path: Option<PathBuf>,
    display: String,
}

impl FixCommand {
    /// Runs `argv`, the first element being the program.
    pub fn new(argv: Vec<OsString>) -> Self {
        let display = argv
            .iter()
            .enumerate()
            .map(|(i, arg)| {
                let arg = Path::new(arg);
                // `/opt/homebrew/bin/brew` is `brew` to whoever copies it.
                let arg = match arg.file_name() {
                    Some(name) if i == 0 && name == "brew" => Path::new(name),
                    _ => arg,
                };
                shell_quote(&arg.to_string_lossy())
            })
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            argv,
            socket_path: None,
            display,
        }
    }

    fn shell(script: &str) -> Self {
        Self {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            socket_path: None,
            display: script.to_owned(),
        }
    }

    /// The command as a person would type it.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// A shell command line for a terminal: it prints the command as a
    /// person would type it, then runs it.
    pub fn terminal_command(&self) -> String {
        let mut script = format!(
            "printf '\\033[1m$ %s\\033[0m\\n\\n' {}; ",
            shell_quote(&self.display)
        );
        if let Some(socket) = &self.socket_path {
            script.push_str(&format!(
                "export HERDR_SOCKET_PATH={}; ",
                shell_quote(&socket.to_string_lossy())
            ));
        }
        script.push_str("exec ");
        let argv = self
            .argv
            .iter()
            .map(|arg| shell_quote(&arg.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        script.push_str(&argv);
        format!("/bin/sh -c {}", shell_quote(&script))
    }
}

impl Endpoint {
    /// Checks the herdr binary and the server on this endpoint's socket.
    /// Blocks while `herdr status` runs.
    pub fn check(&self) -> Check {
        let binary = find_program(&self.bin).unwrap_or_else(|| self.bin.clone());
        let output = match self
            .herdr_command()
            .args(["status", "--json"])
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(err) if err.kind() == ErrorKind::NotFound => return Check::Missing,
            Err(err) => {
                return Check::Broken {
                    binary,
                    reason: err.to_string(),
                };
            }
        };
        let status = output
            .status
            .success()
            .then(|| serde_json::from_slice::<Status>(&output.stdout).ok())
            .flatten();
        let Some(status) = status else {
            // Older herdr has no `status --json`.
            return self.check_version_only(binary);
        };
        let binary = status.client.binary.unwrap_or(binary);
        let installed = Installed {
            method: InstallMethod::of(&binary),
            version: status.client.version.as_deref().and_then(Version::parse),
            binary,
        };
        if installed.version.is_none_or(|v| v < MIN_VERSION) {
            return Check::Outdated(installed);
        }
        let server = status.server.version.as_deref().and_then(Version::parse);
        let server_old =
            server.is_some_and(|v| v < MIN_VERSION) || status.server.compatible == Some(false);
        if status.server.running && server_old {
            return Check::ServerOutdated { installed, server };
        }
        Check::Ready
    }

    fn check_version_only(&self, binary: PathBuf) -> Check {
        let version = self
            .herdr_command()
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| Version::parse(&String::from_utf8_lossy(&output.stdout)));
        if version.is_some_and(|v| v >= MIN_VERSION) {
            // New enough, but its status didn't read; the connection will
            // say what's wrong.
            return Check::Ready;
        }
        Check::Outdated(Installed {
            method: InstallMethod::of(&binary),
            version,
            binary,
        })
    }

    /// The command for `fix`, or `None` when the app can't run it (a Nix
    /// install is updated through the user's own profile or flake).
    pub fn fix_command(&self, fix: Fix) -> Option<FixCommand> {
        let brew = || find_program(Path::new("brew"));
        match fix {
            Fix::Install => Some(match brew() {
                Some(brew) => FixCommand::new(vec![brew.into(), "install".into(), "herdr".into()]),
                None => FixCommand::shell(INSTALL_SCRIPT),
            }),
            Fix::Update(InstallMethod::Homebrew) => Some(FixCommand::new(vec![
                brew().unwrap_or_else(|| "brew".into()).into(),
                "upgrade".into(),
                "herdr".into(),
            ])),
            Fix::Update(InstallMethod::Mise) => Some(FixCommand::new(vec![
                "mise".into(),
                "upgrade".into(),
                "herdr".into(),
            ])),
            Fix::Update(InstallMethod::Nix) => None,
            Fix::Update(InstallMethod::Direct) => Some(FixCommand::new(vec![
                self.bin.clone().into(),
                "update".into(),
            ])),
            Fix::StopServer => {
                let mut argv: Vec<OsString> = vec![self.bin.clone().into()];
                if let Some(session) = &self.session {
                    argv.extend(["--session".into(), session.into()]);
                }
                argv.extend(["server".into(), "stop".into()]);
                let mut command = FixCommand::new(argv);
                // Pasted without the socket, it would stop the default
                // session's server instead.
                let default = default_socket_path(&config_home(), self.session.as_deref());
                if self.socket_path != default {
                    command.display = format!(
                        "HERDR_SOCKET_PATH={} {}",
                        shell_quote(&self.socket_path.to_string_lossy()),
                        command.display
                    );
                }
                command.socket_path = Some(self.socket_path.clone());
                Some(command)
            }
        }
    }

    /// `herdr [--session S]`, aimed at this endpoint's socket.
    fn herdr_command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        if let Some(session) = &self.session {
            command.arg("--session").arg(session);
        }
        command.env("HERDR_SOCKET_PATH", &self.socket_path);
        command
    }
}

/// The parts of `herdr status --json` the check reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Status {
    client: ClientStatus,
    server: ServerStatus,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClientStatus {
    version: Option<String>,
    binary: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ServerStatus {
    running: bool,
    version: Option<String>,
    compatible: Option<bool>,
}

/// `$XDG_CONFIG_HOME`, else `~/.config`: where herdr looks for its socket
/// when `HERDR_SOCKET_PATH` isn't set.
fn config_home() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    var("XDG_CONFIG_HOME").map_or_else(
        || PathBuf::from(var("HOME").unwrap_or_default()).join(".config"),
        PathBuf::from,
    )
}

/// `program` itself if it is a path, else the first match on `PATH`.
fn find_program(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return program.is_file().then(|| program.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-:@".contains(c))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint {
            bin: "/opt/homebrew/bin/herdr".into(),
            session: Some("dev".into()),
            socket_path: "/tmp/herdr.sock".into(),
            auto_start: false,
            server_log: None,
        }
    }

    #[test]
    fn version_should_parse_herdr_version_output() {
        assert_eq!(Version::parse("herdr 0.9.5\n"), Some(Version::new(0, 9, 5)));
    }

    #[test]
    fn version_should_count_a_preview_as_its_release() {
        assert_eq!(
            Version::parse("v0.10.0-preview.2"),
            Some(Version::new(0, 10, 0))
        );
    }

    #[test]
    fn version_should_compare_numerically() {
        assert!(Version::new(0, 10, 0) > Version::new(0, 9, 5));
    }

    #[test]
    fn version_should_reject_text_without_a_number() {
        assert_eq!(Version::parse("herdr unknown"), None);
    }

    #[test]
    fn install_method_should_spot_homebrew_cellar() {
        assert_eq!(
            InstallMethod::of(Path::new("/opt/homebrew/Cellar/herdr/0.9.5/bin/herdr")),
            InstallMethod::Homebrew
        );
    }

    #[test]
    fn install_method_should_default_to_direct() {
        assert_eq!(
            InstallMethod::of(Path::new("/nonexistent/.local/bin/herdr")),
            InstallMethod::Direct
        );
    }

    #[test]
    fn stop_server_should_name_the_session_and_its_socket() {
        let command = endpoint().fix_command(Fix::StopServer).map(|c| c.display);
        assert_eq!(
            command.as_deref(),
            Some(
                "HERDR_SOCKET_PATH=/tmp/herdr.sock /opt/homebrew/bin/herdr --session dev server stop"
            )
        );
    }

    #[test]
    fn stop_server_should_leave_out_the_default_socket() {
        let endpoint = Endpoint {
            socket_path: default_socket_path(&config_home(), Some("dev")),
            ..endpoint()
        };
        let command = endpoint.fix_command(Fix::StopServer).map(|c| c.display);
        assert_eq!(
            command.as_deref(),
            Some("/opt/homebrew/bin/herdr --session dev server stop")
        );
    }

    #[test]
    fn brew_should_display_without_its_path() {
        let command = FixCommand::new(vec!["/opt/homebrew/bin/brew".into(), "upgrade".into()]);
        assert_eq!(command.display(), "brew upgrade");
    }

    #[test]
    fn nix_update_should_have_no_command() {
        assert_eq!(
            endpoint().fix_command(Fix::Update(InstallMethod::Nix)),
            None
        );
    }

    #[test]
    fn missing_binary_should_check_as_missing() {
        let endpoint = Endpoint {
            bin: "/nonexistent/herdr".into(),
            ..endpoint()
        };
        assert_eq!(endpoint.check(), Check::Missing);
    }

    #[test]
    fn terminal_command_should_print_then_run_the_command() {
        let command = FixCommand::new(vec!["/opt/homebrew/bin/brew".into(), "upgrade".into()]);
        let output = Command::new("/bin/sh")
            .args([
                "-c",
                &command.terminal_command().replace("exec ", "exec echo "),
            ])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned());
        assert_eq!(
            output.ok().as_deref(),
            Some("\u{1b}[1m$ brew upgrade\u{1b}[0m\n\n/opt/homebrew/bin/brew upgrade\n")
        );
    }
}
