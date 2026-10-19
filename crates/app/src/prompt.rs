//! A one-field name prompt (GPUI Kit dialog) shared by the file tree and the
//! workspace list, and the workspace list's New Worktree prompt. `submit` runs on OK or Enter: the single-line input passes
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
use gpui_kit::component::v_flex;
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
    open_floating(open, window, cx);
}

/// What the New Worktree prompt asks for. Only the branch is required;
/// empty fields are `None`, leaving herdr's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewWorktree {
    pub branch: String,
    /// The ref a new branch starts from (`worktree.create --base`).
    pub base: Option<String>,
    /// The workspace's label (`--label`).
    pub label: Option<String>,
}

impl NewWorktree {
    /// From the prompt's raw fields, trimmed.
    pub fn from_fields(branch: &str, base: &str, label: &str) -> Self {
        let optional = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
        Self {
            branch: branch.trim().to_owned(),
            base: optional(base),
            label: optional(label),
        }
    }
}

/// The New Worktree prompt: the branch first and focused, then an optional
/// base ref and label. OK or Enter in any field submits.
pub fn ask_new_worktree(
    submit: impl Fn(NewWorktree, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let main = window.window_handle();
    let submit = Rc::new(submit);
    let open = move |window: &mut Window, cx: &mut App| {
        let field = |placeholder: &'static str, window: &mut Window, cx: &mut App| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let branch = field("Branch", window, cx);
        let base = field("Start from (optional): branch, tag or commit", window, cx);
        let label = field("Label (optional)", window, cx);
        let first_build = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first_build.replace(false) {
                branch.focus_handle(cx).focus(window, cx);
            }
            let (fields, submit) = ((branch.clone(), base.clone(), label.clone()), submit.clone());
            dialog
                .title("New Worktree")
                .w(px(420.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(Input::new(&branch))
                        .child(Input::new(&base))
                        .child(Input::new(&label)),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("worktree-ok").primary().label("Create")),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let (branch, base, label) = &fields;
                    let value = NewWorktree::from_fields(
                        &branch.read(cx).value(),
                        &base.read(cx).value(),
                        &label.read(cx).value(),
                    );
                    let submit = submit.clone();
                    cx.defer(move |cx| {
                        cx.update_window(main, |_, window, cx| submit(value, window, cx))
                            .ok();
                    });
                    true
                })
        });
    };
    open_floating(open, window, cx);
}

fn open_floating(open: impl FnOnce(&mut Window, &mut App) + 'static, window: &mut Window, cx: &mut App) {
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

#[cfg(test)]
mod tests {
    use super::NewWorktree;

    #[test]
    fn empty_optional_fields_leave_herdr_defaults() {
        assert_eq!(
            NewWorktree::from_fields(" feat ", "  ", ""),
            NewWorktree {
                branch: "feat".into(),
                base: None,
                label: None,
            }
        );
    }

    #[test]
    fn filled_fields_are_trimmed() {
        let value = NewWorktree::from_fields("feat", " origin/main ", " Feat ");
        assert_eq!(
            (value.base.as_deref(), value.label.as_deref()),
            (Some("origin/main"), Some("Feat"))
        );
    }
}
