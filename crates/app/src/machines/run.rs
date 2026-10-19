//! The card beside a machine command's terminal (sign in, set up, add): the
//! setup screen's pattern (`crate::setup`), with herdr's own command doing
//! the work on the right and the app checking the machine when it exits.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyContent};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex};
use gpui_kit::*;

use crate::chrome::Callback;
use crate::setup;
use crate::state::{AppEvent, AppState, MachineTask, RunStage};
use crate::terminal_area::command_terminal::{CommandRun, CommandTerminal, OnEnded};

/// The terminal a machine's sign-in, setup or add runs in
/// (`AppState::machine_run`): each run gets a fresh one, and its end (or
/// failure to start) has the app check the machine.
pub fn run_terminal(
    state: &Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CommandTerminal> {
    let wanted = |state: &AppState| {
        state.machine_run().map(|run| CommandRun {
            id: run.id,
            command: run.command.clone(),
        })
    };
    let on_ended: OnEnded = {
        let state = state.downgrade();
        Rc::new(move |_, _, cx| {
            state
                .update(cx, |state, cx| state.machine_run_exited(cx))
                .ok();
        })
    };
    cx.new(|cx| {
        let mut terminal = CommandTerminal::new(on_ended, window, cx);
        terminal.follow(cx.subscribe_in(
            state,
            window,
            move |terminal, state, event, window, cx| {
                if matches!(event, AppEvent::MachineRunChanged) {
                    terminal.sync(wanted(state.read(cx)), window, cx);
                }
            },
        ));
        terminal.sync(wanted(state.read(cx)), window, cx);
        terminal
    })
}

/// What the card shows about a run.
pub struct RunCard<'a> {
    pub task: &'a MachineTask,
    pub stage: &'a RunStage,
    /// The machine's label (for an add, once herdr saved it).
    pub label: Option<&'a str>,
    /// The machine's status reason when the run failed.
    pub detail: Option<&'a str>,
}

/// The card for `run`. `on_close` ends the run (Cancel, Close, Done);
/// `on_retry` runs it again.
pub fn run_card(run: RunCard, on_close: Callback, on_retry: Callback, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let name = match (run.label, run.task) {
        (Some(label), _) => label.to_owned(),
        (None, MachineTask::Add { host }) => host.clone(),
        (None, _) => "the machine".to_owned(),
    };
    let close = |label: &'static str| {
        let on_close = on_close.clone();
        Button::new("machine-run-close")
            .ghost()
            .label(label)
            .on_click(move |_, window, cx| on_close(window, cx))
    };
    match run.stage {
        RunStage::Running => {
            let (title, description) = match run.task {
                MachineTask::SignIn(_) => (
                    format!("Signing in to {name}…"),
                    "It's running in the terminal. Answer ssh's prompts there.",
                ),
                MachineTask::SetUp(_) => (
                    format!("Setting up herdr on {name}…"),
                    "herdr's setup is running in the terminal. Answer its prompts there, then \
                     quit herdr.",
                ),
                MachineTask::Add { .. } => (
                    format!("Adding {name}…"),
                    "herdr is setting it up in the terminal. If it asks for anything, like a \
                     password, answer there.",
                ),
            };
            card(setup::progress(&title, Some(description)), close("Cancel"))
        }
        RunStage::Checking => card(
            setup::progress(&format!("Connecting to {name}…"), None),
            close("Cancel"),
        ),
        RunStage::Done => {
            let done = Button::new("machine-run-done")
                .primary()
                .label("Done")
                .on_click(move |_, window, cx| on_close(window, cx));
            Empty::new()
                .header(setup::header(
                    Icon::new(IconName::Check).text_color(theme.success),
                    format!("{name} is ready"),
                    "Its workspaces are in the sidebar.",
                ))
                .content(EmptyContent::new().child(done))
                .into_any_element()
        }
        RunStage::Failed => {
            let title = match (run.task, run.label) {
                (MachineTask::Add { .. }, None) => "No machine was added".to_owned(),
                (MachineTask::Add { .. }, Some(_)) => {
                    format!("{name} was added but isn't connected")
                }
                _ => format!("{name} still isn't connected"),
            };
            let description = match run.detail.map(|d| d.trim().trim_end_matches('.')) {
                Some(detail) if !detail.is_empty() => format!("{detail}. The terminal says why."),
                _ => "The terminal says why.".to_owned(),
            };
            let retry = Button::new("machine-run-retry")
                .primary()
                .label("Try Again")
                .on_click(move |_, window, cx| on_retry(window, cx));
            Empty::new()
                .header(setup::header(
                    Icon::new(IconName::TriangleAlert).text_color(theme.warning),
                    title,
                    description,
                ))
                .content(
                    EmptyContent::new().child(h_flex().gap_2().child(retry).child(close("Close"))),
                )
                .into_any_element()
        }
    }
}

/// A progress card with its one way out under it.
fn card(progress: AnyElement, close: Button) -> AnyElement {
    gpui_kit::component::v_flex()
        .items_center()
        .gap_3()
        .child(progress)
        .child(close)
        .into_any_element()
}
