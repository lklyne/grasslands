//! Files and commands on a saved machine, over its [`MachineLink`]'s ssh
//! master: an SFTP session for file operations and `ssh -S <ctl>` for
//! commands (git, grep). Blocking calls, like the rest of the crate; run them
//! off the UI thread. See docs/decisions/0020-remote-files.md.
//!
//! The SFTP client is a small hand-written SFTP v3 (the version OpenSSH's
//! `sftp-server` speaks) over the stdio of `ssh -s … sftp`, so the crate
//! stays free of an async runtime. One request at a time per session, except
//! reads and writes, which keep several chunks in flight.
//!
//! [`MachineLink`]: crate::MachineLink

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::machines::{LinkAccess, lock, shell_quote};

/// One entry of a remote folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    /// Follows symlinks: a link to a folder is a folder.
    pub is_symlink: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMetadata {
    pub is_dir: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// How long one SFTP request may take before the session is dropped.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Bytes per READ or WRITE request; OpenSSH allows up to 255 KiB.
const CHUNK: u32 = 64 * 1024;
/// READ or WRITE requests in flight at once.
const WINDOW: usize = 16;
/// Larger replies are a broken stream, not a real packet.
const MAX_PACKET: usize = 16 * 1024 * 1024;
const REMOVE_TIMEOUT: Duration = Duration::from_secs(60);

/// A machine's files. Cheap to share (`Arc`); one SFTP session inside,
/// reopened on the next call after the connection drops. Every call fails
/// with `io::ErrorKind::NotConnected` while the machine isn't connected.
pub struct RemoteFs {
    link: LinkAccess,
    session: Mutex<Option<Session>>,
    temp: AtomicU32,
}

impl RemoteFs {
    pub(crate) fn new(link: LinkAccess) -> Self {
        Self {
            link,
            session: Mutex::new(None),
            temp: AtomicU32::new(0),
        }
    }

    /// The remote user's home directory (SFTP `realpath(".")`).
    pub fn home(&self) -> io::Result<PathBuf> {
        self.with_session(|s| s.realpath(b".").map(path_from))
    }

    /// One level, unsorted, without `.` and `..`.
    pub fn read_dir(&self, dir: &Path) -> io::Result<Vec<RemoteEntry>> {
        self.with_session(|s| {
            let dir = bytes(dir);
            let handle = s.handle(OPENDIR, &Body::new().bytes(dir).0)?;
            let names = s.read_names(&handle);
            let closed = s.close(&handle);
            let names = names?;
            closed?;
            names
                .into_iter()
                .filter(|(name, _)| name != b"." && name != b"..")
                .map(|(name, attrs)| {
                    let is_symlink = attrs.is_symlink();
                    let attrs = if is_symlink {
                        // A broken link stays a file with the link's own attrs.
                        s.stat(&join(dir, &name), STAT).unwrap_or(attrs)
                    } else {
                        attrs
                    };
                    Ok(RemoteEntry {
                        name: String::from_utf8_lossy(&name).into_owned(),
                        is_dir: attrs.is_dir(),
                        is_symlink,
                        len: attrs.size.unwrap_or(0),
                        modified: attrs.modified(),
                    })
                })
                .collect()
        })
    }

    pub fn metadata(&self, path: &Path) -> io::Result<RemoteMetadata> {
        self.with_session(|s| {
            let attrs = s.stat(bytes(path), STAT)?;
            Ok(RemoteMetadata {
                is_dir: attrs.is_dir(),
                len: attrs.size.unwrap_or(0),
                modified: attrs.modified(),
            })
        })
    }

    pub fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.with_session(|s| {
            let handle = s.open(bytes(path), FXF_READ, None)?;
            let data = s.read_all(&handle);
            let closed = s.close(&handle);
            let data = data?;
            closed?;
            Ok(data)
        })
    }

    /// Atomic: writes a sibling temp file, keeps the old file's permissions,
    /// then renames over `path`.
    pub fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        let n = self.temp.fetch_add(1, Ordering::Relaxed);
        self.with_session(|s| {
            let path = bytes(path);
            // Write through a symlink, not over it.
            let target = match s.stat(path, LSTAT) {
                Ok(attrs) if attrs.is_symlink() => s.realpath(path)?,
                _ => path.to_vec(),
            };
            let perms = match s.stat(&target, STAT) {
                Ok(attrs) => attrs.permissions.map(|mode| mode & 0o7777),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => return Err(err),
            };
            let temp = temp_sibling(&target, n);
            let handle = s.open(&temp, FXF_WRITE | FXF_CREAT | FXF_EXCL, perms)?;
            let written = s.write_all(&handle, contents).and_then(|()| {
                if let Some(mode) = perms {
                    // The open's mode went through the umask.
                    s.set_mode(&handle, mode)?;
                }
                s.fsync(&handle)
            });
            let closed = s.close(&handle);
            let result = written.and(closed).and_then(|()| s.replace(&temp, &target));
            if result.is_err() && !s.broken {
                let _ = s.status_request(REMOVE, &Body::new().bytes(&temp).0);
            }
            result
        })
    }

    /// Fails with `AlreadyExists` if `path` exists.
    pub fn create_file(&self, path: &Path) -> io::Result<()> {
        self.with_session(|s| {
            let path = bytes(path);
            let handle = s
                .open(path, FXF_WRITE | FXF_CREAT | FXF_EXCL, None)
                .map_err(|err| s.exists_error(path, err))?;
            s.close(&handle)
        })
    }

    pub fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.with_session(|s| {
            let path = bytes(path);
            let body = Body::new().bytes(path).u32(0).0;
            s.status_request(MKDIR, &body)
                .map_err(|err| s.exists_error(path, err))
        })
    }

    /// Fails with `AlreadyExists` if `to` exists.
    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.with_session(|s| {
            let to = bytes(to);
            if s.stat(to, LSTAT).is_ok() {
                return Err(already_exists(to));
            }
            // Plain SFTP rename never replaces an existing file.
            let body = Body::new().bytes(bytes(from)).bytes(to).0;
            s.status_request(RENAME, &body)
                .map_err(|err| s.exists_error(to, err))
        })
    }

    /// Permanent: a file, or a folder and everything in it. There is no
    /// remote trash.
    pub fn remove(&self, path: &Path) -> io::Result<()> {
        let is_dir = self.with_session(|s| {
            let attrs = s.stat(bytes(path), LSTAT)?;
            if attrs.is_dir() {
                return Ok(true);
            }
            s.status_request(REMOVE, &Body::new().bytes(bytes(path)).0)?;
            Ok(false)
        })?;
        if !is_dir {
            return Ok(());
        }
        // One `rm -rf` instead of a request per file.
        let path = path.to_string_lossy();
        let out = self.exec(&["rm", "-rf", "--", &path], None, REMOVE_TIMEOUT)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "rm -rf {path}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    /// Runs `argv` on the machine (each word shell-quoted for the remote
    /// shell), in `cwd` if given, killed after `timeout`.
    pub fn exec(
        &self,
        argv: &[&str],
        cwd: Option<&Path>,
        timeout: Duration,
    ) -> io::Result<ExecOutput> {
        self.link.connection().ok_or_else(not_connected)?;
        let out = self.link.run(&exec_command(argv, cwd), timeout)?;
        // 255 is ssh's own failure; if the link went down, say so.
        if out.status.code() == Some(255) && self.link.connection().is_none() {
            return Err(not_connected());
        }
        Ok(ExecOutput {
            status: out.status,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    /// Runs `f` on a session for the current connection, opening one if
    /// needed, and drops the session if its stream broke.
    fn with_session<T>(&self, f: impl FnOnce(&mut Session) -> io::Result<T>) -> io::Result<T> {
        let connection = self.link.connection().ok_or_else(not_connected)?;
        let mut slot = lock(&self.session);
        if slot.as_ref().is_none_or(|s| s.connection != connection) {
            *slot = None;
            *slot = Some(Session::start(&self.link, connection)?);
        }
        let Some(session) = slot.as_mut() else {
            return Err(not_connected());
        };
        let result = f(session);
        if session.broken {
            *slot = None;
        }
        result
    }
}

/// `[cd <cwd> && ]<argv…>`, every word quoted for the remote shell.
fn exec_command(argv: &[&str], cwd: Option<&Path>) -> String {
    let command = argv
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ");
    match cwd {
        Some(cwd) => format!("cd {} && {command}", shell_quote(&cwd.to_string_lossy())),
        None => command,
    }
}

/// `<dir>/.<name>.grasslands-<pid>-<n>.tmp`, next to `path`.
fn temp_sibling(path: &[u8], n: u32) -> Vec<u8> {
    let split = path.iter().rposition(|&b| b == b'/').map_or(0, |i| i + 1);
    let (dir, name) = path.split_at(split);
    let mut temp = dir.to_vec();
    temp.push(b'.');
    temp.extend_from_slice(name);
    temp.extend_from_slice(format!(".grasslands-{}-{n}.tmp", std::process::id()).as_bytes());
    temp
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    if !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

fn bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn path_from(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&bytes))
}

fn not_connected() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "the machine isn't connected")
}

fn already_exists(path: &[u8]) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} already exists", String::from_utf8_lossy(path)),
    )
}

// SFTP v3 packet types (draft-ietf-secsh-filexfer-02).
const INIT: u8 = 1;
const VERSION: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const FSETSTAT: u8 = 10;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;
const EXTENDED: u8 = 200;

const FXF_READ: u32 = 0x01;
const FXF_WRITE: u32 = 0x02;
const FXF_CREAT: u32 = 0x08;
const FXF_EXCL: u32 = 0x20;

const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;
const FX_NO_SUCH_FILE: u32 = 2;
const FX_PERMISSION_DENIED: u32 = 3;
const FX_BAD_MESSAGE: u32 = 5;
const FX_NO_CONNECTION: u32 = 6;
const FX_CONNECTION_LOST: u32 = 7;
const FX_OP_UNSUPPORTED: u32 = 8;

const ATTR_SIZE: u32 = 0x01;
const ATTR_UIDGID: u32 = 0x02;
const ATTR_PERMISSIONS: u32 = 0x04;
const ATTR_ACMODTIME: u32 = 0x08;
const ATTR_EXTENDED: u32 = 0x8000_0000;

const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;

/// A reply: its type, request id (the version, for VERSION) and the rest.
#[derive(Debug)]
struct Packet {
    kind: u8,
    id: u32,
    body: Vec<u8>,
}

impl Packet {
    /// The status code and message, if this is a STATUS reply.
    fn status(&self) -> io::Result<Option<(u32, String)>> {
        if self.kind != STATUS {
            return Ok(None);
        }
        let mut r = Reader(&self.body);
        let code = r.u32()?;
        // OpenSSH always sends a message; very old servers may not.
        let message = r.bytes().map(|m| String::from_utf8_lossy(m).into_owned());
        Ok(Some((code, message.unwrap_or_default())))
    }

    /// The body of a reply of type `kind`; a STATUS becomes its error.
    fn expect(&self, kind: u8) -> io::Result<Reader<'_>> {
        if self.kind == kind {
            return Ok(Reader(&self.body));
        }
        match self.status()? {
            Some((code, message)) => Err(status_error(code, message)),
            None => Err(bad_message(format!("unexpected reply type {}", self.kind))),
        }
    }

    fn expect_ok(&self) -> io::Result<()> {
        match self.status()? {
            Some((FX_OK, _)) => Ok(()),
            Some((code, message)) => Err(status_error(code, message)),
            None => Err(bad_message(format!("unexpected reply type {}", self.kind))),
        }
    }
}

fn status_error(code: u32, message: String) -> io::Error {
    let kind = match code {
        FX_EOF => io::ErrorKind::UnexpectedEof,
        FX_NO_SUCH_FILE => io::ErrorKind::NotFound,
        FX_PERMISSION_DENIED => io::ErrorKind::PermissionDenied,
        FX_BAD_MESSAGE => io::ErrorKind::InvalidData,
        FX_NO_CONNECTION | FX_CONNECTION_LOST => io::ErrorKind::NotConnected,
        FX_OP_UNSUPPORTED => io::ErrorKind::Unsupported,
        _ => io::ErrorKind::Other,
    };
    let message = if message.is_empty() {
        format!("sftp error {code}")
    } else {
        message
    };
    io::Error::new(kind, message)
}

fn bad_message(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("sftp: {message}"))
}

/// An outgoing packet body, big-endian with u32-length strings.
struct Body(Vec<u8>);

impl Body {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    fn bytes(self, value: &[u8]) -> Self {
        let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
        let mut body = self.u32(len);
        body.0.extend_from_slice(value);
        body
    }

    /// Attributes with only the permissions, when given.
    fn mode(self, mode: Option<u32>) -> Self {
        match mode {
            Some(mode) => self.u32(ATTR_PERMISSIONS).u32(mode),
            None => self.u32(0),
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(bad_message("short packet".to_owned()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u32(&mut self) -> io::Result<u32> {
        let mut buf = [0; 4];
        buf.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(buf))
    }

    fn u64(&mut self) -> io::Result<u64> {
        let mut buf = [0; 8];
        buf.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(buf))
    }

    fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn attrs(&mut self) -> io::Result<Attrs> {
        let flags = self.u32()?;
        let mut attrs = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            attrs.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            self.take(8)?;
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attrs.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            let _atime = self.u32()?;
            attrs.mtime = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(attrs)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Attrs {
    size: Option<u64>,
    permissions: Option<u32>,
    mtime: Option<u32>,
}

impl Attrs {
    fn file_type(&self) -> Option<u32> {
        self.permissions.map(|mode| mode & S_IFMT)
    }

    fn is_dir(&self) -> bool {
        self.file_type() == Some(S_IFDIR)
    }

    fn is_symlink(&self) -> bool {
        self.file_type() == Some(S_IFLNK)
    }

    fn modified(&self) -> Option<SystemTime> {
        self.mtime
            .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs.into()))
    }
}

/// One `ssh -s … sftp` process and the protocol state over its stdio.
struct Session {
    /// The link connection this session was opened on.
    connection: u64,
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<io::Result<Packet>>,
    /// Replies that arrived while waiting for another id.
    early: HashMap<u32, Packet>,
    stderr: Arc<Mutex<Vec<u8>>>,
    next_id: u32,
    /// The stream failed or timed out; the session is dropped after the call.
    broken: bool,
    posix_rename: bool,
    fsync: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Session {
    fn start(link: &LinkAccess, connection: u64) -> io::Result<Self> {
        let mut child = link
            .sftp()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr_pipe)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("ssh started without pipes"));
        };
        let (tx, replies) = mpsc::channel();
        thread::Builder::new()
            .name("sftp-reader".to_owned())
            .spawn(move || read_packets(stdout, &tx))?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr);
        thread::Builder::new()
            .name("sftp-stderr".to_owned())
            .spawn(move || drain_into(stderr_pipe, &sink))?;
        let mut session = Self {
            connection,
            child,
            stdin,
            replies,
            early: HashMap::new(),
            stderr,
            next_id: 0,
            broken: false,
            posix_rename: false,
            fsync: false,
        };
        // INIT carries the version where other requests carry an id.
        session.frame(INIT, 3, &[])?;
        let version = session.next_reply()?;
        if version.kind != VERSION {
            return Err(bad_message(format!(
                "expected VERSION, got {}",
                version.kind
            )));
        }
        let mut r = Reader(&version.body);
        while !r.0.is_empty() {
            let name = r.bytes()?;
            let _data = r.bytes()?;
            match name {
                b"posix-rename@openssh.com" => session.posix_rename = true,
                b"fsync@openssh.com" => session.fsync = true,
                _ => {}
            }
        }
        Ok(session)
    }

    fn frame(&mut self, kind: u8, id: u32, body: &[u8]) -> io::Result<()> {
        let len = u32::try_from(body.len() + 5).map_err(|_| bad_message("too long".to_owned()))?;
        let mut packet = Vec::with_capacity(body.len() + 9);
        packet.extend_from_slice(&len.to_be_bytes());
        packet.push(kind);
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(body);
        let sent = self
            .stdin
            .write_all(&packet)
            .and_then(|()| self.stdin.flush());
        sent.map_err(|_| self.lost())
    }

    fn send(&mut self, kind: u8, body: &[u8]) -> io::Result<u32> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.frame(kind, id, body)?;
        Ok(id)
    }

    fn next_reply(&mut self) -> io::Result<Packet> {
        match self.replies.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(packet)) => Ok(packet),
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => Err(self.lost()),
            Err(RecvTimeoutError::Timeout) => {
                self.broken = true;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "sftp request timed out",
                ))
            }
        }
    }

    fn recv(&mut self, id: u32) -> io::Result<Packet> {
        if let Some(packet) = self.early.remove(&id) {
            return Ok(packet);
        }
        loop {
            let packet = self.next_reply()?;
            if packet.id == id {
                return Ok(packet);
            }
            self.early.insert(packet.id, packet);
        }
    }

    /// Marks the session broken; the error names what ssh said, if anything.
    fn lost(&mut self) -> io::Error {
        self.broken = true;
        // Let the stderr reader catch ssh's last words.
        thread::sleep(Duration::from_millis(20));
        let stderr = String::from_utf8_lossy(&lock(&self.stderr))
            .trim()
            .to_owned();
        let message = if stderr.is_empty() {
            "the sftp session ended".to_owned()
        } else {
            format!("the sftp session ended: {stderr}")
        };
        io::Error::new(io::ErrorKind::NotConnected, message)
    }

    fn request(&mut self, kind: u8, body: &[u8]) -> io::Result<Packet> {
        let id = self.send(kind, body)?;
        self.recv(id)
    }

    fn status_request(&mut self, kind: u8, body: &[u8]) -> io::Result<()> {
        self.request(kind, body)?.expect_ok()
    }

    fn handle(&mut self, kind: u8, body: &[u8]) -> io::Result<Vec<u8>> {
        let reply = self.request(kind, body)?;
        Ok(reply.expect(HANDLE)?.bytes()?.to_vec())
    }

    fn open(&mut self, path: &[u8], flags: u32, mode: Option<u32>) -> io::Result<Vec<u8>> {
        self.handle(OPEN, &Body::new().bytes(path).u32(flags).mode(mode).0)
    }

    fn close(&mut self, handle: &[u8]) -> io::Result<()> {
        self.status_request(CLOSE, &Body::new().bytes(handle).0)
    }

    /// STAT (follows links) or LSTAT.
    fn stat(&mut self, path: &[u8], kind: u8) -> io::Result<Attrs> {
        let reply = self.request(kind, &Body::new().bytes(path).0)?;
        reply.expect(ATTRS)?.attrs()
    }

    fn realpath(&mut self, path: &[u8]) -> io::Result<Vec<u8>> {
        let reply = self.request(REALPATH, &Body::new().bytes(path).0)?;
        let mut r = reply.expect(NAME)?;
        if r.u32()? == 0 {
            return Err(bad_message("realpath returned no name".to_owned()));
        }
        Ok(r.bytes()?.to_vec())
    }

    /// Every name and its (lstat) attributes in an open folder.
    fn read_names(&mut self, handle: &[u8]) -> io::Result<Vec<(Vec<u8>, Attrs)>> {
        let mut names = Vec::new();
        loop {
            let reply = self.request(READDIR, &Body::new().bytes(handle).0)?;
            if let Some((FX_EOF, _)) = reply.status()? {
                return Ok(names);
            }
            let mut r = reply.expect(NAME)?;
            for _ in 0..r.u32()? {
                let name = r.bytes()?.to_vec();
                let _long_name = r.bytes()?;
                names.push((name, r.attrs()?));
            }
        }
    }

    /// The whole file, with up to [`WINDOW`] reads in flight.
    fn read_all(&mut self, handle: &[u8]) -> io::Result<Vec<u8>> {
        let read = |offset: u64, len: u32| Body::new().bytes(handle).u64(offset).u32(len).0;
        let mut chunks = BTreeMap::new();
        let mut in_flight = VecDeque::new();
        let mut next = 0;
        let mut eof: Option<u64> = None;
        loop {
            while in_flight.len() < WINDOW && eof.is_none() {
                let id = self.send(READ, &read(next, CHUNK))?;
                in_flight.push_back((id, next, CHUNK));
                next += u64::from(CHUNK);
            }
            let Some((id, offset, len)) = in_flight.pop_front() else {
                break;
            };
            let reply = self.recv(id)?;
            if let Some((FX_EOF, _)) = reply.status()? {
                eof = Some(eof.map_or(offset, |end| end.min(offset)));
                continue;
            }
            let data = reply.expect(DATA)?.bytes()?.to_vec();
            let got = u32::try_from(data.len()).unwrap_or(u32::MAX);
            if got == 0 {
                eof = Some(eof.map_or(offset, |end| end.min(offset)));
                continue;
            }
            if got < len {
                // Short read: ask for the rest of this chunk.
                let rest = offset + u64::from(got);
                let id = self.send(READ, &read(rest, len - got))?;
                in_flight.push_back((id, rest, len - got));
            }
            chunks.insert(offset, data);
        }
        let end = eof.unwrap_or(next);
        let mut out = Vec::new();
        for (offset, data) in chunks.range(..end) {
            if *offset != out.len() as u64 {
                return Err(bad_message("file changed while reading".to_owned()));
            }
            out.extend_from_slice(data);
        }
        Ok(out)
    }

    /// Writes `data` from offset 0, with up to [`WINDOW`] writes in flight.
    fn write_all(&mut self, handle: &[u8], data: &[u8]) -> io::Result<()> {
        let mut in_flight = VecDeque::new();
        let mut offset = 0u64;
        for chunk in data.chunks(CHUNK as usize) {
            if in_flight.len() == WINDOW
                && let Some(id) = in_flight.pop_front()
            {
                self.recv(id)?.expect_ok()?;
            }
            let body = Body::new().bytes(handle).u64(offset).bytes(chunk).0;
            in_flight.push_back(self.send(WRITE, &body)?);
            offset += chunk.len() as u64;
        }
        while let Some(id) = in_flight.pop_front() {
            self.recv(id)?.expect_ok()?;
        }
        Ok(())
    }

    fn set_mode(&mut self, handle: &[u8], mode: u32) -> io::Result<()> {
        self.status_request(FSETSTAT, &Body::new().bytes(handle).mode(Some(mode)).0)
    }

    /// Flushes to disk where the server supports it.
    fn fsync(&mut self, handle: &[u8]) -> io::Result<()> {
        if !self.fsync {
            return Ok(());
        }
        let body = Body::new().bytes(b"fsync@openssh.com").bytes(handle).0;
        self.status_request(EXTENDED, &body)
    }

    /// Renames `from` over `to`: atomic with posix-rename, else remove first.
    fn replace(&mut self, from: &[u8], to: &[u8]) -> io::Result<()> {
        if self.posix_rename {
            let body = Body::new()
                .bytes(b"posix-rename@openssh.com")
                .bytes(from)
                .bytes(to)
                .0;
            return self.status_request(EXTENDED, &body);
        }
        match self.status_request(REMOVE, &Body::new().bytes(to).0) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        self.status_request(RENAME, &Body::new().bytes(from).bytes(to).0)
    }

    /// SFTP v3 reports "exists" as a bare failure; check and say so.
    fn exists_error(&mut self, path: &[u8], err: io::Error) -> io::Error {
        if err.kind() == io::ErrorKind::Other && !self.broken && self.stat(path, LSTAT).is_ok() {
            already_exists(path)
        } else {
            err
        }
    }
}

/// Splits the server's stdout into packets until it ends.
fn read_packets(mut stdout: impl Read, tx: &mpsc::Sender<io::Result<Packet>>) {
    loop {
        let packet = read_packet(&mut stdout);
        let failed = packet.is_err();
        if tx.send(packet).is_err() || failed {
            return;
        }
    }
}

fn read_packet(stdout: &mut impl Read) -> io::Result<Packet> {
    let mut head = [0; 4];
    stdout.read_exact(&mut head)?;
    let len = u32::from_be_bytes(head) as usize;
    if !(5..=MAX_PACKET).contains(&len) {
        return Err(bad_message(format!("bad packet length {len}")));
    }
    let mut buf = vec![0; len];
    stdout.read_exact(&mut buf)?;
    let mut id = [0; 4];
    id.copy_from_slice(&buf[1..5]);
    Ok(Packet {
        kind: buf[0],
        id: u32::from_be_bytes(id),
        body: buf.split_off(5),
    })
}

fn drain_into(mut pipe: impl Read, sink: &Mutex<Vec<u8>>) {
    let mut buf = [0; 1024];
    while let Ok(n @ 1..) = pipe.read(&mut buf) {
        lock(sink).extend_from_slice(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_command_should_quote_every_word_and_cd_first() {
        let cmd = exec_command(
            &["git", "log", "--format=%s", "it's"],
            Some(Path::new("/a b")),
        );
        assert_eq!(cmd, r"cd '/a b' && git log '--format=%s' 'it'\''s'");
    }

    #[test]
    fn exec_command_should_quote_empty_words() {
        assert_eq!(exec_command(&["printf", ""], None), "printf ''");
    }

    #[test]
    fn temp_sibling_should_sit_next_to_the_file() {
        let temp = temp_sibling(b"/home/u/src/main.rs", 7);
        let expected = format!(
            "/home/u/src/.main.rs.grasslands-{}-7.tmp",
            std::process::id()
        );
        assert_eq!(temp, expected.into_bytes());
    }

    #[test]
    fn temp_sibling_should_handle_a_bare_name() {
        let temp = temp_sibling(b"notes", 0);
        assert!(temp.starts_with(b".notes.grasslands-"));
    }

    #[test]
    fn attrs_should_read_every_field_and_skip_extended() {
        let body = Body::new()
            .u32(ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS | ATTR_ACMODTIME | ATTR_EXTENDED)
            .u64(42)
            .u32(1000)
            .u32(1000)
            .u32(S_IFDIR | 0o755)
            .u32(1)
            .u32(1_700_000_000)
            .u32(1)
            .bytes(b"k")
            .bytes(b"v")
            .u32(99)
            .0;
        let mut r = Reader(&body);
        let attrs = r.attrs().unwrap();
        assert_eq!(
            (attrs.size, attrs.is_dir(), attrs.mtime, r.u32().unwrap()),
            (Some(42), true, Some(1_700_000_000), 99)
        );
    }

    #[test]
    fn attrs_should_see_symlinks() {
        let attrs = Attrs {
            permissions: Some(S_IFLNK | 0o777),
            ..Attrs::default()
        };
        assert!(attrs.is_symlink() && !attrs.is_dir());
    }

    #[test]
    fn status_should_map_no_such_file_to_not_found() {
        let err = status_error(FX_NO_SUCH_FILE, "No such file".to_owned());
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn reader_should_refuse_short_packets() {
        let mut r = Reader(&[0, 0, 0, 9, 1]);
        assert!(r.bytes().is_err());
    }

    #[test]
    fn read_packet_should_split_type_id_and_body() {
        let mut wire: &[u8] = &[0, 0, 0, 7, HANDLE, 0, 0, 0, 5, 0xAB, 0xCD];
        let packet = read_packet(&mut wire).unwrap();
        assert_eq!(
            (packet.kind, packet.id, packet.body),
            (HANDLE, 5, vec![0xAB, 0xCD])
        );
    }
}
