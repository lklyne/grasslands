//! The window's root view: the titlebar tab strip over the dock, which holds
//! the main area in its center and the sidebar panels in its left dock. The
//! main area shows the terminal area (the herdr tab), the editor (a file
//! tab) or the worktree's diff (the Changes tab, after the files). Also routes cross-module actions to
//! the view that owns them (actions bubble up the focus path to here, not
//! sideways).

pub mod dock;
mod main_area;
mod titlebar;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

use self::dock::{Docks, MainPanel};
use self::main_area::MainArea;
use self::titlebar::DrawnTabs;
pub use self::titlebar::window_options;
use crate::actions::{
    CommandPalette, FocusPane, OpenChanges, OpenContextMenu, OpenFile, OpenFileAt,
};
use crate::changes::Changes;
use crate::chrome::Chrome;
use crate::editor::{Editor, EditorEvent};
use crate::overlay;
use crate::sidebar::SidebarPanels;
use crate::state::AppState;
use crate::terminal_area::TerminalArea;
use crate::window_registry;

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

/// What the main area shows: the herdr terminal area, the editor's
/// selected file, or the Changes tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainTab {
    Herdr,
    Files,
    Changes,
}

/// Set when the active drag can leave the terminals on screen: its drop
/// targets aren't under them, unlike a dock panel's. Cleared when it ends.
#[derive(Default)]
struct KeepTerminalsDuringDrag(bool);

impl Global for KeepTerminalsDuringDrag {}

/// Called from an `on_drag` constructor (file rows, pane headers) so the
/// terminals don't blank for that drag.
pub fn keep_terminals_during_drag(cx: &mut App) {
    cx.set_global(KeepTerminalsDuringDrag(true));
}

/// The workspace of the main window `window`, if it has one (for the
/// script hooks).
pub fn get(window: &Window, cx: &App) -> Option<Entity<Workspace>> {
    window_registry::upgrade(window, cx)
}

pub struct Workspace {
    pub state: Entity<AppState>,
    docks: Entity<Docks>,
    main_panel: Entity<MainPanel>,
    /// A panel drag is under way, so the terminals are hidden to show where
    /// it will land (native views would cover the drop indicator). Drags
    /// marked with [`keep_terminals_during_drag`] don't count.
    dragging: bool,
    /// A drag was under way at the last render.
    drag_seen: bool,
    editor: Entity<Editor>,
    /// The Changes tab, while it is open.
    changes: Option<Entity<Changes>>,
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
        let this = cx.entity().downgrade();
        window_registry::insert(window, this.clone(), cx);
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
            drag_seen: false,
            editor,
            changes: None,
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

    pub fn terminal_area(&self) -> &Entity<TerminalArea> {
        &self.terminal_area
    }

    /// The Changes tab, while it is open.
    pub fn changes(&self) -> Option<&Entity<Changes>> {
        self.changes.as_ref()
    }

    pub fn selected(&self) -> MainTab {
        self.selected
    }

    /// Shows `tab` in the main area and moves keyboard focus into it. The
    /// terminal area hides its native views while a file is shown.
    pub fn select(&mut self, tab: MainTab, window: &mut Window, cx: &mut Context<Self>) {
        let tab = match tab {
            MainTab::Files if self.editor.read(cx).active_index().is_none() => MainTab::Herdr,
            MainTab::Changes if self.changes.is_none() => MainTab::Herdr,
            tab => tab,
        };
        let changed = self.selected != tab;
        self.selected = tab;
        let herdr = tab == MainTab::Herdr;
        self.terminal_area.update(cx, |area, cx| {
            area.set_visible(herdr && !self.dragging, window, cx)
        });
        self.main_panel.update(cx, |_, cx| cx.notify());
        if changed {
            match tab {
                MainTab::Herdr => {}
                MainTab::Files => self
                    .editor
                    .update(cx, |editor, cx| editor.focus_active(window, cx)),
                MainTab::Changes => {
                    if let Some(changes) = &self.changes {
                        window.focus(&changes.focus_handle(cx), cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// The Changes tab's titlebar index, after the file tabs.
    fn changes_index(&self, cx: &App) -> Option<usize> {
        self.changes
            .as_ref()
            .map(|_| self.editor.read(cx).tabs(cx).len() + 1)
    }

    /// Selects titlebar tab `index`: 0 is herdr, then the file tabs, then
    /// Changes.
    pub fn select_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if Some(index) == self.changes_index(cx) {
            return self.select(MainTab::Changes, window, cx);
        }
        match index.checked_sub(1) {
            None => self.select(MainTab::Herdr, window, cx),
            // Emits `Activated`, which selects the editor.
            Some(file) => self
                .editor
                .update(cx, |editor, cx| editor.activate(file, window, cx)),
        }
    }

    /// The titlebar index on screen: 0 for herdr, 1 + the file tab's index,
    /// or the Changes tab's.
    pub fn selected_index(&self, cx: &App) -> usize {
        match (self.selected, self.editor.read(cx).active_index()) {
            (MainTab::Files, Some(index)) => index + 1,
            (MainTab::Changes, _) => self.changes_index(cx).unwrap_or(0),
            _ => 0,
        }
    }

    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self
            .changes_index(cx)
            .unwrap_or(self.editor.read(cx).tabs(cx).len()) as isize
            + 1;
        let index = (self.selected_index(cx) as isize + step).rem_euclid(count) as usize;
        crate::sounds::play(cues::Cue::Select, cues::PlayOptions::default());
        self.select_index(index, window, cx);
    }

    fn open_file(&mut self, action: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.open(&action.machine, &action.path, window, cx)
        });
    }

    fn open_file_at(&mut self, action: &OpenFileAt, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.open_at(&action.machine, &action.path, action.line, window, cx)
        });
    }

    /// Opens the Changes tab, or re-reads it if open, and shows it.
    fn open_changes(&mut self, _: &OpenChanges, window: &mut Window, cx: &mut Context<Self>) {
        crate::sounds::play(cues::Cue::Open, crate::sounds::subtle());
        match &self.changes {
            Some(changes) => changes.update(cx, |changes, cx| changes.reload(cx)),
            None => {
                let state = self.state.clone();
                self.changes = Some(cx.new(|cx| Changes::new(state, window, cx)));
            }
        }
        self.select(MainTab::Changes, window, cx);
    }

    fn close_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.changes = None;
        if self.selected == MainTab::Changes {
            self.select(MainTab::Herdr, window, cx);
        }
        cx.notify();
    }

    fn focus_pane(&mut self, action: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        self.select(MainTab::Herdr, window, cx);
        self.terminal_area.update(cx, |area, cx| {
            area.focus_pane(&action.machine, &action.pane_id, window, cx)
        });
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
        let active = cx.has_active_drag();
        let keep = cx
            .try_global::<KeepTerminalsDuringDrag>()
            .is_some_and(|keep| keep.0);
        if !active && keep {
            cx.set_global(KeepTerminalsDuringDrag(false));
        }
        // GPUI paints the drag preview into every window that draws during a
        // drag (the overlay too, for its toasts and tooltips) but redraws only
        // the one the drag ended in; any other would keep showing it.
        if active {
            self.drag_seen = true;
        } else if std::mem::take(&mut self.drag_seen) {
            window.defer(cx, |_, cx| {
                for handle in cx.windows() {
                    handle.update(cx, |_, window, _| window.refresh()).ok();
                }
            });
        }
        // GPUI only leaves keyboard modality on a mouse move or press, and a
        // drag from Finder brings neither, so every hitbox stays unhovered
        // and the drop lands nowhere. Move the pointer where it already is.
        if active && window.last_input_was_keyboard() {
            window.defer(cx, |window, cx| {
                let event = MouseMoveEvent {
                    position: window.mouse_position(),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: window.modifiers(),
                };
                window.dispatch_event(event.to_platform_input(), cx);
            });
        }
        let dragging = active && !keep;
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
            .on_action(cx.listener(Self::open_changes))
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
