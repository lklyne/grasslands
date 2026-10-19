//! One machine status for the whole app.
//!
//! herdr-client keeps two state machines per machine: a saved machine's SSH
//! link (`MachineStatus`) and every machine's sync (`ConnectionStatus`).
//! They stay separate there, since each drives its own retries. The app asks
//! only [`Status`], derived here: a saved machine's from its link (the link
//! already probes herdr there), Local's, which has no link, from the setup
//! check and its sync.

use crate::herdr::{ConnectionStatus, MachineStatus};

/// Whether a machine can be used now, and in herdr's words why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    /// SSH needs a password, passphrase or host-key approval.
    NeedsSignIn(String),
    /// herdr there (or Local's herdr) is missing, too old or not ready.
    NeedsSetup(String),
    /// Couldn't reach it, or the connection dropped. `retrying` while
    /// herdr-client keeps trying with backoff; not when nothing will (a
    /// refused endpoint).
    Offline {
        reason: String,
        retrying: bool,
    },
    /// Disabled in herdr, or not in GRASSLANDS_DEV_MACHINES in a debug build.
    Disabled(String),
}

impl Status {
    /// Local's status: setup first (`needs_setup`, see
    /// `Setup::needs_setup`), then its sync's connection.
    pub fn local(connection: &ConnectionStatus, needs_setup: bool) -> Self {
        if needs_setup {
            return Self::NeedsSetup("herdr needs setup".to_owned());
        }
        match connection {
            ConnectionStatus::Connecting => Self::Connecting,
            ConnectionStatus::Connected => Self::Connected,
            ConnectionStatus::Reconnecting { reason } => Self::Offline {
                reason: reason.clone(),
                retrying: true,
            },
            ConnectionStatus::Disconnected { reason } => Self::Offline {
                reason: reason.clone(),
                retrying: false,
            },
        }
    }

    /// A saved machine's status, from its link.
    pub fn remote(link: &MachineStatus) -> Self {
        match link {
            MachineStatus::Connecting => Self::Connecting,
            MachineStatus::Connected => Self::Connected,
            MachineStatus::NeedsSignIn(reason) => Self::NeedsSignIn(reason.clone()),
            MachineStatus::NeedsSetup(reason) => Self::NeedsSetup(reason.clone()),
            // The link retries with backoff until it's stopped.
            MachineStatus::Offline(reason) => Self::Offline {
                reason: reason.clone(),
                retrying: true,
            },
            MachineStatus::Disabled(reason) => Self::Disabled(reason.clone()),
        }
    }

    /// Whether its herdr and files can be used now.
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Connected)
    }

    /// Why it can't be used, in herdr's (or ssh's) words.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Connecting | Self::Connected => None,
            Self::NeedsSignIn(reason)
            | Self::NeedsSetup(reason)
            | Self::Offline { reason, .. }
            | Self::Disabled(reason) => Some(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Status;
    use crate::herdr::{ConnectionStatus, MachineStatus};

    fn reason() -> String {
        "connection refused".to_owned()
    }

    #[test]
    fn local_derives_from_setup_then_its_sync() {
        let table = [
            (ConnectionStatus::Connecting, false, Status::Connecting),
            (ConnectionStatus::Connected, false, Status::Connected),
            (
                ConnectionStatus::Reconnecting { reason: reason() },
                false,
                Status::Offline {
                    reason: reason(),
                    retrying: true,
                },
            ),
            (
                ConnectionStatus::Disconnected { reason: reason() },
                false,
                Status::Offline {
                    reason: reason(),
                    retrying: false,
                },
            ),
            (
                ConnectionStatus::Connected,
                true,
                Status::NeedsSetup("herdr needs setup".to_owned()),
            ),
        ];
        for (connection, needs_setup, expected) in table {
            assert_eq!(
                Status::local(&connection, needs_setup),
                expected,
                "{connection:?}, needs setup: {needs_setup}"
            );
        }
    }

    #[test]
    fn a_saved_machine_derives_from_its_link() {
        let table = [
            (MachineStatus::Connecting, Status::Connecting),
            (MachineStatus::Connected, Status::Connected),
            (
                MachineStatus::NeedsSignIn(reason()),
                Status::NeedsSignIn(reason()),
            ),
            (
                MachineStatus::NeedsSetup(reason()),
                Status::NeedsSetup(reason()),
            ),
            (
                MachineStatus::Offline(reason()),
                Status::Offline {
                    reason: reason(),
                    retrying: true,
                },
            ),
            (
                MachineStatus::Disabled(reason()),
                Status::Disabled(reason()),
            ),
        ];
        for (link, expected) in table {
            assert_eq!(Status::remote(&link), expected, "{link:?}");
        }
    }

    #[test]
    fn only_connected_is_usable() {
        let usable: Vec<bool> = [
            Status::Connecting,
            Status::Connected,
            Status::NeedsSignIn(reason()),
            Status::NeedsSetup(reason()),
            Status::Offline {
                reason: reason(),
                retrying: true,
            },
            Status::Disabled(reason()),
        ]
        .iter()
        .map(Status::is_usable)
        .collect();
        assert_eq!(usable, [false, true, false, false, false, false]);
    }

    #[test]
    fn an_offline_machine_says_why() {
        let status = Status::Offline {
            reason: reason(),
            retrying: true,
        };
        assert_eq!(status.reason(), Some("connection refused"));
    }
}
