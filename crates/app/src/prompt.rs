//! A one-field name prompt (GPUI Kit dialog) shared by the file tree and the
//! workspace list. `submit` runs on OK or Enter: the single-line input passes
//! Enter on to the dialog, whose `Confirm` runs `on_ok`. Cancel, Escape and ×
//! close it without submitting.
//!
//! The dialog draws where the chrome draws floating UI (the overlay above the
//! terminals), or in the main window when there is no chrome (headless tests).
//! Either way `submit` runs afterwards in the main window, so it can dispatch
//! actions to `Workspace` and focus views there.

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
    let main = window.window_handle();
    let submit = Rc::new(submit);
    let initial = initial.to_owned();
    let open = move |window: &mut Window, cx: &mut App| {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
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
                .on_ok(move |_, _, cx| {
                    let value = input_ok.read(cx).value().to_string();
                    let submit = submit.clone();
                    cx.defer(move |cx| {
                        cx.update_window(main, |_, window, cx| submit(&value, window, cx))
                            .ok();
                    });
                    true
                })
        });
    };
    match crate::chrome::get(window, cx) {
        // An open context menu closes, as for `Chrome::confirm`, so the
        // prompt doesn't open on top of it. Both happen on the next tick;
        // queueing the prompt first keeps the overlay modal throughout, so
        // capture and key status don't flick back to the main window between.
        Some(chrome) => chrome.update(cx, |chrome, cx| {
            chrome.floating(cx, open);
            chrome.close_context_menu(window, cx);
        }),
        None => open(window, cx),
    }
}
