//! `AppState`: the machine list every view starts from.
//!
//! It holds one [`Session`] per herdr server (Local, then herdr's saved SSH
//! machines; see docs/decisions/0019-remote-machines.md), which one is
//! selected, the machine sign-in/add run and Local's [`Setup`]. Each session
//! owns its connection, cache, focused pane, worktree and status, and runs
//! that machine's herdr commands.
//!
//! Views keep an `Entity<AppState>` and read it in `render`. A view that
//! shows one machine subscribes to that machine's session (`SessionEvent`),
//! so another machine's traffic doesn't wake it, and to `AppEvent` for the
//! selection. Views that list every machine (the sidebar) `cx.observe` this:
//! it relays each session's changes as machine-tagged `AppEvent`s and a
//! `notify` (docs/decisions/0002-views-get-state.md).
//!
//! The herdr connection runs on `herdr-client`'s own thread (`SessionSync`);
//! its updates are applied to the session on the foreground. Commands run
//! the blocking socket call on the background executor and return a task:
//! `.detach_and_log_err(cx)` it, or await it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;

use crate::attention::{self, StatusChange};
use crate::fs::Fs;
use crate::herdr::{
    self, Agent, Client, MachineLink, MachineProfile, PaneId, Snapshot, TabId, WorkspaceId,
    machines,
};
use crate::setup::{Setup, SetupEvent};
use crate::sounds;
use cues::{Cue, PlayOptions};

mod machine_list;
mod machine_run;
mod session;
mod status;

pub use machine_list::{MachineId, On};
pub use machine_run::{MachineRun, MachineTask, RunStage};
pub use session::{Session, SessionEvent};
pub use status::Status;

/// herdr 0.9.5 sends no event when a pane's folder changes, so the focused
/// pane's folder is re-read this often (one `pane.get`).
const CWD_POLL: Duration = Duration::from_secs(2);
/// herdr sends nothing when its saved machines change, so the list is re-read
/// this often (and on demand, after the app changes it).
const MACHINES_POLL: Duration = Duration::from_secs(30);
/// How long a machine has to connect after a sign-in, setup or add exits.
const RUN_CHECK: Duration = Duration::from_secs(30);

/// What changed in the machine list or the selection, plus each session's
/// changes relayed with their machine, for the views that list every
/// machine. A view that shows the selected machine subscribes to its
/// session instead (`SessionEvent`), whose events come before the relays.
///
/// A selection sends `MachineSelected`, then `ActiveRootChanged`. Views on
/// the selected session follow the new one on `MachineSelected` and resync
/// as for its `SnapshotReplaced` and `FocusedPaneChanged`, so the order they
/// see is still: selection, cache, focused pane, root.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The active worktree, as a machine and a path, changed: the focused
    /// pane moved to another checkout, or another machine was selected (sent
    /// on every selection, even when the root is the same). The file tree,
    /// search, Changes, the editor's tab set and the sidebar's branch reads
    /// follow it, on any machine (see `AppState::fs_for`).
    ActiveRootChanged(Option<ActiveRoot>),
    /// A known agent's status changed between two caches (chrome toasts it).
    AgentStatusChanged {
        machine: MachineId,
        change: StatusChange,
    },
    /// The machine list, or a machine's label or status, changed (not its
    /// cache: see `MachineSnapshot`).
    MachinesChanged,
    /// A machine's herdr cache was replaced, whichever machine is selected.
    MachineSnapshot(MachineId),
    /// A machine's link (re)connected. Terminals whose attach died with the
    /// old connection attach again.
    MachineConnected(MachineId),
    /// A machine's link was replaced (its target, session or enabled flag
    /// changed): its terminals' attaches died with the old link.
    MachineRestarted(MachineId),
    /// Another machine was selected, or the selected machine's session was
    /// replaced (a restarted link): `AppState::selected_session` is new.
    /// Always followed by `ActiveRootChanged`.
    MachineSelected(MachineId),
    /// The file tree renamed or moved a file or folder; open editor tabs at or
    /// under `from` follow it to `to`.
    FileMoved {
        machine: MachineId,
        from: PathBuf,
        to: PathBuf,
    },
    /// The file tree removed a file or folder: to the Trash on Local, for
    /// good on a saved machine.
    FileRemoved { machine: MachineId, path: PathBuf },
    /// A machine's sign-in, setup or add started, moved on a stage or
    /// closed (`AppState::machine_run`).
    MachineRunChanged,
}

/// Where the active workspace's files are: a folder on a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRoot {
    pub machine: MachineId,
    pub path: PathBuf,
}

/// A session in the list, and the subscription relaying its events.
struct Slot {
    id: MachineId,
    session: Entity<Session>,
    _events: Subscription,
}

pub struct AppState {
    /// Local's herdr. `None` when the endpoint was refused.
    pub herdr_config: Option<herdr::Config>,
    /// What Local's herdr needs before the app can use it, and the fix
    /// running; Local's sync starts when it says herdr is ready.
    setup: Entity<Setup>,
    /// Local first, then herdr's saved machines in herdr's order.
    machines: Vec<Slot>,
    /// The machine whose workspace is active, as in herdr.
    selected: MachineId,
    /// The root last sent as `ActiveRootChanged`: a session's worktree
    /// change is relayed only when it moves the active root.
    announced_root: Option<ActiveRoot>,
    /// A sign-in, setup or add running in a terminal on Local, and its outcome.
    machine_run: Option<MachineRun>,
    runs: u64,
    /// Machines this process's add runs saved (see `edit_allowed`).
    added_here: Vec<MachineId>,
    _run_check: Option<Task<()>>,
    _cwd_poll: Option<Task<()>>,
    _machines_poll: Option<Task<()>>,
    _setup: Subscription,
    _quit: Option<Subscription>,
    /// In-memory disks standing in for remote machines, listed or not (see
    /// `set_test_fs`).
    #[cfg(test)]
    test_fs: std::collections::HashMap<MachineId, Arc<crate::fs::fake::FakeFs>>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    /// Local's machine, offline until [`Self::start`]. A refused endpoint
    /// (see `herdr::Config::from_env`) leaves the app disconnected for good:
    /// no sync, no client, every call fails. Building one has no side
    /// effects, so tests use it as is: GPUI's test scheduler rejects wakeups
    /// from the sync thread, and tests must never reach a real herdr.
    pub fn new(herdr_config: herdr::Result<herdr::Config>, cx: &mut Context<Self>) -> Self {
        let herdr_config = match herdr_config {
            Ok(config) => config,
            Err(error) => {
                tracing::error!("herdr: {error}");
                return Self::disconnected(None, error.to_string(), cx);
            }
        };
        // `from_env` configs auto-start a server on the first call; tests
        // must use an inert one.
        #[cfg(test)]
        assert!(
            !herdr_config.auto_start,
            "tests must not build AppState from the environment; use an auto_start: false config"
        );
        let mut this = Self::disconnected(Some(herdr_config.clone()), String::new(), cx);
        let config = herdr_config.clone();
        this.local()
            .clone()
            .update(cx, |local, cx| local.connect_local(config, cx));
        this.herdr_config = Some(herdr_config);
        // GPUI's quit ends the process without dropping entities, so the
        // links (and their ssh masters) are stopped here.
        this._quit = Some(cx.on_app_quit(|this, cx| {
            stop_links(this.take_links(cx));
            async {}
        }));
        this
    }

    /// Checks herdr, then connects (starting its server if needed); until
    /// the check passes, [`Setup`] says what herdr needs. herdr's saved
    /// machines load in the background and are re-read every
    /// [`MACHINES_POLL`].
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.setup.update(cx, Setup::check_herdr);
        if self.herdr_config.is_some() {
            self._machines_poll = Some(Self::poll_machines(cx));
        }
    }

    fn disconnected(config: Option<herdr::Config>, reason: String, cx: &mut Context<Self>) -> Self {
        let setup = cx.new(|_| Setup::new(config));
        let setup_events = cx.subscribe(&setup, |this, _, event, cx| this.on_setup(*event, cx));
        let local = cx.new(|cx| Session::local(setup.clone(), reason, cx));
        let local = Self::watch(local, cx);
        Self {
            herdr_config: None,
            setup,
            machines: vec![local],
            selected: MachineId::Local,
            announced_root: None,
            machine_run: None,
            runs: 0,
            added_here: Vec::new(),
            _run_check: None,
            _cwd_poll: None,
            _machines_poll: None,
            _setup: setup_events,
            _quit: None,
            #[cfg(test)]
            test_fs: Default::default(),
        }
    }

    /// Lists `session` and relays its events.
    fn watch(session: Entity<Session>, cx: &mut Context<Self>) -> Slot {
        let id = session.read(cx).id().clone();
        let events = cx.subscribe(&session, |this, session, event, cx| {
            this.on_session(&session, event, cx);
        });
        Slot {
            id,
            session,
            _events: events,
        }
    }

    /// Relays a session's change for the views that list every machine, and
    /// the selected one's as the active root.
    fn on_session(
        &mut self,
        session: &Entity<Session>,
        event: &SessionEvent,
        cx: &mut Context<Self>,
    ) {
        let id = session.read(cx).id().clone();
        // A retired session's last events.
        if self.session(&id) != Some(session) {
            return;
        }
        let selected = id == self.selected;
        match event {
            SessionEvent::SnapshotReplaced => cx.emit(AppEvent::MachineSnapshot(id)),
            SessionEvent::AgentStatusChanged(change) => cx.emit(AppEvent::AgentStatusChanged {
                machine: id,
                change: change.clone(),
            }),
            SessionEvent::FocusedPaneChanged(_) => {}
            SessionEvent::WorktreeChanged => {
                let root = self.active_root(cx);
                if selected && root != self.announced_root {
                    self.announced_root.clone_from(&root);
                    cx.emit(AppEvent::ActiveRootChanged(root));
                }
            }
            SessionEvent::StatusChanged => cx.emit(AppEvent::MachinesChanged),
            SessionEvent::Connected => cx.emit(AppEvent::MachineConnected(id)),
        }
        cx.notify();
    }

    /// Local's herdr setup (its check, screen and fixes).
    pub fn setup(&self) -> &Entity<Setup> {
        &self.setup
    }

    fn on_setup(&mut self, event: SetupEvent, cx: &mut Context<Self>) {
        match event {
            SetupEvent::HerdrReady => self.start_sync(cx),
            SetupEvent::StageChanged => {
                let local = self.local().clone();
                local.update(cx, Session::status_changed);
            }
        }
    }

    // Machines

    /// Every session: Local first, then each saved machine in herdr's order.
    pub fn sessions(&self) -> impl Iterator<Item = &Entity<Session>> {
        self.machines.iter().map(|slot| &slot.session)
    }

    /// `id`'s session; `None` once it's gone from the list.
    pub fn session(&self, id: &MachineId) -> Option<&Entity<Session>> {
        self.machines
            .iter()
            .find(|slot| &slot.id == id)
            .map(|slot| &slot.session)
    }

    /// The selected machine's session, which the terminal area shows.
    pub fn selected_session(&self) -> &Entity<Session> {
        self.session(&self.selected).unwrap_or_else(|| self.local())
    }

    /// Local first, then each saved machine in herdr's order.
    pub fn machines<'a>(&'a self, cx: &'a App) -> impl Iterator<Item = &'a Session> {
        self.sessions().map(|session| session.read(cx))
    }

    pub fn machine<'a>(&'a self, id: &MachineId, cx: &'a App) -> Option<&'a Session> {
        self.session(id).map(|session| session.read(cx))
    }

    fn local(&self) -> &Entity<Session> {
        &self.machines[0].session
    }

    pub fn selected(&self) -> &MachineId {
        &self.selected
    }

    pub fn selected_machine<'a>(&'a self, cx: &'a App) -> &'a Session {
        self.selected_session().read(cx)
    }

    /// The selected machine's cache.
    pub fn snapshot<'a>(&'a self, cx: &'a App) -> &'a Arc<Snapshot> {
        self.selected_machine(cx).snapshot()
    }

    /// The selected machine's focused pane.
    pub fn focused_pane<'a>(&'a self, cx: &'a App) -> Option<&'a str> {
        self.selected_machine(cx).focused_pane()
    }

    /// Local and each connected machine: the ones whose agents are listed
    /// (an offline machine's last cache is stale).
    pub fn live_machines<'a>(&'a self, cx: &'a App) -> impl Iterator<Item = &'a Session> {
        self.machines(cx)
            .filter(|m| m.id().is_local() || m.status().is_usable())
    }

    /// Blocked agents plus unseen finished ones (herdr's `done`) on the live
    /// machines: the dock badge count, matching the agent list.
    pub fn attention_count(&self, cx: &App) -> usize {
        self.live_machines(cx)
            .map(|m| attention::count(m.snapshot()))
            .sum()
    }

    /// Every agent on the live machines, with its machine.
    pub fn agents<'a>(&'a self, cx: &'a App) -> impl Iterator<Item = (&'a MachineId, &'a Agent)> {
        self.live_machines(cx)
            .flat_map(|m| m.snapshot().agents.iter().map(move |agent| (m.id(), agent)))
    }

    /// Makes `id` the machine the terminal area shows and its commands act on.
    pub fn select_machine(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        if &self.selected == id || self.session(id).is_none() {
            return;
        }
        tracing::info!("machine selected: {id}");
        self.selected = id.clone();
        self.announce_selection(cx);
    }

    /// The events for a new selection, in the order `AppEvent` documents.
    fn announce_selection(&mut self, cx: &mut Context<Self>) {
        cx.emit(AppEvent::MachineSelected(self.selected.clone()));
        self.announced_root = self.active_root(cx);
        cx.emit(AppEvent::ActiveRootChanged(self.announced_root.clone()));
        cx.notify();
    }

    /// Re-reads herdr's saved machines in the background, then starts,
    /// stops or relabels links to match.
    pub fn refresh_machines(&mut self, cx: &mut Context<Self>) {
        let Some(bin) = self.herdr_config.as_ref().map(|c| c.bin.clone()) else {
            return;
        };
        let list = cx
            .background_executor()
            .spawn(async move { machines::list_machines(&bin) });
        cx.spawn(async move |this, cx| match list.await {
            Ok(profiles) => {
                this.update(cx, |this, cx| this.apply_profiles(profiles, cx))
                    .ok();
            }
            Err(error) => tracing::warn!("herdr machine list: {error}"),
        })
        .detach();
    }

    fn poll_machines(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, Self::refresh_machines).is_err() {
                    break;
                }
                cx.background_executor().timer(MACHINES_POLL).await;
            }
        })
    }

    fn apply_profiles(&mut self, saved: Vec<MachineProfile>, cx: &mut Context<Self>) {
        let running: Vec<MachineProfile> = self
            .machines(cx)
            .filter_map(Session::profile)
            .cloned()
            .collect();
        let diff = machine_list::diff_profiles(&running.iter().collect::<Vec<_>>(), &saved);
        let changed = !diff.start.is_empty() || !diff.stop.is_empty() || !diff.relabel.is_empty();
        if !changed {
            return;
        }
        tracing::info!(
            "machines: start {:?}, stop {:?}, relabel {:?}",
            diff.start.iter().map(|p| &p.label).collect::<Vec<_>>(),
            diff.stop,
            diff.relabel
        );
        let restarted = |id: &MachineId| match id {
            MachineId::Local => false,
            MachineId::Remote(id) => diff.start.iter().any(|p| &p.id == id),
        };
        let selected_restarted = restarted(&self.selected);
        let replaced: Vec<MachineId> = self
            .machines
            .iter()
            .map(|slot| slot.id.clone())
            .filter(|id| restarted(id))
            .collect();
        let gone = |id: &MachineId| {
            restarted(id) || matches!(id, MachineId::Remote(id) if diff.stop.contains(id))
        };
        self.remove_sessions(gone, cx);
        for (id, label) in diff.relabel {
            if let Some(session) = self.session(&MachineId::Remote(id)).cloned() {
                session.update(cx, |session, cx| session.set_label(label, cx));
            }
        }
        for profile in diff.start {
            if let Some(slot) = self.start_machine(profile, cx) {
                self.machines.push(slot);
            }
        }
        // Local first, then herdr's order.
        self.machines.sort_by_key(|slot| match &slot.id {
            MachineId::Local => 0,
            MachineId::Remote(id) => 1 + saved.iter().position(|p| &p.id == id).unwrap_or(0),
        });
        for id in replaced {
            cx.emit(AppEvent::MachineRestarted(id));
        }
        let selected = machine_list::reselect(&self.selected, self.machines.iter().map(|s| &s.id));
        if selected != self.selected || selected_restarted {
            // A restarted machine has a new session, with an empty cache.
            self.selected = selected;
            self.announce_selection(cx);
        }
        cx.emit(AppEvent::MachinesChanged);
        cx.notify();
    }

    /// Takes the sessions `gone` picks out of the list, retired: a view
    /// still holding one gets a failed task for every command.
    fn remove_sessions(&mut self, gone: impl Fn(&MachineId) -> bool, cx: &mut Context<Self>) {
        let links: Vec<MachineLink> = self
            .machines
            .iter()
            .filter(|slot| gone(&slot.id))
            .filter_map(|slot| slot.session.update(cx, |session, _| session.retire()))
            .collect();
        // Stopping a link waits on ssh, so it happens off the main thread.
        cx.background_executor()
            .spawn(async move { stop_links(links) })
            .detach();
        self.machines.retain(|slot| !gone(&slot.id));
    }

    /// Takes every session's link, leaving them linkless.
    fn take_links(&mut self, cx: &mut App) -> Vec<MachineLink> {
        self.machines
            .iter()
            .filter_map(|slot| slot.session.update(cx, |session, _| session.take_link()))
            .collect()
    }

    fn start_machine(&self, profile: MachineProfile, cx: &mut Context<Self>) -> Option<Slot> {
        let bin = self.herdr_config.as_ref()?.bin.clone();
        let Some(dir) = machines::socket_dir() else {
            tracing::warn!(
                "machine {}: no socket directory (HOME unset)",
                profile.label
            );
            return None;
        };
        let session = cx.new(|cx| Session::remote(profile, dir, bin, cx));
        Some(Self::watch(session, cx))
    }

    /// Runs `herdr machine <command> <profile id> <args>` for a saved
    /// machine, then re-reads the list.
    fn machine_command(
        &mut self,
        id: &MachineId,
        command: &str,
        args: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let (MachineId::Remote(profile_id), Some(bin)) =
            (id, self.herdr_config.as_ref().map(|c| c.bin.clone()))
        else {
            return Task::ready(Err(anyhow::anyhow!("{id} isn't a saved machine")));
        };
        if !self.edit_allowed(id, cx) {
            tracing::warn!("machine {command} {id}: refused in a debug build");
            return Task::ready(Err(anyhow::anyhow!(
                "debug builds only change machines listed in GRASSLANDS_DEV_MACHINES"
            )));
        }
        let mut argv = vec![
            "machine".to_owned(),
            command.to_owned(),
            profile_id.to_string(),
        ];
        argv.extend(args);
        let run = cx.background_executor().spawn(async move {
            let output = std::process::Command::new(&bin)
                .args(&argv)
                .stdin(std::process::Stdio::null())
                .output()?;
            if output.status.success() {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "herdr {}: {}",
                    argv.join(" "),
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            if let Err(error) = &result {
                tracing::warn!("{error:#}");
            }
            this.update(cx, Self::refresh_machines).ok();
            result
        })
    }

    /// Debug builds share herdr's saved machines with the user's real
    /// sessions, so they only change a machine listed in
    /// `GRASSLANDS_DEV_MACHINES`, or one this process added with
    /// `RT_ALLOW_MACHINE_EDITS=1`.
    fn edit_allowed(&self, id: &MachineId, cx: &App) -> bool {
        !cfg!(debug_assertions)
            || self
                .machine(id, cx)
                .is_some_and(|m| machines::dev_allowed(m.label()))
            || (edits_allowed_by_env() && self.added_here.contains(id))
    }

    pub fn rename_machine(
        &mut self,
        id: &MachineId,
        label: &str,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        // `=` keeps a label starting with `-` from reading as a flag.
        self.machine_command(id, "rename", vec![format!("--label={label}")], cx)
    }

    pub fn disable_machine(
        &mut self,
        id: &MachineId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        self.machine_command(id, "disable", Vec::new(), cx)
    }

    pub fn enable_machine(
        &mut self,
        id: &MachineId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        self.machine_command(id, "enable", Vec::new(), cx)
    }

    /// Removes the saved machine from herdr; confirm first.
    pub fn remove_machine(
        &mut self,
        id: &MachineId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        sounds::play(Cue::Close, sounds::strong());
        self.machine_command(id, "remove", Vec::new(), cx)
    }

    /// Retries a machine's connection now. For Local, checks herdr again
    /// when it isn't ready.
    pub fn reconnect_machine(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        if let Some(session) = self.session(id).cloned() {
            session.update(cx, Session::reconnect);
        }
    }

    /// The sign-in, setup or add on screen, if any.
    pub fn machine_run(&self) -> Option<&MachineRun> {
        self.machine_run.as_ref()
    }

    /// Runs herdr's own command for `task` in a terminal on Local (the
    /// terminal area shows it), replacing any earlier run. A sign-in or
    /// setup selects its machine.
    pub fn start_machine_run(&mut self, task: MachineTask, cx: &mut Context<Self>) {
        let Some(bin) = self.herdr_config.as_ref().map(|c| c.bin.clone()) else {
            return;
        };
        if let MachineTask::Add { host } = &task
            && (host.is_empty() || host.starts_with('-') || host.contains(char::is_whitespace))
        {
            tracing::warn!("add machine: {host:?} isn't an SSH host");
            return;
        }
        if matches!(task, MachineTask::Add { .. })
            && cfg!(debug_assertions)
            && !edits_allowed_by_env()
        {
            tracing::warn!("add machine: debug builds need RT_ALLOW_MACHINE_EDITS=1");
            return;
        }
        let profile = match &task {
            MachineTask::SignIn(id) | MachineTask::SetUp(id) => {
                self.machine(id, cx).and_then(Session::profile).cloned()
            }
            MachineTask::Add { .. } => None,
        };
        let known = self
            .machines(cx)
            .filter_map(Session::profile)
            .map(|p| p.id.clone())
            .collect();
        self.runs += 1;
        let Some(run) = MachineRun::new(self.runs, task, &bin, profile.as_ref(), known) else {
            tracing::warn!("machine run: no saved profile");
            return;
        };
        tracing::info!("machine run: {}", run.display);
        if let Some(machine) = run.machine.clone() {
            self.select_machine(&machine, cx);
        }
        sounds::play(Cue::Loading, PlayOptions::default());
        self.machine_run = Some(run);
        self._run_check = None;
        cx.emit(AppEvent::MachineRunChanged);
        cx.notify();
    }

    /// The run's terminal process ended: retry its machine (or re-read the
    /// list after an add), then give it [`RUN_CHECK`] to connect.
    pub fn machine_run_exited(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self
            .machine_run
            .as_mut()
            .filter(|r| r.stage == RunStage::Running)
        else {
            return;
        };
        run.stage = RunStage::Checking;
        let (task, run_id) = (run.task.clone(), run.id);
        match task {
            MachineTask::SignIn(id) | MachineTask::SetUp(id) => self.reconnect_machine(&id, cx),
            MachineTask::Add { .. } => self.refresh_machines(cx),
        }
        self._run_check = Some(cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let last = started.elapsed() >= RUN_CHECK;
                let settled = this.update(cx, |this, cx| this.check_machine_run(run_id, last, cx));
                if !matches!(settled, Ok(false)) {
                    break;
                }
            }
        }));
        cx.emit(AppEvent::MachineRunChanged);
        cx.notify();
    }

    /// Whether run `run_id` has settled: its machine connected (done), or
    /// `last` and it hasn't (failed).
    fn check_machine_run(&mut self, run_id: u64, last: bool, cx: &mut Context<Self>) -> bool {
        let added = self.machine_run.as_ref().and_then(|run| match run.task {
            MachineTask::Add { .. } if run.machine.is_none() => {
                machine_run::added(&run.known, self.machines(cx).filter_map(Session::profile))
                    .map(|p| MachineId::Remote(p.id.clone()))
            }
            _ => None,
        });
        let Some(run) = self
            .machine_run
            .as_mut()
            .filter(|r| r.id == run_id && r.stage == RunStage::Checking)
        else {
            return true;
        };
        if let Some(id) = &added {
            self.added_here.push(id.clone());
            run.machine = added;
            // The card names the machine now.
            cx.emit(AppEvent::MachineRunChanged);
        }
        let machine = run.machine.clone();
        let connected = machine
            .as_ref()
            .and_then(|id| self.machine(id, cx))
            .is_some_and(|m| m.status().is_usable());
        let stage = if connected {
            sounds::play(Cue::Success, sounds::strong());
            RunStage::Done
        } else if last {
            sounds::play(Cue::Error, PlayOptions::default());
            RunStage::Failed
        } else {
            return false;
        };
        if let Some(run) = self.machine_run.as_mut() {
            run.stage = stage;
        }
        cx.emit(AppEvent::MachineRunChanged);
        cx.notify();
        true
    }

    /// Closes the run's screen and terminal. A finished add selects the
    /// machine it added.
    pub fn finish_machine_run(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.machine_run.take() else {
            return;
        };
        self._run_check = None;
        if run.stage == RunStage::Done
            && matches!(run.task, MachineTask::Add { .. })
            && let Some(machine) = &run.machine
        {
            self.select_machine(machine, cx);
        }
        cx.emit(AppEvent::MachineRunChanged);
        cx.notify();
    }

    /// Runs the last run's task again.
    pub fn retry_machine_run(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.machine_run.as_ref().map(|r| r.task.clone()) {
            self.start_machine_run(task, cx);
        }
    }

    /// A remote machine with no link, for tests.
    #[cfg(test)]
    pub(crate) fn add_test_machine(
        &mut self,
        profile_id: &str,
        label: &str,
        snapshot: Arc<Snapshot>,
        cx: &mut Context<Self>,
    ) -> Entity<Session> {
        let id = MachineId::Remote(profile_id.into());
        let test_fs = self.test_fs.get(&id).cloned();
        let session = cx.new(|_| {
            let mut session = Session::test(id, label.into(), snapshot);
            session.test_fs = test_fs;
            session
        });
        let slot = Self::watch(session.clone(), cx);
        self.machines.push(slot);
        session
    }

    // Local

    /// Starts Local's sync thread, once.
    fn start_sync(&mut self, cx: &mut Context<Self>) {
        let local = self.local().clone();
        if local.update(cx, Session::start_local_sync) {
            self._cwd_poll = Some(Self::poll_focused_dir(cx));
        }
    }

    /// Follows Local's focused pane, and the selected machine's, into
    /// another folder (`cd`, or an agent changing directory), which herdr
    /// reports no event for.
    fn poll_focused_dir(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CWD_POLL).await;
                let Ok(polls) = this.update(cx, |this, cx| {
                    let mut sessions = vec![this.local().clone()];
                    if !this.selected.is_local() {
                        sessions.push(this.selected_session().clone());
                    }
                    sessions
                        .into_iter()
                        .map(|session| session.update(cx, Session::poll_focused_dir))
                        .collect::<Vec<_>>()
                }) else {
                    break;
                };
                for poll in polls {
                    poll.await;
                }
            }
        })
    }

    /// Local's active worktree while Local is selected; `None` while
    /// another machine is. For any machine, see `active_root`.
    pub fn active_worktree<'a>(&'a self, cx: &'a App) -> Option<&'a PathBuf> {
        self.local()
            .read(cx)
            .worktree()
            .filter(|_| self.selected.is_local())
    }

    /// The selected machine's active worktree.
    pub fn active_root(&self, cx: &App) -> Option<ActiveRoot> {
        let machine = self.selected_machine(cx);
        Some(ActiveRoot {
            machine: machine.id().clone(),
            path: machine.worktree()?.clone(),
        })
    }

    /// The files on `machine`: Local always, a saved machine while it's
    /// connected.
    pub fn fs_for(&self, machine: &MachineId, cx: &App) -> Option<Fs> {
        if machine.is_local() {
            return Some(Fs::Local);
        }
        #[cfg(test)]
        if let Some(fake) = self.test_fs.get(machine) {
            let online = !fake.offline.load(std::sync::atomic::Ordering::SeqCst);
            return online.then(|| Fs::Fake(fake.clone()));
        }
        self.machine(machine, cx)?.fs()
    }

    /// Makes `fs_for(machine)` an in-memory disk, offline while its
    /// `offline` flag is set.
    #[cfg(test)]
    pub(crate) fn set_test_fs(
        &mut self,
        machine: MachineId,
        fake: Arc<crate::fs::fake::FakeFs>,
        cx: &mut App,
    ) {
        if let Some(session) = self.session(&machine) {
            session.update(cx, |session, _| session.test_fs = Some(fake.clone()));
        }
        self.test_fs.insert(machine, fake);
    }

    /// Announces a rename done in the app (see `AppEvent::FileMoved`).
    pub fn file_moved(
        &mut self,
        machine: MachineId,
        from: PathBuf,
        to: PathBuf,
        cx: &mut Context<Self>,
    ) {
        cx.emit(AppEvent::FileMoved { machine, from, to });
    }

    /// Announces a removal done in the app (see `AppEvent::FileRemoved`).
    pub fn file_removed(&mut self, machine: MachineId, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(AppEvent::FileRemoved { machine, path });
    }

    /// Sets Local's active worktree.
    pub fn set_active_worktree(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        self.set_worktree(&MachineId::Local, root, cx);
    }

    /// Sets `id`'s active worktree; announced as the active root only while
    /// `id` is selected.
    pub(crate) fn set_worktree(
        &mut self,
        id: &MachineId,
        root: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.session(id).cloned() {
            session.update(cx, |session, cx| session.set_worktree(root, cx));
        }
    }

    /// Replaces the selected machine's cache, as a sync update would.
    pub fn replace_snapshot(&mut self, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        let session = self.selected_session().clone();
        session.update(cx, |session, cx| session.apply_snapshot(snapshot, cx));
    }

    /// Replaces `id`'s cache, as its sync would, for tests.
    #[cfg(test)]
    pub(crate) fn set_snapshot_on(
        &mut self,
        id: &MachineId,
        snapshot: Arc<Snapshot>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.session(id).cloned() {
            session.update(cx, |session, cx| session.apply_snapshot(snapshot, cx));
        }
    }

    // Commands
    //
    // Every command names its machine: "the selected machine" is resolved
    // once, by the key binding, action or palette entry that sends it. The
    // command itself runs on that machine's `Session`.

    /// `machine`'s session, or the failed task for a command on a machine
    /// that's gone from the list.
    #[track_caller]
    fn target<'a, T>(
        &'a self,
        machine: &MachineId,
        cx: &'a App,
    ) -> Result<&'a Session, Task<herdr::Result<T>>> {
        self.machine(machine, cx).ok_or_else(|| {
            let caller = std::panic::Location::caller();
            tracing::warn!("herdr call on {machine} from {caller} skipped: no such machine");
            Task::ready(Err(session::gone(machine)))
        })
    }

    /// Runs `command` on `machine`'s session; fails when it's gone.
    #[track_caller]
    pub fn on<T>(
        &self,
        machine: &MachineId,
        cx: &App,
        command: impl FnOnce(&Session, &App) -> Task<herdr::Result<T>>,
    ) -> Task<herdr::Result<T>> {
        match self.target(machine, cx) {
            Ok(session) => command(session, cx),
            Err(gone) => gone,
        }
    }

    /// Runs one blocking herdr call on `machine` (see `Session::call`).
    #[track_caller]
    pub fn call<T: Send + 'static>(
        &self,
        machine: &MachineId,
        cx: &App,
        call: impl FnOnce(&Client) -> herdr::Result<T> + Send + 'static,
    ) -> Task<herdr::Result<T>> {
        match self.target(machine, cx) {
            Ok(session) => session.call(cx, call),
            Err(gone) => gone,
        }
    }

    /// Selects `machine`, then focuses `pane_id` on it. Fails, selecting
    /// nothing, when `machine` is gone.
    #[track_caller]
    pub fn focus_pane(
        &mut self,
        machine: &MachineId,
        pane_id: &PaneId,
        cx: &mut Context<Self>,
    ) -> Task<herdr::Result<()>> {
        let Some(session) = self.session(machine).cloned() else {
            return Task::ready(Err(session::gone(machine)));
        };
        self.select_machine(machine, cx);
        session.read(cx).focus_pane(pane_id, cx)
    }

    /// Selects `machine`, then focuses `tab_id` on it. Fails, selecting
    /// nothing, when `machine` is gone.
    #[track_caller]
    pub fn focus_tab(
        &mut self,
        machine: &MachineId,
        tab_id: &TabId,
        cx: &mut Context<Self>,
    ) -> Task<herdr::Result<()>> {
        let Some(session) = self.session(machine).cloned() else {
            return Task::ready(Err(session::gone(machine)));
        };
        self.select_machine(machine, cx);
        session.read(cx).focus_tab(tab_id, cx)
    }

    /// Focuses a workspace on any machine, selecting that machine first
    /// (workspace ids are per machine). Fails, selecting nothing, when
    /// `machine` is gone.
    #[track_caller]
    pub fn focus_workspace(
        &mut self,
        machine: &MachineId,
        workspace_id: &WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<herdr::Result<()>> {
        let Some(session) = self.session(machine).cloned() else {
            return Task::ready(Err(session::gone(machine)));
        };
        // The session sounds for a workspace not yet focused; switching
        // machines is a selection too.
        if &self.selected != machine && session.read(cx).is_focused_workspace(workspace_id) {
            sounds::play(Cue::Select, PlayOptions::default());
        }
        self.select_machine(machine, cx);
        session.read(cx).focus_workspace(workspace_id, cx)
    }
}

/// `RT_ALLOW_MACHINE_EDITS=1`: a debug build may add machines (and change
/// the ones it added).
fn edits_allowed_by_env() -> bool {
    std::env::var_os("RT_ALLOW_MACHINE_EDITS").is_some_and(|v| v == "1")
}

/// Stops `links` at once, each on its own thread: each one waits on ssh.
fn stop_links(links: Vec<MachineLink>) {
    std::thread::scope(|scope| {
        for link in links {
            scope.spawn(move || drop(link));
        }
    });
}

/// A config that can never reach a herdr: no server start, a socket nobody
/// listens on. For tests that need an `AppState`.
#[cfg(test)]
pub fn offline_config() -> herdr::Config {
    herdr::Config {
        bin: "/nonexistent/herdr".into(),
        session: None,
        socket_path: std::env::temp_dir().join("grasslands-test-no-herdr.sock"),
        auto_start: false,
        server_log: None,
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{AppContext as _, Entity, Task, TestAppContext};

    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use super::Status;
    use super::{ActiveRoot, AppEvent, AppState, MachineId, Session, SessionEvent, offline_config};
    use crate::herdr::{self, Snapshot};

    fn agent(pane: &str) -> herdr::Agent {
        herdr::Agent {
            pane_id: pane.into(),
            terminal_id: "".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            focused: false,
            agent_status: herdr::AgentStatus::Working,
            revision: 0,
            name: None,
            agent: Some("claude".into()),
            display_agent: None,
            cwd: None,
            foreground_cwd: None,
            title: None,
            completion_seq: None,
            state_change_seq: 0,
            interactive_ready: false,
            launch_pending: false,
            state_labels: Default::default(),
        }
    }

    fn remote() -> MachineId {
        MachineId::Remote("b87d".into())
    }

    fn with_remote(cx: &mut TestAppContext) -> gpui_kit::Entity<AppState> {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, cx| {
            let snapshot = Snapshot {
                generation: 7,
                agents: vec![agent("w1:p1")],
                ..Snapshot::default()
            };
            state.add_test_machine("b87d", "dockerbox", Arc::new(snapshot), cx);
        });
        state
    }

    #[gpui::test]
    fn local_is_selected_at_launch(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let selected = cx.read(|cx| state.read(cx).selected().clone());
        assert_eq!(selected, MachineId::Local);
    }

    #[gpui::test]
    fn snapshot_follows_the_selected_machine(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| state.select_machine(&remote(), cx));
        let generation = cx.read(|cx| state.read(cx).snapshot(cx).generation);
        assert_eq!(generation, 7);
    }

    #[gpui::test]
    fn selecting_an_unknown_machine_keeps_the_selection(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| {
            state.select_machine(&MachineId::Remote("gone".into()), cx);
        });
        let selected = cx.read(|cx| state.read(cx).selected().clone());
        assert_eq!(selected, MachineId::Local);
    }

    #[gpui::test]
    fn a_remote_machine_has_no_active_worktree(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| state.select_machine(&remote(), cx));
        let root = cx.read(|cx| state.read(cx).active_worktree(cx).cloned());
        assert_eq!(root, None);
    }

    #[gpui::test]
    fn selecting_local_again_restores_its_worktree(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let before = cx.read(|cx| state.read(cx).active_worktree(cx).cloned());
        state.update(cx, |state, cx| {
            state.select_machine(&remote(), cx);
            state.select_machine(&MachineId::Local, cx);
        });
        let after = cx.read(|cx| state.read(cx).active_worktree(cx).cloned());
        assert_eq!(after, before);
    }

    #[gpui::test]
    fn the_active_root_names_the_selected_machine(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| {
            state.set_worktree(&remote(), Some("/home/lyle/api".into()), cx);
            state.select_machine(&remote(), cx);
        });
        let root = cx.read(|cx| state.read(cx).active_root(cx));
        assert_eq!(
            root,
            Some(ActiveRoot {
                machine: remote(),
                path: "/home/lyle/api".into()
            })
        );
    }

    #[gpui::test]
    fn selecting_a_machine_announces_its_root(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let roots = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let roots = roots.clone();
            cx.subscribe(&state, move |_, event, _| {
                if let AppEvent::ActiveRootChanged(root) = event {
                    roots
                        .borrow_mut()
                        .push(root.as_ref().map(|r| r.machine.clone()));
                }
            })
        });
        state.update(cx, |state, cx| {
            state.set_worktree(&remote(), Some("/home/lyle".into()), cx);
            state.select_machine(&remote(), cx);
        });
        assert_eq!(roots.borrow().as_slice(), &[Some(remote())]);
    }

    /// An event's variant name, without its fields.
    fn kind(event: &impl std::fmt::Debug) -> String {
        let debug = format!("{event:?}");
        let end = debug.find(['(', ' ']).unwrap_or(debug.len());
        debug[..end].to_owned()
    }

    /// The events `AppState` (as `app:`) and `session` (as `session:`) emit
    /// while `f` runs, by kind, in delivery order.
    fn emitted(
        state: &Entity<AppState>,
        session: Option<&Entity<Session>>,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut AppState, &mut gpui_kit::Context<AppState>),
    ) -> Vec<String> {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut subscriptions = vec![cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(state, move |_, event: &AppEvent, _| {
                events.borrow_mut().push(format!("app:{}", kind(event)));
            })
        })];
        if let Some(session) = session {
            subscriptions.push(cx.update(|cx| {
                let events = events.clone();
                cx.subscribe(session, move |_, event: &SessionEvent, _| {
                    events.borrow_mut().push(format!("session:{}", kind(event)));
                })
            }));
        }
        state.update(cx, f);
        drop(subscriptions);
        events.take()
    }

    fn remote_snapshot() -> Arc<Snapshot> {
        Arc::new(Snapshot {
            generation: 8,
            ..Snapshot::default()
        })
    }

    fn session_of(
        state: &Entity<AppState>,
        id: &MachineId,
        cx: &mut TestAppContext,
    ) -> Entity<Session> {
        cx.read(|cx| state.read(cx).session(id).cloned())
            .expect("listed")
    }

    #[gpui::test]
    fn a_background_machines_snapshot_is_relayed_only_as_its_own(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let events = emitted(&state, None, cx, |state, cx| {
            state.set_snapshot_on(&remote(), remote_snapshot(), cx);
        });
        assert_eq!(events, ["app:MachineSnapshot"]);
    }

    #[gpui::test]
    fn a_background_sessions_snapshot_doesnt_reach_the_selected_session(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let local = session_of(&state, &MachineId::Local, cx);
        let events = emitted(&state, Some(&local), cx, |state, cx| {
            state.set_snapshot_on(&remote(), remote_snapshot(), cx);
        });
        assert!(!events.iter().any(|e| e.starts_with("session:")));
    }

    #[gpui::test]
    fn a_selection_announces_the_machine_then_its_root(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let events = emitted(&state, None, cx, |state, cx| {
            state.select_machine(&remote(), cx);
        });
        assert_eq!(events, ["app:MachineSelected", "app:ActiveRootChanged"]);
    }

    #[gpui::test]
    fn a_sessions_events_come_in_order_before_their_relays(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| state.select_machine(&remote(), cx));
        let session = session_of(&state, &remote(), cx);
        state.update(cx, |state, cx| {
            let working = Snapshot {
                agents: vec![agent("w1:p1")],
                ..Snapshot::default()
            };
            state.set_snapshot_on(&remote(), Arc::new(working), cx);
        });
        let events = emitted(&state, Some(&session), cx, |state, cx| {
            let mut blocked = agent("w1:p1");
            blocked.agent_status = herdr::AgentStatus::Blocked;
            let snapshot = Snapshot {
                agents: vec![blocked],
                focused_pane_id: Some("w1:p1".into()),
                ..Snapshot::default()
            };
            state.set_snapshot_on(&remote(), Arc::new(snapshot), cx);
            state.set_worktree(&remote(), Some("/home/lyle/api".into()), cx);
        });
        assert_eq!(
            events,
            [
                "session:SnapshotReplaced",
                "session:AgentStatusChanged",
                "session:FocusedPaneChanged",
                "session:WorktreeChanged",
                "app:MachineSnapshot",
                "app:AgentStatusChanged",
                "app:ActiveRootChanged",
            ]
        );
    }

    #[gpui::test]
    fn a_background_sessions_worktree_isnt_the_active_root(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let events = emitted(&state, None, cx, |state, cx| {
            state.set_worktree(&remote(), Some("/home/lyle/api".into()), cx);
        });
        assert!(events.is_empty());
    }

    #[gpui::test]
    fn a_command_runs_on_the_machine_it_names(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let task = cx.update(|cx| {
            state.read(cx).on(&remote(), cx, |session, _| {
                Task::ready(Ok(session.id().clone()))
            })
        });
        let ran_on = cx.foreground_executor().block_test(task).ok();
        assert_eq!(ran_on, Some(remote()));
    }

    #[gpui::test]
    fn a_removed_sessions_commands_fail(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let session = session_of(&state, &remote(), cx);
        state.update(cx, |state, cx| {
            state.remove_sessions(|id| id == &remote(), cx)
        });
        let task = cx.update(|cx| session.read(cx).close_pane(&"w1:p1".into(), cx));
        let result = cx.foreground_executor().block_test(task);
        assert!(matches!(result, Err(herdr::Error::Refused(m)) if m.contains("no longer saved")));
    }

    #[gpui::test]
    fn local_files_are_always_available(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let local = cx.read(|cx| {
            state
                .read(cx)
                .fs_for(&MachineId::Local, cx)
                .map(|f| f.machine().is_local())
        });
        assert_eq!(local, Some(true));
    }

    #[gpui::test]
    fn a_machine_without_a_link_has_no_files(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let files = cx.read(|cx| state.read(cx).fs_for(&remote(), cx).is_some());
        assert!(!files);
    }

    #[gpui::test]
    fn agents_name_their_machine(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let machines: Vec<MachineId> = cx.read(|cx| {
            state
                .read(cx)
                .agents(cx)
                .map(|(machine, _)| machine.clone())
                .collect()
        });
        assert_eq!(machines, vec![remote()]);
    }

    fn refused() -> herdr::Error {
        herdr::Error::Refused("socket outside the dev root".into())
    }

    #[gpui::test]
    fn following_a_folder_inside_a_repo_roots_at_the_repo(cx: &mut TestAppContext) {
        let repo =
            std::env::temp_dir().join(format!("grasslands-state-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        crate::fs::test_repo::TestRepo::init(&repo, "main");
        let repo = repo.canonicalize().unwrap();
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, cx| {
            let local = state.local().clone();
            local.update(cx, |local, cx| local.follow_dir(Some(repo.join("src")), cx));
        });
        let root = cx.read(|cx| state.read(cx).active_worktree(cx).cloned());
        assert_eq!(root, Some(repo));
    }

    #[gpui::test]
    fn refused_endpoint_leaves_app_disconnected(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Err(refused()), cx));
        let status = cx.read(|cx| state.read(cx).selected_machine(cx).status().clone());
        assert!(matches!(
            status,
            Status::Offline {
                retrying: false,
                ..
            }
        ));
    }

    #[gpui::test]
    fn refused_endpoint_fails_every_call(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Err(refused()), cx));
        let task = state.update(cx, |state, cx| {
            state.focus_pane(&MachineId::Local, &"w1:p1".into(), cx)
        });
        let result = cx.foreground_executor().block_test(task);
        assert!(matches!(result, Err(herdr::Error::Refused(_))));
    }

    fn gone_machine() -> MachineId {
        MachineId::Remote("gone".into())
    }

    #[gpui::test]
    fn focusing_a_pane_on_a_gone_machine_fails(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let task = state.update(cx, |state, cx| {
            state.focus_pane(&gone_machine(), &"w1:p1".into(), cx)
        });
        let result = cx.foreground_executor().block_test(task);
        assert!(matches!(result, Err(herdr::Error::Refused(m)) if m.contains("no longer saved")));
    }

    #[gpui::test]
    fn focusing_a_workspace_on_a_gone_machine_keeps_the_selection(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| {
            state.select_machine(&remote(), cx);
            state
                .focus_workspace(&gone_machine(), &"w1".into(), cx)
                .detach();
        });
        let selected = cx.read(|cx| state.read(cx).selected().clone());
        assert_eq!(selected, remote());
    }

    #[gpui::test]
    fn a_call_on_a_gone_machine_fails_instead_of_using_the_selected_one(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let task = cx.update(|cx| {
            state.read(cx).on(&gone_machine(), cx, |session, cx| {
                session.close_pane(&"w1:p1".into(), cx)
            })
        });
        let result = cx.foreground_executor().block_test(task);
        assert!(matches!(result, Err(herdr::Error::Refused(m)) if m.contains("no longer saved")));
    }

    #[gpui::test]
    fn an_offline_machine_adds_nothing_to_the_badge(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        let count = state.update(cx, |state, cx| {
            let mut blocked = agent("w1:p1");
            blocked.agent_status = herdr::AgentStatus::Blocked;
            let snapshot = Arc::new(Snapshot {
                agents: vec![blocked],
                ..Snapshot::default()
            });
            let session = state.add_test_machine("b87d", "dockerbox", snapshot, cx);
            session.update(cx, |session, cx| {
                session.set_link_status(herdr::MachineStatus::Offline("gone".into()), cx);
            });
            state.attention_count(cx)
        });
        assert_eq!(count, 0);
    }

    #[gpui::test]
    fn a_debug_build_wont_rename_a_machine_outside_the_dev_list(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, cx| {
            state.add_test_machine("c0ffee", "users-real-box", Arc::default(), cx);
        });
        let task = state.update(cx, |state, cx| {
            state.rename_machine(&MachineId::Remote("c0ffee".into()), "x", cx)
        });
        let result = cx.foreground_executor().block_test(task);
        assert!(result.is_err_and(|e| e.to_string().contains("GRASSLANDS_DEV_MACHINES")));
    }

    #[gpui::test]
    fn a_debug_build_wont_add_a_machine_without_the_env_switch(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, cx| {
            let host = "dockerbox2".into();
            state.start_machine_run(super::MachineTask::Add { host }, cx);
        });
        let started = cx.read(|cx| state.read(cx).machine_run().is_some());
        assert!(!started);
    }

    #[gpui::test]
    fn local_needs_setup_while_its_check_fails(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        let setup = cx.read(|cx| state.read(cx).setup().clone());
        setup.update(cx, |setup, cx| setup.set_needed(herdr::Check::Missing, cx));
        cx.run_until_parked();
        let status = cx.read(|cx| state.read(cx).selected_machine(cx).status().clone());
        assert!(matches!(status, Status::NeedsSetup(_)));
    }

    #[gpui::test]
    #[should_panic(expected = "tests must not build AppState from the environment")]
    fn tests_cannot_use_an_auto_starting_config(cx: &mut TestAppContext) {
        let config = herdr::Config {
            auto_start: true,
            ..offline_config()
        };
        cx.new(|cx| AppState::new(Ok(config), cx));
    }
}
