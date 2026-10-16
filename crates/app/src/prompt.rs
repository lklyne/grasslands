//! A one-field name prompt (GPUI Kit dialog) shared by the file tree and the
//! workspace list. `submit` runs on OK or Enter: the single-line input passes
//! Enter on to the dialog, whose `Confirm` runs `on_ok`. Cancel, Escape and ×
//! close it without submitting.
//!
//! Actions dispatched from `submit` start in the Kit's dialog layer, outside
//! `Workspace`, and never reach it; defer them until the dialog has closed.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;

pub fn ask_name(
    title: &'static str,
    ok_label: &'static str,
    initial: &str,
    submit: impl Fn(&str, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial.to_owned()));
    let submit = Rc::new(submit);
    let first_build = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first_build.replace(false) {
            input.focus_handle(cx).focus(window, cx);
        }
        let (input_ok, submit) = (input.clone(), submit.clone());
        dialog
            .title(title)
            .w(px(420.))
            .child(Input::new(&input))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                    .child(
                        DialogAction::new()
                            .child(Button::new("prompt-ok").primary().label(ok_label)),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let value = input_ok.read(cx).value().to_string();
                submit(&value, window, cx);
                true
            })
    });
}
