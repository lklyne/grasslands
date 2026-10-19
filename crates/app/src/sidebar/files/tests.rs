//! The file tree in a headless window. Watcher events are fed to
//! `on_fs_changes` directly; the watcher has its own test.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::dialog::{Cancel, Confirm};
use gpui_kit::component::tree::TreeEvent;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};

use super::{FileTree, Scope};
use crate::fs::Fs;
use crate::fs::fake::FakeFs;
use crate::herdr;
use crate::state::{ActiveRoot, AppEvent, AppState, MachineId};

struct Fixture {
    window: AnyWindowHandle,
    files: Entity<FileTree>,
    state: Entity<AppState>,
    dir: PathBuf,
}

/// A git repo with `src/main.rs`, `a.txt`, an ignored `target/` and `.gitignore`.
fn repo(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("grasslands-filetree-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
    git2::Repository::init(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn fixture(cx: &mut TestAppContext, name: &str) -> Fixture {
    let dir = repo(name);
    cx.update(gpui_kit::init);
    let (window, (files, state)) = cx.update(|cx| {
        let state = cx.new(|cx| AppState::new(Ok(crate::state::offline_config()), cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(400.), px(600.)),
            })),
            ..WindowOptions::default()
        };
        let mut built = None;
        let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
            let files = cx.new(|cx| FileTree::new(state.clone(), window, cx));
            built = Some(files.clone());
            files
        })
        .unwrap();
        (window, (built.unwrap(), state))
    });
    let fixture = Fixture {
        window,
        files,
        state,
        dir,
    };
    let root = fixture.dir.clone();
    fixture
        .state
        .update(cx, |state, cx| state.set_active_worktree(Some(root), cx));
    cx.run_until_parked();
    fixture
}

impl Fixture {
    /// Visible rows, indented two spaces per level.
    fn rows(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.read(|cx| {
            let tree = self.files.read(cx).tree.read(cx);
            (0..)
                .map_while(|ix| tree.entry(ix))
                .map(|e| format!("{}{}", "  ".repeat(e.depth()), e.item().label))
                .collect()
        })
    }

    fn expand(&self, cx: &mut TestAppContext, dir: &Path) {
        let id = dir.to_string_lossy().into_owned().into();
        self.files.update(cx, |files, cx| {
            files.on_tree_event(&TreeEvent::Expanded(id), cx)
        });
        cx.run_until_parked();
    }

    fn fs_changed(&self, cx: &mut TestAppContext, path: &Path) {
        self.files.update(cx, |files, cx| {
            files.on_fs_changes(&[path.to_path_buf()], cx)
        });
        cx.run_until_parked();
    }

    fn with_window(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut FileTree, &mut gpui_kit::Window, &mut gpui_kit::Context<FileTree>),
    ) {
        cx.update_window(self.window, |_, window, cx| {
            self.files.update(cx, |files, cx| f(files, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui::test]
fn tree_should_list_worktree_root_without_ignored_files(cx: &mut TestAppContext) {
    let f = fixture(cx, "list");
    assert_eq!(f.rows(cx), vec!["src", ".gitignore", "a.txt"]);
}

#[gpui::test]
fn expanding_folder_should_load_its_children(cx: &mut TestAppContext) {
    let f = fixture(cx, "expand");
    f.expand(cx, &f.dir.join("src"));
    assert_eq!(f.rows(cx), vec!["src", "  main.rs", ".gitignore", "a.txt"]);
}

#[gpui::test]
fn watcher_change_should_show_new_file(cx: &mut TestAppContext) {
    let f = fixture(cx, "watch");
    let path = f.dir.join("b.txt");
    std::fs::write(&path, "b").unwrap();
    f.fs_changed(cx, &path);
    assert!(f.rows(cx).contains(&"b.txt".to_owned()));
}

#[gpui::test]
fn watcher_change_should_drop_deleted_file(cx: &mut TestAppContext) {
    let f = fixture(cx, "delete");
    let path = f.dir.join("a.txt");
    std::fs::remove_file(&path).unwrap();
    f.fs_changed(cx, &path);
    assert!(!f.rows(cx).contains(&"a.txt".to_owned()));
}

#[gpui::test]
fn new_file_should_be_created_and_listed(cx: &mut TestAppContext) {
    let f = fixture(cx, "create");
    let src = f.dir.join("src");
    f.with_window(cx, |files, window, cx| {
        let scope = files.scope(cx).unwrap();
        files.create(&scope, src, "lib.rs", false, window, cx)
    });
    assert!(f.dir.join("src/lib.rs").is_file());
    assert_eq!(
        f.rows(cx),
        vec!["src", "  lib.rs", "  main.rs", ".gitignore", "a.txt"]
    );
}

#[gpui::test]
fn rename_should_move_file_and_relist(cx: &mut TestAppContext) {
    let f = fixture(cx, "rename");
    let path = f.dir.join("a.txt");
    f.with_window(cx, |files, window, cx| {
        let scope = files.scope(cx).unwrap();
        files.rename(&scope, &path, "z.txt", window, cx)
    });
    assert_eq!(f.rows(cx), vec!["src", ".gitignore", "z.txt"]);
}

#[gpui::test]
fn invalid_name_should_not_create_anything(cx: &mut TestAppContext) {
    let f = fixture(cx, "invalid");
    let root = f.dir.clone();
    f.with_window(cx, |files, window, cx| {
        let scope = files.scope(cx).unwrap();
        files.create(&scope, root, "a/b", false, window, cx)
    });
    assert!(!f.dir.join("a").exists());
}

/// A snapshot whose focused pane works in `dir`.
fn snapshot_focused_in(dir: &Path) -> Arc<herdr::Snapshot> {
    let pane: herdr::PaneInfo = serde_json::from_value(serde_json::json!({
        "pane_id": "w1:p1", "terminal_id": "t", "workspace_id": "w1", "tab_id": "w1:t1",
        "focused": true, "agent_status": "idle", "revision": 0,
        "cwd": "/", "foreground_cwd": dir,
    }))
    .unwrap();
    Arc::new(herdr::Snapshot {
        generation: 1,
        focused_pane_id: Some(pane.pane_id.clone()),
        panes: [(pane.pane_id.clone(), pane)].into(),
        ..Default::default()
    })
}

#[gpui::test]
fn focusing_pane_in_other_repo_should_reroot_tree(cx: &mut TestAppContext) {
    let f = fixture(cx, "follow-a");
    let other = repo("follow-b");
    std::fs::create_dir_all(other.join("src/deep")).unwrap();
    let snapshot = snapshot_focused_in(&other.join("src/deep"));
    f.state
        .update(cx, |state, cx| state.replace_snapshot(snapshot, cx));
    cx.run_until_parked();
    let root = cx.read(|cx| f.files.read(cx).root().map(Path::to_path_buf));
    assert_eq!(root, Some(other));
}

#[gpui::test]
fn enter_in_new_file_prompt_should_create_file(cx: &mut TestAppContext) {
    let f = fixture(cx, "prompt");
    let root = f.dir.clone();
    f.with_window(cx, |files, window, cx| {
        files.prompt(
            "New File",
            "Create",
            "",
            move |files, name, window, cx| {
                let scope = files.scope(cx).unwrap();
                files.create(&scope, root.clone(), name, false, window, cx)
            },
            window,
            cx,
        );
    });
    cx.simulate_input(f.window, "typed.txt");
    cx.simulate_keystrokes(f.window, "enter");
    cx.run_until_parked();
    assert!(f.dir.join("typed.txt").is_file());
}

impl Fixture {
    /// Opens the New File prompt in the repo root and types `name`.
    fn new_file_prompt(&self, cx: &mut TestAppContext, name: &str) {
        let root = self.dir.clone();
        self.with_window(cx, |files, window, cx| {
            files.prompt(
                "New File",
                "Create",
                "",
                move |files, name, window, cx| {
                    let scope = files.scope(cx).unwrap();
                    files.create(&scope, root.clone(), name, false, window, cx)
                },
                window,
                cx,
            );
        });
        cx.simulate_input(self.window, name);
    }
}

/// The footer's OK button dispatches the dialog's `Confirm`, like Enter.
#[gpui::test]
fn ok_button_action_in_prompt_should_create_file(cx: &mut TestAppContext) {
    let f = fixture(cx, "prompt-ok");
    f.new_file_prompt(cx, "ok.txt");
    cx.dispatch_action(f.window, Confirm { secondary: false });
    cx.run_until_parked();
    assert!(f.dir.join("ok.txt").is_file());
}

#[gpui::test]
fn cancel_in_prompt_should_create_nothing(cx: &mut TestAppContext) {
    let f = fixture(cx, "prompt-cancel");
    f.new_file_prompt(cx, "no.txt");
    cx.dispatch_action(f.window, Cancel);
    cx.run_until_parked();
    assert!(!f.dir.join("no.txt").exists());
}

#[gpui::test]
fn move_into_should_move_file_into_folder(cx: &mut TestAppContext) {
    let f = fixture(cx, "move");
    let (path, src) = (f.dir.join("a.txt"), f.dir.join("src"));
    f.with_window(cx, |files, window, cx| {
        let scope = files.scope(cx).unwrap();
        files.move_into(&scope, &path, &src, window, cx)
    });
    assert_eq!(
        f.rows(cx),
        vec!["src", "  a.txt", "  main.rs", ".gitignore"]
    );
}

#[gpui::test]
fn move_into_should_refuse_folder_into_its_own_child(cx: &mut TestAppContext) {
    let f = fixture(cx, "move-self");
    let src = f.dir.join("src");
    std::fs::create_dir_all(src.join("deep")).unwrap();
    let deep = src.join("deep");
    f.with_window(cx, |files, window, cx| {
        let scope = files.scope(cx).unwrap();
        files.move_into(&scope, &src, &deep, window, cx)
    });
    assert!(src.join("main.rs").is_file());
}

#[gpui::test]
fn new_file_button_should_create_in_selected_folder(cx: &mut TestAppContext) {
    let f = fixture(cx, "new-in-selected");
    f.with_window(cx, |files, _, cx| {
        files
            .tree
            .update(cx, |tree, cx| tree.set_selected_index(Some(0), cx))
    });
    f.with_window(cx, |files, window, cx| {
        files.new_in_selected(false, window, cx)
    });
    cx.simulate_input(f.window, "lib.rs");
    cx.simulate_keystrokes(f.window, "enter");
    cx.run_until_parked();
    assert!(f.dir.join("src/lib.rs").is_file());
}

#[gpui::test]
fn typing_a_query_should_show_name_and_content_matches(cx: &mut TestAppContext) {
    let f = fixture(cx, "search");
    f.with_window(cx, |files, window, cx| {
        files.query.update(cx, |query, cx| query.focus(window, cx))
    });
    cx.simulate_input(f.window, "main");
    cx.executor().advance_clock(super::SEARCH_DEBOUNCE);
    cx.run_until_parked();
    let found = cx.read(|cx| {
        f.files.read(cx).results.as_ref().map(|r| {
            (
                r.names
                    .iter()
                    .map(|h| h.relative.clone())
                    .collect::<Vec<_>>(),
                r.contents
                    .iter()
                    .map(|h| h.relative.clone())
                    .collect::<Vec<_>>(),
            )
        })
    });
    assert_eq!(
        found,
        Some((
            vec!["src/main.rs".to_owned()],
            vec!["src/main.rs".to_owned()]
        ))
    );
}

// A tree action acts on the machine and root it was chosen on, never on
// whatever the tree shows when it runs.

const BOX_ROOT: &str = "/home/lyle/api";

fn fake_machine() -> MachineId {
    MachineId::Remote("fake".into())
}

impl Fixture {
    /// Roots the tree at `BOX_ROOT` on an in-memory machine holding
    /// `x.txt`; returns its disk and the scope an action chosen now captures.
    fn root_on_fake(&self, cx: &mut TestAppContext) -> (Arc<FakeFs>, Scope) {
        let fake = Arc::new(FakeFs::default());
        fake.put(&Path::new(BOX_ROOT).join("x.txt"), "x");
        let root = ActiveRoot {
            machine: fake_machine(),
            path: BOX_ROOT.into(),
        };
        self.files
            .update(cx, |files, cx| files.set_root(Some(root), cx));
        cx.run_until_parked();
        let scope = Scope {
            machine: fake_machine(),
            root: BOX_ROOT.into(),
            fs: Fs::Fake(fake.clone()),
        };
        (fake, scope)
    }

    /// Moves the tree back to the local repo, as a focus change would.
    fn root_on_local(&self, cx: &mut TestAppContext) {
        let root = ActiveRoot {
            machine: MachineId::Local,
            path: self.dir.clone(),
        };
        self.files
            .update(cx, |files, cx| files.set_root(Some(root), cx));
        cx.run_until_parked();
    }
}

#[gpui::test]
fn remove_should_delete_on_captured_machine(cx: &mut TestAppContext) {
    let f = fixture(cx, "scope-remove");
    let (fake, scope) = f.root_on_fake(cx);
    let path = Path::new(BOX_ROOT).join("x.txt");
    f.with_window(cx, |files, window, cx| {
        files.remove(&scope, &path, window, cx)
    });
    assert_eq!(fake.get(&path), None);
}

#[gpui::test]
fn remove_should_refuse_once_root_has_moved(cx: &mut TestAppContext) {
    let f = fixture(cx, "scope-remove-moved");
    let (fake, scope) = f.root_on_fake(cx);
    f.root_on_local(cx);
    let path = Path::new(BOX_ROOT).join("x.txt");
    f.with_window(cx, |files, window, cx| {
        files.remove(&scope, &path, window, cx)
    });
    assert_eq!(fake.get(&path).as_deref(), Some("x"));
}

#[gpui::test]
fn rename_should_refuse_once_root_has_moved(cx: &mut TestAppContext) {
    let f = fixture(cx, "scope-rename-moved");
    let (fake, scope) = f.root_on_fake(cx);
    f.root_on_local(cx);
    let path = Path::new(BOX_ROOT).join("x.txt");
    f.with_window(cx, |files, window, cx| {
        files.rename(&scope, &path, "y.txt", window, cx)
    });
    assert_eq!(fake.get(&path).as_deref(), Some("x"));
}

#[gpui::test]
fn ask_remove_should_not_delete_remote_file_without_confirmation(cx: &mut TestAppContext) {
    let f = fixture(cx, "scope-ask");
    let (fake, scope) = f.root_on_fake(cx);
    let path = Path::new(BOX_ROOT).join("x.txt");
    f.with_window(cx, |files, window, cx| {
        files.ask_remove(&scope, &path, window, cx)
    });
    assert_eq!(fake.get(&path).as_deref(), Some("x"));
}

#[gpui::test]
fn remove_should_report_captured_machine_when_root_moves_mid_operation(cx: &mut TestAppContext) {
    let f = fixture(cx, "scope-report");
    let (_fake, scope) = f.root_on_fake(cx);
    let removed = Rc::new(RefCell::new(Vec::new()));
    let _subscription = cx.update(|cx| {
        let removed = removed.clone();
        cx.subscribe(&f.state, move |_, event, _| {
            if let AppEvent::FileRemoved { machine, .. } = event {
                removed.borrow_mut().push(machine.clone());
            }
        })
    });
    let path = Path::new(BOX_ROOT).join("x.txt");
    // Start the delete, then move the root before it completes.
    cx.update_window(f.window, |_, window, cx| {
        f.files
            .update(cx, |files, cx| files.remove(&scope, &path, window, cx));
    })
    .unwrap();
    f.root_on_local(cx);
    assert_eq!(*removed.borrow(), vec![fake_machine()]);
}
