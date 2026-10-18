//! The pure parts of the machine list: ids, diffing herdr's saved machines
//! against the running links, selection and status cues.

use std::fmt;

use crate::herdr::{ConnectionStatus, MachineProfile, MachineStatus, machines};
use cues::Cue;

/// Which herdr server: the local one, or a saved machine by profile id.
/// herdr ids (workspaces, panes, terminals) are only unique within one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum MachineId {
    #[default]
    Local,
    Remote(String),
}

impl MachineId {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => f.write_str("local"),
            Self::Remote(id) => f.write_str(id),
        }
    }
}

/// What a refreshed machine list asks of the running links.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProfilesDiff {
    /// New profiles, or ones whose connection settings changed: start a link
    /// (after stopping the old one, if any).
    pub start: Vec<MachineProfile>,
    /// Profile ids no longer saved.
    pub stop: Vec<String>,
    /// Only the label changed: `(id, new label)`.
    pub relabel: Vec<(String, String)>,
}

/// Compares the running links' profiles with herdr's current list.
pub fn diff_profiles(running: &[&MachineProfile], saved: &[MachineProfile]) -> ProfilesDiff {
    let mut diff = ProfilesDiff::default();
    for profile in saved {
        match running.iter().find(|p| p.id == profile.id) {
            None => diff.start.push(profile.clone()),
            Some(old) if !same_connection(old, profile) => diff.start.push(profile.clone()),
            Some(old) if old.label != profile.label => {
                diff.relabel
                    .push((profile.id.clone(), profile.label.clone()));
            }
            Some(_) => {}
        }
    }
    diff.stop = running
        .iter()
        .filter(|old| !saved.iter().any(|p| p.id == old.id))
        .map(|old| old.id.clone())
        .collect();
    diff
}

/// Debug builds connect by label (`GRASSLANDS_DEV_MACHINES`), so a rename
/// in or out of that list counts as a new connection too.
fn same_connection(a: &MachineProfile, b: &MachineProfile) -> bool {
    a.target == b.target
        && a.session == b.session
        && a.enabled == b.enabled
        && machines::dev_allowed(&a.label) == machines::dev_allowed(&b.label)
}

/// The selected machine after the list changed: it stays if it still
/// exists, else Local.
pub fn reselect<'a>(
    selected: &MachineId,
    mut ids: impl Iterator<Item = &'a MachineId>,
) -> MachineId {
    if ids.any(|id| id == selected) {
        selected.clone()
    } else {
        MachineId::Local
    }
}

/// Local's machine status, from its connection and the setup check.
pub fn local_status(connection: &ConnectionStatus, setup_needed: Option<String>) -> MachineStatus {
    if let Some(reason) = setup_needed {
        return MachineStatus::NeedsSetup(reason);
    }
    match connection {
        ConnectionStatus::Connecting => MachineStatus::Connecting,
        ConnectionStatus::Connected => MachineStatus::Connected,
        ConnectionStatus::Reconnecting { reason } | ConnectionStatus::Disconnected { reason } => {
            MachineStatus::Offline(reason.clone())
        }
    }
}

/// A connected machine going offline warns; connecting again after losing
/// the connection (`lost`, whatever statuses came between) succeeds.
pub fn status_cue(previous: &MachineStatus, next: &MachineStatus, lost: bool) -> Option<Cue> {
    match (previous, next) {
        (MachineStatus::Connected, MachineStatus::Offline(_)) => Some(Cue::Warning),
        (_, MachineStatus::Connected) if lost => Some(Cue::Success),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MachineId, ProfilesDiff, diff_profiles, reselect, status_cue};
    use crate::herdr::{MachineProfile, MachineStatus};
    use cues::Cue;

    fn profile(id: &str, label: &str) -> MachineProfile {
        MachineProfile {
            id: id.into(),
            label: label.into(),
            target: format!("ssh://{label}"),
            session: "default".into(),
            enabled: true,
        }
    }

    #[test]
    fn diff_starts_new_profiles() {
        let saved = [profile("a", "box")];
        let diff = diff_profiles(&[], &saved);
        assert_eq!(diff.start, saved.to_vec());
    }

    #[test]
    fn diff_stops_removed_profiles() {
        let old = profile("a", "box");
        let diff = diff_profiles(&[&old], &[]);
        assert_eq!(diff.stop, vec!["a".to_owned()]);
    }

    #[test]
    fn diff_relabels_without_restarting() {
        let old = profile("a", "box");
        let new = MachineProfile {
            label: "gpu".into(),
            ..old.clone()
        };
        let diff = diff_profiles(&[&old], &[new]);
        assert_eq!(
            diff,
            ProfilesDiff {
                relabel: vec![("a".into(), "gpu".into())],
                ..ProfilesDiff::default()
            }
        );
    }

    #[test]
    fn diff_restarts_when_enabled_changes() {
        let old = profile("a", "box");
        let new = MachineProfile {
            enabled: false,
            ..old.clone()
        };
        let diff = diff_profiles(&[&old], std::slice::from_ref(&new));
        assert_eq!(diff.start, vec![new]);
    }

    #[test]
    fn diff_is_empty_for_an_unchanged_list() {
        let old = profile("a", "box");
        let diff = diff_profiles(&[&old], std::slice::from_ref(&old));
        assert_eq!(diff, ProfilesDiff::default());
    }

    #[test]
    fn machine_ids_with_the_same_terminal_id_differ() {
        let local = (MachineId::Local, "term_1");
        let remote = (MachineId::Remote("a".into()), "term_1");
        assert_ne!(local, remote);
    }

    #[test]
    fn reselect_keeps_a_machine_still_listed() {
        let ids = [MachineId::Local, MachineId::Remote("a".into())];
        assert_eq!(
            reselect(&MachineId::Remote("a".into()), ids.iter()),
            MachineId::Remote("a".into())
        );
    }

    #[test]
    fn reselect_falls_back_to_local_when_the_machine_is_gone() {
        let ids = [MachineId::Local];
        assert_eq!(
            reselect(&MachineId::Remote("a".into()), ids.iter()),
            MachineId::Local
        );
    }

    #[test]
    fn going_offline_from_connected_warns() {
        let cue = status_cue(
            &MachineStatus::Connected,
            &MachineStatus::Offline("x".into()),
            false,
        );
        assert_eq!(cue, Some(Cue::Warning));
    }

    #[test]
    fn connecting_again_after_losing_the_connection_succeeds() {
        // Offline, then setup needed, then a manual retry's Connecting.
        let cue = status_cue(&MachineStatus::Connecting, &MachineStatus::Connected, true);
        assert_eq!(cue, Some(Cue::Success));
    }

    #[test]
    fn first_connection_plays_nothing() {
        let cue = status_cue(&MachineStatus::Connecting, &MachineStatus::Connected, false);
        assert_eq!(cue, None);
    }
}
