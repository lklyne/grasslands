//! The file tree, rooted at the active worktree on its machine (through
//! `crate::fs::Fs`), with .gitignore respected.
//!
//! Folders are listed lazily, one level when first expanded, on the
//! background executor. On Local a recursive watcher on the root re-lists any
//! loaded folder whose contents change, so files agents create, edit and
//! delete show up live; a saved machine has no watcher, so its shown folders
//! and footer are re-read every few seconds while the window is active, and
//! at once when it becomes active. Clicking a file sends `OpenFile`; the
//! context menu creates, renames, removes (Trash on Local, a confirmed
//! permanent delete elsewhere) and reveals. Rows drag onto folders to move them. The
//! search box above swaps the tree for name and content matches. A footer
//! shows the branch, its parent and the lines changed since (see
//! `crate::changes`); clicking it opens the Changes tab. Rows changed on the
//! branch, committed or not, are colored: added green, modified yellow, and
//! folders holding a change yellow.

mod model;
mod watch;

#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tree::{TreeEntry, TreeEvent, TreeState, tree};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::model::FileModel;
use self::watch::RootWatcher;
use crate::actions::{OpenChanges, OpenFile, OpenFileAt};
use crate::chrome::{self, Confirmation, MenuCommand, MenuEntry};
use crate::fs::{Caps, Fs, Removal, git, search};
use crate::state::{ActiveRoot, AppEvent, AppState, MachineId};

/// Watcher events arriving within this window are folded into one re-list.
const FS_DEBOUNCE: Duration = Duration::from_millis(75);
/// Typing within this window runs one search.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
/// The git footer re-reads at most this often, and at least this often too
/// (5x), for commits in a linked worktree's git dir the watcher can't see.
const GIT_THROTTLE: Duration = Duration::from_secs(1);
const GIT_REFRESH: Duration = Duration::from_secs(5);
/// How often a saved machine's shown folders and footer are re-read.
const REMOTE_POLL: Duration = Duration::from_secs(3);
const INDENT: f32 = 12.;
const ROW_HEIGHT: Pixels = px(24.);
/// The chevron's column; files leave it empty so names line up.
const CHEVRON: Pixels = px(12.);

/// A deferred operation on the tree, run from a menu item or prompt.
type TreeOp = Rc<dyn Fn(&mut FileTree, &mut Window, &mut Context<FileTree>)>;

pub fn init(_cx: &mut App) {}

pub struct FileTree {
    state: Entity<AppState>,
    tree: Entity<TreeState>,
    model: Option<FileModel>,
    /// The machine the root is on.
    machine: MachineId,
    /// Why a saved machine's root can't be listed, shown in place of the tree.
    root_error: Option<String>,
    watcher: Option<RootWatcher>,
    fs_changes: async_channel::Sender<Vec<PathBuf>>,
    focus: FocusHandle,
    query: Entity<InputState>,
    /// Matches for the query; `None` while it is empty, which shows the tree.
    results: Option<search::Results>,
    searching: Option<(Arc<AtomicBool>, Task<()>)>,
    /// A search's command is running (see `run_search`).
    search_running: bool,
    /// The latest query, waiting for the running search to end.
    search_pending: Option<SearchJob>,
    git: Option<git::Summary>,
    /// A read in flight, then the throttle pause after it.
    git_reading: Option<Task<()>>,
    /// Something changed while `git_reading`; read again after it.
    git_stale: bool,
    /// A saved machine's poll of the shown folders still out; the next
    /// waits for it, so a stalled link doesn't pile them up.
    polling: Option<Task<()>>,
    _subscriptions: [Subscription; 4],
    _fs_task: Task<()>,
    _git_timer: Task<()>,
    _remote_poll: Task<()>,
}

/// Where a tree action runs: the root and machine it was chosen on, with
/// that machine's files. Captured when the user picks the action, so a root
/// that moves meanwhile (an agent `cd`s, focus goes to another machine)
/// can't redirect it; `run_fs` refuses once the tree has left it.
#[derive(Clone)]
struct Scope {
    machine: MachineId,
    root: PathBuf,
    fs: Fs,
}

/// One query to run against a root.
struct SearchJob {
    root: PathBuf,
    fs: Fs,
    query: String,
    cancel: Arc<AtomicBool>,
}

/// A file or folder being dragged to another folder.
#[derive(Clone)]
pub struct DraggedPath {
    pub path: PathBuf,
    name: SharedString,
    /// Where the drag started; `None` if that machine was offline.
    scope: Option<Scope>,
}

impl Render for DraggedPath {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.name.clone())
    }
}

impl FileTree {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search files"));
        let subscriptions = [
            cx.subscribe(&state, |this, state, event, cx| match event {
                AppEvent::ActiveRootChanged(root) => this.set_root(root.clone(), cx),
                // The remote placeholder names the selected machine.
                AppEvent::MachineSelected(_) => cx.notify(),
                AppEvent::MachinesChanged if !state.read(cx).selected().is_local() => {
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe(&tree, |this, _, event, cx| this.on_tree_event(event, cx)),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.poll(cx);
                }
            }),
            cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search(cx);
                }
            }),
        ];
        let (fs_changes, receiver) = async_channel::unbounded::<Vec<PathBuf>>();
        let fs_task = cx.spawn(async move |this, cx| {
            while let Ok(mut paths) = receiver.recv().await {
                cx.background_executor().timer(FS_DEBOUNCE).await;
                while let Ok(more) = receiver.try_recv() {
                    paths.extend(more);
                }
                if this
                    .update(cx, |this, cx| this.on_fs_changes(&paths, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let git_timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(GIT_REFRESH).await;
                // Polled file systems refresh with their poll.
                let refreshed = this.update(cx, |this, cx| {
                    if Caps::of(&this.machine).watch {
                        this.refresh_git(cx);
                    }
                });
                if refreshed.is_err() {
                    break;
                }
            }
        });
        let remote_poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(REMOTE_POLL).await;
                let polled = this.update_in(cx, |this, window, cx| {
                    if window.is_window_active() {
                        this.poll(cx);
                    }
                });
                if polled.is_err() {
                    break;
                }
            }
        });
        let root = state.read(cx).active_root(cx);
        let mut this = Self {
            state,
            tree,
            model: None,
            machine: MachineId::Local,
            root_error: None,
            watcher: None,
            fs_changes,
            focus: cx.focus_handle(),
            query,
            results: None,
            searching: None,
            search_running: false,
            search_pending: None,
            git: None,
            git_reading: None,
            git_stale: false,
            polling: None,
            _subscriptions: subscriptions,
            _fs_task: fs_task,
            _git_timer: git_timer,
            _remote_poll: remote_poll,
        };
        this.set_root(root, cx);
        this
    }

    pub fn root(&self) -> Option<&Path> {
        self.model.as_ref().map(FileModel::root)
    }

    // Script hooks (`RT_SCRIPT=remote-files`).

    /// The names listed in `dir`, once it's loaded.
    pub fn listed(&self, dir: &Path) -> Option<Vec<String>> {
        let entries = self.model.as_ref()?.listing(dir)?;
        Some(entries.iter().map(|e| e.name.clone()).collect())
    }

    /// What the footer shows.
    pub fn git_summary(&self) -> Option<&git::Summary> {
        self.git.as_ref()
    }

    /// Why the root can't be listed, as shown in place of the tree.
    pub fn root_error(&self) -> Option<&str> {
        self.root_error.as_deref()
    }

    /// The search box's matches; `None` while it's empty.
    pub fn search_results(&self) -> Option<&search::Results> {
        self.results.as_ref()
    }

    /// Expands `dir`, as clicking its chevron does.
    pub fn expand(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let Some(model) = self.model.as_mut() else {
            return;
        };
        model.set_expanded(dir, true);
        if !model.is_loaded(dir) {
            self.load(vec![dir.to_path_buf()], cx);
        }
        self.rebuild(cx);
    }

    /// Types `query` into the search box.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        self.search(cx);
    }

    /// The context menu's New File, Rename and Delete, past their prompts.
    pub fn create_file(
        &mut self,
        dir: PathBuf,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.scope(cx) {
            Ok(scope) => self.create(&scope, dir, name, false, window, cx),
            Err(message) => error(message, window, cx),
        }
    }

    pub fn rename_to(
        &mut self,
        path: &Path,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.scope(cx) {
            Ok(scope) => self.rename(&scope, path, name, window, cx),
            Err(message) => error(message, window, cx),
        }
    }

    pub fn ask_to_remove(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        match self.scope(cx) {
            Ok(scope) => self.ask_remove(&scope, path, window, cx),
            Err(message) => error(message, window, cx),
        }
    }

    /// The root's files; `None` while its machine is offline.
    fn fs(&self, cx: &App) -> Option<Fs> {
        self.state.read(cx).fs_for(&self.machine, cx)
    }

    /// The root as it is now, for an action being chosen; the error says why
    /// there's nothing to act on.
    fn scope(&self, cx: &App) -> Result<Scope, String> {
        let root = self.root().ok_or("No worktree")?.to_path_buf();
        let fs = self
            .fs(cx)
            .ok_or_else(|| format!("{} is offline", self.machine_label(&self.machine, cx)))?;
        Ok(Scope {
            machine: self.machine.clone(),
            root,
            fs,
        })
    }

    fn machine_label(&self, machine: &MachineId, cx: &App) -> String {
        self.state
            .read(cx)
            .machine(machine, cx)
            .map_or_else(|| machine.to_string(), |m| m.label().to_owned())
    }

    fn set_root(&mut self, root: Option<ActiveRoot>, cx: &mut Context<Self>) {
        let same_machine = root.as_ref().is_none_or(|r| r.machine == self.machine);
        if same_machine && self.root() == root.as_ref().map(|r| r.path.as_path()) {
            return;
        }
        self.watcher = None;
        let root = root.map(|root| {
            self.machine = root.machine;
            root.path
        });
        self.model = root.map(FileModel::new);
        self.root_error = None;
        self.polling = None;
        if let Some(root) = self.root().map(Path::to_path_buf) {
            // GPUI's test scheduler rejects wakeups from the watcher's thread;
            // tests call `on_fs_changes` directly and test the watcher alone.
            // Saved machines are polled instead.
            if !cfg!(test) && Caps::of(&self.machine).watch {
                self.watcher = RootWatcher::new(&root, self.fs_changes.clone())
                    .inspect_err(|error| {
                        log::warn!("files: not watching {}: {error}", root.display());
                    })
                    .ok();
            }
            self.load(vec![root], cx);
        }
        self.rebuild(cx);
        self.search(cx);
        self.git = None;
        self.refresh_git(cx);
    }

    /// Re-reads a saved machine's shown folders and footer, which nothing
    /// watches. Local's watcher covers it there.
    fn poll(&mut self, cx: &mut Context<Self>) {
        if Caps::of(&self.machine).watch {
            return;
        }
        let Some(model) = &self.model else { return };
        if self.polling.is_none()
            && let Some(listing) = self.list(model.shown_dirs(), cx)
        {
            self.polling = Some(cx.spawn(async move |this, cx| {
                listing.await;
                let _ = this.update(cx, |this, _| this.polling = None);
            }));
        }
        self.refresh_git(cx);
    }

    /// Whether `root` on `machine` is still the tree's root.
    fn is_root(&self, machine: &MachineId, root: &Path) -> bool {
        &self.machine == machine && self.root() == Some(root)
    }

    /// Re-reads the git footer on the background executor, at most once per
    /// `GIT_THROTTLE`.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git_reading.is_some() {
            self.git_stale = true;
            return;
        }
        let (Some(root), Some(fs)) = (self.root().map(Path::to_path_buf), self.fs(cx)) else {
            return;
        };
        let machine = self.machine.clone();
        self.git_reading = Some(cx.spawn(async move |this, cx| {
            let summary = cx
                .background_executor()
                .spawn({
                    let root = root.clone();
                    async move { fs.git_summary(&root) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A read for a root we have since left, or one that failed
                // (the machine dropped): keep what's shown.
                let Ok(summary) = summary else { return };
                if this.is_root(&machine, &root) && this.git != summary {
                    this.git = summary;
                    cx.notify();
                }
            });
            cx.background_executor().timer(GIT_THROTTLE).await;
            let _ = this.update(cx, |this, cx| {
                this.git_reading = None;
                if std::mem::take(&mut this.git_stale) {
                    this.refresh_git(cx);
                }
            });
        }));
    }

    /// Runs the query against the root after a pause in typing, replacing
    /// any search still running.
    fn search(&mut self, cx: &mut Context<Self>) {
        if let Some((cancel, _)) = self.searching.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.search_pending = None;
        let query = self.query.read(cx).value().trim().to_owned();
        let (Some(root), Some(fs)) = (
            self.root()
                .map(Path::to_path_buf)
                .filter(|_| !query.is_empty()),
            self.fs(cx),
        ) else {
            self.results = None;
            cx.notify();
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let job = SearchJob {
            root,
            fs,
            query,
            cancel: cancel.clone(),
        };
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.run_search(job, cx));
        });
        self.searching = Some((cancel, task));
    }

    /// Runs `job` off the main thread, or keeps it for when the search
    /// still running ends: a saved machine's search can't be stopped
    /// part way, so one runs at a time and only the latest query waits.
    fn run_search(&mut self, job: SearchJob, cx: &mut Context<Self>) {
        if self.search_running {
            self.search_pending = Some(job);
            return;
        }
        self.search_running = true;
        let cancel = job.cancel.clone();
        let results = cx.background_executor().spawn(async move {
            let results = job.fs.search(&job.root, &job.query, &job.cancel);
            (!job.cancel.load(Ordering::Relaxed)).then_some(results)
        });
        // Detached, so `search_running` is cleared even when a newer query
        // replaces `searching`.
        cx.spawn(async move |this, cx| {
            let results = results.await;
            let _ = this.update(cx, |this, cx| this.search_done(&cancel, results, cx));
        })
        .detach();
    }

    /// The search for `cancel`'s query ended; `None` if it was cancelled.
    fn search_done(
        &mut self,
        cancel: &Arc<AtomicBool>,
        results: Option<search::Results>,
        cx: &mut Context<Self>,
    ) {
        self.search_running = false;
        if let Some(results) = results {
            self.results = Some(results);
            if self
                .searching
                .as_ref()
                .is_some_and(|(current, _)| Arc::ptr_eq(current, cancel))
            {
                self.searching = None;
            }
            cx.notify();
        }
        if let Some(job) = self.search_pending.take() {
            self.run_search(job, cx);
        }
    }

    /// Lists `dirs` on the background executor, then updates the tree.
    fn load(&self, dirs: Vec<PathBuf>, cx: &mut Context<Self>) {
        if let Some(task) = self.list(dirs, cx) {
            task.detach();
        }
    }

    /// `load`, as a task that ends once the tree is updated; `None` if
    /// there's nothing to list.
    fn list(&self, dirs: Vec<PathBuf>, cx: &mut Context<Self>) -> Option<Task<()>> {
        let (Some(root), Some(fs)) = (self.root().map(Path::to_path_buf), self.fs(cx)) else {
            return None;
        };
        if dirs.is_empty() {
            return None;
        }
        let machine = self.machine.clone();
        Some(cx.spawn(async move |this, cx| {
            let listings = cx
                .background_executor()
                .spawn(async move {
                    let listings = fs.read_dirs(&dirs);
                    dirs.into_iter().zip(listings).collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Results for a root we have since left.
                if this.is_root(&machine, &root) {
                    this.apply_listings(&root, listings, cx);
                }
            });
        }))
    }

    fn apply_listings(
        &mut self,
        root: &Path,
        listings: Vec<(PathBuf, io::Result<Vec<model::Entry>>)>,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.model.as_mut() else {
            return;
        };
        let mut failed = None;
        for (dir, listing) in listings {
            if dir == root && !self.machine.is_local() {
                self.root_error = listing
                    .as_ref()
                    .err()
                    .map(|error| format!("Can't read {}: {error}", root.display()));
            }
            match listing {
                Ok(entries) => model.set_listing(dir, entries),
                // A dropped connection isn't an empty folder: keep what's shown.
                Err(error) if is_connection_error(&error) => failed = Some(error),
                Err(_) if dir == root => model.set_listing(dir, Vec::new()),
                Err(_) => model.forget(&dir),
            }
        }
        if let Some(error) = failed {
            tracing::warn!("files: listing on {}: {error}", self.machine);
        }
        self.rebuild(cx);
    }

    /// Rebuilds the Kit tree from the model, keeping the selection.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let items = self
            .model
            .as_ref()
            .map(FileModel::items)
            .unwrap_or_default();
        self.tree.update(cx, |tree, cx| {
            let selected = tree.selected_item().map(|item| item.id.clone());
            tree.set_items(items, cx);
            if let Some(id) = selected {
                let ix = tree.index_of(&id);
                tree.set_selected_index(ix, cx);
            }
        });
        cx.notify();
    }

    fn on_tree_event(&mut self, event: &TreeEvent, cx: &mut Context<Self>) {
        let Some(model) = self.model.as_mut() else {
            return;
        };
        match event {
            TreeEvent::Expanded(id) => {
                let dir = PathBuf::from(id.as_ref());
                model.set_expanded(&dir, true);
                if !model.is_loaded(&dir) {
                    self.load(vec![dir], cx);
                }
            }
            TreeEvent::Collapsed(id) => model.set_expanded(Path::new(id.as_ref()), false),
        }
    }

    fn on_fs_changes(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let Some(model) = &self.model else { return };
        let dirs = model.dirs_to_rescan(paths);
        self.load(dirs.into_iter().collect(), cx);
        if self.results.is_some() {
            self.search(cx);
        }
        self.refresh_git(cx);
    }

    // File operations. Each runs on the background executor against the
    // root's file system, then re-lists the affected folder right away
    // rather than waiting for the watcher or the poll.

    /// Runs `op` on `scope`'s files off the main thread, then `done` here;
    /// a failure toasts `failed`'s wording. Refuses if the tree has left
    /// `scope` since the action was chosen.
    fn run_fs<T: Send + 'static>(
        &self,
        scope: &Scope,
        op: impl FnOnce(&Fs) -> io::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        failed: impl FnOnce(io::Error) -> String + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_root(&scope.machine, &scope.root) {
            return error(
                format!(
                    "The file tree moved off {}; nothing was changed",
                    scope.root.display()
                ),
                window,
                cx,
            );
        }
        let fs = scope.fs.clone();
        let task = cx.background_executor().spawn(async move { op(&fs) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(value) => done(this, value, window, cx),
                Err(err) => error(failed(err), window, cx),
            })
            .ok();
        })
        .detach();
    }

    fn create(
        &mut self,
        scope: &Scope,
        dir: PathBuf,
        name: &str,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = match valid_name(name) {
            Ok(name) => dir.join(name),
            Err(message) => return error(message, window, cx),
        };
        let shown = path.display().to_string();
        let (here, machine) = (scope.clone(), scope.machine.clone());
        self.run_fs(
            scope,
            {
                let path = path.clone();
                move |fs| {
                    if folder {
                        fs.create_dir(&path)
                    } else {
                        fs.create_file(&path)
                    }
                }
            },
            move |this, (), window, cx| {
                crate::sounds::play(cues::Cue::Tap, crate::sounds::subtle());
                if this.is_root(&here.machine, &here.root) {
                    if let Some(model) = this.model.as_mut()
                        && model.root() != dir
                    {
                        model.set_expanded(&dir, true);
                    }
                    this.load(vec![dir], cx);
                }
                if folder {
                    return;
                }
                // Called from the name prompt, which lives in the Kit's dialog
                // layer, outside `Workspace`: an action dispatched there never
                // reaches the editor. Focus the tree (inside `Workspace`) and
                // dispatch from there.
                cx.defer_in(window, move |this, window, cx| {
                    this.tree.update(cx, |tree, cx| tree.focus(window, cx));
                    window.dispatch_action(Box::new(OpenFile { path, machine }), cx);
                });
            },
            move |err| format!("Could not create {shown}: {err}"),
            window,
            cx,
        );
    }

    fn rename(
        &mut self,
        scope: &Scope,
        path: &Path,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = match valid_name(name) {
            Ok(name) => path.with_file_name(name),
            Err(message) => return error(message, window, cx),
        };
        if target == path {
            return;
        }
        let from = path.to_path_buf();
        let shown = (from.display().to_string(), target.display().to_string());
        let here = scope.clone();
        self.run_fs(
            scope,
            {
                let (from, target) = (from.clone(), target.clone());
                move |fs| fs.rename(&from, &target)
            },
            move |this, (), _, cx| {
                crate::sounds::play(cues::Cue::Tap, crate::sounds::subtle());
                let parent = from.parent().map(Path::to_path_buf);
                this.moved(&here, from, target, cx);
                this.reload_in(&here, parent.into_iter().collect(), cx);
            },
            move |err| move_error("rename", &shown.0, &shown.1, &err),
            window,
            cx,
        );
    }

    /// Moves `path` into `dir`, keeping its name.
    fn move_into(
        &mut self,
        scope: &Scope,
        path: &Path,
        dir: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = path.file_name() else { return };
        if path.parent() == Some(dir) {
            return;
        }
        if dir.starts_with(path) {
            return error(
                format!("Cannot move {} into itself", path.display()),
                window,
                cx,
            );
        }
        let (from, dir) = (path.to_path_buf(), dir.to_path_buf());
        let target = dir.join(name);
        let shown = (from.display().to_string(), target.display().to_string());
        let here = scope.clone();
        self.run_fs(
            scope,
            {
                let (from, target) = (from.clone(), target.clone());
                move |fs| fs.rename(&from, &target)
            },
            move |this, (), _, cx| {
                crate::sounds::play(cues::Cue::Tap, crate::sounds::strong());
                let dirs: Vec<PathBuf> = from
                    .parent()
                    .into_iter()
                    .chain([dir.as_path()])
                    .map(Path::to_path_buf)
                    .collect();
                this.moved(&here, from, target, cx);
                if this.is_root(&here.machine, &here.root)
                    && let Some(model) = this.model.as_mut()
                    && model.root() != dir
                {
                    model.set_expanded(&dir, true);
                }
                this.reload_in(&here, dirs, cx);
            },
            move |err| move_error("move", &shown.0, &shown.1, &err),
            window,
            cx,
        );
    }

    /// Moves a dragged row into `dir`, on the machine the drag started on.
    fn drop_into(
        &mut self,
        dragged: &DraggedPath,
        dir: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = &dragged.scope else {
            return error("That machine is offline", window, cx);
        };
        self.move_into(scope, &dragged.path, dir, window, cx);
    }

    /// Tells open editor tabs on `scope`'s machine to follow a move.
    fn moved(&self, scope: &Scope, from: PathBuf, to: PathBuf, cx: &mut Context<Self>) {
        let machine = scope.machine.clone();
        self.state
            .update(cx, |state, cx| state.file_moved(machine, from, to, cx));
    }

    /// Re-lists `dirs` after an operation in `scope`, unless the tree has
    /// left it meanwhile.
    fn reload_in(&self, scope: &Scope, dirs: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.is_root(&scope.machine, &scope.root) {
            self.load(dirs, cx);
        }
    }

    /// With a Trash, removes at once, since a slip is recoverable; without
    /// one (a saved machine), asks first.
    fn ask_remove(
        &mut self,
        scope: &Scope,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if scope.fs.caps().trash {
            self.remove(scope, path, window, cx);
        } else {
            self.confirm_remove(scope, path, window, cx);
        }
    }

    fn remove(&mut self, scope: &Scope, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let removed = path.to_path_buf();
        let shown = removed.display().to_string();
        let trash = scope.fs.caps().trash;
        let here = scope.clone();
        self.run_fs(
            scope,
            {
                let path = removed.clone();
                move |fs| fs.remove(&path)
            },
            move |this, _: Removal, _, cx| {
                crate::sounds::play(cues::Cue::Close, crate::sounds::strong());
                let parent = removed.parent().map(Path::to_path_buf);
                let machine = here.machine.clone();
                this.state
                    .update(cx, |state, cx| state.file_removed(machine, removed, cx));
                this.reload_in(&here, parent.into_iter().collect(), cx);
            },
            move |err| {
                if trash {
                    format!("Could not move {shown} to the Trash: {err}")
                } else {
                    format!("Could not delete {shown}: {err}")
                }
            },
            window,
            cx,
        );
    }

    /// Asks before deleting on a saved machine, where it is permanent.
    fn confirm_remove(
        &mut self,
        scope: &Scope,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chrome) = crate::chrome::get(window, cx) else {
            tracing::warn!("files: no chrome to confirm deleting {}", path.display());
            return;
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let label = self.machine_label(&scope.machine, cx);
        let confirmation = Confirmation {
            title: "Delete permanently?".into(),
            message: format!(
                "{name} is deleted from {label} for good. There's no Trash to undo it."
            ),
            confirm_label: "Delete".into(),
        };
        let (this, main, path, scope) = (
            cx.entity().downgrade(),
            window.window_handle(),
            path.to_path_buf(),
            scope.clone(),
        );
        // The dialog calls back in the overlay; delete from the main window,
        // where failures toast.
        let on_confirm = move |_: &mut Window, cx: &mut App| {
            let (this, path, scope) = (this.clone(), path.clone(), scope.clone());
            cx.defer(move |cx| {
                cx.update_window(main, |_, window, cx| {
                    this.update(cx, |this, cx| this.remove(&scope, &path, window, cx))
                        .ok();
                })
                .ok();
            });
        };
        chrome.update(cx, |chrome, cx| {
            chrome.confirm(confirmation, on_confirm, window, cx)
        });
    }

    fn reveal(path: &Path) {
        if let Err(err) = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
        {
            log::warn!("files: reveal {}: {err}", path.display());
        }
    }

    /// Asks for a name (see `crate::prompt`); `submit` runs on OK or Enter.
    fn prompt(
        &mut self,
        title: &'static str,
        ok_label: &'static str,
        initial: &str,
        submit: impl Fn(&mut Self, &str, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        crate::prompt::ask_name(
            title,
            ok_label,
            initial,
            move |value, window, cx| {
                let _ = this.update(cx, |this, cx| submit(this, value, window, cx));
            },
            window,
            cx,
        );
    }

    /// The panel's title-bar menu: new entries in the selected folder (as
    /// the toolbar buttons).
    fn panel_entries(this: &WeakEntity<Self>) -> Vec<MenuEntry> {
        let item = |label: &'static str, folder: bool| {
            let this = this.clone();
            MenuEntry::item(
                label,
                MenuCommand::Callback(Rc::new(move |window, cx| {
                    let _ = this.update(cx, |this, cx| this.new_in_selected(folder, window, cx));
                })),
            )
        };
        vec![item("New File…", false), item("New Folder…", true)]
    }

    /// A row's context menu, drawn by the chrome in the overlay so it stays
    /// above the terminals. `scope` is the root as the menu opened; every
    /// item acts there. Placeholder rows get none.
    fn context_entries(
        this: &WeakEntity<Self>,
        scope: Option<&Scope>,
        id: &str,
        is_folder: bool,
        name: &str,
    ) -> Vec<MenuEntry> {
        let Some(scope) = scope.filter(|_| !model::is_placeholder(id)) else {
            return Vec::new();
        };
        let trash = scope.fs.caps().trash;
        let path = PathBuf::from(id);
        let dir = if is_folder {
            path.clone()
        } else {
            path.parent()
                .map_or_else(|| path.clone(), Path::to_path_buf)
        };
        let name = name.to_owned();
        let item = |label: &'static str, run: TreeOp| {
            let this = this.clone();
            MenuEntry::item(
                label,
                MenuCommand::Callback(Rc::new(move |window, cx| {
                    let _ = this.update(cx, |this, cx| run(this, window, cx));
                })),
            )
        };
        let new_in = |folder: bool| {
            let (dir, scope) = (dir.clone(), scope.clone());
            Rc::new(
                move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let (dir, scope) = (dir.clone(), scope.clone());
                    let title = if folder { "New Folder" } else { "New File" };
                    this.prompt(
                        title,
                        "Create",
                        "",
                        move |this, name, window, cx| {
                            this.create(&scope, dir.clone(), name, folder, window, cx)
                        },
                        window,
                        cx,
                    );
                },
            ) as TreeOp
        };
        let rename = {
            let (path, scope) = (path.clone(), scope.clone());
            Rc::new(
                move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let (path, scope) = (path.clone(), scope.clone());
                    this.prompt(
                        "Rename",
                        "Rename",
                        &name,
                        move |this, name, window, cx| this.rename(&scope, &path, name, window, cx),
                        window,
                        cx,
                    );
                },
            )
        };
        let remove = {
            let (path, scope) = (path.clone(), scope.clone());
            Rc::new(
                move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.ask_remove(&scope, &path, window, cx)
                },
            )
        };
        let reveal = {
            let path = path.clone();
            Rc::new(move |_: &mut Self, _: &mut Window, _: &mut Context<Self>| Self::reveal(&path))
        };
        let copy_path = Rc::new(
            move |_: &mut Self, _: &mut Window, cx: &mut Context<Self>| {
                cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
            },
        );
        let mut entries = vec![
            item("New File…", new_in(false)),
            item("New Folder…", new_in(true)),
            MenuEntry::Separator,
            item("Rename…", rename),
            item(if trash { "Move to Trash" } else { "Delete…" }, remove),
            MenuEntry::Separator,
        ];
        // Finder shows this Mac's files only.
        if scope.machine.is_local() {
            entries.push(item("Reveal in Finder", reveal));
        }
        entries.push(item("Copy Path", copy_path));
        entries
    }

    /// Asks for a name, then creates a file (or folder) in the selected
    /// folder, the selected file's folder, or the root.
    fn new_in_selected(&mut self, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let scope = match self.scope(cx) {
            Ok(scope) => scope,
            Err(message) => return error(message, window, cx),
        };
        let root = scope.root.clone();
        let selected = self.tree.read(cx).selected_entry().and_then(|entry| {
            let id = entry.item().id.as_ref();
            if model::is_placeholder(id) {
                return None;
            }
            let path = PathBuf::from(id);
            if entry.is_folder() {
                Some(path)
            } else {
                path.parent().map(Path::to_path_buf)
            }
        });
        let dir = selected
            .filter(|dir| dir.starts_with(&root))
            .unwrap_or(root);
        let title = if folder { "New Folder" } else { "New File" };
        self.prompt(
            title,
            "Create",
            "",
            move |this, name, window, cx| {
                this.create(&scope, dir.clone(), name, folder, window, cx)
            },
            window,
            cx,
        );
    }
}

/// One compact row: a chevron on folders, the icon, then the name. Rows drag;
/// dropping on a folder moves into it, on a file into the file's folder.
fn render_row(
    ix: usize,
    entry: &TreeEntry,
    this: &WeakEntity<FileTree>,
    scope: Option<&Scope>,
    machine: &MachineId,
    git: Option<&git::Summary>,
    cx: &App,
) -> ListItem {
    let item = entry.item();
    let indent = px(6. + INDENT * entry.depth() as f32);
    let muted = cx.theme().muted_foreground;
    let row = ListItem::new(ix)
        .h(ROW_HEIGHT)
        .py_0()
        .pl(indent)
        .pr_2()
        .rounded_sm()
        .text_sm();
    if model::is_placeholder(&item.id) {
        return row.child(
            div()
                .pl(CHEVRON + px(4.))
                .text_color(muted)
                .child(item.label.clone()),
        );
    }
    let chevron = entry.is_folder().then(|| {
        let icon = if entry.is_expanded() {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        Icon::new(icon).xsmall().text_color(muted)
    });
    let icon = match (entry.is_folder(), entry.is_expanded()) {
        (true, true) => IconName::FolderOpen,
        (true, false) => IconName::Folder,
        (false, _) => IconName::File,
    };
    let path = PathBuf::from(item.id.as_ref());
    let changed = git.and_then(|git| {
        if entry.is_folder() {
            git.folders
                .contains(&path)
                .then_some(git::FileChange::Modified)
        } else {
            git.files.get(&path).copied()
        }
    });
    let label_color = match changed {
        Some(git::FileChange::Added) => Some(cx.theme().success),
        Some(git::FileChange::Modified) => Some(cx.theme().warning),
        None => None,
    };
    let drop_dir = if entry.is_folder() {
        path.clone()
    } else {
        path.parent()
            .map_or_else(|| path.clone(), Path::to_path_buf)
    };
    let drop_bg = cx.theme().drop_target;
    let row = row
        .on_mouse_down(MouseButton::Right, {
            let (this, scope) = (this.clone(), scope.cloned());
            let (id, is_folder, name) = (item.id.clone(), entry.is_folder(), item.label.clone());
            move |event, window, cx| {
                let entries =
                    FileTree::context_entries(&this, scope.as_ref(), &id, is_folder, &name);
                chrome::open_menu(entries, event.position, window, cx);
            }
        })
        .on_drag(
            DraggedPath {
                path: path.clone(),
                name: item.label.clone(),
                scope: scope.cloned(),
            },
            |dragged, _, _, cx| {
                crate::workspace::keep_terminals_during_drag(cx);
                cx.new(|_| dragged.clone())
            },
        )
        .drag_over::<DraggedPath>(move |style, _, _, _| style.bg(drop_bg))
        .on_drop({
            let this = this.clone();
            move |dragged: &DraggedPath, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.drop_into(dragged, &drop_dir, window, cx)
                });
            }
        });
    let row = row.child(
        h_flex()
            .gap_1()
            .min_w_0()
            .child(
                div()
                    .flex_shrink_0()
                    .w(CHEVRON)
                    .flex()
                    .justify_center()
                    .children(chevron),
            )
            .child(Icon::new(icon).small().text_color(muted))
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .when_some(label_color, |this, color| this.text_color(color))
                    .child(item.label.clone()),
            ),
    );
    if entry.is_folder() {
        return row;
    }
    let machine = machine.clone();
    row.on_click(move |_, window, cx| {
        window.dispatch_action(
            Box::new(OpenFile {
                path: path.clone(),
                machine: machine.clone(),
            }),
            cx,
        );
    })
}

/// The query's matches: file names first, then lines grouped by file.
fn render_results(results: &search::Results, machine: &MachineId, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let mark = HighlightStyle {
        color: Some(theme.foreground),
        font_weight: Some(FontWeight::BOLD),
        background_color: Some(theme.drop_target),
        ..HighlightStyle::default()
    };
    let label = |text: &str| {
        div()
            .px_2()
            .pt_2()
            .pb_0p5()
            .text_xs()
            .text_color(muted)
            .child(text.to_owned())
    };
    let row = |id: ElementId, depth: f32| {
        h_flex()
            .id(id)
            .h(ROW_HEIGHT)
            .pl(px(6. + INDENT * depth))
            .pr_2()
            .gap_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|this| this.bg(theme.tokens.list_hover))
            .whitespace_nowrap()
            .overflow_hidden()
    };
    let mut list = v_flex()
        .id("files-search-results")
        .flex_1()
        .min_h_0()
        .p_1()
        .overflow_y_scroll()
        .text_sm();
    if results.names.is_empty() && results.contents.is_empty() {
        list = list.child(label("No matches"));
    }
    if !results.names.is_empty() {
        list = list.child(label("Files"));
    }
    for (ix, hit) in results.names.iter().enumerate() {
        let path = hit.path.clone();
        let machine = machine.clone();
        list = list.child(
            row(
                ElementId::NamedInteger("files-name-hit".into(), ix as u64),
                0.,
            )
            .child(Icon::new(IconName::File).small().text_color(muted))
            .child(div().overflow_hidden().text_ellipsis().child(
                StyledText::new(hit.relative.clone()).with_highlights([(hit.range.clone(), mark)]),
            ))
            .on_click(move |_, window, cx| {
                window.dispatch_action(
                    Box::new(OpenFile {
                        path: path.clone(),
                        machine: machine.clone(),
                    }),
                    cx,
                );
            }),
        );
    }
    if !results.contents.is_empty() {
        list = list.child(label("In files"));
    }
    for (file_ix, file) in results.contents.iter().enumerate() {
        let path = file.path.clone();
        let file_machine = machine.clone();
        list = list.child(
            row(
                ElementId::NamedInteger("files-content-file".into(), file_ix as u64),
                0.,
            )
            .child(Icon::new(IconName::File).small().text_color(muted))
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(file.relative.clone()),
            )
            .on_click(move |_, window, cx| {
                window.dispatch_action(
                    Box::new(OpenFile {
                        path: path.clone(),
                        machine: file_machine.clone(),
                    }),
                    cx,
                );
            }),
        );
        for (line_ix, hit) in file.lines.iter().enumerate() {
            let path = file.path.clone();
            let machine = machine.clone();
            let line = hit.line;
            let id =
                ElementId::NamedInteger(format!("files-line-hit-{file_ix}").into(), line_ix as u64);
            list = list.child(
                row(id, 1.)
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(muted)
                            .child(format!("{}", line + 1)),
                    )
                    .child(
                        div().overflow_hidden().text_ellipsis().child(
                            StyledText::new(hit.text.clone())
                                .with_highlights([(hit.range.clone(), mark)]),
                        ),
                    )
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(OpenFileAt {
                                path: path.clone(),
                                line,
                                machine: machine.clone(),
                            }),
                            cx,
                        );
                    }),
            );
        }
    }
    if results.truncated {
        list = list.child(label(
            "Showing the first matches; refine the search for more",
        ));
    }
    list.into_any_element()
}

/// A single path component a user typed.
/// The footer: branch, parent branch, and lines added and removed.
fn render_git(git: &git::Summary, cx: &App) -> Stateful<Div> {
    let muted = cx.theme().muted_foreground;
    let branch = |icon: assets::IconName, name: &str| {
        h_flex()
            .gap_1()
            .min_w_0()
            .child(Icon::new(icon).xsmall().text_color(muted).flex_none())
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(name.to_owned()),
            )
    };
    h_flex()
        .id("files-git")
        .flex_none()
        .cursor_pointer()
        .hover(|style| style.bg(cx.theme().list_hover))
        .gap_3()
        .px_2p5()
        .py_1p5()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_xs()
        .text_color(muted)
        .child(branch(
            assets::IconName::GitBranch,
            git.branch.as_deref().unwrap_or("detached"),
        ))
        .children(
            git.parent
                .as_deref()
                .map(|parent| branch(assets::IconName::GitMerge, parent)),
        )
        .children(git.diff.map(|(added, removed)| {
            h_flex()
                .ml_auto()
                .flex_none()
                .gap_1p5()
                .child(
                    div()
                        .text_color(cx.theme().success)
                        .child(format!("+{added}")),
                )
                .child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(format!("-{removed}")),
                )
        }))
}

fn valid_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(format!("“{name}” is not a valid name"));
    }
    Ok(name)
}

/// A rename or move's failure, worded for the toast.
fn move_error(verb: &str, from: &str, to: &str, err: &io::Error) -> String {
    if err.kind() == io::ErrorKind::AlreadyExists {
        format!("{to} already exists")
    } else {
        format!("Could not {verb} {from}: {err}")
    }
}

/// The machine dropped off rather than the folder being unreadable.
fn is_connection_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::TimedOut
    )
}

fn error(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    crate::sounds::play(cues::Cue::Error, crate::sounds::subtle());
    window.push_notification(Notification::error(message), cx);
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let machine = self.state.read(cx).selected_machine(cx);
        if !machine.id().is_local() && !machine.status().is_usable() {
            let label = machine.label().to_owned();
            return div()
                .size_full()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(crate::machines::remote_files_empty(&label, cx))
                .into_any_element();
        }
        if self.root().is_none() {
            return v_flex()
                .size_full()
                .border_t_1()
                .border_color(cx.theme().border)
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No worktree")
                .into_any_element();
        };
        let this = cx.entity().downgrade();
        let search = div().p_1p5().child(
            Input::new(&self.query).cleanable(true).prefix(
                Icon::new(IconName::Search)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            ),
        );
        let body = match (&self.results, &self.root_error) {
            (_, Some(message)) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message.clone())
                .into_any_element(),
            (Some(results), None) => render_results(results, &self.machine, cx),
            (None, None) => {
                let rows = this.clone();
                let machine = self.machine.clone();
                let scope = self.scope(cx).ok();
                let git = self.git.clone();
                let root = self.root().map(Path::to_path_buf);
                let drop_bg = cx.theme().drop_target;
                // Drops that miss every row land in the root.
                div()
                    .flex_1()
                    .min_h_0()
                    .drag_over::<DraggedPath>(move |style, _, _, _| style.bg(drop_bg))
                    .on_drop(move |dragged: &DraggedPath, window, cx| {
                        let Some(root) = &root else { return };
                        let _ =
                            this.update(cx, |this, cx| this.drop_into(dragged, root, window, cx));
                    })
                    .child(
                        tree(&self.tree, move |ix, entry, _selected, _window, cx| {
                            render_row(ix, entry, &rows, scope.as_ref(), &machine, git.as_ref(), cx)
                        })
                        .size_full()
                        .p_1(),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .track_focus(&self.focus)
            .size_full()
            // Under the dock's title bar, like the main area's tab strip.
            .border_t_1()
            .border_color(cx.theme().border)
            .child(search)
            .child(body)
            .children(self.git.as_ref().map(|git| {
                render_git(git, cx).on_click(cx.listener(|this, _, window, cx| {
                    // Dispatch from inside `Workspace`, which opens the tab.
                    window.focus(&this.focus, cx);
                    window.dispatch_action(Box::new(OpenChanges), cx);
                }))
            }))
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for FileTree {
    fn panel_name(&self) -> &'static str {
        "FilesPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for FileTree {
    /// The active worktree's folder name, then its branch.
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = match self.root() {
            Some(root) => root.file_name().map_or_else(
                || root.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            None => "Files".to_owned(),
        };
        let branch = self.git.as_ref().and_then(|git| git.branch.clone());
        h_flex()
            .min_w_0()
            .gap_1()
            .text_sm()
            .child(div().flex_shrink_0().child(name))
            .children(branch.map(|branch| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(branch)
            }))
    }

    fn toolbar_buttons(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<Vec<Button>> {
        self.root()?;
        Some(vec![
            Button::new("files-new-file")
                .icon(IconName::Plus)
                .tooltip("New File")
                .on_click(
                    cx.listener(|this, _, window, cx| this.new_in_selected(false, window, cx)),
                ),
            Button::new("files-new-folder")
                .icon(IconName::FolderClosed)
                .tooltip("New Folder")
                .on_click(
                    cx.listener(|this, _, window, cx| this.new_in_selected(true, window, cx)),
                ),
        ])
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        if self.root().is_none() {
            return menu;
        }
        let entries = Self::panel_entries(&cx.entity().downgrade());
        chrome::fill_popup(menu, &entries, window, cx)
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
