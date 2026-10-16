//! Editor workflows in a headless window, driven through real focus, key
//! bindings and text input. Disk changes are fed to `on_disk_change`
//! directly; the watcher has its own test.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};

use super::buffer::Prompt;
use super::watcher::FileWatcher;
use super::{Answer, Editor};
use crate::state::AppState;

struct Fixture {
    window: AnyWindowHandle,
    editor: Entity<Editor>,
    state: Entity<AppState>,
    dir: PathBuf,
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rusted-tools-editor-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn fixture(cx: &mut TestAppContext, name: &str) -> Fixture {
    let dir = temp_dir(name);
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::init(cx);
    });
    let (window, (editor, state)) = cx.update(|cx| {
        let state = cx.new(|cx| AppState::new(Ok(crate::state::offline_config()), cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(900.), px(600.)),
            })),
            ..WindowOptions::default()
        };
        let mut built = None;
        let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
            let editor = cx.new(|cx| Editor::new(state.clone(), window, cx));
            built = Some(editor.clone());
            editor
        })
        .unwrap();
        (window, (built.unwrap(), state))
    });
    Fixture {
        window,
        editor,
        state,
        dir,
    }
}

impl Fixture {
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn open(&self, cx: &mut TestAppContext, path: &Path) {
        cx.update_window(self.window, |_, window, cx| {
            self.editor
                .update(cx, |editor, cx| editor.open(path, window, cx));
        })
        .unwrap();
    }

    fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        cx.simulate_input(self.window, text);
        cx.run_until_parked();
    }

    fn press(&self, cx: &mut TestAppContext, key: &str) {
        cx.simulate_keystrokes(self.window, key);
        cx.run_until_parked();
    }

    fn disk_changed(&self, cx: &mut TestAppContext, path: &Path) {
        cx.update_window(self.window, |_, window, cx| {
            self.editor.update(cx, |editor, cx| {
                editor.on_disk_change(&[path.to_path_buf()], window, cx)
            });
        })
        .unwrap();
    }

    fn answer(&self, cx: &mut TestAppContext, answer: Answer) {
        cx.update_window(self.window, |_, window, cx| {
            self.editor
                .update(cx, |editor, cx| editor.answer(answer, window, cx));
        })
        .unwrap();
    }

    fn text(&self, cx: &mut TestAppContext) -> String {
        cx.read(|cx| {
            let editor = self.editor.read(cx);
            editor
                .group()
                .and_then(|g| g.active())
                .map(|b| b.state.read(cx).value().to_string())
                .unwrap_or_default()
        })
    }

    fn dirty(&self, cx: &mut TestAppContext) -> bool {
        cx.read(|cx| {
            self.editor
                .read(cx)
                .group()
                .and_then(|g| g.active())
                .is_some_and(|b| b.is_dirty())
        })
    }

    fn prompt(&self, cx: &mut TestAppContext) -> Option<Prompt> {
        cx.read(|cx| {
            self.editor
                .read(cx)
                .group()
                .and_then(|g| g.active())
                .and_then(|b| b.prompt.clone())
        })
    }

    fn tab_titles(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.read(|cx| {
            self.editor
                .read(cx)
                .group()
                .map(|g| g.titles())
                .unwrap_or_default()
        })
    }
}

#[gpui::test]
fn typing_marks_dirty_and_cmd_s_saves(cx: &mut TestAppContext) {
    let f = fixture(cx, "save");
    let path = f.file("a.rs", "fn a() {}\n");
    f.open(cx, &path);
    assert!(!f.dirty(cx));
    f.type_text(cx, "// hi\n");
    assert!(f.dirty(cx));
    f.press(cx, "cmd-s");
    assert!(!f.dirty(cx));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), f.text(cx));
    assert!(f.text(cx).contains("// hi"));
}

#[gpui::test]
fn opening_twice_reuses_the_tab(cx: &mut TestAppContext) {
    let f = fixture(cx, "reuse");
    let a = f.file("a.rs", "a");
    let b = f.file("b.toml", "b = 1");
    f.open(cx, &a);
    f.open(cx, &b);
    f.open(cx, &a);
    assert_eq!(f.tab_titles(cx), ["a.rs", "b.toml"]);
}

#[gpui::test]
fn same_name_tabs_show_their_folders(cx: &mut TestAppContext) {
    let f = fixture(cx, "samename");
    std::fs::create_dir_all(f.dir.join("one")).unwrap();
    std::fs::create_dir_all(f.dir.join("two")).unwrap();
    let a = f.file("one/mod.rs", "a");
    let b = f.file("two/mod.rs", "b");
    f.open(cx, &a);
    f.open(cx, &b);
    assert_eq!(f.tab_titles(cx), ["one/mod.rs", "two/mod.rs"]);
}

#[gpui::test]
fn clean_buffer_reloads_on_disk_change(cx: &mut TestAppContext) {
    let f = fixture(cx, "reload");
    let path = f.file("a.rs", "old\n");
    f.open(cx, &path);
    std::fs::write(&path, "new\n").unwrap();
    f.disk_changed(cx, &path);
    assert_eq!(f.text(cx), "new\n");
    assert_eq!(f.prompt(cx), None);
}

#[gpui::test]
fn own_save_does_not_prompt(cx: &mut TestAppContext) {
    let f = fixture(cx, "ownsave");
    let path = f.file("a.rs", "x\n");
    f.open(cx, &path);
    f.type_text(cx, "y");
    f.press(cx, "cmd-s");
    f.disk_changed(cx, &path);
    assert_eq!(f.prompt(cx), None);
}

#[gpui::test]
fn dirty_buffer_asks_and_take_theirs_replaces(cx: &mut TestAppContext) {
    let f = fixture(cx, "theirs");
    let path = f.file("a.rs", "base\n");
    f.open(cx, &path);
    f.type_text(cx, "mine ");
    std::fs::write(&path, "theirs\n").unwrap();
    f.disk_changed(cx, &path);
    assert!(matches!(f.prompt(cx), Some(Prompt::Conflict { .. })));
    f.answer(cx, Answer::TakeTheirs);
    assert_eq!(f.text(cx), "theirs\n");
    assert!(!f.dirty(cx));
}

#[gpui::test]
fn dirty_buffer_keep_mine_stays_dirty_and_saves_over(cx: &mut TestAppContext) {
    let f = fixture(cx, "mine");
    let path = f.file("a.rs", "base\n");
    f.open(cx, &path);
    f.type_text(cx, "mine ");
    std::fs::write(&path, "theirs\n").unwrap();
    f.disk_changed(cx, &path);
    f.answer(cx, Answer::KeepMine);
    assert_eq!(f.prompt(cx), None);
    assert!(f.dirty(cx));
    f.press(cx, "cmd-s");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine base\n");
}

#[gpui::test]
fn closing_a_dirty_tab_asks_first(cx: &mut TestAppContext) {
    let f = fixture(cx, "close");
    let path = f.file("a.rs", "a\n");
    f.open(cx, &path);
    f.type_text(cx, "edit");
    f.press(cx, "cmd-w");
    assert_eq!(f.prompt(cx), Some(Prompt::CloseUnsaved));
    assert_eq!(f.tab_titles(cx), ["a.rs"]);
    f.answer(cx, Answer::Discard);
    assert!(f.tab_titles(cx).is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\n");
}

#[gpui::test]
fn clean_tab_closes_with_cmd_w(cx: &mut TestAppContext) {
    let f = fixture(cx, "closeclean");
    let a = f.file("a.rs", "a");
    let b = f.file("b.rs", "b");
    f.open(cx, &a);
    f.open(cx, &b);
    f.press(cx, "cmd-w");
    assert_eq!(f.tab_titles(cx), ["a.rs"]);
}

#[gpui::test]
fn switching_worktree_swaps_tab_sets(cx: &mut TestAppContext) {
    let f = fixture(cx, "worktrees");
    let one = f.dir.join("one");
    let two = f.dir.join("two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let a = f.file("one/a.rs", "a");
    let b = f.file("two/b.rs", "b");
    let set_root = |cx: &mut TestAppContext, root: &Path| {
        let root = root.to_path_buf();
        f.state
            .update(cx, |state, cx| state.set_active_worktree(Some(root), cx));
        cx.run_until_parked();
    };
    set_root(cx, &one);
    f.open(cx, &a);
    set_root(cx, &two);
    assert!(f.tab_titles(cx).is_empty());
    f.open(cx, &b);
    assert_eq!(f.tab_titles(cx), ["b.rs"]);
    set_root(cx, &one);
    assert_eq!(f.tab_titles(cx), ["a.rs"]);
}

impl Fixture {
    fn moved_in_tree(&self, cx: &mut TestAppContext, from: &Path, to: &Path) {
        std::fs::rename(from, to).unwrap();
        let (from, to) = (from.to_path_buf(), to.to_path_buf());
        self.state
            .update(cx, |state, cx| state.file_moved(from, to, cx));
        cx.run_until_parked();
    }

    fn trashed_in_tree(&self, cx: &mut TestAppContext, path: &Path) {
        // The real tree moves it to the Trash; for the editor only the event matters.
        std::fs::remove_file(path).unwrap();
        let path = path.to_path_buf();
        self.state
            .update(cx, |state, cx| state.file_trashed(path, cx));
        cx.run_until_parked();
    }
}

#[gpui::test]
fn rename_in_tree_retargets_the_tab(cx: &mut TestAppContext) {
    let f = fixture(cx, "treerename");
    let a = f.file("a.rs", "a\n");
    f.open(cx, &a);
    f.moved_in_tree(cx, &a, &f.dir.join("b.rs"));
    assert_eq!(f.tab_titles(cx), ["b.rs"]);
}

#[gpui::test]
fn save_after_rename_writes_the_new_path(cx: &mut TestAppContext) {
    let f = fixture(cx, "renamesave");
    let a = f.file("a.rs", "a\n");
    f.open(cx, &a);
    let b = f.dir.join("b.rs");
    f.moved_in_tree(cx, &a, &b);
    f.type_text(cx, "x");
    f.press(cx, "cmd-s");
    assert_eq!(
        (a.exists(), std::fs::read_to_string(&b).unwrap()),
        (false, "xa\n".to_owned())
    );
}

#[gpui::test]
fn folder_rename_in_tree_retargets_tabs_inside_it(cx: &mut TestAppContext) {
    let f = fixture(cx, "folderrename");
    std::fs::create_dir_all(f.dir.join("old")).unwrap();
    let a = f.file("old/a.rs", "a");
    f.open(cx, &a);
    f.moved_in_tree(cx, &f.dir.join("old"), &f.dir.join("new"));
    f.type_text(cx, "y");
    f.press(cx, "cmd-s");
    assert!(
        std::fs::read_to_string(f.dir.join("new/a.rs"))
            .unwrap()
            .contains('y')
    );
}

#[gpui::test]
fn rename_on_disk_retargets_the_tab(cx: &mut TestAppContext) {
    let f = fixture(cx, "diskrename");
    let a = f.file("a.rs", "a\n");
    f.open(cx, &a);
    std::fs::rename(&a, f.dir.join("c.rs")).unwrap();
    f.disk_changed(cx, &a);
    assert_eq!(
        (f.tab_titles(cx), f.dirty(cx)),
        (vec!["c.rs".to_owned()], false)
    );
}

#[gpui::test]
fn delete_on_disk_still_keeps_the_tab_dirty(cx: &mut TestAppContext) {
    let f = fixture(cx, "diskdelete");
    let a = f.file("a.rs", "a\n");
    f.open(cx, &a);
    std::fs::remove_file(&a).unwrap();
    f.disk_changed(cx, &a);
    assert_eq!(
        (f.tab_titles(cx), f.dirty(cx)),
        (vec!["a.rs".to_owned()], true)
    );
}

#[gpui::test]
fn trash_in_tree_closes_a_clean_tab(cx: &mut TestAppContext) {
    let f = fixture(cx, "trashclean");
    let a = f.file("a.rs", "a\n");
    let b = f.file("b.rs", "b\n");
    f.open(cx, &a);
    f.open(cx, &b);
    f.trashed_in_tree(cx, &a);
    assert_eq!(f.tab_titles(cx), ["b.rs"]);
}

#[gpui::test]
fn trash_in_tree_keeps_a_dirty_tab(cx: &mut TestAppContext) {
    let f = fixture(cx, "trashdirty");
    let a = f.file("a.rs", "a\n");
    f.open(cx, &a);
    f.type_text(cx, "unsaved");
    f.trashed_in_tree(cx, &a);
    assert_eq!(
        (f.tab_titles(cx), f.dirty(cx)),
        (vec!["a.rs".to_owned()], true)
    );
}

#[test]
fn watcher_reports_a_write_to_a_watched_file() {
    let dir = temp_dir("watcher");
    let path = dir.join("w.txt");
    std::fs::write(&path, "1").unwrap();
    let (sender, receiver) = async_channel::unbounded();
    let mut watcher = FileWatcher::new(sender).unwrap();
    watcher.add(&path);
    // FSEvents starts delivering shortly after the watch is set up.
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(&path, "2").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let seen = loop {
        if let Ok(paths) = receiver.try_recv()
            && paths.contains(&path)
        {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(seen, "no change event for {}", path.display());
}
