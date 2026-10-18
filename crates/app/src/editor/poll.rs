//! Remote files have no watcher: while the window is active, open remote
//! buffers are checked every few seconds by modification time, and re-read
//! only when it moved.

use std::io;
use std::time::{Duration, SystemTime};

pub const INTERVAL: Duration = Duration::from_secs(3);

/// What one check of a remote file found.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    /// Same modification time as last seen: nothing to read.
    Same,
    /// The time moved (or the machine reports none): read the file, and
    /// remember this time.
    Read(Option<SystemTime>),
    /// The file is gone.
    Deleted,
    /// The check failed (offline, permission); try again next time.
    Failed,
}

/// Whether a buffer is checked this round. One mid-save is skipped: its own
/// write would look like someone else's.
pub fn wanted(window_active: bool, online: bool, saving: bool) -> bool {
    window_active && online && !saving
}

/// Compares what the machine reports for a file with the time last seen.
pub fn check(seen: Option<SystemTime>, now: &io::Result<Option<SystemTime>>) -> Check {
    match now {
        Ok(Some(now)) if seen == Some(*now) => Check::Same,
        Ok(now) => Check::Read(*now),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Check::Deleted,
        Err(_) => Check::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn same_time_skips_the_read() {
        assert_eq!(check(Some(at(5)), &Ok(Some(at(5)))), Check::Same);
    }

    #[test]
    fn a_new_time_reads_and_remembers_it() {
        assert_eq!(
            check(Some(at(5)), &Ok(Some(at(6)))),
            Check::Read(Some(at(6)))
        );
    }

    #[test]
    fn no_time_reported_always_reads() {
        assert_eq!(check(None, &Ok(None)), Check::Read(None));
    }

    #[test]
    fn not_found_is_deleted() {
        assert_eq!(
            check(Some(at(5)), &Err(io::ErrorKind::NotFound.into())),
            Check::Deleted
        );
    }

    #[test]
    fn a_dropped_connection_is_not_a_deletion() {
        assert_eq!(
            check(Some(at(5)), &Err(io::ErrorKind::NotConnected.into())),
            Check::Failed
        );
    }

    #[test]
    fn only_an_active_window_polls() {
        assert!(!wanted(false, true, false));
    }

    #[test]
    fn an_offline_machine_is_not_polled() {
        assert!(!wanted(true, false, false));
    }

    #[test]
    fn a_buffer_mid_save_is_not_polled() {
        assert!(!wanted(true, true, true));
    }

    #[test]
    fn an_idle_online_buffer_is_polled() {
        assert!(wanted(true, true, false));
    }
}
