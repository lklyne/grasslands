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
mod fs;
mod herdr;
mod herdr_prefs;
mod label;
mod logging;
mod machines;
mod overlay;
mod prompt;
#[cfg(debug_assertions)]
mod script;
mod settings;
mod setup;
mod sidebar;
mod sounds;
mod state;
mod status;
mod terminal_area;
mod theme;
mod updates;
mod window_registry;
mod workspace;
mod worktree;

use gpui_kit::*;

use crate::state::AppState;
use crate::workspace::Workspace;

fn main() {
    add_install_dirs_to_path();
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
            herdr_prefs::init(cx);
            settings::init(cx);
            updates::init(cx);
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

/// Apps started from Finder or the Dock get launchd's minimal `PATH`, so
/// `herdr` from Homebrew or `~/.local/bin` wouldn't be found, and a herdr
/// server the app starts would hand that `PATH` to every pane. Inside a
/// bundle, put the usual install locations first.
fn add_install_dirs_to_path() {
    let in_bundle = std::env::current_exe()
        .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    if !in_bundle {
        return;
    }
    let mut dirs = vec![
        std::path::PathBuf::from("/opt/homebrew/bin"),
        std::path::PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    let Ok(path) = std::env::join_paths(dirs) else {
        return;
    };
    // SAFETY: first thing in `main`, before logging or GPUI start any threads.
    unsafe { std::env::set_var("PATH", path) };
}

fn open_main_window(cx: &mut App) -> anyhow::Result<()> {
    let state = cx.new(|cx| AppState::new(setup::config_from_env(), cx));
    state.update(cx, AppState::start);
    herdr_prefs::attach(&state, cx);
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
            // The watcher follows herdr's config; coming back to the window
            // rereads it too, in case a change slipped past.
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    herdr_prefs::reload(cx);
                }
            })
            .detach();
            Workspace::new(state, window, cx)
        })
    })?;
    // Files named on the command line open in the editor.
    window.update(cx, |_, window, cx| {
        for path in std::env::args_os().skip(1) {
            window.dispatch_action(
                Box::new(actions::OpenFile {
                    path: path.into(),
                    machine: crate::state::MachineId::Local,
                }),
                cx,
            );
        }
    })?;
    #[cfg(debug_assertions)]
    script::start(window, cx);
    cx.activate(true);
    updates::start();
    Ok(())
}
