//! Getting herdr ready: the setup screen the terminal area shows until the
//! app can connect, and the fixes it runs (install, update, restart) in a
//! terminal beside it.
//!
//! The app uses the user's own herdr, so each fix goes through whatever
//! installed it; see `herdr_client::setup` and docs/decisions/0018-herdr-setup.md.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use gpui_kit::*;

use crate::herdr::{self, Check, Fix, InstallMethod, MIN_VERSION, Version};
use std::rc::Rc;

use crate::state::{AppState, MachineId, Status};
use crate::terminal_area::command_terminal::{CommandRun, CommandTerminal, Ended, OnEnded};
use crate::{logging, sounds};
use cues::{Cue, PlayOptions};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Stage {
    /// `herdr status` is running.
    Checking,
    /// herdr needs something before the app can use it.
    Needed(Check),
    /// A fix is running in the setup terminal; see [`Setup::run`].
    Fixing(Fix),
    /// herdr was set up here and is ready; the screen stays until the user
    /// finishes, while the app connects behind it.
    Done,
    /// Nothing to do: the app connects (or is connected).
    #[default]
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixRun {
    /// Counts fixes, so each run gets a fresh terminal.
    pub id: u64,
    /// The shell command line the terminal runs.
    pub command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupEvent {
    /// A check passed: Local's herdr can be connected to.
    HerdrReady,
    /// A check ended or a fix started: [`Setup::needs_setup`] may have
    /// changed, and Local's machine status follows it.
    StageChanged,
}

/// What Local's herdr needs before the app can use it, and the fix running.
/// `AppState` owns it and starts Local's sync on [`SetupEvent::HerdrReady`];
/// the terminal area draws its screen and runs its fixes.
pub struct Setup {
    /// Local's herdr; `None` when the endpoint was refused (nothing to check).
    config: Option<herdr::Config>,
    stage: Stage,
    /// The last fix that ran. Its terminal stays on screen until the next
    /// fix replaces it or setup finishes.
    run: Option<FixRun>,
    /// The command for the fix `Needed` offers, found with the check.
    fix_command: Option<herdr::FixCommand>,
    /// Why the last fix's terminal couldn't start.
    spawn_error: Option<String>,
    /// The old server version last sent to the check, so it's checked once
    /// rather than on every snapshot.
    old_server_checked: Option<String>,
    _check: Option<Task<()>>,
}

impl EventEmitter<SetupEvent> for Setup {}

impl Setup {
    /// Idle until [`Self::check_herdr`].
    pub fn new(config: Option<herdr::Config>) -> Self {
        Self {
            config,
            stage: Stage::Ready,
            run: None,
            fix_command: None,
            spawn_error: None,
            old_server_checked: None,
            _check: None,
        }
    }

    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The fix whose terminal is on screen, while setup isn't over.
    pub fn run(&self) -> Option<&FixRun> {
        self.run.as_ref().filter(|_| !self.is_ready())
    }

    pub fn is_ready(&self) -> bool {
        self.stage == Stage::Ready
    }

    /// herdr needs something, or a fix is running: Local can't be used yet.
    pub fn needs_setup(&self) -> bool {
        matches!(self.stage, Stage::Needed(_) | Stage::Fixing(_))
    }

    /// Checks the herdr binary and server (`herdr status`); emits
    /// [`SetupEvent::HerdrReady`] when they're fine, else shows the screen.
    pub fn check_herdr(&mut self, cx: &mut Context<Self>) {
        self.check(false, cx);
    }

    /// `after_fix` when the setup terminal's command just ended: the check
    /// says whether the fix worked.
    fn check(&mut self, after_fix: bool, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        // While a fix runs, only its end says anything new.
        if matches!(self.stage, Stage::Fixing(_)) && !after_fix {
            return;
        }
        let was = self.stage.clone();
        // A screen already showing stays put while it checks again.
        if !matches!(was, Stage::Needed(_)) {
            self.stage = Stage::Checking;
            cx.notify();
        }
        let shown = matches!(was, Stage::Needed(_)) || self.run.is_some();
        // The fix's command searches PATH (for brew), so it's found here
        // rather than on every render of the setup screen.
        let check = cx.background_executor().spawn(async move {
            let check = config.check();
            let command = check.fix().and_then(|fix| config.fix_command(fix));
            (check, command)
        });
        self._check = Some(cx.spawn(async move |this, cx| {
            let (check, command) = check.await;
            this.update(cx, |this, cx| {
                this.checked(check, command, was, after_fix, shown, cx)
            })
            .ok();
        }));
    }

    /// A check's result: `was` is the stage it started from, `shown`
    /// whether the screen was up.
    fn checked(
        &mut self,
        check: Check,
        command: Option<herdr::FixCommand>,
        was: Stage,
        after_fix: bool,
        shown: bool,
        cx: &mut Context<Self>,
    ) {
        tracing::info!("herdr check: {check:?}");
        if check == Check::Ready {
            if after_fix {
                sounds::play(Cue::Success, sounds::strong());
            }
            self.stage = if shown { Stage::Done } else { Stage::Ready };
            cx.emit(SetupEvent::HerdrReady);
        } else {
            let stage = Stage::Needed(check);
            if after_fix {
                sounds::play(Cue::Error, PlayOptions::default());
            } else if was != stage {
                sounds::play(Cue::Warning, PlayOptions::default());
            }
            self.stage = stage;
        }
        self.fix_command = command;
        cx.emit(SetupEvent::StageChanged);
        cx.notify();
    }

    /// Starts `fix` in the setup terminal; the terminal area runs it and
    /// calls [`Self::fix_exited`] when it ends.
    pub fn run_fix(&mut self, fix: Fix, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Needed(_)) {
            return;
        }
        let Some(command) = self.fix_command.clone() else {
            return;
        };
        // A check still running would overwrite `Fixing` when it ends.
        self._check = None;
        self.spawn_error = None;
        tracing::info!("herdr setup: running {}", command.display());
        sounds::play(Cue::Loading, PlayOptions::default());
        let id = self.run.as_ref().map_or(0, |run| run.id + 1);
        self.stage = Stage::Fixing(fix);
        self.run = Some(FixRun {
            id,
            command: command.terminal_command(),
        });
        cx.emit(SetupEvent::StageChanged);
        cx.notify();
    }

    /// The setup terminal couldn't start; the screen says why, then checks
    /// again.
    pub fn fix_failed_to_start(&mut self, error: String, cx: &mut Context<Self>) {
        self.spawn_error = Some(error);
        self.fix_exited(cx);
    }

    /// The setup terminal's command ended (or couldn't start); checks again.
    pub fn fix_exited(&mut self, cx: &mut Context<Self>) {
        if matches!(self.stage, Stage::Fixing(_)) {
            tracing::info!("herdr setup: the fix ended");
            self.check(true, cx);
        }
    }

    /// Closes the setup screen once herdr is ready.
    pub fn finish_setup(&mut self, cx: &mut Context<Self>) {
        if self.stage == Stage::Done {
            sounds::play(Cue::Tap, PlayOptions::default());
            self.stage = Stage::Ready;
            self.run = None;
            self.fix_command = None;
            self.spawn_error = None;
            cx.notify();
        }
    }

    /// Local's server reports `version`. One from an older herdr (started
    /// before an update, say) runs the check, which names it and offers the
    /// restart.
    pub fn server_seen(&mut self, version: &str, cx: &mut Context<Self>) {
        let old = Version::parse(version).is_some_and(|v| v < MIN_VERSION);
        let unchecked = self.old_server_checked.as_deref() != Some(version);
        if old && unchecked && self.is_ready() {
            // Once per version and connection: a check that comes back
            // `Ready` (a server it can't identify) mustn't run again on
            // every event.
            self.old_server_checked = Some(version.to_owned());
            self.check_herdr(cx);
        }
    }

    /// Local's connection dropped: the server it comes back to is checked
    /// afresh.
    pub fn connection_lost(&mut self) {
        self.old_server_checked = None;
    }

    /// A check that found `check`, for tests.
    #[cfg(test)]
    pub(crate) fn set_needed(&mut self, check: Check, cx: &mut Context<Self>) {
        self.stage = Stage::Needed(check);
        cx.emit(SetupEvent::StageChanged);
    }
}

/// The terminal the setup screen runs its fixes in: each fix gets a fresh
/// one, and its end (or failure to start) checks herdr again.
pub fn fix_terminal(
    setup: &Entity<Setup>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CommandTerminal> {
    let wanted = |setup: &Setup| {
        setup.run().map(|run| CommandRun {
            id: run.id,
            command: run.command.clone(),
        })
    };
    let on_ended: OnEnded = {
        let setup = setup.downgrade();
        Rc::new(move |ended, _, cx| {
            setup
                .update(cx, |setup, cx| match ended {
                    Ended::Exited => setup.fix_exited(cx),
                    Ended::FailedToStart(error) => setup.fix_failed_to_start(error, cx),
                })
                .ok();
        })
    };
    cx.new(|cx| {
        let mut terminal = CommandTerminal::new(on_ended, window, cx);
        terminal.follow(
            cx.observe_in(setup, window, move |terminal, setup, window, cx| {
                terminal.sync(wanted(setup.read(cx)), window, cx);
            }),
        );
        terminal.sync(wanted(setup.read(cx)), window, cx);
        terminal
    })
}

/// The setup screen, with the fix's `terminal` beside it once one has run,
/// or `app`'s connection problem while there is nothing to show yet; `None`
/// when the terminal area should draw as usual.
pub fn render(
    setup: &Entity<Setup>,
    app: &AppState,
    terminal: Option<AnyElement>,
    cx: &App,
) -> Option<AnyElement> {
    let card = match &setup.read(cx).stage {
        Stage::Ready => return connection_problem(app, cx),
        Stage::Checking => progress("Checking herdr…", None),
        Stage::Fixing(fix) => progress(
            match fix {
                Fix::Install => "Installing herdr…",
                Fix::Update(_) => "Updating herdr…",
                Fix::StopServer => "Restarting herdr…",
            },
            Some(
                "It's running in the terminal. If it asks for anything, like your password, answer there.",
            ),
        ),
        Stage::Done => done(setup, cx),
        Stage::Needed(check) => needed(setup, check, terminal.is_some(), cx),
    };
    Some(screen("herdr-setup", card, terminal, cx))
}

/// A setup card, centred in the terminal area, or on the left with the
/// `terminal` running its command on the right.
pub(crate) fn screen(
    id: &'static str,
    card: AnyElement,
    terminal: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    let card = v_flex()
        .id(id)
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .overflow_y_scroll()
        .child(card);
    let Some(terminal) = terminal else {
        return card.into_any_element();
    };
    h_flex()
        .size_full()
        .child(div().w(px(380.)).flex_none().h_full().child(card))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(terminal),
        )
        .into_any_element()
}

/// "Can't reach herdr" while the first read hasn't arrived and the last
/// attempt failed. Before that, the plain connecting message is enough.
fn connection_problem(app: &AppState, cx: &App) -> Option<AnyElement> {
    let local = app.machine(&MachineId::Local, cx)?;
    let Status::Offline {
        reason,
        retrying: true,
    } = local.status()
    else {
        return None;
    };
    if local.snapshot().generation != 0 {
        return None;
    }
    Some(
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(
                Empty::new()
                    .header(header(
                        Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning),
                        "Can't reach herdr",
                        format!("{reason}. Retrying…"),
                    ))
                    .content(EmptyContent::new().child(open_log_button())),
            )
            .into_any_element(),
    )
}

/// What herdr needs and the button that fixes it. Before a fix has run, the
/// command is offered too, for anyone who'd rather run it in their own
/// terminal; after one, the fix's terminal beside the screen says why it
/// didn't work.
fn needed(setup: &Entity<Setup>, check: &Check, fix_ran: bool, cx: &App) -> AnyElement {
    let setup_ref = setup.read(cx);
    let command = setup_ref.fix_command.clone();
    let spawn_error = setup_ref.spawn_error.clone();
    let (title, description) = match check {
        Check::Ready => return div().into_any_element(),
        Check::Missing => (
            "herdr isn't installed".to_owned(),
            "grasslands runs on herdr, the agent multiplexer.".to_owned(),
        ),
        Check::Broken { binary, reason } => (
            "herdr couldn't run".to_owned(),
            format!("{}: {reason}", binary.display()),
        ),
        Check::Outdated(installed) => {
            let version = installed
                .version
                .map_or_else(|| "An older herdr".to_owned(), |v| format!("herdr {v}"));
            let mut text = format!(
                "{version} is installed at {}; grasslands needs {MIN_VERSION} or newer.",
                installed.binary.display()
            );
            if installed.method == InstallMethod::Nix {
                text.push_str(" Update it through your Nix profile or flake, then come back here.");
            }
            ("herdr needs an update".to_owned(), text)
        }
        Check::ServerOutdated { server, .. } => {
            let running =
                server.map_or_else(|| "an older herdr".to_owned(), |v| format!("herdr {v}"));
            (
                "Restart herdr to finish updating".to_owned(),
                format!(
                    "The herdr server still running is {running}; grasslands needs \
                     {MIN_VERSION} or newer. Restarting closes every pane in the session."
                ),
            )
        }
    };
    let description = match (&spawn_error, fix_ran) {
        (Some(error), _) => format!("{description} Couldn't open a terminal for the fix: {error}."),
        (None, true) => format!("{description} That didn't fix it; the terminal says why."),
        (None, false) => description,
    };
    let mut actions = v_flex().gap_3().items_center();
    if let Some(fix) = check.fix().filter(|_| command.is_some()) {
        actions = actions.child(fix_button(setup, fix));
    }
    if let Some(command) = command.filter(|_| !fix_ran || spawn_error.is_some()) {
        actions = actions.child(command_line(command.display().to_owned(), cx));
    }
    Empty::new()
        .header(header(
            Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning),
            title,
            description,
        ))
        .content(EmptyContent::new().child(actions))
        .into_any_element()
}

fn done(setup: &Entity<Setup>, cx: &App) -> AnyElement {
    let setup = setup.clone();
    let finish = Button::new("herdr-setup-finish")
        .primary()
        .label("Finish Setup")
        .on_click(move |_, _, cx| setup.update(cx, |setup, cx| setup.finish_setup(cx)));
    Empty::new()
        .header(header(
            Icon::new(IconName::Check).text_color(cx.theme().success),
            "herdr is ready",
            "Everything grasslands needs is set up.",
        ))
        .content(EmptyContent::new().child(finish))
        .into_any_element()
}

fn fix_button(setup: &Entity<Setup>, fix: Fix) -> Button {
    let label = match fix {
        Fix::Install => "Install herdr",
        Fix::Update(_) => "Update herdr",
        Fix::StopServer => "Restart herdr",
    };
    let setup = setup.clone();
    Button::new("herdr-setup-fix")
        .primary()
        .label(label)
        .on_click(move |_, _, cx| setup.update(cx, |setup, cx| setup.run_fix(fix, cx)))
}

/// The command a fix runs, with a copy button, for anyone who'd rather run
/// it in their own terminal.
pub(crate) fn command_line(command: String, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .max_w(px(480.))
        .gap_1()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child("Or run")
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.foreground)
                .child(command.clone()),
        )
        .child(
            Clipboard::new("herdr-setup-copy")
                .tooltip("Copy command")
                .value(command),
        )
}

pub(crate) fn progress(title: &str, description: Option<&str>) -> AnyElement {
    let header = EmptyHeader::new()
        .media(EmptyMedia::new().child(Spinner::new()))
        .title(EmptyTitle::new().child(title.to_owned()));
    let header = match description {
        Some(description) => {
            header.description(EmptyDescription::new().child(description.to_owned()))
        }
        None => header,
    };
    Empty::new().header(header).into_any_element()
}

pub(crate) fn header(
    icon: Icon,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) -> EmptyHeader {
    EmptyHeader::new()
        .media(EmptyMedia::new().child(icon))
        .title(EmptyTitle::new().child(title.into()))
        .description(EmptyDescription::new().child(description.into()))
}

fn open_log_button() -> Button {
    Button::new("herdr-setup-log")
        .ghost()
        .label("Show Logs")
        .on_click(|_, _, cx| {
            if let Some(dir) = logging::log_dir() {
                cx.reveal_path(&dir);
            }
        })
}

/// Where a herdr server the app starts writes its output.
pub fn server_log() -> Option<std::path::PathBuf> {
    logging::log_dir().map(|dir| dir.join("herdr-server.log"))
}

/// The config the app runs with: the environment's, logging the server.
pub fn config_from_env() -> herdr::Result<herdr::Config> {
    herdr::Config::from_env().map(|config| herdr::Config {
        server_log: server_log(),
        ..config
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::{AppContext as _, Entity, TestAppContext};

    use super::{Setup, SetupEvent, Stage};
    use crate::herdr;
    use crate::state::offline_config;

    fn needing_install(cx: &mut TestAppContext) -> Entity<Setup> {
        cx.new(|_| {
            let mut setup = Setup::new(Some(offline_config()));
            setup.stage = Stage::Needed(herdr::Check::Missing);
            setup.fix_command = Some(herdr::FixCommand::new(vec!["true".into()]));
            setup
        })
    }

    fn events(setup: &Entity<Setup>, cx: &mut TestAppContext) -> Rc<RefCell<Vec<SetupEvent>>> {
        let events = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(setup, move |_, event, _| events.borrow_mut().push(*event))
                .detach();
        });
        events
    }

    #[gpui::test]
    fn a_check_started_before_a_fix_doesnt_overwrite_it(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        setup.update(cx, |setup, cx| {
            setup.check_herdr(cx);
            setup.run_fix(herdr::Fix::Install, cx);
        });
        cx.run_until_parked();
        let stage = cx.read(|cx| setup.read(cx).stage().clone());
        assert_eq!(stage, Stage::Fixing(herdr::Fix::Install));
    }

    #[gpui::test]
    fn checking_while_a_fix_runs_does_nothing(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        setup.update(cx, |setup, cx| {
            setup.run_fix(herdr::Fix::Install, cx);
            setup.check_herdr(cx);
        });
        cx.run_until_parked();
        let stage = cx.read(|cx| setup.read(cx).stage().clone());
        assert_eq!(stage, Stage::Fixing(herdr::Fix::Install));
    }

    #[gpui::test]
    fn a_second_fix_click_doesnt_start_another_run(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        setup.update(cx, |setup, cx| {
            setup.run_fix(herdr::Fix::Install, cx);
            setup.run_fix(herdr::Fix::Install, cx);
        });
        let run = cx.read(|cx| setup.read(cx).run().map(|r| r.id));
        assert_eq!(run, Some(0));
    }

    #[gpui::test]
    fn starting_a_fix_announces_the_stage(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        let events = events(&setup, cx);
        setup.update(cx, |setup, cx| setup.run_fix(herdr::Fix::Install, cx));
        cx.run_until_parked();
        assert_eq!(events.borrow().as_slice(), &[SetupEvent::StageChanged]);
    }

    #[gpui::test]
    fn a_fix_that_ends_checks_again(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        setup.update(cx, |setup, cx| {
            setup.run_fix(herdr::Fix::Install, cx);
            setup.fix_exited(cx);
        });
        let stage = cx.read(|cx| setup.read(cx).stage().clone());
        assert_eq!(stage, Stage::Checking);
    }

    #[gpui::test]
    fn a_fix_that_didnt_work_needs_setup_again(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        setup.update(cx, |setup, cx| {
            setup.run_fix(herdr::Fix::Install, cx);
            setup.fix_exited(cx);
        });
        cx.run_until_parked();
        let needs = cx.read(|cx| setup.read(cx).needs_setup());
        assert!(needs);
    }

    #[gpui::test]
    fn a_passing_check_announces_herdr_ready(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        let events = events(&setup, cx);
        setup.update(cx, |setup, cx| {
            let was = setup.stage.clone();
            setup.checked(herdr::Check::Ready, None, was, false, true, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            &[SetupEvent::HerdrReady, SetupEvent::StageChanged]
        );
    }

    #[gpui::test]
    fn a_check_passing_after_the_screen_showed_waits_for_finish(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        setup.update(cx, |setup, cx| {
            let was = setup.stage.clone();
            setup.checked(herdr::Check::Ready, None, was, true, true, cx);
        });
        let stage = cx.read(|cx| setup.read(cx).stage().clone());
        assert_eq!(stage, Stage::Done);
    }

    #[gpui::test]
    fn finishing_setup_closes_the_screen(cx: &mut TestAppContext) {
        let setup = needing_install(cx);
        setup.update(cx, |setup, cx| {
            setup.stage = Stage::Done;
            setup.finish_setup(cx);
        });
        let closed = cx.read(|cx| setup.read(cx).is_ready() && setup.read(cx).run().is_none());
        assert!(closed);
    }

    #[gpui::test]
    fn an_old_local_server_is_checked_once_per_version(cx: &mut TestAppContext) {
        let setup = cx.new(|_| Setup::new(Some(offline_config())));
        setup.update(cx, |setup, cx| setup.server_seen("0.9.0", cx));
        cx.run_until_parked();
        setup.update(cx, |setup, cx| {
            setup.stage = Stage::Ready;
            setup.server_seen("0.9.0", cx);
        });
        let stage = cx.read(|cx| setup.read(cx).stage().clone());
        assert_eq!(stage, Stage::Ready);
    }
}
