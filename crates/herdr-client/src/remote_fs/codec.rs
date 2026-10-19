//! The SFTP v3 wire format: packet constants, outgoing bodies, a
//! bounds-checked reader for replies, and file attributes. Everything read
//! here comes from the remote machine, so every length is checked.

use std::io::{self, Read};
use std::time::{Duration, SystemTime};

/// Larger replies are a broken stream, not a real packet.
pub(super) const MAX_PACKET: usize = 16 * 1024 * 1024;

// SFTP v3 packet types (draft-ietf-secsh-filexfer-02).
pub(super) const INIT: u8 = 1;
pub(super) const VERSION: u8 = 2;
pub(super) const OPEN: u8 = 3;
pub(super) const CLOSE: u8 = 4;
pub(super) const READ: u8 = 5;
pub(super) const WRITE: u8 = 6;
pub(super) const LSTAT: u8 = 7;
pub(super) const FSETSTAT: u8 = 10;
pub(super) const OPENDIR: u8 = 11;
pub(super) const READDIR: u8 = 12;
pub(super) const REMOVE: u8 = 13;
pub(super) const MKDIR: u8 = 14;
pub(super) const REALPATH: u8 = 16;
pub(super) const STAT: u8 = 17;
pub(super) const RENAME: u8 = 18;
pub(super) const STATUS: u8 = 101;
pub(super) const HANDLE: u8 = 102;
pub(super) const DATA: u8 = 103;
pub(super) const NAME: u8 = 104;
pub(super) const ATTRS: u8 = 105;
pub(super) const EXTENDED: u8 = 200;

pub(super) const FXF_READ: u32 = 0x01;
pub(super) const FXF_WRITE: u32 = 0x02;
pub(super) const FXF_CREAT: u32 = 0x08;
pub(super) const FXF_EXCL: u32 = 0x20;

pub(super) const FX_OK: u32 = 0;
pub(super) const FX_EOF: u32 = 1;
pub(super) const FX_NO_SUCH_FILE: u32 = 2;
pub(super) const FX_PERMISSION_DENIED: u32 = 3;
pub(super) const FX_BAD_MESSAGE: u32 = 5;
pub(super) const FX_NO_CONNECTION: u32 = 6;
pub(super) const FX_CONNECTION_LOST: u32 = 7;
pub(super) const FX_OP_UNSUPPORTED: u32 = 8;

pub(super) const ATTR_SIZE: u32 = 0x01;
pub(super) const ATTR_UIDGID: u32 = 0x02;
pub(super) const ATTR_PERMISSIONS: u32 = 0x04;
pub(super) const ATTR_ACMODTIME: u32 = 0x08;
pub(super) const ATTR_EXTENDED: u32 = 0x8000_0000;

pub(super) const S_IFMT: u32 = 0o170_000;
pub(super) const S_IFREG: u32 = 0o100_000;
pub(super) const S_IFDIR: u32 = 0o040_000;
pub(super) const S_IFLNK: u32 = 0o120_000;

/// A reply: its type, request id (the version, for VERSION) and the rest.
#[derive(Debug)]
pub(super) struct Packet {
    pub(super) kind: u8,
    pub(super) id: u32,
    pub(super) body: Vec<u8>,
}

impl Packet {
    /// The status code and message, if this is a STATUS reply.
    pub(super) fn status(&self) -> io::Result<Option<(u32, String)>> {
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
    pub(super) fn expect(&self, kind: u8) -> io::Result<Reader<'_>> {
        if self.kind == kind {
            return Ok(Reader(&self.body));
        }
        match self.status()? {
            Some((code, message)) => Err(status_error(code, message)),
            None => Err(bad_message(format!("unexpected reply type {}", self.kind))),
        }
    }

    pub(super) fn expect_ok(&self) -> io::Result<()> {
        match self.status()? {
            Some((FX_OK, _)) => Ok(()),
            Some((code, message)) => Err(status_error(code, message)),
            None => Err(bad_message(format!("unexpected reply type {}", self.kind))),
        }
    }
}

pub(super) fn status_error(code: u32, message: String) -> io::Error {
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

pub(super) fn bad_message(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("sftp: {message}"))
}

/// An outgoing packet body, big-endian with u32-length strings.
pub(super) struct Body(pub(super) Vec<u8>);

impl Body {
    pub(super) fn new() -> Self {
        Self(Vec::new())
    }

    pub(super) fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(super) fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(super) fn bytes(self, value: &[u8]) -> Self {
        let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
        let mut body = self.u32(len);
        body.0.extend_from_slice(value);
        body
    }

    /// Attributes with only the permissions, when given.
    pub(super) fn mode(self, mode: Option<u32>) -> Self {
        match mode {
            Some(mode) => self.u32(ATTR_PERMISSIONS).u32(mode),
            None => self.u32(0),
        }
    }
}

pub(super) struct Reader<'a>(pub(super) &'a [u8]);

impl<'a> Reader<'a> {
    pub(super) fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(bad_message("short packet".to_owned()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    pub(super) fn u32(&mut self) -> io::Result<u32> {
        let mut buf = [0; 4];
        buf.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(buf))
    }

    pub(super) fn u64(&mut self) -> io::Result<u64> {
        let mut buf = [0; 8];
        buf.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(buf))
    }

    pub(super) fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    pub(super) fn attrs(&mut self) -> io::Result<Attrs> {
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
pub(super) struct Attrs {
    pub(super) size: Option<u64>,
    pub(super) permissions: Option<u32>,
    pub(super) mtime: Option<u32>,
}

impl Attrs {
    pub(super) fn file_type(&self) -> Option<u32> {
        self.permissions.map(|mode| mode & S_IFMT)
    }

    pub(super) fn is_file(&self) -> bool {
        self.file_type() == Some(S_IFREG)
    }

    pub(super) fn is_dir(&self) -> bool {
        self.file_type() == Some(S_IFDIR)
    }

    pub(super) fn is_symlink(&self) -> bool {
        self.file_type() == Some(S_IFLNK)
    }

    pub(super) fn modified(&self) -> Option<SystemTime> {
        self.mtime
            .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs.into()))
    }
}

pub(super) fn read_packet(stdout: &mut impl Read) -> io::Result<Packet> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
