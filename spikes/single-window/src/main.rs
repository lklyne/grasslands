//! Spike: one window, Ghostty terminals *below* GPUI.
//!
//! The window is transparent, so GPUI's Metal layer is non-opaque and cleared
//! to alpha 0. Each Ghostty view is moved under GPUI's view (`native.rs`), and
//! GPUI paints nothing over the terminal slots, so the terminals show through
//! those holes. Menus, dialogs and toasts are ordinary GPUI elements in the
//! same window, drawn over the terminals with GPUI's normal focus and
//! hit-testing. Input still reaches the terminals the way it always has: the
//! Ghostty view never hit-tests, GPUI gets every event and the `Terminal`
//! element forwards it through libghostty's surface input API.
//!
//! ```sh
//! scripts/herdr-dev workspace create --label spike --no-focus   # then split it once
//! scripts/dev-run cargo run --manifest-path spikes/single-window/Cargo.toml                   # by hand
//! scripts/dev-run cargo run --manifest-path spikes/single-window/Cargo.toml -- --script --quit
//! ```
//!
//! Keys: cmd-1 toggles the right pane, cmd-2 opens a menu over the right
//! terminal, cmd-3 opens a dialog with a text input, cmd-4 posts a toast.

mod herdr;
mod native;
mod script;

use std::path::PathBuf;

use gpui_kit::base::Root;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_libghostty::{TerminalConfiguration, TerminalOptions};

gpui_libghostty::bind_gpui!(gpui_kit);

actions!(spike, [ToggleRight, OpenMenu, OpenDialog, Toast, Quit]);

pub const TOOLBAR_HEIGHT: f32 = 32.;
/// Width of the GPUI-painted frame around each terminal hole. GPUI draws it
/// on top of the Ghostty view's edge, so it doesn't need a gap.
pub const FRAME_INSET: f32 = 2.;

fn config_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("config")
        .join(name)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

pub struct Menu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

pub struct Spike {
    pub left: Entity<Terminal>,
    pub right: Entity<Terminal>,
    split: Entity<ResizableState>,
    right_visible: bool,
    pub menu: Option<Menu>,
    pub dialog_input: Option<Entity<InputState>>,
    pub picked: Option<SharedString>,
    status: SharedString,
}

impl Spike {
    fn new(terminals: &[herdr::Pane], window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut spawn = |pane: &herdr::Pane, config: &str, focus: bool| {
            let mut options =
                TerminalOptions::new(herdr::attach_command(&pane.terminal_id), home());
            options.configuration = TerminalConfiguration::File(config_path(config));
            options.focus_on_spawn = focus;
            options.quiet_login = true;
            let terminal = Terminal::spawn(options, window, cx).unwrap_or_else(|error| {
                // A spike has nothing to show without its terminals.
                eprintln!("single-window: spawn {}: {error}", pane.pane_id);
                std::process::exit(1)
            });
            let moved = native::lower_terminal_views(window);
            eprintln!("single-window: moved {moved} Ghostty view(s) below GPUI");
            terminal
        };
        let left = spawn(&terminals[0], "left.ghostty", true);
        let right = spawn(&terminals[1], "right.ghostty", false);
        // The left terminal's background, so a strip no view has drawn yet
        // matches the terminal rather than showing the desktop.
        native::set_backdrop(
            window,
            0x10 as f64 / 255.,
            0x18 as f64 / 255.,
            0x2a as f64 / 255.,
        );
        Self {
            left,
            right,
            split: cx.new(|_| ResizableState::default()),
            right_visible: true,
            menu: None,
            dialog_input: None,
            picked: None,
            status: format!(
                "left {}  right {}",
                terminals[0].pane_id, terminals[1].pane_id
            )
            .into(),
        }
    }

    pub fn set_right_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right_visible = visible;
        // A focused element that leaves the tree takes key dispatch with it
        // (actions no longer reach this view), so hand focus over first.
        if !visible && self.right.focus_handle(cx).contains_focused(window, cx) {
            self.left
                .update(cx, |terminal, cx| terminal.focus(window, cx));
        }
        // Still needed below GPUI: the left terminal's view grows into the
        // right one's slot, and sibling views stack in insertion order, so a
        // stale right view would sit on top of the left terminal there.
        self.right
            .update(cx, |terminal, _| terminal.set_visible(visible));
        cx.notify();
    }

    pub fn resize_divider(
        &mut self,
        left_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split.update(cx, |split, cx| {
            split.resize_panel(0, left_width, window, cx)
        });
    }

    pub fn left_width(&self, cx: &App) -> f32 {
        self.split
            .read(cx)
            .sizes()
            .first()
            .map_or(0.0, |width| f32::from(*width))
    }

    /// A Kit `PopupMenu` anchored over the right terminal, as a plain GPUI
    /// element in this window.
    pub fn open_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let spike = cx.entity().downgrade();
        let view = PopupMenu::build(window, cx, move |mut menu, _, _| {
            for label in ["Split right", "Rename pane", "Close pane"] {
                let spike = spike.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    spike
                        .update(cx, |spike, cx| {
                            spike.picked = Some(label.into());
                            spike.status = format!("menu picked: {label}").into();
                            cx.notify();
                        })
                        .ok();
                }));
            }
            menu.min_w(px(200.))
        });
        let dismiss = cx.subscribe_in(&view, window, |spike, _, _: &DismissEvent, window, cx| {
            spike.menu = None;
            // Back to the terminal it was opened over.
            spike
                .left
                .update(cx, |terminal, cx| terminal.focus(window, cx));
            cx.notify();
        });
        view.focus_handle(cx).focus(window, cx);
        self.menu = Some(Menu {
            view,
            position,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    /// A Kit dialog with a text input, drawn over both terminals.
    pub fn open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Workspace name"));
        self.dialog_input = Some(input.clone());
        let focused = std::cell::Cell::new(false);
        window.open_dialog(cx, move |dialog, window, cx| {
            if !focused.replace(true) {
                input.focus_handle(cx).focus(window, cx);
            }
            dialog
                .title("New workspace")
                .w(px(420.))
                .child(Input::new(&input))
        });
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .items_center()
            .text_sm()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .border_b_1()
            .border_color(cx.theme().border)
            .child("single-window spike")
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("cmd-1 right  cmd-2 menu  cmd-3 dialog  cmd-4 toast"),
            )
    }

    /// A terminal slot: GPUI paints only a frame and a corner badge; the
    /// inside stays transparent, so the Ghostty view below shows through.
    fn slot(
        &self,
        terminal: &Entity<Terminal>,
        label: &'static str,
        window: &Window,
        cx: &App,
    ) -> impl IntoElement {
        let focused = terminal.focus_handle(cx).contains_focused(window, cx);
        div()
            .relative()
            .size_full()
            .child(terminal.clone())
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .border(px(FRAME_INSET))
                    .border_color(if focused {
                        cx.theme().ring
                    } else {
                        cx.theme().border
                    }),
            )
            .child(
                // Drawn by GPUI inside the hole, over the terminal text: the
                // kind of thing the overlay design can't do in the main window.
                div()
                    .absolute()
                    .right(px(10.))
                    .bottom(px(10.))
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .child(label),
            )
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let left = self.slot(&self.left, "left · GPUI badge", window, cx);
        // A one-panel Kit group keeps the left panel at full width after the
        // right one comes back, so the hidden state isn't a group at all.
        let body = if self.right_visible {
            h_resizable("terminals")
                .with_state(&self.split)
                .child(resizable_panel().size(px(560.)).child(left))
                .child(resizable_panel().child(self.slot(
                    &self.right,
                    "right · GPUI badge",
                    window,
                    cx,
                )))
                .into_any_element()
        } else {
            left.into_any_element()
        };
        let menu = self.menu.as_ref().map(|menu| {
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(menu.view.clone()),
            )
            .with_priority(1)
        });
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &ToggleRight, window, cx| {
                this.set_right_visible(!this.right_visible, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMenu, window, cx| {
                let at = point(px(this.left_width(cx) + 40.), px(TOOLBAR_HEIGHT + 60.));
                this.open_menu(at, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenDialog, window, cx| this.open_dialog(window, cx)))
            .on_action(cx.listener(|_, _: &Toast, window, cx| {
                window.push_notification("A toast over the terminals, same window", cx)
            }))
            .child(self.toolbar(cx))
            .child(div().flex_1().min_h_0().child(body))
            .children(menu)
    }
}

fn main() {
    let scripted = std::env::args().any(|arg| arg == "--script");
    let terminals = match herdr::spike_panes() {
        Ok(panes) => panes,
        Err(error) => {
            eprintln!("single-window: {error:#}");
            std::process::exit(1);
        }
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-1", ToggleRight, None),
                KeyBinding::new("cmd-2", OpenMenu, None),
                KeyBinding::new("cmd-3", OpenDialog, None),
                KeyBinding::new("cmd-4", Toast, None),
                KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(140.), px(100.)),
                    size(px(1200.), px(720.)),
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("single-window spike".into()),
                    ..TitlebarOptions::default()
                }),
                // Non-opaque CAMetalLayer cleared to alpha 0 (gpui-pre-macos
                // `set_background_appearance`).
                window_background: WindowBackgroundAppearance::Transparent,
                ..WindowOptions::default()
            };
            let mut built = None;
            let opened = cx.open_window(options, |window, cx| {
                let spike = cx.new(|cx| Spike::new(&terminals, window, cx));
                built = Some(spike.clone());
                // Root paints the theme background over the whole window by
                // default, which would cover every terminal.
                cx.new(|cx| Root::new(spike, window, cx).bg(transparent_black()))
            });
            let (window, spike) = match (opened, built) {
                (Ok(window), Some(spike)) => (window.into(), spike),
                (Err(error), _) => {
                    eprintln!("single-window: could not open the window: {error:#}");
                    cx.quit();
                    return;
                }
                (Ok(_), None) => return,
            };
            cx.activate(true);
            if scripted {
                cx.spawn(async move |cx| script::run(window, spike, cx).await)
                    .detach();
            }
        });
}
