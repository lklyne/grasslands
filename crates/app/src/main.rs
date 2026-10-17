//! The grasslands desktop app: a GPUI Kit window over a headless herdr.

// The module seams are filled by parallel work; drop this once they are.
#![allow(dead_code)]

// GPUI's derive macros expand to `gpui::` paths; this resolves them through
// the Kit (see the facade note in gpui-kit's lib.rs).
extern crate gpui_kit as gpui;

mod actions;
mod assets;
mod attention;
mod changes;
mod chrome;
mod editor;
mod herdr;
mod logging;
mod overlay;
mod prompt;
mod settings;
mod sidebar;
mod sounds;
mod state;
mod terminal_area;
mod theme;
mod workspace;
mod worktree;

use gpui_kit::*;

use crate::state::AppState;
use crate::workspace::Workspace;

fn main() {
    let _log_guard = logging::init();
    tracing::info!(
        "grasslands starting; logs in {}",
        logging::log_dir()
            .map_or_else(|| "stderr only".to_owned(), |dir| dir.display().to_string())
    );
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            actions::init(cx);
            theme::init(cx);
            sounds::init(cx);
            settings::init(cx);
            editor::init(cx);
            terminal_area::init(cx);
            sidebar::init(cx);
            chrome::init(cx);
            workspace::init(cx);
            if let Err(error) = open_main_window(cx) {
                tracing::error!("could not open the window: {error:#}");
                cx.quit();
            }
        });
}

fn open_main_window(cx: &mut App) -> anyhow::Result<()> {
    let state = cx.new(|cx| AppState::new(herdr::Config::from_env(), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1400.), px(900.)), cx)),
        ..workspace::window_options()
    };
    let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::sync_with_system(window, cx);
        cx.new(|cx| {
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::sync_with_system(window, cx);
            })
            .detach();
            Workspace::new(state, window, cx)
        })
    })?;
    // Files named on the command line open in the editor.
    window.update(cx, |_, window, cx| {
        for path in std::env::args_os().skip(1) {
            window.dispatch_action(Box::new(actions::OpenFile { path: path.into() }), cx);
        }
    })?;
    cx.activate(true);
    Ok(())
}
