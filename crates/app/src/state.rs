//! `AppState`: the one shared model every view reads.
//!
//! It holds one herdr connection per machine (Local, then herdr's saved SSH
//! machines; see docs/decisions/0019-remote-machines.md), which one is
//! selected, and each machine's active worktree. `snapshot`, `call` and the command
//! methods act on the selected machine. Views keep an
//! `Entity<AppState>`, read it in `render`, and `cx.observe` it to re-render.
//! Discrete changes that views must react to (not just redraw) are emitted
//! as `AppEvent`s; views `cx.subscribe` to those.
//!
//! The herdr connection runs on `herdr-client`'s own thread (`SessionSync`);
//! its updates are applied here on the foreground. Commands go through the
//! methods below, which run the blocking socket call on the background
//! executor and return a task: `.detach_and_log_err(cx)` it, or await it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;

use crate::attention::{self, Attention, StatusChange};
use crate::fs::Fs;
use crate::herdr::{
    self, Agent, Check, Client, ConnectionStatus, Fix, MIN_VERSION, MachineLink, MachineProfile,
    MachineStatus, PaneId, SessionSync, Snapshot, Update, Version, machines,
};
use crate::setup::{FixRun, Setup, Stage};
use crate::{sounds, worktree};
use cues::{Cue, PlayOptions};

mod machine_list;
mod machine_run;

pub use machine_list::MachineId;
pub use machine_run::{MachineRun, MachineTask, RunStage};

/// herdr 0.9.5 sends no event when a pane's folder changes, so the focused
/// pane's folder is re-read this often (one `pane.get`).
const CWD_POLL: Duration = Duration::from_secs(2);
/// herdr sends nothing when its saved machines change, so the list is re-read
/// this often (and on demand, after the app changes it).
const MACHINES_POLL: Duration = Duration::from_secs(30);
/// How long a machine has to connect after a sign-in, setup or add exits.
const RUN_CHECK: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The focused pane's worktree root changed. The file tree re-roots and
    /// the editor swaps to that worktree's tab set. Always `None` while a
    /// remote machine is selected.
    ActiveWorktreeChanged(Option<PathBuf>),
    /// The active worktree, as a machine and a path, changed: the focused
    /// pane moved, or another machine was selected. The file tree, search
    /// and Changes follow this one, on any machine (see `AppState::fs_for`).
    ActiveRootChanged(Option<ActiveRoot>),
    /// The selected machine's herdr cache was replaced (any change: startup,
    /// events, reconnect, or another machine being selected). The new one is
    /// `AppState::snapshot`.
    SnapshotReplaced,
    /// The selected machine's focused pane changed.
    FocusedPaneChanged(Option<PaneId>),
    /// A herdr event from the selected machine as it arrived, before the
    /// cache re-read it triggers. For reactions the cache can't express
    /// (toasts, "pane just created").
    Herdr(Arc<herdr::Event>),
    /// A known agent's status changed between two caches (chrome toasts it).
    AgentStatusChanged {
        machine: MachineId,
        change: StatusChange,
    },
    /// The machine list, a machine's label or status, or a machine's cache
    /// changed. Views listing every machine (the sidebar) redraw from
    /// `AppState::machines`.
    MachinesChanged,
    /// A machine's link (re)connected. Terminals whose attach died with the
    /// old connection attach again.
    MachineConnected(MachineId),
    /// A machine's link was replaced (its target, session or enabled flag
    /// changed): its terminals' attaches died with the old link.
    MachineRestarted(MachineId),
    /// Another machine was selected; `SnapshotReplaced` follows.
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
}

/// Where the active workspace's files are: a folder on a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRoot {
    pub machine: MachineId,
    pub path: PathBuf,
}

/// One herdr server: Local, or a saved SSH machine.
pub struct Machine {
    id: MachineId,
    label: String,
    status: MachineStatus,
    connection: ConnectionStatus,
    /// The latest herdr cache: workspaces → tabs → split trees, panes, agents.
    snapshot: Arc<Snapshot>,
    focused_pane: Option<PaneId>,
    /// The worktree its focused pane works in (see `AppState::follow_dir`).
    worktree: Option<PathBuf>,
    /// The focused pane's folder `worktree` was last resolved from.
    focused_dir: Option<PathBuf>,
    /// Status changes, for toasts and sounds.
    attention: Attention,
    /// Was connected, and hasn't been since: the next connection is a
    /// reconnect.
    lost: bool,
    /// `None` when Local's endpoint was refused, or before a remote machine
    /// first connects.
    client: Option<Client>,
    /// Remote machines only.
    link: Option<MachineLink>,
    sync: Option<(SessionSync, Task<()>)>,
    _statuses: Option<Task<()>>,
}

impl Machine {
    fn new(id: MachineId, label: String, status: MachineStatus) -> Self {
        Self {
            id,
            label,
            status,
            connection: ConnectionStatus::Connecting,
            snapshot: Arc::default(),
            focused_pane: None,
            worktree: None,
            focused_dir: None,
            attention: Attention::default(),
            lost: false,
            client: None,
            link: None,
            sync: None,
            _statuses: None,
        }
    }

    pub fn id(&self) -> &MachineId {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn status(&self) -> &MachineStatus {
        &self.status
    }

    pub fn connection(&self) -> &ConnectionStatus {
        &self.connection
    }

    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    pub fn focused_pane(&self) -> Option<&str> {
        self.focused_pane.as_deref()
    }

    /// The machine's files over its link; `None` for Local and while the
    /// link isn't connected.
    pub fn remote_fs(&self) -> Option<Arc<herdr_client::RemoteFs>> {
        self.link.as_ref()?.remote_fs()
    }

    /// herdr's saved profile; `None` for Local.
    pub fn profile(&self) -> Option<&MachineProfile> {
        self.link.as_ref().map(MachineLink::profile)
    }

    /// The shell command that attaches a terminal surface to `terminal_id`
    /// on this machine. `None` until a remote machine's herdr is found.
    pub fn attach_command(&self, terminal_id: &str) -> Option<String> {
        self.link.as_ref()?.attach_command(terminal_id)
    }
}

pub struct AppState {
    /// Local's herdr. `None` when the endpoint was refused.
    pub herdr_config: Option<herdr::Config>,
    /// What Local's herdr needs before the app can use it, and the fix running.
    pub setup: Setup,
    /// Local first, then herdr's saved machines in herdr's order.
    machines: Vec<Machine>,
    /// The machine whose workspace is active, as in herdr.
    selected: MachineId,
    /// A sign-in, setup or add running in a terminal on Local, and its outcome.
    machine_run: Option<MachineRun>,
    runs: u64,
    /// The old Local server version last sent to the setup check, so it's
    /// checked once rather than on every snapshot.
    old_server_checked: Option<String>,
    /// Machines this process's add runs saved (see `edit_allowed`).
    added_here: Vec<MachineId>,
    _run_check: Option<Task<()>>,
    _cwd_poll: Option<Task<()>>,
    _check: Option<Task<()>>,
    _machines_poll: Option<Task<()>>,
    _quit: Option<Subscription>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    /// Checks herdr, then connects (starting its server if needed); until the
    /// check passes, `setup` says what herdr needs. A refused endpoint (see
    /// `herdr::Config::from_env`) leaves the app disconnected: no sync, no
    /// client, every call fails. Under `cargo test` it stays offline: GPUI's
    /// test scheduler rejects wakeups from the sync thread, and tests must
    /// never reach a real herdr by accident.
    ///
    /// herdr's saved machines load in the background and are re-read every
    /// [`MACHINES_POLL`].
    pub fn new(herdr_config: herdr::Result<herdr::Config>, cx: &mut Context<Self>) -> Self {
        let herdr_config = match herdr_config {
            Ok(config) => config,
            Err(error) => {
                tracing::error!("herdr: {error}");
                return Self::disconnected(error.to_string());
            }
        };
        // `from_env` configs auto-start a server; tests must use an inert one.
        #[cfg(test)]
        assert!(
            !herdr_config.auto_start,
            "tests must not build AppState from the environment; use an auto_start: false config"
        );
        let mut this = Self::disconnected(String::new());
        this.local_mut().client = Some(Client::new(herdr_config.clone()));
        this.local_mut().connection = ConnectionStatus::Connecting;
        this.herdr_config = Some(herdr_config);
        this.refresh_local_status();
        if !cfg!(test) {
            this.check_herdr(cx);
            this._machines_poll = Some(Self::poll_machines(cx));
        }
        // GPUI's quit ends the process without dropping entities, so the
        // links (and their ssh masters) are stopped here.
        this._quit = Some(cx.on_app_quit(|this, _| {
            stop_links(this.take_links(|_| true));
            async {}
        }));
        this
    }

    fn disconnected(reason: String) -> Self {
        let mut local = Machine::new(
            MachineId::Local,
            "Local".into(),
            MachineStatus::Offline(reason.clone()),
        );
        local.connection = ConnectionStatus::Disconnected { reason };
        local.worktree = std::env::current_dir().ok();
        Self {
            herdr_config: None,
            setup: Setup::default(),
            machines: vec![local],
            selected: MachineId::Local,
            machine_run: None,
            runs: 0,
            old_server_checked: None,
            added_here: Vec::new(),
            _run_check: None,
            _cwd_poll: None,
            _check: None,
            _machines_poll: None,
            _quit: None,
        }
    }

    // Machines

    /// Local first, then each saved machine in herdr's order.
    pub fn machines(&self) -> impl Iterator<Item = &Machine> {
        self.machines.iter()
    }

    pub fn machine(&self, id: &MachineId) -> Option<&Machine> {
        self.machines.iter().find(|m| &m.id == id)
    }

    fn machine_mut(&mut self, id: &MachineId) -> Option<&mut Machine> {
        self.machines.iter_mut().find(|m| &m.id == id)
    }

    fn local(&self) -> &Machine {
        &self.machines[0]
    }

    fn local_mut(&mut self) -> &mut Machine {
        &mut self.machines[0]
    }

    pub fn selected(&self) -> &MachineId {
        &self.selected
    }

    pub fn selected_machine(&self) -> &Machine {
        self.machine(&self.selected).unwrap_or_else(|| self.local())
    }

    /// The selected machine's cache.
    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.selected_machine().snapshot
    }

    /// The selected machine's connection to its herdr server.
    pub fn connection(&self) -> &ConnectionStatus {
        &self.selected_machine().connection
    }

    /// The selected machine's focused pane.
    pub fn focused_pane(&self) -> Option<&str> {
        self.selected_machine().focused_pane()
    }

    /// Local and each connected machine: the ones whose agents are listed
    /// (an offline machine's last cache is stale).
    pub fn live_machines(&self) -> impl Iterator<Item = &Machine> {
        self.machines
            .iter()
            .filter(|m| m.id.is_local() || m.status == MachineStatus::Connected)
    }

    /// Blocked agents plus unseen finished ones (herdr's `done`) on the live
    /// machines: the dock badge count, matching the agent list.
    pub fn attention_count(&self) -> usize {
        self.live_machines()
            .map(|m| attention::count(&m.snapshot))
            .sum()
    }

    /// Every agent on the live machines, with its machine.
    pub fn agents(&self) -> impl Iterator<Item = (&MachineId, &Agent)> {
        self.live_machines()
            .flat_map(|m| m.snapshot.agents.iter().map(move |agent| (&m.id, agent)))
    }

    /// Makes `id` the machine `snapshot`, `call` and the terminal area act on.
    pub fn select_machine(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        if &self.selected == id || self.machine(id).is_none() {
            return;
        }
        tracing::info!("machine selected: {id}");
        let was_local = self.selected.is_local();
        self.selected = id.clone();
        self.announce_selection(was_local, cx);
    }

    fn announce_selection(&mut self, was_local: bool, cx: &mut Context<Self>) {
        cx.emit(AppEvent::MachineSelected(self.selected.clone()));
        cx.emit(AppEvent::SnapshotReplaced);
        cx.emit(AppEvent::FocusedPaneChanged(
            self.selected_machine().focused_pane.clone(),
        ));
        if was_local != self.selected.is_local() {
            cx.emit(AppEvent::ActiveWorktreeChanged(
                self.active_worktree().cloned(),
            ));
        }
        cx.emit(AppEvent::ActiveRootChanged(self.active_root()));
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
        let running: Vec<&MachineProfile> =
            self.machines.iter().filter_map(Machine::profile).collect();
        let diff = machine_list::diff_profiles(&running, &saved);
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
            .map(|m| m.id.clone())
            .filter(|id| restarted(id))
            .collect();
        let gone = |id: &MachineId| {
            restarted(id) || matches!(id, MachineId::Remote(id) if diff.stop.contains(id))
        };
        // Stopping a link waits on ssh, so it happens off the main thread.
        let links = self.take_links(gone);
        cx.background_executor()
            .spawn(async move { stop_links(links) })
            .detach();
        self.machines.retain(|m| !gone(&m.id));
        for (id, label) in diff.relabel {
            if let Some(machine) = self.machine_mut(&MachineId::Remote(id)) {
                machine.label = label;
            }
        }
        for profile in diff.start {
            if let Some(machine) = self.start_machine(profile, cx) {
                self.machines.push(machine);
            }
        }
        // Local first, then herdr's order.
        self.machines.sort_by_key(|m| match &m.id {
            MachineId::Local => 0,
            MachineId::Remote(id) => 1 + saved.iter().position(|p| &p.id == id).unwrap_or(0),
        });
        for id in replaced {
            cx.emit(AppEvent::MachineRestarted(id));
        }
        let selected = machine_list::reselect(&self.selected, self.machines.iter().map(|m| &m.id));
        if selected != self.selected {
            let was_local = self.selected.is_local();
            self.selected = selected;
            self.announce_selection(was_local, cx);
        } else if selected_restarted {
            // A new link starts with an empty cache.
            cx.emit(AppEvent::SnapshotReplaced);
        }
        cx.emit(AppEvent::MachinesChanged);
        cx.notify();
    }

    /// Takes the links of the machines `which` picks, leaving those
    /// machines linkless.
    fn take_links(&mut self, which: impl Fn(&MachineId) -> bool) -> Vec<MachineLink> {
        self.machines
            .iter_mut()
            .filter(|m| which(&m.id))
            .filter_map(|m| m.link.take())
            .collect()
    }

    fn start_machine(&self, profile: MachineProfile, cx: &mut Context<Self>) -> Option<Machine> {
        let Some(dir) = machines::socket_dir() else {
            tracing::warn!(
                "machine {}: no socket directory (HOME unset)",
                profile.label
            );
            return None;
        };
        let id = MachineId::Remote(profile.id.clone());
        let label = profile.label.clone();
        let link = MachineLink::start(profile, dir);
        let mut machine = Machine::new(id.clone(), label, link.status());
        let statuses = link.statuses();
        machine._statuses = Some(cx.spawn(async move |this, cx| {
            while let Ok(status) = statuses.recv().await {
                let id = id.clone();
                if this
                    .update(cx, |this, cx| this.machine_status(&id, status, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        let connected = machine.status == MachineStatus::Connected;
        machine.link = Some(link);
        if connected {
            Self::start_remote_sync(&mut machine, cx);
        }
        Some(machine)
    }

    fn machine_status(&mut self, id: &MachineId, status: MachineStatus, cx: &mut Context<Self>) {
        let Some(machine) = self.machine_mut(id) else {
            return;
        };
        if machine.status == status {
            return;
        }
        tracing::info!("machine {}: {status:?}", machine.label);
        if let Some(cue) = machine_list::status_cue(&machine.status, &status, machine.lost) {
            sounds::play(cue, PlayOptions::default());
        }
        let connected = status == MachineStatus::Connected;
        if connected {
            machine.lost = false;
        } else if machine.status == MachineStatus::Connected {
            machine.lost = true;
        }
        machine.status = status;
        // The forwarded socket is fixed for the link's life, so one sync
        // started on the first connection follows every reconnect.
        if machine.status == MachineStatus::Connected && machine.sync.is_none() {
            Self::start_remote_sync(machine, cx);
        }
        if connected {
            cx.emit(AppEvent::MachineConnected(id.clone()));
            // Its files can be read now: find the focused pane's worktree.
            if let Some(machine) = self.machine_mut(id) {
                machine.focused_dir = None;
            }
            self.follow_focused_pane(id, cx);
        }
        cx.emit(AppEvent::MachinesChanged);
        cx.notify();
    }

    fn start_remote_sync(machine: &mut Machine, cx: &mut Context<Self>) {
        let Some(link) = &machine.link else {
            return;
        };
        match SessionSync::start(link.endpoint()) {
            Ok(sync) => {
                machine.client = Some(sync.client().clone());
                let task = Self::forward_updates(machine.id.clone(), &sync, cx);
                machine.sync = Some((sync, task));
            }
            Err(error) => {
                tracing::warn!("machine {}: {error}", machine.label);
                machine.connection = ConnectionStatus::Reconnecting {
                    reason: error.to_string(),
                };
            }
        }
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
        if !self.edit_allowed(id) {
            tracing::warn!("machine {command} {id}: refused in a debug build");
            return Task::ready(Err(anyhow::anyhow!(
                "debug builds only change machines listed in GRASSLANDS_DEV_MACHINES"
            )));
        }
        let mut argv = vec!["machine".to_owned(), command.to_owned(), profile_id.clone()];
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
    fn edit_allowed(&self, id: &MachineId) -> bool {
        !cfg!(debug_assertions)
            || self
                .machine(id)
                .is_some_and(|m| machines::dev_allowed(&m.label))
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
        match id {
            MachineId::Local => {
                if !self.setup.is_ready() {
                    self.check_herdr(cx);
                }
            }
            MachineId::Remote(_) => {
                if let Some(link) = self.machine(id).and_then(|m| m.link.as_ref()) {
                    link.reconnect();
                }
            }
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
                self.machine(id).and_then(Machine::profile).cloned()
            }
            MachineTask::Add { .. } => None,
        };
        let known = self
            .machines
            .iter()
            .filter_map(Machine::profile)
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
        cx.notify();
    }

    /// Whether run `run_id` has settled: its machine connected (done), or
    /// `last` and it hasn't (failed).
    fn check_machine_run(&mut self, run_id: u64, last: bool, cx: &mut Context<Self>) -> bool {
        let added = self.machine_run.as_ref().and_then(|run| match run.task {
            MachineTask::Add { .. } if run.machine.is_none() => machine_run::added(
                &run.known,
                self.machines.iter().filter_map(Machine::profile),
            )
            .map(|p| MachineId::Remote(p.id.clone())),
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
        }
        let connected = run
            .machine
            .as_ref()
            .and_then(|id| self.machines.iter().find(|m| &m.id == id))
            .is_some_and(|m| m.status == MachineStatus::Connected);
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
    ) {
        let mut machine = Machine::new(
            MachineId::Remote(profile_id.into()),
            label.into(),
            MachineStatus::Connected,
        );
        machine.snapshot = snapshot;
        self.machines.push(machine);
    }

    fn refresh_local_status(&mut self) {
        let setup_needed = match &self.setup.stage {
            Stage::Needed(_) | Stage::Fixing(_) => Some("herdr needs setup".to_owned()),
            Stage::Checking | Stage::Done | Stage::Ready => None,
        };
        let local = self.local_mut();
        local.status = machine_list::local_status(&local.connection, setup_needed);
    }

    // Local

    /// Starts Local's sync thread, once.
    fn start_sync(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self
            .herdr_config
            .clone()
            .filter(|_| self.local().sync.is_none())
        else {
            return;
        };
        match SessionSync::start(config) {
            Ok(sync) => {
                let task = Self::forward_updates(MachineId::Local, &sync, cx);
                self.local_mut().sync = Some((sync, task));
                self._cwd_poll = Some(Self::poll_focused_dir(cx));
            }
            Err(error) => {
                self.local_mut().connection = ConnectionStatus::Reconnecting {
                    reason: error.to_string(),
                };
                self.refresh_local_status();
            }
        }
    }

    /// Checks the herdr binary and server (`herdr status`); connects when
    /// they're fine, else shows the setup screen.
    pub fn check_herdr(&mut self, cx: &mut Context<Self>) {
        self.check(false, cx);
    }

    /// `after_fix` when the setup terminal's command just ended: the check
    /// says whether the fix worked.
    fn check(&mut self, after_fix: bool, cx: &mut Context<Self>) {
        let Some(config) = self.herdr_config.clone() else {
            return;
        };
        // While a fix runs, only its end says anything new.
        if matches!(self.setup.stage, Stage::Fixing(_)) && !after_fix {
            return;
        }
        let was = self.setup.stage.clone();
        // A screen already showing stays put while it checks again.
        if !matches!(was, Stage::Needed(_)) {
            self.setup.stage = Stage::Checking;
            cx.notify();
        }
        let shown = matches!(was, Stage::Needed(_)) || self.setup.run.is_some();
        // The fix's command searches PATH (for brew), so it's found here
        // rather than on every render of the setup screen.
        let check = cx.background_executor().spawn(async move {
            let check = config.check();
            let command = check.fix().and_then(|fix| config.fix_command(fix));
            (check, command)
        });
        self._check = Some(cx.spawn(async move |this, cx| {
            let (check, command) = check.await;
            this.update(cx, |this, cx| {
                tracing::info!("herdr check: {check:?}");
                if check == Check::Ready {
                    if after_fix {
                        sounds::play(Cue::Success, sounds::strong());
                    }
                    this.setup.stage = if shown { Stage::Done } else { Stage::Ready };
                    this.start_sync(cx);
                } else {
                    let stage = Stage::Needed(check);
                    if after_fix {
                        sounds::play(Cue::Error, PlayOptions::default());
                    } else if was != stage {
                        sounds::play(Cue::Warning, PlayOptions::default());
                    }
                    this.setup.stage = stage;
                }
                this.setup.fix_command = command;
                this.refresh_local_status();
                cx.emit(AppEvent::MachinesChanged);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Starts `fix` in the setup terminal; the terminal area runs it and
    /// calls [`Self::fix_exited`] when it ends.
    pub fn run_fix(&mut self, fix: Fix, cx: &mut Context<Self>) {
        if !matches!(self.setup.stage, Stage::Needed(_)) {
            return;
        }
        let Some(command) = self.setup.fix_command.clone() else {
            return;
        };
        // A check still running would overwrite `Fixing` when it ends.
        self._check = None;
        self.setup.spawn_error = None;
        tracing::info!("herdr setup: running {}", command.display());
        sounds::play(Cue::Loading, PlayOptions::default());
        let id = self.setup.run.as_ref().map_or(0, |run| run.id + 1);
        self.setup.stage = Stage::Fixing(fix);
        self.setup.run = Some(FixRun {
            id,
            command: command.terminal_command(),
        });
        self.refresh_local_status();
        cx.emit(AppEvent::MachinesChanged);
        cx.notify();
    }

    /// The setup terminal couldn't start; the screen says why, then checks
    /// again.
    pub fn fix_failed_to_start(&mut self, error: String, cx: &mut Context<Self>) {
        self.setup.spawn_error = Some(error);
        self.fix_exited(cx);
    }

    /// The setup terminal's command ended (or couldn't start); checks again.
    pub fn fix_exited(&mut self, cx: &mut Context<Self>) {
        if matches!(self.setup.stage, Stage::Fixing(_)) {
            tracing::info!("herdr setup: the fix ended");
            self.check(true, cx);
        }
    }

    /// Closes the setup screen once herdr is ready.
    pub fn finish_setup(&mut self, cx: &mut Context<Self>) {
        if self.setup.stage == Stage::Done {
            sounds::play(Cue::Tap, PlayOptions::default());
            self.setup = Setup::default();
            cx.notify();
        }
    }

    fn forward_updates(machine: MachineId, sync: &SessionSync, cx: &mut Context<Self>) -> Task<()> {
        let updates = sync.updates();
        cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this
                    .update(cx, |this, cx| this.apply(&machine, update, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Follows Local's focused pane, and the selected machine's, into
    /// another folder (`cd`, or an agent changing directory), which herdr
    /// reports no event for.
    fn poll_focused_dir(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CWD_POLL).await;
                let Ok(requests) = this.update(cx, |this, cx| {
                    let mut ids = vec![MachineId::Local];
                    if !this.selected.is_local() {
                        ids.push(this.selected.clone());
                    }
                    ids.into_iter()
                        .filter_map(|id| {
                            let pane_id = this.machine(&id)?.focused_pane.clone()?;
                            let request =
                                this.call_on(&id, cx, move |client| client.pane_get(&pane_id));
                            Some((id, request))
                        })
                        .collect::<Vec<_>>()
                }) else {
                    break;
                };
                for (id, request) in requests {
                    let Ok(pane) = request.await else {
                        continue;
                    };
                    let followed = this.update(cx, |this, cx| {
                        let focused = this.machine(&id).and_then(|m| m.focused_pane.as_ref());
                        if focused == Some(&pane.pane_id) {
                            this.follow_dir(&id, pane.working_dir().map(PathBuf::from), cx);
                        }
                    });
                    if followed.is_err() {
                        return;
                    }
                }
            }
        })
    }

    fn apply(&mut self, machine: &MachineId, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::State(snapshot) => self.apply_snapshot(machine, snapshot, cx),
            Update::Event(event) => {
                if machine == &self.selected {
                    cx.emit(AppEvent::Herdr(Arc::from(event)));
                }
            }
            Update::Status(status) => {
                let Some(target) = self.machine_mut(machine) else {
                    return;
                };
                if target.id.is_local() {
                    match (&target.connection, &status) {
                        (ConnectionStatus::Connected, ConnectionStatus::Reconnecting { .. }) => {
                            sounds::play(Cue::Warning, PlayOptions::default());
                        }
                        (ConnectionStatus::Reconnecting { .. }, ConnectionStatus::Connected) => {
                            sounds::play(Cue::Success, sounds::subtle());
                        }
                        _ => {}
                    }
                }
                target.connection = status;
                if machine.is_local() {
                    if !matches!(target.connection, ConnectionStatus::Connected) {
                        self.old_server_checked = None;
                    }
                    self.refresh_local_status();
                }
                cx.emit(AppEvent::MachinesChanged);
                cx.notify();
            }
        }
    }

    /// Local's active worktree while Local is selected; `None` while
    /// another machine is. For any machine, see `active_root`.
    pub fn active_worktree(&self) -> Option<&PathBuf> {
        self.local()
            .worktree
            .as_ref()
            .filter(|_| self.selected.is_local())
    }

    /// The selected machine's active worktree.
    pub fn active_root(&self) -> Option<ActiveRoot> {
        let machine = self.selected_machine();
        Some(ActiveRoot {
            machine: machine.id.clone(),
            path: machine.worktree.clone()?,
        })
    }

    /// The files on `machine`: Local always, a saved machine while it's
    /// connected.
    pub fn fs_for(&self, machine: &MachineId) -> Option<Fs> {
        if machine.is_local() {
            return Some(Fs::Local);
        }
        let target = self.machine(machine)?;
        if target.status != MachineStatus::Connected {
            return None;
        }
        Some(Fs::Remote {
            machine: machine.clone(),
            label: target.label.clone(),
            files: target.link.as_ref()?.remote_fs()?,
        })
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

    /// Sets `id`'s active worktree; announced only while `id` is selected.
    pub(crate) fn set_worktree(
        &mut self,
        id: &MachineId,
        root: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(machine) = self.machine_mut(id).filter(|m| m.worktree != root) else {
            return;
        };
        machine.worktree.clone_from(&root);
        if id != &self.selected {
            return;
        }
        if id.is_local() {
            cx.emit(AppEvent::ActiveWorktreeChanged(root));
        }
        cx.emit(AppEvent::ActiveRootChanged(self.active_root()));
        cx.notify();
    }

    /// Replaces the selected machine's cache, as a sync update would.
    pub fn replace_snapshot(&mut self, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        let selected = self.selected.clone();
        self.apply_snapshot(&selected, snapshot, cx);
    }

    fn apply_snapshot(&mut self, id: &MachineId, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        // A server from an older herdr (one started before an update, say):
        // the setup check names it and offers the restart.
        if id.is_local() {
            let old_server = Version::parse(&snapshot.version).is_some_and(|v| v < MIN_VERSION);
            let unchecked = self.old_server_checked.as_ref() != Some(&snapshot.version);
            if old_server && unchecked && self.setup.is_ready() {
                // Once per version and connection: a check that comes back
                // `Ready` (a server it can't identify) mustn't run again on
                // every event.
                self.old_server_checked = Some(snapshot.version.clone());
                self.check_herdr(cx);
            }
        }
        let selected = id == &self.selected;
        let Some(machine) = self.machine_mut(id) else {
            return;
        };
        let focused = snapshot.focused_pane_id.clone();
        let changes = machine.attention.update(&snapshot);
        machine.snapshot = snapshot;
        let focus_changed = machine.focused_pane != focused;
        machine.focused_pane.clone_from(&focused);
        if selected {
            cx.emit(AppEvent::SnapshotReplaced);
        }
        cx.emit(AppEvent::MachinesChanged);
        for change in changes {
            cx.emit(AppEvent::AgentStatusChanged {
                machine: id.clone(),
                change,
            });
        }
        if selected && focus_changed {
            cx.emit(AppEvent::FocusedPaneChanged(focused));
        }
        self.follow_focused_pane(id, cx);
        cx.notify();
    }

    /// Re-roots `id` on its focused pane's `foreground_cwd` (falling back
    /// to `cwd`), so it also follows an agent that moves into another
    /// folder. With no focused pane, or a folder that doesn't exist there,
    /// the root stays put.
    fn follow_focused_pane(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        let dir = self
            .machine(id)
            .and_then(|m| m.snapshot.focused_pane())
            .and_then(herdr::PaneInfo::working_dir)
            .map(PathBuf::from);
        self.follow_dir(id, dir, cx);
    }

    /// Resolves `dir` to its worktree (see `Fs::resolve_root`): at once on
    /// Local, on the background executor elsewhere. A machine whose files
    /// can't be read yet tries again on the next snapshot, poll or connect.
    fn follow_dir(&mut self, id: &MachineId, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(dir) = dir else { return };
        let Some(fs) = self.fs_for(id) else { return };
        let Some(machine) = self.machine_mut(id) else {
            return;
        };
        if machine.focused_dir.as_ref() == Some(&dir) {
            return;
        }
        machine.focused_dir = Some(dir.clone());
        if fs.is_local() {
            if let Some(root) = worktree::resolve_root(&dir) {
                self.set_worktree(id, Some(root), cx);
            }
            return;
        }
        let id = id.clone();
        let resolve = cx.background_executor().spawn({
            let dir = dir.clone();
            async move { fs.resolve_root(&dir) }
        });
        cx.spawn(async move |this, cx| {
            let root = resolve.await;
            this.update(cx, |this, cx| {
                let current = this.machine(&id).and_then(|m| m.focused_dir.as_ref());
                match root {
                    // A pane that has moved on since.
                    _ if current != Some(&dir) => {}
                    Some(root) => this.set_worktree(&id, Some(root), cx),
                    // Ask again next time.
                    None => {
                        if let Some(machine) = this.machine_mut(&id) {
                            machine.focused_dir = None;
                        }
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Runs one blocking herdr call on the selected machine, on the
    /// background executor. The typed methods below cover the common ones;
    /// use this for anything else.
    #[track_caller]
    pub fn call<T: Send + 'static>(
        &self,
        cx: &App,
        call: impl FnOnce(&Client) -> herdr::Result<T> + Send + 'static,
    ) -> Task<herdr::Result<T>> {
        self.call_on(&self.selected, cx, call)
    }

    /// [`Self::call`] on a given machine.
    #[track_caller]
    pub fn call_on<T: Send + 'static>(
        &self,
        machine: &MachineId,
        cx: &App,
        call: impl FnOnce(&Client) -> herdr::Result<T> + Send + 'static,
    ) -> Task<herdr::Result<T>> {
        // Every herdr command error is logged here, whatever the caller does
        // with it, so a detached or `.ok()`ed command never fails silently.
        let caller = std::panic::Location::caller();
        let Some(target) = self.machine(machine) else {
            tracing::warn!("herdr call on {machine} from {caller} skipped: no such machine");
            return Task::ready(Err(gone(machine)));
        };
        let Some(client) = target.client.clone() else {
            tracing::warn!("herdr call on {machine} from {caller} skipped: disconnected");
            return Task::ready(Err(herdr::Error::Refused(
                "herdr is disconnected; see the connection status".into(),
            )));
        };
        cx.background_executor().spawn(async move {
            call(&client)
                .inspect_err(|error| tracing::warn!("herdr call from {caller} failed: {error}"))
        })
    }

    // Panes

    #[track_caller]
    pub fn focus_pane(&self, pane_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_focus(&pane_id))
    }

    /// Selects `machine`, then focuses `pane_id` on it. Fails, selecting
    /// nothing, when `machine` is gone.
    #[track_caller]
    pub fn focus_pane_on(
        &mut self,
        machine: &MachineId,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<herdr::Result<()>> {
        if self.machine(machine).is_none() {
            return Task::ready(Err(gone(machine)));
        }
        self.select_machine(machine, cx);
        let pane_id = pane_id.to_owned();
        self.call_on(machine, cx, move |c| c.pane_focus(&pane_id))
    }

    /// Splits `pane_id` (herdr's focused pane when `None`). New panes forward
    /// right-clicks to the program in them; our context menu handles the rest.
    #[track_caller]
    pub fn split_pane(
        &self,
        pane_id: Option<&str>,
        direction: herdr::SplitDirection,
        cx: &App,
    ) -> Task<herdr::Result<herdr::PaneInfo>> {
        self.split_pane_on(&self.selected, pane_id, direction, cx)
    }

    #[track_caller]
    pub fn split_pane_on(
        &self,
        machine: &MachineId,
        pane_id: Option<&str>,
        direction: herdr::SplitDirection,
        cx: &App,
    ) -> Task<herdr::Result<herdr::PaneInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let mut split = herdr::PaneSplit::new(pane_id.map(str::to_owned), direction);
        split.focus = true;
        split.right_click = Some(herdr::RightClickTarget::Pane);
        self.call_on(machine, cx, move |c| c.pane_split(&split))
    }

    #[track_caller]
    pub fn close_pane(&self, pane_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        self.close_pane_on(&self.selected, pane_id, cx)
    }

    #[track_caller]
    pub fn close_pane_on(
        &self,
        machine: &MachineId,
        pane_id: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::subtle());
        let pane_id = pane_id.to_owned();
        self.call_on(machine, cx, move |c| c.pane_close(&pane_id))
    }

    #[track_caller]
    pub fn swap_panes(&self, source: &str, target: &str, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        let (source, target) = (source.to_owned(), target.to_owned());
        self.call(cx, move |c| c.pane_swap(&source, &target).map(drop))
    }

    #[track_caller]
    pub fn move_pane(
        &self,
        pane_id: &str,
        destination: herdr::PaneMoveDestination,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| {
            c.pane_move(&pane_id, &destination, true).map(drop)
        })
    }

    #[track_caller]
    pub fn set_right_click(
        &self,
        pane_id: &str,
        target: herdr::RightClickTarget,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let pane_id = pane_id.to_owned();
        self.call(cx, move |c| c.pane_set_right_click(&pane_id, target))
    }

    /// Sends a divider's final ratio. `path` comes from `SplitTree::splits`.
    #[track_caller]
    pub fn set_split_ratio(
        &self,
        tab_id: &str,
        path: Vec<bool>,
        ratio: f32,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| {
            c.layout_set_split_ratio(&tab_id, &path, ratio).map(drop)
        })
    }

    // Tabs

    #[track_caller]
    pub fn focus_tab(&self, tab_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        if self.snapshot().focused_tab_id.as_deref() != Some(tab_id) {
            sounds::play(Cue::Select, PlayOptions::default());
        }
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_focus(&tab_id))
    }

    /// Selects `machine`, then focuses `tab_id` on it. Fails, selecting
    /// nothing, when `machine` is gone.
    #[track_caller]
    pub fn focus_tab_on(
        &mut self,
        machine: &MachineId,
        tab_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<herdr::Result<()>> {
        if self.machine(machine).is_none() {
            return Task::ready(Err(gone(machine)));
        }
        self.select_machine(machine, cx);
        self.focus_tab(tab_id, cx)
    }

    #[track_caller]
    pub fn new_tab(
        &self,
        workspace_id: Option<&str>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::TabInfo>> {
        self.new_tab_on(&self.selected, workspace_id, cx)
    }

    #[track_caller]
    pub fn new_tab_on(
        &self,
        machine: &MachineId,
        workspace_id: Option<&str>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::TabInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let params = herdr::NewTab {
            workspace_id: workspace_id.map(str::to_owned),
            focus: true,
            ..Default::default()
        };
        self.call_on(machine, cx, move |c| c.tab_create(&params).map(|t| t.tab))
    }

    #[track_caller]
    pub fn rename_tab(&self, tab_id: &str, label: &str, cx: &App) -> Task<herdr::Result<()>> {
        self.rename_tab_on(&self.selected, tab_id, label, cx)
    }

    #[track_caller]
    pub fn rename_tab_on(
        &self,
        machine: &MachineId,
        tab_id: &str,
        label: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        let (tab_id, label) = (tab_id.to_owned(), label.to_owned());
        self.call_on(machine, cx, move |c| c.tab_rename(&tab_id, &label))
    }

    #[track_caller]
    pub fn move_tab(&self, tab_id: &str, index: usize, cx: &App) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        let tab_id = tab_id.to_owned();
        self.call(cx, move |c| c.tab_move(&tab_id, index))
    }

    #[track_caller]
    pub fn close_tab(&self, tab_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        self.close_tab_on(&self.selected, tab_id, cx)
    }

    #[track_caller]
    pub fn close_tab_on(
        &self,
        machine: &MachineId,
        tab_id: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::subtle());
        let tab_id = tab_id.to_owned();
        self.call_on(machine, cx, move |c| c.tab_close(&tab_id))
    }

    // Workspaces and worktrees

    /// Focuses a workspace on the selected machine.
    #[track_caller]
    pub fn focus_workspace(&self, workspace_id: &str, cx: &App) -> Task<herdr::Result<()>> {
        if self.snapshot().focused_workspace_id.as_deref() != Some(workspace_id) {
            sounds::play(Cue::Select, PlayOptions::default());
        }
        let id = workspace_id.to_owned();
        self.call(cx, move |c| c.workspace_focus(&id))
    }

    /// Focuses a workspace on any machine, selecting that machine first
    /// (workspace ids are per machine). Fails, selecting nothing, when
    /// `machine` is gone.
    #[track_caller]
    pub fn focus_workspace_on(
        &mut self,
        machine: &MachineId,
        workspace_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<herdr::Result<()>> {
        if self.machine(machine).is_none() {
            return Task::ready(Err(gone(machine)));
        }
        if &self.selected != machine {
            self.select_machine(machine, cx);
            // `focus_workspace` only sounds for a workspace not yet focused.
            if self.snapshot().focused_workspace_id.as_deref() == Some(workspace_id) {
                sounds::play(Cue::Select, PlayOptions::default());
            }
        }
        self.focus_workspace(workspace_id, cx)
    }

    #[track_caller]
    pub fn new_workspace(
        &self,
        cwd: Option<PathBuf>,
        label: Option<String>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
        self.new_workspace_on(&self.selected, cwd, label, cx)
    }

    /// A new, focused workspace on `machine`. The caller selects the machine
    /// to show it.
    #[track_caller]
    pub fn new_workspace_on(
        &self,
        machine: &MachineId,
        cwd: Option<PathBuf>,
        label: Option<String>,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
        sounds::play(Cue::Open, sounds::subtle());
        let params = herdr::NewWorkspace {
            cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
            label,
            focus: true,
            ..Default::default()
        };
        self.call_on(machine, cx, move |c| {
            c.workspace_create(&params).map(|w| w.workspace)
        })
    }

    #[track_caller]
    pub fn rename_workspace(
        &self,
        workspace_id: &str,
        label: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        self.rename_workspace_on(&self.selected, workspace_id, label, cx)
    }

    #[track_caller]
    pub fn rename_workspace_on(
        &self,
        machine: &MachineId,
        workspace_id: &str,
        label: &str,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::subtle());
        let (id, label) = (workspace_id.to_owned(), label.to_owned());
        self.call_on(machine, cx, move |c| c.workspace_rename(&id, &label))
    }

    /// Puts every workspace in `order` (all of them, first to last).
    #[track_caller]
    pub fn reorder_workspaces(&self, order: Vec<String>, cx: &App) -> Task<herdr::Result<()>> {
        self.reorder_workspaces_on(&self.selected, order, cx)
    }

    #[track_caller]
    pub fn reorder_workspaces_on(
        &self,
        machine: &MachineId,
        order: Vec<String>,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Tap, sounds::strong());
        self.call_on(machine, cx, move |c| c.workspace_move_block(&order, None))
    }

    /// Closes immediately; confirm first if it holds running agents.
    #[track_caller]
    pub fn close_workspace(
        &self,
        workspace_id: &str,
        close_group: bool,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        self.close_workspace_on(&self.selected, workspace_id, close_group, cx)
    }

    #[track_caller]
    pub fn close_workspace_on(
        &self,
        machine: &MachineId,
        workspace_id: &str,
        close_group: bool,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::strong());
        let id = workspace_id.to_owned();
        self.call_on(machine, cx, move |c| c.workspace_close(&id, close_group))
    }

    /// Closes each of `workspace_ids` on `machine` with its own
    /// `workspace.close`, all of them even when one fails (the first error
    /// is returned). For a group herdr's `close_group` doesn't define.
    #[track_caller]
    pub fn close_workspaces_on(
        &self,
        machine: &MachineId,
        workspace_ids: Vec<String>,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        sounds::play(Cue::Close, sounds::strong());
        self.call_on(machine, cx, move |c| {
            let mut outcome = Ok(());
            for id in &workspace_ids {
                let closed = c.workspace_close(id, false);
                if outcome.is_ok() {
                    outcome = closed;
                }
            }
            outcome
        })
    }

    #[track_caller]
    pub fn create_worktree_on(
        &self,
        machine: &MachineId,
        params: herdr::WorktreeParams,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceInfo>> {
        self.call_on(machine, cx, move |c| {
            c.worktree_create(&params).map(|w| w.workspace)
        })
    }

    /// The worktrees of `workspace_id`'s repo (`worktree.list`).
    #[track_caller]
    pub fn list_worktrees_on(
        &self,
        machine: &MachineId,
        workspace_id: &str,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorktreeList>> {
        let id = workspace_id.to_owned();
        self.call_on(machine, cx, move |c| c.worktree_list(Some(&id), None))
    }

    /// Opens an existing worktree as a workspace (`worktree.open`); herdr
    /// focuses the open one if there is one.
    #[track_caller]
    pub fn open_worktree_on(
        &self,
        machine: &MachineId,
        params: herdr::WorktreeParams,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorkspaceCreated>> {
        self.call_on(machine, cx, move |c| c.worktree_open(&params))
    }

    /// `worktree.remove`: deletes the linked worktree's checkout and closes
    /// its workspace. Without `force` herdr refuses a dirty checkout.
    ///
    /// herdr removes only workspaces it knows as worktrees. One made with a
    /// plain `workspace.create` in a linked checkout has no worktree info
    /// (the spaces list found it with git2), so herdr first adopts it:
    /// `worktree.list` names the checkout that workspace has open, and
    /// `worktree.open` on that path answers `already_open` and records it.
    #[track_caller]
    pub fn remove_worktree_on(
        &self,
        machine: &MachineId,
        workspace_id: &str,
        force: bool,
        cx: &App,
    ) -> Task<herdr::Result<herdr::WorktreeRemoved>> {
        let adopt = self
            .machine(machine)
            .and_then(|m| m.snapshot().workspace(workspace_id))
            .is_some_and(|w| w.info.worktree.is_none());
        let id = workspace_id.to_owned();
        self.call_on(machine, cx, move |c| {
            if adopt {
                adopt_worktree(c, &id)?;
            }
            c.worktree_remove(&id, force)
        })
    }

    // Agents and plugins

    #[track_caller]
    pub fn focus_agent(&self, target: &str, cx: &App) -> Task<herdr::Result<()>> {
        let target = target.to_owned();
        self.call(cx, move |c| c.agent_focus(&target))
    }

    #[track_caller]
    pub fn plugin_actions(&self, cx: &App) -> Task<herdr::Result<Vec<herdr::PluginActionInfo>>> {
        self.call(cx, |c| c.plugin_action_list(None))
    }

    #[track_caller]
    pub fn invoke_plugin_action_on(
        &self,
        machine: &MachineId,
        action: &herdr::PluginActionInfo,
        context: herdr::PluginInvocationContext,
        cx: &App,
    ) -> Task<herdr::Result<()>> {
        let (plugin_id, action_id) = (action.plugin_id.clone(), action.action_id.clone());
        self.call_on(machine, cx, move |c| {
            c.plugin_action_invoke(Some(&plugin_id), &action_id, Some(&context))
                .map(drop)
        })
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

/// The error for a command aimed at a machine no longer in the list.
fn gone(machine: &MachineId) -> herdr::Error {
    herdr::Error::Refused(format!("machine {machine} is no longer saved"))
}

/// Has herdr record `workspace_id`'s linked checkout as its worktree, so
/// `worktree.remove` accepts it. Does nothing when herdr lists no linked
/// checkout open in that workspace; the remove then says why.
fn adopt_worktree(client: &herdr::Client, workspace_id: &str) -> herdr::Result<()> {
    let list = client.worktree_list(Some(workspace_id), None)?;
    let Some(checkout) = list
        .worktrees
        .into_iter()
        .find(|w| w.is_linked_worktree && w.open_workspace_id.as_deref() == Some(workspace_id))
    else {
        return Ok(());
    };
    let params = herdr::WorktreeParams {
        workspace_id: Some(workspace_id.to_owned()),
        path: Some(checkout.path),
        focus: false,
        ..herdr::WorktreeParams::default()
    };
    client.worktree_open(&params).map(drop)
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
    use gpui_kit::{AppContext as _, TestAppContext};

    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use super::{ActiveRoot, AppEvent, AppState, MachineId, offline_config};
    use crate::herdr::{self, ConnectionStatus, Snapshot};
    use crate::setup::Stage;

    fn agent(pane: &str) -> herdr::Agent {
        herdr::Agent {
            pane_id: pane.into(),
            terminal_id: String::new(),
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
        state.update(cx, |state, _| {
            let snapshot = Snapshot {
                generation: 7,
                agents: vec![agent("w1:p1")],
                ..Snapshot::default()
            };
            state.add_test_machine("b87d", "dockerbox", Arc::new(snapshot));
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
        let generation = cx.read(|cx| state.read(cx).snapshot().generation);
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
        let root = cx.read(|cx| state.read(cx).active_worktree().cloned());
        assert_eq!(root, None);
    }

    #[gpui::test]
    fn selecting_local_again_restores_its_worktree(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let before = cx.read(|cx| state.read(cx).active_worktree().cloned());
        state.update(cx, |state, cx| {
            state.select_machine(&remote(), cx);
            state.select_machine(&MachineId::Local, cx);
        });
        let after = cx.read(|cx| state.read(cx).active_worktree().cloned());
        assert_eq!(after, before);
    }

    #[gpui::test]
    fn the_active_root_names_the_selected_machine(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| {
            state.set_worktree(&remote(), Some("/home/lyle/api".into()), cx);
            state.select_machine(&remote(), cx);
        });
        let root = cx.read(|cx| state.read(cx).active_root());
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

    #[gpui::test]
    fn local_files_are_always_available(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let local = cx.read(|cx| {
            state
                .read(cx)
                .fs_for(&MachineId::Local)
                .map(|f| f.is_local())
        });
        assert_eq!(local, Some(true));
    }

    #[gpui::test]
    fn a_machine_without_a_link_has_no_files(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let files = cx.read(|cx| state.read(cx).fs_for(&remote()).is_some());
        assert!(!files);
    }

    #[gpui::test]
    fn agents_name_their_machine(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let machines: Vec<MachineId> = cx.read(|cx| {
            state
                .read(cx)
                .agents()
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
        git2::Repository::init(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, cx| {
            state.follow_dir(&MachineId::Local, Some(repo.join("src")), cx)
        });
        let root = cx.read(|cx| state.read(cx).active_worktree().cloned());
        assert_eq!(root, Some(repo));
    }

    #[gpui::test]
    fn refused_endpoint_leaves_app_disconnected(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Err(refused()), cx));
        let connection = cx.read(|cx| state.read(cx).connection().clone());
        assert!(matches!(connection, ConnectionStatus::Disconnected { .. }));
    }

    #[gpui::test]
    fn refused_endpoint_fails_every_call(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Err(refused()), cx));
        let task = cx.update(|cx| state.read(cx).focus_pane("w1:p1", cx));
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
            state.focus_pane_on(&gone_machine(), "w1:p1", cx)
        });
        let result = cx.foreground_executor().block_test(task);
        assert!(matches!(result, Err(herdr::Error::Refused(m)) if m.contains("no longer saved")));
    }

    #[gpui::test]
    fn focusing_a_workspace_on_a_gone_machine_keeps_the_selection(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        state.update(cx, |state, cx| {
            state.select_machine(&remote(), cx);
            state.focus_workspace_on(&gone_machine(), "w1", cx).detach();
        });
        let selected = cx.read(|cx| state.read(cx).selected().clone());
        assert_eq!(selected, remote());
    }

    #[gpui::test]
    fn a_call_on_a_gone_machine_fails_instead_of_using_the_selected_one(cx: &mut TestAppContext) {
        let state = with_remote(cx);
        let task = cx.update(|cx| state.read(cx).close_pane_on(&gone_machine(), "w1:p1", cx));
        let result = cx.foreground_executor().block_test(task);
        assert!(matches!(result, Err(herdr::Error::Refused(m)) if m.contains("no longer saved")));
    }

    #[gpui::test]
    fn an_offline_machine_adds_nothing_to_the_badge(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        let count = state.update(cx, |state, _| {
            let mut blocked = agent("w1:p1");
            blocked.agent_status = herdr::AgentStatus::Blocked;
            let snapshot = Arc::new(Snapshot {
                agents: vec![blocked],
                ..Snapshot::default()
            });
            state.add_test_machine("b87d", "dockerbox", snapshot);
            if let Some(machine) = state.machine_mut(&remote()) {
                machine.status = herdr::MachineStatus::Offline("gone".into());
            }
            state.attention_count()
        });
        assert_eq!(count, 0);
    }

    #[gpui::test]
    fn a_debug_build_wont_rename_a_machine_outside_the_dev_list(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, _| {
            state.add_test_machine("c0ffee", "users-real-box", Arc::default());
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

    fn needing_install(cx: &mut TestAppContext) -> gpui_kit::Entity<AppState> {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        state.update(cx, |state, _| {
            state.setup.stage = Stage::Needed(herdr::Check::Missing);
            state.setup.fix_command = Some(herdr::FixCommand::new(vec!["true".into()]));
        });
        state
    }

    #[gpui::test]
    fn a_check_started_before_a_fix_doesnt_overwrite_it(cx: &mut TestAppContext) {
        let state = needing_install(cx);
        state.update(cx, |state, cx| {
            state.check_herdr(cx);
            state.run_fix(herdr::Fix::Install, cx);
        });
        cx.run_until_parked();
        let stage = cx.read(|cx| state.read(cx).setup.stage.clone());
        assert_eq!(stage, Stage::Fixing(herdr::Fix::Install));
    }

    #[gpui::test]
    fn checking_while_a_fix_runs_does_nothing(cx: &mut TestAppContext) {
        let state = needing_install(cx);
        state.update(cx, |state, cx| {
            state.run_fix(herdr::Fix::Install, cx);
            state.check_herdr(cx);
        });
        cx.run_until_parked();
        let stage = cx.read(|cx| state.read(cx).setup.stage.clone());
        assert_eq!(stage, Stage::Fixing(herdr::Fix::Install));
    }

    #[gpui::test]
    fn a_second_fix_click_doesnt_start_another_run(cx: &mut TestAppContext) {
        let state = needing_install(cx);
        state.update(cx, |state, cx| {
            state.run_fix(herdr::Fix::Install, cx);
            state.run_fix(herdr::Fix::Install, cx);
        });
        let run = cx.read(|cx| state.read(cx).setup.run.as_ref().map(|r| r.id));
        assert_eq!(run, Some(0));
    }

    #[gpui::test]
    fn an_old_local_server_is_checked_once_per_version(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(Ok(offline_config()), cx));
        let old = || {
            Arc::new(Snapshot {
                version: "0.9.0".into(),
                ..Snapshot::default()
            })
        };
        state.update(cx, |state, cx| state.replace_snapshot(old(), cx));
        cx.run_until_parked();
        state.update(cx, |state, cx| {
            state.setup.stage = Stage::Ready;
            state.replace_snapshot(old(), cx);
        });
        let stage = cx.read(|cx| state.read(cx).setup.stage.clone());
        assert_eq!(stage, Stage::Ready);
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
