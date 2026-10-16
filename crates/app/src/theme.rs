//! Light and dark theme: follows the system at start, toggled from the View menu.

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::*;

use crate::actions::ToggleTheme;

pub fn init(cx: &mut App) {
    cx.on_action(|_: &ToggleTheme, cx| {
        let mode = if cx.theme().is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(mode, None, cx);
    });
}

pub fn sync_with_system(window: &mut Window, cx: &mut App) {
    Theme::sync_system_appearance(Some(window), cx);
}
