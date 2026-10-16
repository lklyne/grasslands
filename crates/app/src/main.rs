//! The rusted-tools desktop app: a GPUI Kit window over a headless herdr.

// The module seams are filled by parallel work; drop this once they are.
#![allow(dead_code)]

// GPUI's derive macros expand to `gpui::` paths; this resolves them through
// the Kit (see the facade note in gpui-kit's lib.rs).
extern crate gpui_kit as gpui;

mod actions;
mod chrome;
mod editor;
mod herdr;
mod placeholder;
mod sidebar;
mod state;
mod terminal_area;
mod theme;
mod workspace;

use gpui_kit::*;

use crate::state::AppState;
use crate::workspace::Workspace;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            actions::init(cx);
            theme::init(cx);
            editor::init(cx);
            terminal_area::init(cx);
            sidebar::init(cx);
            chrome::init(cx);
            if let Err(error) = open_main_window(cx) {
                eprintln!("could not open the window: {error:#}");
                cx.quit();
            }
        });
}

fn open_main_window(cx: &mut App) -> anyhow::Result<()> {
    let state = cx.new(|cx| AppState::new(herdr::Config::from_env(), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1400.), px(900.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some("rusted-tools".into()),
            ..TitlebarOptions::default()
        }),
        ..WindowOptions::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        theme::sync_with_system(window, cx);
        cx.new(|cx| Workspace::new(state, window, cx))
    })?;
    cx.activate(true);
    Ok(())
}
