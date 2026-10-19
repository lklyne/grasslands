//! A terminal that runs one command until it exits, outside herdr: the
//! herdr setup fix (`crate::setup::fix_terminal`) and a machine's sign-in,
//! setup or add (`crate::machines::run_terminal`). Each flow owns its
//! instance and says which run it wants; the terminal area only places it
//! and says whether it's on screen.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::*;

use super::terminals::{self, Terminal};

/// One run of a command: a new `id` gets a fresh terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRun {
    pub id: u64,
    /// The shell command line.
    pub command: String,
}

/// How a run's terminal ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Its process exited (the output stays on screen).
    Exited,
    /// Its terminal couldn't start.
    FailedToStart(String),
}

/// Told once per run, when it ends.
pub type OnEnded = Rc<dyn Fn(Ended, &mut Window, &mut App)>;

/// What a sync does with the terminal on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    Keep,
    /// Hide the old terminal (if any) and start one for the wanted run.
    Replace,
    /// Hide and drop the old terminal.
    Drop,
}

fn plan(current: Option<u64>, wanted: Option<u64>) -> Plan {
    match (current, wanted) {
        (_, None) if current.is_some() => Plan::Drop,
        (_, None) => Plan::Keep,
        (Some(current), Some(wanted)) if current == wanted => Plan::Keep,
        (_, Some(_)) => Plan::Replace,
    }
}

struct Running {
    id: u64,
    terminal: Entity<Terminal>,
    end: EndWatch,
}

/// Whether a run's end was reported, so it's reported once.
#[derive(Debug, Default)]
struct EndWatch {
    reported: bool,
}

impl EndWatch {
    /// Whether the run has just ended: its process isn't `alive` and that
    /// wasn't reported yet.
    fn just_ended(&mut self, alive: bool) -> bool {
        let ended = !alive && !self.reported;
        self.reported |= !alive;
        ended
    }
}

pub struct CommandTerminal {
    running: Option<Running>,
    /// The run whose terminal couldn't start, so it isn't tried again.
    failed: Option<u64>,
    /// Whether the terminal area shows it; a native view has to be hidden
    /// explicitly.
    visible: bool,
    on_ended: OnEnded,
    /// The owner's subscriptions that call [`Self::sync`].
    _source: Vec<Subscription>,
    _liveness: Task<()>,
}

impl CommandTerminal {
    pub fn new(on_ended: OnEnded, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            running: None,
            failed: None,
            visible: true,
            on_ended,
            _source: Vec::new(),
            _liveness: Self::watch_liveness(window, cx),
        }
    }

    /// Keeps `subscription` (the owner's, calling [`Self::sync`]) as long as
    /// the terminal lives.
    pub fn follow(&mut self, subscription: Subscription) {
        self._source.push(subscription);
    }

    /// Shows `wanted`'s terminal, starting one for a new run and dropping it
    /// once there is none.
    pub fn sync(
        &mut self,
        wanted: Option<CommandRun>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.running.as_ref().map(|r| r.id).or(self.failed);
        match plan(current, wanted.as_ref().map(|r| r.id)) {
            Plan::Keep => return,
            Plan::Drop => self.hide_old(cx),
            Plan::Replace => {
                self.hide_old(cx);
                let Some(run) = wanted else { return };
                match terminals::spawn_command(run.command, window, cx) {
                    Ok(terminal) => {
                        let visible = self.visible;
                        terminal.update(cx, |terminal, _| terminal.set_visible(visible));
                        self.running = Some(Running {
                            id: run.id,
                            terminal,
                            end: EndWatch::default(),
                        });
                    }
                    Err(error) => {
                        tracing::error!("command terminal: {error:#}");
                        self.failed = Some(run.id);
                        (self.on_ended)(Ended::FailedToStart(format!("{error:#}")), window, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    fn hide_old(&mut self, cx: &mut Context<Self>) {
        self.failed = None;
        if let Some(old) = self.running.take() {
            old.terminal
                .update(cx, |terminal, _| terminal.set_visible(false));
        }
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if let Some(running) = &self.running {
            running
                .terminal
                .update(cx, |terminal, _| terminal.set_visible(visible));
        }
    }

    /// The run's terminal, while it has one.
    pub fn terminal(&self) -> Option<&Entity<Terminal>> {
        self.running.as_ref().map(|r| &r.terminal)
    }

    /// The terminal only reports whether its process is alive, so check once
    /// a second.
    fn watch_liveness(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    let Some(running) = this.running.as_mut() else {
                        return;
                    };
                    let alive = running.terminal.read(cx).is_alive();
                    if running.end.just_ended(alive) {
                        (this.on_ended)(Ended::Exited, window, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{EndWatch, Plan, plan};

    #[test]
    fn a_new_run_gets_a_terminal() {
        assert_eq!(plan(None, Some(0)), Plan::Replace);
    }

    #[test]
    fn the_same_run_keeps_its_terminal() {
        assert_eq!(plan(Some(3), Some(3)), Plan::Keep);
    }

    #[test]
    fn a_later_run_replaces_the_terminal() {
        assert_eq!(plan(Some(3), Some(4)), Plan::Replace);
    }

    #[test]
    fn no_run_drops_the_terminal() {
        assert_eq!(plan(Some(3), None), Plan::Drop);
    }

    #[test]
    fn nothing_to_run_and_nothing_running_does_nothing() {
        assert_eq!(plan(None, None), Plan::Keep);
    }

    #[test]
    fn a_run_ends_once() {
        let mut end = EndWatch::default();
        let ended: Vec<bool> = [true, false, false]
            .into_iter()
            .map(|alive| end.just_ended(alive))
            .collect();
        assert_eq!(ended, [false, true, false]);
    }
}
