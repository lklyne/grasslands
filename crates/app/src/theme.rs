//! Light and dark theme: follows the system at start, toggled from the View
//! menu. The UI base size (the rem every `text_*`, spacing and rem-sized
//! control scales from) is 14px rather than the Kit's 16, for desktop
//! density: `text_xs` 10.5, `text_sm` 12.25, `text_base` 14.

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::*;

use crate::actions::ToggleTheme;

/// The UI base size. A theme reload resets it from the theme's tokens, so
/// every load goes through [`apply_base`].
const BASE: Pixels = px(14.);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &ToggleTheme, cx| {
        let mode = if cx.theme().is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(mode, None, cx);
        apply_base(cx);
    });
}

pub fn sync_with_system(window: &mut Window, cx: &mut App) {
    Theme::sync_system_appearance(Some(window), cx);
    apply_base(cx);
}

fn apply_base(cx: &mut App) {
    Theme::update(cx, |theme| theme.font_size = BASE);
}
