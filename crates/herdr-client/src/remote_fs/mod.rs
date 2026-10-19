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

mod codec;

use codec::*;

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

    /// Atomic: writes a sibling temp file and renames it over `path`, so no
    /// one reads half a file. The new file keeps the old one's permission
    /// bits but not its owner, group or extended attributes, and hard links
    /// to the old file keep the old contents.
    ///
    /// Fails with `PermissionDenied` if `path` exists but we couldn't write
    /// it in place, as a local write would, and with `Unsupported` if the
    /// server can't rename over a file atomically (no posix-rename).
    pub fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        let n = self.temp.fetch_add(1, Ordering::Relaxed);
        self.with_session(|s| {
            if !s.posix_rename {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the machine's SFTP server can't replace files atomically",
                ));
            }
            let path = bytes(path);
            // Write through a symlink, not over it.
            let target = match s.stat(path, LSTAT) {
                Ok(attrs) if attrs.is_symlink() => s.realpath(path)?,
                _ => path.to_vec(),
            };
            let perms = match s.stat(&target, STAT) {
                Ok(attrs) => {
                    if attrs.is_file() {
                        s.check_writable(&target)?;
                    }
                    attrs.permissions.map(|mode| mode & 0o7777)
                }
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

    /// Renames `from` over `to` atomically (posix-rename; check it first).
    fn replace(&mut self, from: &[u8], to: &[u8]) -> io::Result<()> {
        let body = Body::new()
            .bytes(b"posix-rename@openssh.com")
            .bytes(from)
            .bytes(to)
            .0;
        self.status_request(EXTENDED, &body)
    }

    /// Opens `path` for writing without truncating it, then closes it: the
    /// server's own answer to "could we write this in place", which covers
    /// root, groups and ACLs that mode bits alone don't.
    fn check_writable(&mut self, path: &[u8]) -> io::Result<()> {
        let handle = self.open(path, FXF_WRITE, None).map_err(|err| {
            if err.kind() == io::ErrorKind::PermissionDenied {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is read-only", String::from_utf8_lossy(path)),
                )
            } else {
                err
            }
        })?;
        self.close(&handle)
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
}
