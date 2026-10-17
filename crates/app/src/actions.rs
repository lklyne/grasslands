//! App-wide actions, the app menu and the global key bindings.
//!
//! Each feature module owns its own actions and key bindings in its `init`;
//! this module holds the ones that cross module boundaries. See
//! docs/decisions/0002-app-structure.md.

use std::path::PathBuf;

use gpui_kit::*;

use crate::chrome::MenuTarget;

actions!(app, [Quit, ToggleTheme, CommandPalette]);

/// Open a file in the editor. Sent by the file tree, the command palette, and
/// anything else that wants a file on screen.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = app, no_json)]
pub struct OpenFile {
    pub path: PathBuf,
}

/// Open a file with the cursor on `line` (0-based). Sent by the file search.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = app, no_json)]
pub struct OpenFileAt {
    pub path: PathBuf,
    pub line: u32,
}

/// Focus a herdr pane: the app sends `pane.focus` and herdr's `pane.focused`
/// event moves the focus ring. Sent by the agent list and the terminal area.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = app, no_json)]
pub struct FocusPane {
    pub pane_id: String,
}

/// Open the chrome's context menu for a pane, tab or workspace at a window
/// position. Sent by the terminal area (pane and tab right-clicks) and the
/// sidebar; `Workspace` forwards it to `Chrome::context_menu`.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = app, no_json)]
pub struct OpenContextMenu {
    pub target: MenuTarget,
    pub position: Point<Pixels>,
}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-shift-p", CommandPalette, None),
    ]);
    cx.set_menus(vec![
        Menu {
            name: "grasslands".into(),
            items: vec![
                MenuItem::action("Command Palette", CommandPalette),
                MenuItem::separator(),
                MenuItem::action("Quit grasslands", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![MenuItem::action("Toggle Light/Dark", ToggleTheme)],
            disabled: false,
        },
    ]);
}
