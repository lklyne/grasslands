//! Spike: two herdr terminals as Ghostty views inside a GPUI Kit resizable
//! split, each with its own Ghostty config, plus a transparent overlay window
//! that draws a popover above them.
//!
//! ```sh
//! scripts/herdr-dev workspace create --label spike --no-focus   # then split it once
//! scripts/dev-run cargo run --manifest-path spikes/ghostty-kit/Cargo.toml            # interactive
//! scripts/dev-run cargo run --manifest-path spikes/ghostty-kit/Cargo.toml -- --script # scripted, screenshots to shots/
//! ```
//!
//! Keys (interactive): cmd-1 toggles the right pane, cmd-2 toggles the overlay
//! popover, cmd-3 swaps the right terminal's config file.

mod herdr;
mod native;
mod script;

use std::path::PathBuf;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use gpui_libghostty::{TerminalConfiguration, TerminalOptions};

gpui_libghostty::bind_gpui!(gpui_kit);

actions!(spike, [ToggleRight, TogglePopover, SwapConfig]);

fn config_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("config")
        .join(name)
}

pub struct Spike {
    left: Entity<Terminal>,
    right: Entity<Terminal>,
    split: Entity<ResizableState>,
    right_visible: bool,
    alt_config: bool,
    overlay: Option<WindowHandle<Overlay>>,
    status: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl Spike {
    fn new(terminals: &[herdr::Pane], window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut spawn = |pane: &herdr::Pane, config: &str, focus: bool| {
            let mut options =
                TerminalOptions::new(herdr::attach_command(&pane.terminal_id), home());
            options.configuration = TerminalConfiguration::File(config_path(config));
            options.focus_on_spawn = focus;
            options.quiet_login = true;
            Terminal::spawn(options, window, cx).unwrap_or_else(|error| {
                // A spike has nothing useful to show without its terminals.
                eprintln!("ghostty-kit: spawn {}: {error}", pane.pane_id);
                std::process::exit(1)
            })
        };
        let left = spawn(&terminals[0], "left.ghostty", true);
        let right = spawn(&terminals[1], "right.ghostty", false);
        let subscriptions =
            vec![
                cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
                    this.sync_overlay_frame(window, cx);
                }),
            ];
        Self {
            left,
            right,
            split: cx.new(|_| ResizableState::default()),
            right_visible: true,
            alt_config: false,
            overlay: None,
            status: format!(
                "left {} ({})  right {} ({})",
                terminals[0].pane_id,
                terminals[0].terminal_id,
                terminals[1].pane_id,
                terminals[1].terminal_id
            )
            .into(),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_right_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.right_visible = visible;
        // A native view that is no longer laid out keeps its last frame, so it
        // has to be hidden explicitly. Showing waits for the next prepaint to
        // give it a fresh frame (set_frame re-applies the requested visibility).
        self.right
            .update(cx, |terminal, _| terminal.set_visible(visible));
        cx.notify();
    }

    pub fn swap_config(&mut self, cx: &mut Context<Self>) {
        self.alt_config = !self.alt_config;
        let name = if self.alt_config {
            "right-alt.ghostty"
        } else {
            "right.ghostty"
        };
        let result = self.right.update(cx, |terminal, _| {
            terminal.load_config_file(&config_path(name))
        });
        self.status = match result {
            Ok(()) => format!("right terminal now uses {name}").into(),
            Err(error) => format!("config swap failed: {error}").into(),
        };
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

    pub fn left_width(&self, cx: &App) -> f64 {
        self.split
            .read(cx)
            .sizes()
            .first()
            .map_or(0.0, |width| f64::from(f32::from(*width)))
    }

    pub fn toggle_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(overlay) = self.overlay.take() {
            overlay
                .update(cx, |_, window, _| window.remove_window())
                .ok();
            return;
        }
        match Overlay::open(window, cx) {
            Ok(handle) => self.overlay = Some(handle),
            Err(error) => self.status = format!("overlay failed: {error:#}").into(),
        }
        cx.notify();
    }

    fn sync_overlay_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay else { return };
        let Some(parent) = native::ns_window(window) else {
            return;
        };
        overlay
            .update(cx, |_, overlay_window, _| {
                if let Some(child) = native::ns_window(overlay_window) {
                    native::match_content_frame(&parent, &child);
                }
            })
            .ok();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .items_center()
            .text_sm()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .border_b_1()
            .border_color(cx.theme().border)
            .child("ghostty-kit spike")
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("cmd-1 right pane  cmd-2 popover  cmd-3 swap config"),
            )
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

impl Render for Spike {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut split = h_resizable("terminals")
            .with_state(&self.split)
            .child(resizable_panel().size(px(560.)).child(self.left.clone()));
        if self.right_visible {
            split = split.child(resizable_panel().child(self.right.clone()));
        }
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &ToggleRight, _, cx| {
                this.set_right_visible(!this.right_visible, cx)
            }))
            .on_action(
                cx.listener(|this, _: &TogglePopover, window, cx| this.toggle_popover(window, cx)),
            )
            .on_action(cx.listener(|this, _: &SwapConfig, _, cx| this.swap_config(cx)))
            .child(self.toolbar(cx))
            .child(div().flex_1().min_h_0().child(split))
    }
}

/// A borderless, transparent child window that covers the main window's
/// content area. GPUI draws into it like any window; AppKit keeps it above the
/// parent (and so above the Ghostty views) and moves it with the parent.
pub struct Overlay;

impl Overlay {
    fn open(parent: &mut Window, cx: &mut App) -> anyhow::Result<WindowHandle<Self>> {
        let parent_window = native::ns_window(parent)
            .ok_or_else(|| anyhow::anyhow!("main window has no NSWindow"))?;
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(parent.bounds())),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..WindowOptions::default()
        };
        let handle = cx.open_window(options, |_, cx| cx.new(|_| Overlay))?;
        handle.update(cx, |_, window, _| -> anyhow::Result<()> {
            let child = native::ns_window(window)
                .ok_or_else(|| anyhow::anyhow!("overlay has no NSWindow"))?;
            native::attach_overlay(&parent_window, &child);
            Ok(())
        })??;
        Ok(handle)
    }
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        // Anchored over the right half, where the right terminal sits.
        let origin = point(viewport.width * 0.55, px(90.));
        div().size_full().child(
            v_flex()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .w(px(300.))
                .p_3()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .shadow_lg()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Popover in the overlay window"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "Drawn by GPUI in a transparent child window above the Ghostty views.",
                        ),
                )
                .child(div().h(px(28.)).rounded_md().bg(cx.theme().primary)),
        )
    }
}

fn main() {
    let scripted = std::env::args().any(|arg| arg == "--script");
    let terminals = match herdr::spike_panes() {
        Ok(panes) => panes,
        Err(error) => {
            eprintln!("ghostty-kit: {error:#}");
            std::process::exit(1);
        }
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-1", ToggleRight, None),
                KeyBinding::new("cmd-2", TogglePopover, None),
                KeyBinding::new("cmd-3", SwapConfig, None),
                KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(140.), px(100.)),
                    size(px(1200.), px(720.)),
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("ghostty-kit spike".into()),
                    ..TitlebarOptions::default()
                }),
                ..WindowOptions::default()
            };
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Spike::new(&terminals, window, cx))
            });
            let (window, spike) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    eprintln!("ghostty-kit: could not open the window: {error:#}");
                    cx.quit();
                    return;
                }
            };
            cx.activate(true);
            if scripted {
                cx.spawn(async move |cx| script::run(window, spike, cx).await)
                    .detach();
            }
        });
}

actions!(spike, [Quit]);
