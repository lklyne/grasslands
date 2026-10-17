//! The window's root view: the titlebar tab strip over the dock, which holds
//! the main area in its center and the sidebar panels in its left dock. The
//! main area shows the terminal area (the herdr tab) or the editor (a file
//! tab). Also routes cross-module actions to
//! the view that owns them (actions bubble up the focus path to here, not
//! sideways).

pub mod dock;
mod main_area;
mod titlebar;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

use self::dock::{Docks, MainPanel};
use self::main_area::MainArea;
use self::titlebar::DrawnTabs;
pub use self::titlebar::window_options;
use crate::actions::{CommandPalette, FocusPane, OpenContextMenu, OpenFile, OpenFileAt};
use crate::chrome::Chrome;
use crate::editor::{Editor, EditorEvent};
use crate::overlay;
use crate::sidebar::SidebarPanels;
use crate::state::AppState;
use crate::terminal_area::TerminalArea;

actions!(workspace, [NextMainTab, PreviousMainTab, ShowHerdr]);

const CONTEXT: &str = "Workspace";

pub fn init(cx: &mut App) {
    // The terminal area binds cmd-shift-[ / ] to herdr's own tabs; being the
    // deeper context, it wins there. ctrl-tab cycles the titlebar everywhere.
    cx.bind_keys([
        KeyBinding::new("ctrl-tab", NextMainTab, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousMainTab, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-]", NextMainTab, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-[", PreviousMainTab, Some(CONTEXT)),
        KeyBinding::new("cmd-1", ShowHerdr, Some(CONTEXT)),
    ]);
}

/// What the main area shows: the herdr terminal area, or the editor's
/// selected file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainTab {
    Herdr,
    Files,
}

/// Each main window's workspace, for the script hooks: see [`get`].
#[derive(Default)]
struct Workspaces(HashMap<WindowId, WeakEntity<Workspace>>);

impl Global for Workspaces {}

/// The workspace of the main window `window`, if it has one.
pub fn get(window: &Window, cx: &App) -> Option<Entity<Workspace>> {
    let id = window.window_handle().window_id();
    cx.try_global::<Workspaces>()?.0.get(&id)?.upgrade()
}

pub struct Workspace {
    pub state: Entity<AppState>,
    docks: Entity<Docks>,
    main_panel: Entity<MainPanel>,
    /// A panel drag is under way, so the terminals are hidden to show where
    /// it will land (native views would cover the drop indicator).
    dragging: bool,
    editor: Entity<Editor>,
    terminal_area: Entity<TerminalArea>,
    chrome: Entity<Chrome>,
    selected: MainTab,
    /// Where the titlebar tabs were drawn last frame (script hooks).
    drawn_tabs: Rc<RefCell<DrawnTabs>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let editor = cx.new(|cx| Editor::new(state.clone(), window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
                EditorEvent::Activated => this.select(MainTab::Files, window, cx),
            }),
            // Closing the last file tab, or switching to a worktree with no
            // open files, leaves nothing to show but herdr.
            cx.observe_in(&editor, window, |this, editor, window, cx| {
                if this.selected == MainTab::Files && editor.read(cx).active_index().is_none() {
                    this.select(MainTab::Herdr, window, cx);
                }
                cx.notify();
            }),
        ];
        let id = window.window_handle().window_id();
        let this = cx.entity().downgrade();
        cx.default_global::<Workspaces>().0.insert(id, this.clone());
        let terminal_area = cx.new(|cx| TerminalArea::new(state.clone(), window, cx));
        let main_area = cx.new(|_| MainArea::new(this));
        let main_panel = cx.new(|cx| {
            let mut panel = MainPanel::new(cx);
            panel.set_content(main_area.into(), cx);
            panel
        });
        let sidebar = SidebarPanels::new(state.clone(), window, cx);
        let docks =
            cx.new(|cx| Docks::new(main_panel.clone(), sidebar, dock::layout_path(), window, cx));
        Self {
            docks,
            main_panel,
            dragging: false,
            editor,
            terminal_area,
            chrome: cx.new(|cx| Chrome::new(state.clone(), window, cx)),
            state,
            selected: MainTab::Herdr,
            drawn_tabs: Rc::default(),
            focus,
            _subscriptions: subscriptions,
        }
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    pub fn selected(&self) -> MainTab {
        self.selected
    }

    /// Shows `tab` in the main area and moves keyboard focus into it. The
    /// terminal area hides its native views while a file is shown.
    pub fn select(&mut self, tab: MainTab, window: &mut Window, cx: &mut Context<Self>) {
        let tab = match tab {
            MainTab::Files if self.editor.read(cx).active_index().is_none() => MainTab::Herdr,
            tab => tab,
        };
        let changed = self.selected != tab;
        self.selected = tab;
        let herdr = tab == MainTab::Herdr;
        self.terminal_area.update(cx, |area, cx| {
            area.set_visible(herdr && !self.dragging, window, cx)
        });
        self.main_panel.update(cx, |_, cx| cx.notify());
        if changed && !herdr {
            self.editor
                .update(cx, |editor, cx| editor.focus_active(window, cx));
        }
        cx.notify();
    }

    /// Selects titlebar tab `index`: 0 is herdr, then the file tabs.
    pub fn select_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match index.checked_sub(1) {
            None => self.select(MainTab::Herdr, window, cx),
            // Emits `Activated`, which selects the editor.
            Some(file) => self
                .editor
                .update(cx, |editor, cx| editor.activate(file, window, cx)),
        }
    }

    /// The titlebar index on screen: 0 for herdr, 1 + the file tab's index.
    pub fn selected_index(&self, cx: &App) -> usize {
        match (self.selected, self.editor.read(cx).active_index()) {
            (MainTab::Files, Some(index)) => index + 1,
            _ => 0,
        }
    }

    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.editor.read(cx).tabs().len() as isize + 1;
        let index = (self.selected_index(cx) as isize + step).rem_euclid(count) as usize;
        self.select_index(index, window, cx);
    }

    fn open_file(&mut self, action: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.open(&action.path, window, cx));
    }

    fn open_file_at(&mut self, action: &OpenFileAt, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.open_at(&action.path, action.line, window, cx)
        });
    }

    fn focus_pane(&mut self, action: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        self.select(MainTab::Herdr, window, cx);
        self.terminal_area
            .update(cx, |area, cx| area.focus_pane(&action.pane_id, window, cx));
    }

    fn command_palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome
            .update(cx, |chrome, cx| chrome.open_command_palette(window, cx));
    }

    fn open_context_menu(
        &mut self,
        action: &OpenContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chrome.update(cx, |chrome, cx| {
            chrome.context_menu(
                action.target.clone(),
                action.position,
                Vec::new(),
                window,
                cx,
            );
        });
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Every frame renders this view, so a drag starting or ending shows
        // up here. Hiding can't happen mid-render; defer it.
        let dragging = cx.has_active_drag();
        if dragging != self.dragging {
            self.dragging = dragging;
            let visible = self.selected == MainTab::Herdr && !dragging;
            let area = self.terminal_area.clone();
            window.defer(cx, move |window, cx| {
                area.update(cx, |area, cx| area.set_visible(visible, window, cx));
            });
        }
        v_flex()
            .id("workspace")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::open_file_at))
            .on_action(cx.listener(Self::focus_pane))
            .on_action(cx.listener(Self::command_palette))
            .on_action(cx.listener(Self::open_context_menu))
            .on_action(cx.listener(|this, _: &NextMainTab, window, cx| this.cycle(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousMainTab, window, cx| this.cycle(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowHerdr, window, cx| {
                this.select(MainTab::Herdr, window, cx)
            }))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_titlebar(cx))
            .child(div().flex_1().min_h_0().child(self.docks.clone()))
            .child(self.chrome.clone())
            .child(
                // The overlay lets clicks through, so it can't see the
                // pointer itself; tell it on every move, whichever tab shows.
                canvas(
                    |_, _, _| {},
                    |_, _, window, _| {
                        window.on_mouse_event(|event: &MouseMoveEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble {
                                overlay::track_pointer(event.position, window, cx);
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            )
    }
}
