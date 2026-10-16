//! A labelled empty region, used by each module's view until it is built.

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

pub fn placeholder(title: &'static str, note: impl Into<SharedString>, cx: &App) -> Div {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .child(div().text_sm().child(title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(note.into()),
        )
}
