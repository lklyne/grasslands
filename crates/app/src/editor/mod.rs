//! The code editor: GPUI Kit's code editor over a list of open files.
//!
//! Open files are grouped per machine and worktree; when the active worktree
//! changes the visible group swaps. The tabs themselves are drawn by the
//! window's titlebar (`workspace`), from `tabs` and `active_index`. A buffer
//! is a path on one machine. Local files are watched and remote ones polled
//! while the window is active; a file that changes on disk reloads unless it
//! has unsaved edits, in which case the user picks between their edits and
//! the disk contents. An HTML file shows as a web page, with its source a
//! toggle away (decision 0025).

mod buffer;
mod language;
mod poll;
mod scrollback;
mod titles;
mod watcher;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Editor as CodeEditor, EditorState, InputEvent, Position};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::buffer::{Buffer, DiskChange, Page, Prompt};
use self::poll::{Check, Stamp};
use self::watcher::FileWatcher;
use crate::fs::{Caps, Fs};
use crate::herdr::PaneId;
use crate::state::{AppEvent, AppState, MachineId};

actions!(editor, [Save, CloseTab]);

const CONTEXT: &str = "Editor";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-s", Save, Some(CONTEXT)),
        KeyBinding::new("cmd-w", CloseTab, Some(CONTEXT)),
    ]);
}

/// A file on one machine's disk.
type FileKey = (MachineId, PathBuf);

/// Each open buffer of a file, by its editor state, with its generation.
type Generations = Vec<(EntityId, u64)>;

/// One remote file to poll: the stamp last seen, and the buffers it was
/// taken from as they were then.
struct PollJob {
    key: FileKey,
    seen: Option<Stamp>,
    buffers: Generations,
    fs: Fs,
}

/// One remote file's poll result: the check, and the contents if it read them.
struct Polled {
    key: FileKey,
    buffers: Generations,
    check: Check,
    text: Option<Result<String, io::ErrorKind>>,
}

/// Stats each job's file, and reads it if its stamp moved. Blocking.
fn run_poll(jobs: Vec<PollJob>) -> Vec<Polled> {
    jobs.into_iter()
        .map(|job| {
            let check = poll::check(job.seen, &stamp(&job.fs, &job.key.1));
            let text = matches!(check, Check::Read(_)).then(|| {
                job.fs
                    .read_to_string(&job.key.1)
                    .map_err(|error| error.kind())
            });
            Polled {
                key: job.key,
                buffers: job.buffers,
                check,
                text,
            }
        })
        .collect()
}

fn stamp(fs: &Fs, path: &Path) -> io::Result<Stamp> {
    fs.metadata(path).map(|meta| (meta.modified, meta.len))
}

/// Which tab set is shown: the selected machine's active worktree
/// (`AppState::active_root`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
struct GroupKey {
    machine: MachineId,
    /// `None`: no worktree.
    root: Option<PathBuf>,
}

impl GroupKey {
    fn of(state: &AppState, cx: &App) -> Self {
        Self {
            machine: state.selected().clone(),
            root: state.active_root(cx).map(|root| root.path),
        }
    }
}

/// The open tabs of one worktree.
#[derive(Default)]
struct Group {
    buffers: Vec<Buffer>,
    active: usize,
}

impl Group {
    fn active(&self) -> Option<&Buffer> {
        self.buffers.get(self.active)
    }

    fn position(&self, machine: &MachineId, path: &Path) -> Option<usize> {
        self.buffers
            .iter()
            .position(|buffer| buffer.is(machine, path))
    }

    fn titles(&self, label: impl Fn(&MachineId) -> String) -> Vec<String> {
        let files: Vec<(&MachineId, &Path)> = self
            .buffers
            .iter()
            .map(|buffer| (&buffer.machine, buffer.path.as_path()))
            .collect();
        titles::titles_on(&files, label)
    }
}

/// One open file as the titlebar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTab {
    pub machine: MachineId,
    pub path: PathBuf,
    pub title: String,
    pub dirty: bool,
}

/// Emitted when a file tab becomes the active one: opened, picked, or
/// brought forward to ask about unsaved edits. The workspace then shows the
/// editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    Activated,
}

impl EventEmitter<EditorEvent> for Editor {}

pub struct Editor {
    state: Entity<AppState>,
    groups: HashMap<GroupKey, Group>,
    visible: GroupKey,
    /// Remote files being read for opening, so a second click doesn't open
    /// them twice.
    opening: HashSet<FileKey>,
    /// A poll of remote files is running.
    polling: bool,
    watcher: Option<FileWatcher>,
    /// The workspace is showing the editor (`set_shown`). Pages are native
    /// views, so they must be hidden explicitly when it isn't.
    shown: bool,
    /// Holds keyboard focus while a page is shown, so the editor's key
    /// bindings still apply.
    focus: FocusHandle,
    _subscriptions: [Subscription; 3],
    _disk_changes: Task<()>,
    _poll: Option<Task<()>>,
}

impl Editor {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe_in(
            &state,
            window,
            |this, state, event, window, cx| match event {
                // Also sent after every `MachineSelected`.
                AppEvent::ActiveRootChanged(_) => {
                    this.visible = GroupKey::of(state.read(cx), cx);
                    this.sync_pages(window);
                    cx.notify();
                }
                AppEvent::MachineConnected(_) => {
                    this.poll_remote(window.is_window_active(), window, cx)
                }
                AppEvent::FileMoved { machine, from, to } => {
                    this.follow_move(machine, from, to, window, cx)
                }
                AppEvent::FileRemoved { machine, path } => {
                    this.follow_remove(machine, path, window, cx)
                }
                _ => {}
            },
        );
        let focus = cx.focus_handle();
        // Focus can leave by keyboard, which no mouse-down sees.
        let focus_out = cx.on_focus_out(&focus, window, |this, _, _, _| this.release_pages());
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.poll_remote(window.is_window_active(), window, cx)
        });
        let (sender, receiver) = async_channel::unbounded::<Vec<PathBuf>>();
        // GPUI's test scheduler rejects wakeups from the watcher's thread;
        // tests feed `on_disk_change` directly and test the watcher alone.
        let watcher = (!cfg!(test))
            .then(|| FileWatcher::new(sender))
            .transpose()
            .inspect_err(|error| tracing::warn!("editor: file watching is off: {error}"))
            .ok()
            .flatten();
        let disk_changes = cx.spawn_in(window, async move |this, cx| {
            while let Ok(paths) = receiver.recv().await {
                let updated = this.update_in(cx, |this, window, cx| {
                    this.on_disk_change(&paths, window, cx);
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        // Tests call `poll_remote` themselves.
        let poll = (!cfg!(test)).then(|| {
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(poll::INTERVAL).await;
                    let polled = this.update_in(cx, |this, window, cx| {
                        this.poll_remote(window.is_window_active(), window, cx)
                    });
                    if polled.is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            visible: GroupKey::of(state.read(cx), cx),
            state,
            groups: HashMap::new(),
            opening: HashSet::new(),
            polling: false,
            watcher,
            shown: false,
            focus,
            _subscriptions: [events, activation, focus_out],
            _disk_changes: disk_changes,
            _poll: poll,
        }
    }

    /// The visible group's tabs, in order.
    pub fn tabs(&self, cx: &App) -> Vec<FileTab> {
        let Some(group) = self.group() else {
            return Vec::new();
        };
        group
            .buffers
            .iter()
            .zip(group.titles(|machine| self.label(machine, cx)))
            .map(|(buffer, title)| FileTab {
                machine: buffer.machine.clone(),
                path: buffer.path.clone(),
                title,
                dirty: buffer.is_dirty(),
            })
            .collect()
    }

    /// The selected file's text, for script hooks.
    pub fn active_text(&self, cx: &App) -> Option<String> {
        let buffer = self.group().and_then(Group::active)?;
        Some(buffer.state.read(cx).value().to_string())
    }

    /// The selected tab of the visible group, if it has any.
    pub fn active_index(&self) -> Option<usize> {
        self.group()
            .filter(|group| !group.buffers.is_empty())
            .map(|group| group.active)
    }

    /// Gives keyboard focus to the selected file.
    pub fn focus_active(&self, window: &mut Window, cx: &mut App) {
        match self.group().and_then(Group::active) {
            Some(buffer) if buffer.shown_page().is_some() => window.focus(&self.focus, cx),
            Some(buffer) => buffer.state.update(cx, |state, cx| state.focus(window, cx)),
            None => {}
        }
    }

    /// The workspace shows the editor, or stops showing it.
    pub fn set_shown(&mut self, shown: bool, window: &Window, cx: &mut Context<Self>) {
        if self.shown != shown {
            self.shown = shown;
            self.sync_pages(window);
            cx.notify();
        }
    }

    /// Shows the selected tab's page, if it shows one and the editor is on
    /// screen, making its web view on first use; hides every other page.
    /// Called on every change to what's selected or shown, since a native
    /// view stays on screen until it's hidden.
    fn sync_pages(&mut self, window: &Window) {
        let (shown, visible) = (self.shown, &self.visible);
        for (key, group) in &mut self.groups {
            let active = group.active;
            for (index, buffer) in group.buffers.iter_mut().enumerate() {
                let show =
                    shown && key == visible && index == active && buffer.shown_page().is_some();
                if show {
                    buffer.make_page_view(window);
                }
                if let Some(view) = buffer.page.as_ref().and_then(Page::view) {
                    view.set_visible(show);
                }
            }
        }
    }

    /// Takes the keyboard back from the shown page; hidden pages already
    /// gave it back. GPUI only sees clicks outside the page, so the
    /// workspace calls this on every mouse down, and the editor when it
    /// loses focus.
    pub fn release_pages(&self) {
        if let Some(view) = self
            .group()
            .and_then(Group::active)
            .and_then(|buffer| buffer.page.as_ref()?.view())
        {
            view.release_keyboard();
        }
    }

    /// What a local page at `path` in `group` may read: the worktree it's
    /// in, so links between its files work, or else only itself.
    fn page_access(group: &GroupKey, path: &Path) -> PathBuf {
        group
            .root
            .as_deref()
            .and_then(|root| root.canonicalize().ok())
            .filter(|root| path.starts_with(root))
            .unwrap_or_else(|| path.to_path_buf())
    }

    /// Switches the selected HTML tab between its page and its source.
    fn show_page(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self
            .active_buffer_mut()
            .and_then(|buffer| buffer.page.as_mut())
            .filter(|page| page.shown != shown)
        else {
            return;
        };
        page.shown = shown;
        crate::sounds::play(cues::Cue::Select, crate::sounds::subtle());
        self.sync_pages(window);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn group(&self) -> Option<&Group> {
        self.groups.get(&self.visible)
    }

    fn group_mut(&mut self) -> &mut Group {
        self.groups.entry(self.visible.clone()).or_default()
    }

    fn active_buffer_mut(&mut self) -> Option<&mut Buffer> {
        let group = self.groups.get_mut(&self.visible)?;
        group.buffers.get_mut(group.active)
    }

    fn buffers_mut(&mut self) -> impl Iterator<Item = &mut Buffer> {
        self.groups
            .values_mut()
            .flat_map(|group| group.buffers.iter_mut())
    }

    fn buffer_mut(&mut self, state: &Entity<EditorState>) -> Option<&mut Buffer> {
        self.buffers_mut().find(|buffer| &buffer.state == state)
    }

    fn label(&self, machine: &MachineId, cx: &App) -> String {
        self.state
            .read(cx)
            .machine(machine, cx)
            .map_or_else(|| machine.to_string(), |m| m.label().to_owned())
    }

    /// `path` as error messages name it: on a remote machine, with the
    /// machine's label.
    fn describe(&self, machine: &MachineId, path: &Path, cx: &App) -> String {
        if machine.is_local() {
            path.display().to_string()
        } else {
            format!("{} on {}", path.display(), self.label(machine, cx))
        }
    }

    /// Open `path` on `machine` in the visible group, or switch to its tab.
    pub fn open(
        &mut self,
        machine: &MachineId,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with(machine, path, None, window, cx);
    }

    /// Open `path` with the cursor at the start of `line` (0-based).
    pub fn open_at(
        &mut self,
        machine: &MachineId,
        path: &Path,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with(machine, path, Some(line), window, cx);
    }

    fn open_with(
        &mut self,
        machine: &MachineId,
        path: &Path,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cannot_open = |this: &Self, error: &dyn std::fmt::Display, cx: &App| {
            format!("Cannot open {}: {error}", this.describe(machine, path, cx))
        };
        // Watch events report canonical paths (/private/tmp, not /tmp).
        let caps = Caps::of(machine);
        let path = if caps.watch {
            match path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    let message = cannot_open(self, &error, cx);
                    notify_error(message, window, cx);
                    return;
                }
            }
        } else {
            path.to_path_buf()
        };
        if let Some(index) = self
            .group()
            .and_then(|group| group.position(machine, &path))
        {
            self.activate(index, window, cx);
            self.go_to_line(line, window, cx);
            return;
        }
        let Some(fs) = self.state.read(cx).fs_for(machine, cx) else {
            let offline = format!("{} is offline", self.label(machine, cx));
            let message = cannot_open(self, &offline, cx);
            notify_error(message, window, cx);
            return;
        };
        let key = (machine.clone(), path);
        // No stamp: a watched file needn't be polled.
        if caps.cheap && caps.watch {
            match fs.read_to_string(&key.1) {
                Ok(text) => {
                    let group = self.visible.clone();
                    self.add_buffer(group, key, (text, None), line, None, window, cx);
                }
                Err(error) => {
                    let message = cannot_open(self, &error, cx);
                    notify_error(message, window, cx);
                }
            }
            return;
        }
        if !self.opening.insert(key.clone()) {
            return;
        }
        let group = self.visible.clone();
        cx.spawn_in(window, async move |this, cx| {
            let path = key.1.clone();
            let read = cx
                .background_spawn(async move {
                    // Time first: a change between the two shows up next poll.
                    let stamp = stamp(&fs, &path).ok();
                    fs.read_to_string(&path).map(|text| (text, stamp))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.opening.remove(&key);
                match read {
                    Ok(file) => this.add_buffer(group, key, file, line, None, window, cx),
                    Err(error) => {
                        let message =
                            format!("Cannot open {}: {error}", this.describe(&key.0, &key.1, cx));
                        notify_error(message, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Opens `pane_id`'s scrollback (on `machine`) as a read-only tab with
    /// the cursor at the end, or re-reads and shows the one already open.
    /// `columns` is the width of the pane's terminal here, if it's attached.
    pub fn open_scrollback(
        &mut self,
        machine: &MachineId,
        pane_id: &PaneId,
        columns: Option<u16>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let name = state
            .machine(machine, cx)
            .and_then(|session| {
                let snapshot = session.snapshot();
                let pane = snapshot.pane(pane_id)?;
                Some(crate::label::pane(snapshot, pane).into_owned())
            })
            .unwrap_or_else(|| pane_id.to_string());
        // The pane id keeps two panes with one name apart; titles show it
        // only then. Titles are often paths, so their slashes are swapped
        // for a look-alike that doesn't split the tab's path.
        let title = format!("{} scrollback", name.replace('/', "\u{2215}"));
        let path = PathBuf::from(pane_id.as_str()).join(title);
        let read = {
            let pane_id = pane_id.clone();
            state.call(machine, cx, move |client| client.pane_scrollback(&pane_id, columns))
        };
        let (machine, pane_id) = (machine.clone(), pane_id.clone());
        cx.spawn_in(window, async move |this, cx| {
            let read = read.await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok(text) => {
                    // The group shown now: the worktree may have changed
                    // while herdr read.
                    let group = this.visible.clone();
                    this.show_scrollback(group, (machine, path), pane_id, text, window, cx)
                }
                Err(error) => {
                    notify_error(format!("Cannot read {name}'s scrollback: {error}"), window, cx)
                }
            });
        })
        .detach();
    }

    fn show_scrollback(
        &mut self,
        group: GroupKey,
        (machine, path): FileKey,
        pane_id: PaneId,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let end = u32::try_from(text.lines().count().saturating_sub(1)).unwrap_or(u32::MAX);
        let open = self.groups.get_mut(&group).and_then(|group| {
            group
                .buffers
                .iter_mut()
                .enumerate()
                .find(|(_, b)| b.machine == machine && b.scrollback.as_ref() == Some(&pane_id))
        });
        if let Some((index, buffer)) = open {
            buffer.path = path;
            buffer.take(text, window, cx);
            self.reveal(group, index, Some(end), window, cx);
            return;
        }
        let file = (text, None);
        self.add_buffer(group, (machine, path), file, Some(end), Some(pane_id), window, cx);
    }

    /// Adds a tab to `group`, and selects it if that group is still shown.
    /// With `scrollback` it's that pane's read-only scrollback, not a file.
    #[expect(clippy::too_many_arguments, reason = "GPUI's window and cx come last")]
    fn add_buffer(
        &mut self,
        group: GroupKey,
        (machine, path): FileKey,
        file: (String, Option<Stamp>),
        line: Option<u32>,
        scrollback: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = Buffer::new_state(&path, window, cx);
        if scrollback.is_some() {
            state.update(cx, |state, cx| {
                state.set_readonly(true, cx);
                state.lsp_mut().definition_provider = Some(std::rc::Rc::new(scrollback::UrlLinks));
            });
        }
        let changes = cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_buffer_edited(&state, cx);
            }
        });
        let html = scrollback.is_none() && language::for_path(&path) == "html";
        let mut buffer = Buffer::new((machine, path), file, state, changes, window, cx);
        buffer.scrollback = scrollback;
        buffer.page = html.then(|| Page::new(Self::page_access(&group, &buffer.path)));
        if buffer.is_watched()
            && let Some(watcher) = &mut self.watcher
        {
            watcher.add(&buffer.path);
        }
        let buffers = &mut self.groups.entry(group.clone()).or_default().buffers;
        buffers.push(buffer);
        let index = buffers.len() - 1;
        self.reveal(group, index, line, window, cx);
    }

    /// Selects tab `index` of `group` at `line` if that group is shown.
    fn reveal(
        &mut self,
        group: GroupKey,
        index: usize,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if group == self.visible {
            self.activate(index, window, cx);
            self.go_to_line(line, window, cx);
        } else {
            cx.notify();
        }
    }

    fn go_to_line(&mut self, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(line) = line
            && let Some(buffer) = self.group().and_then(Group::active)
        {
            buffer.state.update(cx, |state, cx| {
                state.set_cursor_position(Position::new(line, 0), window, cx)
            });
        }
    }

    /// Selects tab `index` of the visible group and focuses it.
    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group_mut();
        if index >= group.buffers.len() {
            return;
        }
        group.active = index;
        self.sync_pages(window);
        self.focus_active(window, cx);
        cx.emit(EditorEvent::Activated);
        cx.notify();
    }

    fn on_buffer_edited(&mut self, state: &Entity<EditorState>, cx: &mut Context<Self>) {
        // The edited buffer may be in a group other than the visible one.
        if let Some(buffer) = self.buffer_mut(state) {
            let was_dirty = buffer.is_dirty();
            buffer.refresh_dirty(cx);
            if was_dirty != buffer.is_dirty() {
                cx.notify();
            }
        }
    }

    /// The watcher saw `paths` change on this Mac's disk.
    fn on_disk_change(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let mut moved = Vec::new();
        for buffer in self
            .buffers_mut()
            .filter(|buffer| buffer.machine.is_local() && paths.contains(&buffer.path))
        {
            buffer.refresh_file_id();
            match buffer.classify(Fs::Local.read_to_string(&buffer.path)) {
                Ok(DiskChange::Deleted) => match buffer.find_moved(paths) {
                    Some(to) => moved.push((buffer.path.clone(), to)),
                    None => buffer.mark_deleted(),
                },
                Ok(change) => apply_change(buffer, change, window, cx),
                Err(error) => {
                    tracing::warn!("editor: cannot reload {}: {error}", buffer.path.display())
                }
            }
        }
        for (from, to) in moved {
            self.follow_move(&MachineId::Local, &from, &to, window, cx);
        }
        cx.notify();
    }

    /// Checks open remote files for changes on their machines, off the UI
    /// thread, and reloads or asks as the watcher does for local ones.
    /// Skipped unless `window_active`.
    fn poll_remote(&mut self, window_active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        let jobs = self.poll_jobs(window_active, cx);
        if jobs.is_empty() {
            return;
        }
        self.polling = true;
        cx.spawn_in(window, async move |this, cx| {
            let found = cx.background_spawn(async move { run_poll(jobs) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.polling = false;
                this.apply_poll(found, window, cx);
            });
        })
        .detach();
    }

    /// The open remote files due a check, one job per file.
    fn poll_jobs(&self, window_active: bool, cx: &App) -> Vec<PollJob> {
        let mut jobs: Vec<PollJob> = Vec::new();
        for buffer in self.groups.values().flat_map(|group| &group.buffers) {
            let machine = &buffer.machine;
            if buffer.caps().watch || !buffer.is_file() {
                continue;
            }
            let fs = self.state.read(cx).fs_for(machine, cx);
            if !poll::wanted(window_active, fs.is_some(), buffer.is_saving()) {
                continue;
            }
            let generation = (buffer.state.entity_id(), buffer.generation());
            if let Some(job) = jobs
                .iter_mut()
                .find(|job| buffer.is(&job.key.0, &job.key.1))
            {
                job.buffers.push(generation);
            } else if let Some(fs) = fs {
                jobs.push(PollJob {
                    key: (machine.clone(), buffer.path.clone()),
                    seen: buffer.stamp,
                    buffers: vec![generation],
                    fs,
                });
            }
        }
        jobs
    }

    /// Applies poll results to buffers that haven't changed since the poll
    /// read their files: a save, reload or answer meanwhile makes the result
    /// stale, and it is dropped (the next poll reads afresh).
    fn apply_poll(&mut self, found: Vec<Polled>, window: &mut Window, cx: &mut Context<Self>) {
        for Polled {
            key: (machine, path),
            buffers,
            check,
            text,
        } in found
        {
            for buffer in self.buffers_mut().filter(|buffer| {
                buffer.is(&machine, &path)
                    && !buffer.is_saving()
                    && buffers.contains(&(buffer.state.entity_id(), buffer.generation()))
            }) {
                let disk = match (&check, &text) {
                    (Check::Deleted, _) => Err(io::ErrorKind::NotFound.into()),
                    (Check::Read(stamp), Some(text)) => {
                        buffer.stamp = Some(*stamp);
                        text.clone().map_err(io::Error::from)
                    }
                    _ => continue,
                };
                match buffer.classify(disk) {
                    Ok(change) => apply_change(buffer, change, window, cx),
                    Err(error) => {
                        tracing::warn!("editor: cannot reload {}: {error}", path.display())
                    }
                }
            }
        }
        cx.notify();
    }

    /// Points tabs at or under `from` (a file or folder on `machine`) at
    /// their new place.
    fn follow_move(
        &mut self,
        machine: &MachineId,
        from: &Path,
        to: &Path,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let watcher = &mut self.watcher;
        for (key, buffer) in self
            .groups
            .iter_mut()
            .flat_map(|(key, group)| group.buffers.iter_mut().map(move |buffer| (key, buffer)))
            .filter(|(_, buffer)| &buffer.machine == machine)
        {
            let Ok(rest) = buffer.path.strip_prefix(from) else {
                continue;
            };
            // `join("")` would add a trailing slash.
            let new = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            if buffer.is_watched()
                && let Some(watcher) = watcher
            {
                watcher.remove(&buffer.path);
                watcher.add(&new);
            }
            buffer.path = new;
            buffer.refresh_file_id();
            // A rename can make the file HTML or stop it being HTML, and a
            // move changes what the page may read.
            let html = buffer.is_file() && language::for_path(&buffer.path) == "html";
            let access = Self::page_access(key, &buffer.path);
            match &mut buffer.page {
                Some(page) if html => page.access = access,
                _ => buffer.page = html.then(|| Page::new(access)),
            }
            buffer.load_page();
        }
        self.sync_pages(window);
        cx.notify();
    }

    /// The user removed `path` (a file or folder on `machine`) from the tree:
    /// clean tabs at or under it close; tabs with unsaved edits stay, marked
    /// deleted.
    fn follow_remove(
        &mut self,
        machine: &MachineId,
        path: &Path,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        for group in self.groups.values_mut() {
            let active = group.active().map(|buffer| buffer.state.clone());
            let watcher = &mut self.watcher;
            group.buffers.retain_mut(|buffer| {
                if &buffer.machine != machine || !buffer.path.starts_with(path) {
                    return true;
                }
                if buffer.is_dirty() {
                    buffer.mark_deleted();
                    return true;
                }
                if buffer.is_watched()
                    && let Some(watcher) = watcher
                {
                    watcher.remove(&buffer.path);
                }
                false
            });
            group.active = active
                .and_then(|active| group.buffers.iter().position(|b| b.state == active))
                .unwrap_or(group.active)
                .min(group.buffers.len().saturating_sub(1));
        }
        self.sync_pages(window);
        cx.notify();
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(buffer) = self.group().and_then(Group::active)
            && buffer.is_file()
        {
            let state = buffer.state.clone();
            self.start_save(state, false, window, cx);
        }
    }

    /// Writes the buffer off the UI thread; closes its tab afterwards if
    /// `close_after` and the write worked. A remote machine that's offline
    /// refuses the save and the edits stay.
    fn start_save(
        &mut self,
        target: Entity<EditorState>,
        close_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(buffer) = self
            .groups
            .values()
            .flat_map(|g| &g.buffers)
            .find(|b| b.state == target)
        else {
            return;
        };
        let (machine, path) = (buffer.machine.clone(), buffer.path.clone());
        let Some(fs) = self.state.read(cx).fs_for(&machine, cx) else {
            let message = format!(
                "Cannot save {}: {} is offline. Your edits are kept; save again once it reconnects.",
                path.display(),
                self.label(&machine, cx)
            );
            notify_error(message, window, cx);
            return;
        };
        let Some(text) = self
            .buffer_mut(&target)
            .and_then(|buffer| buffer.begin_save(cx))
        else {
            return;
        };
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let written = cx
                .background_spawn(async move {
                    fs.write(&path, &text)?;
                    Ok(stamp(&fs, &path).ok())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_save(&target, written, close_after, window, cx)
            });
        })
        .detach();
    }

    fn finish_save(
        &mut self,
        target: &Entity<EditorState>,
        written: io::Result<Option<Stamp>>,
        close_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(buffer) = self.buffer_mut(target) else {
            return;
        };
        let failed = written.as_ref().err().map(|error| {
            (
                buffer.machine.clone(),
                buffer.path.clone(),
                error.to_string(),
            )
        });
        buffer.finish_save(written, cx);
        match failed {
            Some((machine, path, error)) => {
                let message = format!(
                    "Cannot save {}: {error}",
                    self.describe(&machine, &path, cx)
                );
                notify_error(message, window, cx);
            }
            None if close_after => self.close_buffer(target, window, cx),
            None => {}
        }
        cx.notify();
    }

    fn close_active(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.group().map(|group| group.active) {
            self.request_close(index, window, cx);
        }
    }

    /// Close tab `index`, asking first if it has unsaved edits.
    pub fn request_close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.group_mut().buffers.get_mut(index) else {
            return;
        };
        if buffer.is_dirty() {
            buffer.prompt = Some(Prompt::CloseUnsaved);
            crate::sounds::play(cues::Cue::Attention, crate::sounds::subtle());
            self.activate(index, window, cx);
        } else {
            self.close(index, window, cx);
        }
    }

    /// Closes the tab showing `target`, in whichever group it is.
    fn close_buffer(
        &mut self,
        target: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self.groups.iter().find_map(|(key, group)| {
            let index = group.buffers.iter().position(|b| &b.state == target)?;
            Some((key.clone(), index))
        });
        let Some((key, index)) = found else {
            return;
        };
        if key == self.visible {
            self.close(index, window, cx);
        } else if let Some(group) = self.groups.get_mut(&key) {
            let buffer = remove_tab(group, index);
            self.unwatch(&buffer);
        }
    }

    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group_mut();
        if index >= group.buffers.len() {
            return;
        }
        let buffer = remove_tab(group, index);
        let active = group.active;
        let empty = group.buffers.is_empty();
        self.unwatch(&buffer);
        if empty {
            cx.notify();
        } else {
            self.activate(active, window, cx);
        }
    }

    fn unwatch(&mut self, buffer: &Buffer) {
        if buffer.is_watched()
            && let Some(watcher) = &mut self.watcher
        {
            watcher.remove(&buffer.path);
        }
    }

    /// Answer the active buffer's prompt. `choice` is the button pressed.
    pub fn answer(&mut self, choice: Answer, window: &mut Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.active_buffer_mut() else {
            return;
        };
        match (buffer.prompt.take(), choice) {
            (Some(Prompt::Conflict { theirs }), Answer::KeepMine) => buffer.keep_mine(theirs),
            (Some(Prompt::Conflict { theirs }), Answer::TakeTheirs) => {
                buffer.take(theirs, window, cx)
            }
            (Some(Prompt::CloseUnsaved), Answer::Save) => {
                let state = buffer.state.clone();
                self.start_save(state, true, window, cx);
            }
            (Some(Prompt::CloseUnsaved), Answer::Discard) => {
                if let Some(index) = self.group().map(|group| group.active) {
                    self.close(index, window, cx);
                }
                return;
            }
            (prompt, _) => buffer.prompt = prompt,
        }
        if let Some(buffer) = self.active_buffer_mut() {
            buffer.refresh_dirty(cx);
        }
        cx.notify();
    }

    /// Above an HTML tab: Preview / Source, and the page's actions.
    fn render_page_bar(&self, buffer: &Buffer, page: &Page, cx: &mut Context<Self>) -> AnyElement {
        let path = buffer.path.clone();
        h_flex()
            .flex_shrink_0()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                ButtonGroup::new("page-mode")
                    .outline()
                    .xsmall()
                    .child(Button::new("page").label("Preview").selected(page.shown))
                    .child(Button::new("source").label("Source").selected(!page.shown))
                    .on_click(cx.listener(|this, clicked: &Vec<usize>, window, cx| {
                        this.show_page(clicked.first() == Some(&0), window, cx);
                    })),
            )
            .child(div().flex_1())
            .when(page.view().is_some() && page.shown, |this| {
                this.child(
                    Button::new("reload-page")
                        .ghost()
                        .xsmall()
                        .label("Reload")
                        .on_click(cx.listener(|this, _, _, _| {
                            crate::sounds::play(cues::Cue::Tap, crate::sounds::subtle());
                            if let Some(buffer) = this.group().and_then(Group::active) {
                                buffer.load_page();
                            }
                        })),
                )
            })
            .when(buffer.machine.is_local(), |this| {
                this.child(
                    Button::new("open-in-browser")
                        .ghost()
                        .xsmall()
                        .label("Open in Browser")
                        .on_click(move |_, _, cx| {
                            crate::sounds::play(cues::Cue::Tap, crate::sounds::subtle());
                            cx.open_with_system(&path);
                        }),
                )
            })
            .into_any_element()
    }

    fn render_prompt(&self, prompt: &Prompt, cx: &mut Context<Self>) -> impl IntoElement {
        let (message, buttons): (&str, &[(Answer, &str)]) = match prompt {
            Prompt::Conflict { .. } => (
                "This file changed on disk and you have unsaved edits.",
                &[
                    (Answer::KeepMine, "Keep mine"),
                    (Answer::TakeTheirs, "Take theirs"),
                ],
            ),
            Prompt::CloseUnsaved => (
                "Save changes before closing?",
                &[
                    (Answer::Save, "Save"),
                    (Answer::Discard, "Don't save"),
                    (Answer::Cancel, "Cancel"),
                ],
            ),
        };
        h_flex()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .bg(cx.theme().warning.opacity(0.15))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().text_sm().child(message))
            .children(buttons.iter().enumerate().map(|(index, &(answer, label))| {
                let button = Button::new(("prompt", index)).small().label(label);
                let button = if index == 0 { button.primary() } else { button };
                button.on_click(
                    cx.listener(move |this, _, window, cx| this.answer(answer, window, cx)),
                )
            }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    KeepMine,
    TakeTheirs,
    Save,
    Discard,
    Cancel,
}

/// Takes the disk's news into the buffer: reload, ask, or mark deleted.
fn apply_change(buffer: &mut Buffer, change: DiskChange, window: &mut Window, cx: &mut App) {
    match change {
        DiskChange::Unchanged => {}
        DiskChange::Reload(text) => buffer.take(text, window, cx),
        DiskChange::Conflict(theirs) => buffer.prompt = Some(Prompt::Conflict { theirs }),
        DiskChange::Deleted => buffer.mark_deleted(),
    }
}

/// Removes tab `index` from `group`, keeping a valid selection.
fn remove_tab(group: &mut Group, index: usize) -> Buffer {
    let buffer = group.buffers.remove(index);
    if index < group.active || group.active >= group.buffers.len() {
        group.active = group.active.saturating_sub(1);
    }
    buffer
}

fn notify_error(message: String, window: &mut Window, cx: &mut App) {
    tracing::error!("editor: {message}");
    crate::sounds::play(cues::Cue::Error, crate::sounds::subtle());
    // In the overlay, so a shown page doesn't cover it.
    crate::chrome::floating_in(window.window_handle(), cx, move |window, cx| {
        window.push_notification(Notification::error(message), cx)
    });
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .id("editor")
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close_active))
            .size_full()
            .overflow_hidden();
        // No empty state: with no files the workspace shows herdr instead.
        let Some(buffer) = self.group().and_then(Group::active) else {
            return root;
        };
        let body = match buffer.shown_page() {
            Some(page) => div()
                .size_full()
                .children(page.view().cloned().map(|view| {
                    canvas(move |bounds, _, _| view.set_frame(bounds), |_, _, _, _| {}).size_full()
                }))
                .into_any_element(),
            None => CodeEditor::new(&buffer.state)
                .bordered(false)
                .h(relative(1.))
                .into_any_element(),
        };
        let toolbar = buffer.page.as_ref().map(|page| self.render_page_bar(buffer, page, cx));
        root.track_focus(&self.focus)
            .when_some(buffer.prompt.as_ref(), |this, prompt| {
                this.child(self.render_prompt(prompt, cx))
            })
            .children(toolbar)
            .child(div().flex_1().min_h_0().child(body))
    }
}
