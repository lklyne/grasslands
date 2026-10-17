//! The file tree, rooted at the active worktree, with .gitignore respected.
//!
//! Folders are listed lazily, one level when first expanded, on the
//! background executor. A recursive watcher on the root re-lists any loaded
//! folder whose contents change, so files agents create, edit and delete show
//! up live. Clicking a file sends `OpenFile`; the context menu creates,
//! renames, trashes and reveals. Rows drag onto folders to move them. The
//! search box above swaps the tree for name and content matches. A footer
//! shows the branch, its parent and the lines changed since (see
//! `crate::changes`); clicking it opens the Changes tab. Rows changed on the
//! branch, committed or not, are colored: added green, modified yellow, and
//! folders holding a change yellow.

mod model;
mod search;
mod watch;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
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
use crate::changes::git;
use crate::state::{AppEvent, AppState};

/// Watcher events arriving within this window are folded into one re-list.
const FS_DEBOUNCE: Duration = Duration::from_millis(75);
/// Typing within this window runs one search.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
/// The git footer re-reads at most this often, and at least this often too
/// (5x), for commits in a linked worktree's git dir the watcher can't see.
const GIT_THROTTLE: Duration = Duration::from_secs(1);
const GIT_REFRESH: Duration = Duration::from_secs(5);
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
    watcher: Option<RootWatcher>,
    fs_changes: async_channel::Sender<Vec<PathBuf>>,
    focus: FocusHandle,
    query: Entity<InputState>,
    /// Matches for the query; `None` while it is empty, which shows the tree.
    results: Option<search::Results>,
    searching: Option<(Arc<AtomicBool>, Task<()>)>,
    git: Option<git::Summary>,
    /// A read in flight, then the throttle pause after it.
    git_reading: Option<Task<()>>,
    /// Something changed while `git_reading`; read again after it.
    git_stale: bool,
    _subscriptions: [Subscription; 3],
    _fs_task: Task<()>,
    _git_timer: Task<()>,
}

/// A file or folder being dragged to another folder.
#[derive(Clone)]
pub struct DraggedPath {
    pub path: PathBuf,
    name: SharedString,
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
            cx.subscribe(&state, |this, _, event, cx| {
                if let AppEvent::ActiveWorktreeChanged(root) = event {
                    this.set_root(root.clone(), cx);
                }
            }),
            cx.subscribe(&tree, |this, _, event, cx| this.on_tree_event(event, cx)),
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
                if this.update(cx, |this, cx| this.refresh_git(cx)).is_err() {
                    break;
                }
            }
        });
        let root = state.read(cx).active_worktree().cloned();
        let mut this = Self {
            state,
            tree,
            model: None,
            watcher: None,
            fs_changes,
            focus: cx.focus_handle(),
            query,
            results: None,
            searching: None,
            git: None,
            git_reading: None,
            git_stale: false,
            _subscriptions: subscriptions,
            _fs_task: fs_task,
            _git_timer: git_timer,
        };
        this.set_root(root, cx);
        this
    }

    pub fn root(&self) -> Option<&Path> {
        self.model.as_ref().map(FileModel::root)
    }

    fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.root() == root.as_deref() {
            return;
        }
        self.watcher = None;
        self.model = root.map(FileModel::new);
        if let Some(root) = self.root().map(Path::to_path_buf) {
            // GPUI's test scheduler rejects wakeups from the watcher's thread;
            // tests call `on_fs_changes` directly and test the watcher alone.
            if !cfg!(test) {
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

    /// Re-reads the git footer on the background executor, at most once per
    /// `GIT_THROTTLE`.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git_reading.is_some() {
            self.git_stale = true;
            return;
        }
        let Some(root) = self.root().map(Path::to_path_buf) else {
            return;
        };
        self.git_reading = Some(cx.spawn(async move |this, cx| {
            let summary = cx
                .background_executor()
                .spawn({
                    let root = root.clone();
                    async move { git::read(&root) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A read for a root we have since left.
                if this.root() == Some(root.as_path()) && this.git != summary {
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
        let query = self.query.read(cx).value().trim().to_owned();
        let Some(root) = self
            .root()
            .map(Path::to_path_buf)
            .filter(|_| !query.is_empty())
        else {
            self.results = None;
            cx.notify();
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let task = cx.spawn({
            let cancel = cancel.clone();
            async move |this, cx| {
                cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                let results = cx
                    .background_executor()
                    .spawn(async move {
                        let results = search::search(&root, &query, &cancel);
                        (!cancel.load(Ordering::Relaxed)).then_some(results)
                    })
                    .await;
                if let Some(results) = results {
                    let _ = this.update(cx, |this, cx| {
                        this.results = Some(results);
                        this.searching = None;
                        cx.notify();
                    });
                }
            }
        });
        self.searching = Some((cancel, task));
    }

    /// Lists `dirs` on the background executor, then updates the tree.
    fn load(&self, dirs: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(root) = self.root().map(Path::to_path_buf) else {
            return;
        };
        if dirs.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let listings = cx
                .background_executor()
                .spawn(async move {
                    dirs.into_iter()
                        .map(|dir| {
                            let listing = model::read_dir(&dir);
                            (dir, listing)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| this.apply_listings(&root, listings, cx));
        })
        .detach();
    }

    fn apply_listings(
        &mut self,
        root: &Path,
        listings: Vec<(PathBuf, std::io::Result<Vec<model::Entry>>)>,
        cx: &mut Context<Self>,
    ) {
        // Results for a root we have since left.
        let Some(model) = self.model.as_mut().filter(|m| m.root() == root) else {
            return;
        };
        for (dir, listing) in listings {
            match listing {
                Ok(entries) => model.set_listing(dir, entries),
                Err(_) if dir == root => model.set_listing(dir, Vec::new()),
                Err(_) => model.forget(&dir),
            }
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

    /// Re-lists every loaded folder.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(model) = &self.model {
            self.load(model.loaded_dirs(), cx);
        }
    }

    // File operations. Each re-lists the affected folder right away rather than
    // waiting for the watcher.

    fn create(
        &mut self,
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
        let created = if folder {
            std::fs::create_dir(&path)
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map(drop)
        };
        if let Err(err) = created {
            return error(
                format!("Could not create {}: {err}", path.display()),
                window,
                cx,
            );
        }
        if let Some(model) = self.model.as_mut()
            && model.root() != dir
        {
            model.set_expanded(&dir, true);
        }
        self.load(vec![dir], cx);
        if !folder {
            // Called from the name prompt, which lives in the Kit's dialog layer,
            // outside `Workspace`: an action dispatched there never reaches the
            // editor. Once the dialog has closed, focus the tree (inside
            // `Workspace`) and dispatch from there.
            cx.defer_in(window, move |this, window, cx| {
                this.tree.update(cx, |tree, cx| tree.focus(window, cx));
                window.dispatch_action(Box::new(OpenFile { path }), cx);
            });
        }
    }

    fn rename(&mut self, path: &Path, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = match valid_name(name) {
            Ok(name) => path.with_file_name(name),
            Err(message) => return error(message, window, cx),
        };
        if target == path {
            return;
        }
        if target.exists() {
            return error(format!("{} already exists", target.display()), window, cx);
        }
        if let Err(err) = std::fs::rename(path, &target) {
            return error(
                format!("Could not rename {}: {err}", path.display()),
                window,
                cx,
            );
        }
        let from = path.to_path_buf();
        self.state
            .update(cx, |state, cx| state.file_moved(from, target, cx));
        self.load(
            path.parent().map(Path::to_path_buf).into_iter().collect(),
            cx,
        );
    }

    /// Moves `path` into `dir`, keeping its name.
    fn move_into(&mut self, path: &Path, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
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
        let target = dir.join(name);
        if target.exists() {
            return error(format!("{} already exists", target.display()), window, cx);
        }
        if let Err(err) = std::fs::rename(path, &target) {
            return error(
                format!("Could not move {}: {err}", path.display()),
                window,
                cx,
            );
        }
        self.state.update(cx, |state, cx| {
            state.file_moved(path.to_path_buf(), target, cx)
        });
        if let Some(model) = self.model.as_mut()
            && model.root() != dir
        {
            model.set_expanded(dir, true);
        }
        let dirs = path.parent().into_iter().chain([dir]);
        self.load(dirs.map(Path::to_path_buf).collect(), cx);
    }

    /// Moves to the Trash rather than deleting, so a slip is recoverable.
    fn trash(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = trash::delete(path) {
            return error(
                format!("Could not move {} to the Trash: {err}", path.display()),
                window,
                cx,
            );
        }
        let trashed = path.to_path_buf();
        self.state
            .update(cx, |state, cx| state.file_trashed(trashed, cx));
        self.load(
            path.parent().map(Path::to_path_buf).into_iter().collect(),
            cx,
        );
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

    fn context_menu(this: &WeakEntity<Self>, entry: &TreeEntry, menu: PopupMenu) -> PopupMenu {
        let id = entry.item().id.as_ref();
        if model::is_placeholder(id) {
            return menu;
        }
        let path = PathBuf::from(id);
        let dir = if entry.is_folder() {
            path.clone()
        } else {
            path.parent()
                .map_or_else(|| path.clone(), Path::to_path_buf)
        };
        let name = entry.item().label.to_string();
        let item = |label: &'static str, run: TreeOp| {
            let this = this.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let run = run.clone();
                let _ = this.update(cx, |this, cx| run(this, window, cx));
            })
        };
        let new_in = |folder: bool| {
            let dir = dir.clone();
            Rc::new(
                move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let dir = dir.clone();
                    let title = if folder { "New Folder" } else { "New File" };
                    this.prompt(
                        title,
                        "Create",
                        "",
                        move |this, name, window, cx| {
                            this.create(dir.clone(), name, folder, window, cx)
                        },
                        window,
                        cx,
                    );
                },
            ) as TreeOp
        };
        let rename = {
            let path = path.clone();
            Rc::new(
                move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let path = path.clone();
                    this.prompt(
                        "Rename",
                        "Rename",
                        &name,
                        move |this, name, window, cx| this.rename(&path, name, window, cx),
                        window,
                        cx,
                    );
                },
            )
        };
        let trash = {
            let path = path.clone();
            Rc::new(
                move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.trash(&path, window, cx);
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
        menu.item(item("New File…", new_in(false)))
            .item(item("New Folder…", new_in(true)))
            .separator()
            .item(item("Rename…", rename))
            .item(item("Move to Trash", trash))
            .separator()
            .item(item("Reveal in Finder", reveal))
            .item(item("Copy Path", copy_path))
    }

    /// Asks for a name, then creates a file (or folder) in the selected
    /// folder, the selected file's folder, or the root.
    fn new_in_selected(&mut self, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root().map(Path::to_path_buf) else {
            return;
        };
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
            move |this, name, window, cx| this.create(dir.clone(), name, folder, window, cx),
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
        .on_drag(
            DraggedPath {
                path: path.clone(),
                name: item.label.clone(),
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
                    this.move_into(&dragged.path, &drop_dir, window, cx)
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
    row.on_click(move |_, window, cx| {
        window.dispatch_action(Box::new(OpenFile { path: path.clone() }), cx);
    })
}

/// The query's matches: file names first, then lines grouped by file.
fn render_results(results: &search::Results, cx: &App) -> AnyElement {
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
                window.dispatch_action(Box::new(OpenFile { path: path.clone() }), cx);
            }),
        );
    }
    if !results.contents.is_empty() {
        list = list.child(label("In files"));
    }
    for (file_ix, file) in results.contents.iter().enumerate() {
        let path = file.path.clone();
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
                window.dispatch_action(Box::new(OpenFile { path: path.clone() }), cx);
            }),
        );
        for (line_ix, hit) in file.lines.iter().enumerate() {
            let path = file.path.clone();
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

fn error(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::error(message), cx);
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let body = match &self.results {
            Some(results) => render_results(results, cx),
            None => {
                let rows = this.clone();
                let git = self.git.clone();
                let menu = this.clone();
                let root = self.root().map(Path::to_path_buf);
                let drop_bg = cx.theme().drop_target;
                // Drops that miss every row land in the root.
                div()
                    .flex_1()
                    .min_h_0()
                    .drag_over::<DraggedPath>(move |style, _, _, _| style.bg(drop_bg))
                    .on_drop(move |dragged: &DraggedPath, window, cx| {
                        let Some(root) = &root else { return };
                        let _ = this.update(cx, |this, cx| {
                            this.move_into(&dragged.path, root, window, cx)
                        });
                    })
                    .child(
                        tree(&self.tree, move |ix, entry, _selected, _window, cx| {
                            render_row(ix, entry, &rows, git.as_ref(), cx)
                        })
                        .size_full()
                        .p_1()
                        .context_menu(
                            move |_ix, entry, popup, _window, _cx| {
                                Self::context_menu(&menu, entry, popup)
                            },
                        ),
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
    /// The active worktree's folder name.
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let name = match self.root() {
            Some(root) => root.file_name().map_or_else(
                || root.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            None => "Files".to_owned(),
        };
        div().text_sm().child(name)
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
            Button::new("files-refresh")
                .icon(IconName::RefreshCw)
                .tooltip("Refresh")
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
        ])
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
