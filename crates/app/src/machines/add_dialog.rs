//! The Add Machine dialog: an SSH host field that suggests `Host` entries
//! from `~/.ssh/config` as you type. It is the Kit's command list in a
//! dialog: up and down move through the suggestions, Enter or Connect takes
//! the highlighted one, or the typed text when nothing matches. The
//! suggestions fill in once the config has been read off the main thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::base::actions::Confirm;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogDescription, DialogFooter};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::SshHost;

type Submit = Rc<dyn Fn(String, &mut App)>;

/// Opens the dialog, suggesting `hosts`. Connect calls `on_connect` with the
/// host text, in the main window `window`, after the dialog closes.
pub fn open_add_machine(
    hosts: Vec<SshHost>,
    on_connect: impl Fn(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    open_add_machine_with(Task::ready(hosts), on_connect, window, cx);
}

/// [`open_add_machine`], opening at once and suggesting `hosts` when the
/// task finishes.
pub fn open_add_machine_with(
    hosts: Task<Vec<SshHost>>,
    on_connect: impl Fn(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let main = window.window_handle();
    // `None` until the config has been read.
    let loaded: Rc<RefCell<Option<Rc<Vec<SshHost>>>>> = Rc::default();
    {
        let loaded = loaded.clone();
        cx.spawn(async move |cx| {
            let hosts = hosts.await;
            *loaded.borrow_mut() = Some(Rc::new(hosts));
            cx.update(|cx| cx.refresh_windows());
        })
        .detach();
    }
    let on_connect = Rc::new(on_connect);
    // Back in the main window once the dialog is gone, as `prompt::ask_name` does.
    let submit: Submit = Rc::new(move |host, cx| {
        let on_connect = on_connect.clone();
        cx.defer(move |cx| {
            cx.update_window(main, |_, window, cx| on_connect(host, window, cx))
                .ok();
        });
    });
    let open = move |window: &mut Window, cx: &mut App| {
        crate::sounds::play(cues::Cue::Open, cues::PlayOptions::default());
        let query = cx.new(|cx| CommandState::new(window, cx));
        let first_build = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first_build.replace(false) {
                query.update(cx, |query, cx| query.focus(window, cx));
            }
            let loading = loaded.borrow().is_none();
            let hosts = loaded.borrow().clone().unwrap_or_default();
            let chosen = {
                let (query, hosts) = (query.clone(), hosts.clone());
                move |cx: &App| chosen_host(&query, &hosts, cx)
            };
            let on_ok = {
                let (chosen, submit) = (chosen.clone(), submit.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| match chosen(cx) {
                    Some(host) if check_host(&host).is_ok() => {
                        submit(host, cx);
                        true
                    }
                    _ => false,
                }
            };
            // Enter goes to the list, which ignores it with nothing
            // highlighted; then it means the typed text.
            let typed_enter = {
                let (query, submit) = (query.clone(), submit.clone());
                move |_: &Confirm, window: &mut Window, cx: &mut App| {
                    if query.read(cx).selected_index().is_some() {
                        return;
                    }
                    let host = query.read(cx).query(cx).trim().to_owned();
                    cx.stop_propagation();
                    if check_host(&host).is_ok() {
                        submit(host, cx);
                        window.close_dialog(cx);
                    }
                }
            };
            let picked = {
                let (hosts, submit) = (hosts.clone(), submit.clone());
                move |index: gpui_kit::component::IndexPath, window: &mut Window, cx: &mut App| {
                    if let Some(host) = hosts.get(index.row)
                        && check_host(&host.alias).is_ok()
                    {
                        submit(host.alias.clone(), cx);
                        window.close_dialog(cx);
                    }
                }
            };
            let invalid = chosen(cx).map(|host| check_host(&host));
            let connectable = matches!(invalid, Some(Ok(())));
            let problem = invalid.and_then(Result::err);
            dialog
                .title("Add Machine")
                .w(px(440.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            DialogDescription::new().child(
                                "grasslands connects over SSH and uses herdr on that machine.",
                            ),
                        )
                        .child(
                            div()
                                .capture_action(typed_enter)
                                .child(host_list(&query, &hosts, loading, cx).on_confirm(picked)),
                        )
                        .when_some(problem, |this, problem| {
                            this.child(div().text_sm().text_color(cx.theme().danger).child(problem))
                        })
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Next, herdr's setup runs in a terminal beside the \
                                     terminals so you can answer sign-in or install prompts.",
                                ),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                        .child(
                            DialogAction::new().child(
                                Button::new("add-machine-connect")
                                    .primary()
                                    .label("Connect")
                                    .disabled(!connectable),
                            ),
                        ),
                )
                .on_ok(on_ok)
        });
    };
    match crate::chrome::get(window, cx) {
        Some(chrome) => chrome.update(cx, |chrome, cx| {
            chrome.floating(cx, open);
            chrome.close_context_menu(window, cx);
        }),
        None => open(window, cx),
    }
}

/// The highlighted suggestion's alias, else the typed text; `None` when
/// both are empty.
fn chosen_host(query: &Entity<CommandState>, hosts: &[SshHost], cx: &App) -> Option<String> {
    let state = query.read(cx);
    if let Some(host) = state
        .selected_index()
        .and_then(|index| hosts.get(index.row))
    {
        return Some(host.alias.clone());
    }
    let typed = state.query(cx).trim().to_owned();
    (!typed.is_empty()).then_some(typed)
}

/// Why `host` can't be passed to `herdr machine add`: empty, a leading `-`
/// (ssh would read it as an option) or whitespace.
fn check_host(host: &str) -> Result<(), &'static str> {
    if host.is_empty() {
        Err("Type a host.")
    } else if host.starts_with('-') {
        Err("A host can't start with \u{201c}-\u{201d}.")
    } else if host.contains(char::is_whitespace) {
        Err("A host can't contain spaces.")
    } else {
        Ok(())
    }
}

fn host_list(query: &Entity<CommandState>, hosts: &[SshHost], loading: bool, cx: &App) -> Command {
    let theme = cx.theme();
    let mono = theme.mono_font_family.clone();
    let muted = theme.muted_foreground;
    let items = hosts.iter().map(|host| {
        let alias: SharedString = host.alias.clone().into();
        let destination: SharedString = host.destination().into();
        let mono = mono.clone();
        CommandItem::new()
            .label(alias.clone())
            .keywords([destination.clone()])
            .child(move |_, _| {
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Icon::new(assets::IconName::Server)
                            .small()
                            .text_color(muted),
                    )
                    .child(div().min_w_0().truncate().child(alias.clone()))
                    .child(
                        div()
                            .ml_auto()
                            .flex_shrink_0()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(muted)
                            .child(destination.clone()),
                    )
            })
    });
    let has_hosts = !hosts.is_empty();
    Command::new(query)
        .max_h(px(220.))
        .placeholder("Host, user@host or ssh alias")
        .group(CommandGroup::new().label("From ~/.ssh/config").items(items))
        .empty(move |state, _, cx| {
            let query = state.query(cx);
            let text = if !query.trim().is_empty() {
                format!("Connect to {}", query.trim())
            } else if has_hosts {
                String::new()
            } else if loading {
                "Reading ~/.ssh/config\u{2026}".to_owned()
            } else {
                "No hosts in ~/.ssh/config. Type one, like user@host.".to_owned()
            };
            div()
                .px_2()
                .py_1p5()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        })
}

#[cfg(test)]
mod tests {
    use super::check_host;

    #[test]
    fn a_plain_host_is_accepted() {
        assert!(check_host("lyle@gpu-box:2222").is_ok());
    }

    #[test]
    fn an_empty_host_is_refused() {
        assert!(check_host("").is_err());
    }

    #[test]
    fn a_leading_dash_is_refused() {
        assert!(check_host("-oProxyCommand=x").is_err());
    }

    #[test]
    fn whitespace_is_refused() {
        assert!(check_host("me@box -v").is_err());
    }
}
