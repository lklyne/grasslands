//! herdr's own interactive machine commands, run in a terminal on Local the
//! way the herdr setup fixes run (docs/decisions/0019-remote-machines.md):
//! signing in to a saved machine, setting up herdr on it, and adding one.
//! The command does the work; the app only checks the machine afterwards.

use std::ffi::OsString;
use std::path::Path;

use super::{Machine, MachineId};
use crate::herdr::{FixCommand, MachineProfile, MachineStatus};

/// What a run is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineTask {
    /// `herdr machine reconnect <label>`: answers ssh's prompts once.
    SignIn(MachineId),
    /// `herdr --remote <target> [--session S]`: herdr's remote setup.
    SetUp(MachineId),
    /// `herdr machine add <host>`.
    Add { host: String },
}

impl MachineTask {
    /// What fixes a saved machine that can't connect: signing in when ssh
    /// refused (or the machine is unreachable, which a sign-in shows),
    /// herdr's setup when herdr there isn't ready.
    pub fn fix(machine: &Machine) -> Option<Self> {
        let id = machine.id().clone();
        match machine.status() {
            _ if id.is_local() => None,
            MachineStatus::NeedsSignIn(_) | MachineStatus::Offline(_) => Some(Self::SignIn(id)),
            MachineStatus::NeedsSetup(_) => Some(Self::SetUp(id)),
            MachineStatus::Connecting | MachineStatus::Connected | MachineStatus::Disabled(_) => {
                None
            }
        }
    }
}

/// Where a run is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStage {
    /// The command is running in the terminal.
    Running,
    /// It exited; the machine is being checked.
    Checking,
    /// The machine is connected.
    Done,
    /// It didn't work; the terminal says why.
    Failed,
}

#[derive(Debug, Clone)]
pub struct MachineRun {
    /// Counts runs, so each gets a fresh terminal.
    pub id: u64,
    pub task: MachineTask,
    pub stage: RunStage,
    /// The machine the run is about: the task's, or the one an add saved.
    pub machine: Option<MachineId>,
    /// The shell command line the terminal runs.
    pub command: String,
    /// The command as a person would type it.
    pub display: String,
    /// Add only: the profiles saved before it ran, to tell the new one.
    pub(super) known: Vec<String>,
}

impl MachineRun {
    pub(super) fn new(
        id: u64,
        task: MachineTask,
        bin: &Path,
        profile: Option<&MachineProfile>,
        known: Vec<String>,
    ) -> Option<Self> {
        let argv = argv(&task, bin, profile)?;
        let command = FixCommand::from_argv(argv);
        let machine = match &task {
            MachineTask::SignIn(id) | MachineTask::SetUp(id) => Some(id.clone()),
            MachineTask::Add { .. } => None,
        };
        Some(Self {
            id,
            task,
            stage: RunStage::Running,
            machine,
            command: command.terminal_command(),
            display: command.display().to_owned(),
            known,
        })
    }
}

/// The herdr command for `task`; `None` when a sign-in or setup has no
/// saved profile to act on.
fn argv(task: &MachineTask, bin: &Path, profile: Option<&MachineProfile>) -> Option<Vec<OsString>> {
    let mut argv: Vec<OsString> = vec![bin.into()];
    match task {
        MachineTask::SignIn(_) => {
            argv.extend([
                "machine".into(),
                "reconnect".into(),
                profile?.label.clone().into(),
            ]);
        }
        MachineTask::SetUp(_) => {
            let profile = profile?;
            argv.extend(["--remote".into(), profile.target.clone().into()]);
            if profile.session != "default" {
                argv.extend(["--session".into(), profile.session.clone().into()]);
            }
        }
        MachineTask::Add { host } => {
            argv.extend(["machine".into(), "add".into(), host.into()]);
        }
    }
    Some(argv)
}

/// The profile in `saved` that isn't in `known`: what an add saved.
pub fn added<'a>(
    known: &[String],
    saved: impl Iterator<Item = &'a MachineProfile>,
) -> Option<&'a MachineProfile> {
    saved.into_iter().find(|p| !known.contains(&p.id))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{MachineRun, MachineTask, added};
    use crate::herdr::MachineProfile;
    use crate::state::MachineId;

    fn profile(id: &str, session: &str) -> MachineProfile {
        MachineProfile {
            id: id.into(),
            label: format!("box-{id}"),
            target: "ssh://me@box:2222".into(),
            session: session.into(),
            enabled: true,
        }
    }

    fn display(task: MachineTask, profile: Option<&MachineProfile>) -> Option<String> {
        MachineRun::new(1, task, Path::new("herdr"), profile, Vec::new()).map(|run| run.display)
    }

    #[test]
    fn sign_in_reconnects_by_label() {
        let p = profile("a", "default");
        assert_eq!(
            display(MachineTask::SignIn(MachineId::Remote("a".into())), Some(&p)).as_deref(),
            Some("herdr machine reconnect box-a")
        );
    }

    #[test]
    fn set_up_passes_a_named_session() {
        let p = profile("a", "work");
        assert_eq!(
            display(MachineTask::SetUp(MachineId::Remote("a".into())), Some(&p)).as_deref(),
            Some("herdr --remote ssh://me@box:2222 --session work")
        );
    }

    #[test]
    fn set_up_leaves_out_the_default_session() {
        let p = profile("a", "default");
        assert_eq!(
            display(MachineTask::SetUp(MachineId::Remote("a".into())), Some(&p)).as_deref(),
            Some("herdr --remote ssh://me@box:2222")
        );
    }

    #[test]
    fn sign_in_without_a_profile_has_no_command() {
        assert_eq!(display(MachineTask::SignIn(MachineId::Local), None), None);
    }

    #[test]
    fn add_runs_machine_add() {
        let task = MachineTask::Add {
            host: "dockerbox2".into(),
        };
        assert_eq!(
            display(task, None).as_deref(),
            Some("herdr machine add dockerbox2")
        );
    }

    #[test]
    fn added_finds_the_new_profile() {
        let saved = [profile("a", "default"), profile("b", "default")];
        let new = added(&["a".to_owned()], saved.iter()).map(|p| p.id.as_str());
        assert_eq!(new, Some("b"));
    }
}
