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

use crate::herdr::{self, Check, ConnectionStatus, Fix, InstallMethod, MIN_VERSION};
use crate::logging;
use crate::state::AppState;

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

#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub stage: Stage,
    /// The last fix that ran. Its terminal stays on screen until the next
    /// fix replaces it or setup finishes.
    pub run: Option<FixRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixRun {
    /// Counts fixes, so each run gets a fresh terminal.
    pub id: u64,
    /// The shell command line the terminal runs.
    pub command: String,
}

impl Setup {
    pub fn is_ready(&self) -> bool {
        self.stage == Stage::Ready
    }
}

/// The setup screen for `state`, with the fix's `terminal` beside it once
/// one has run, or the connection problem while there is nothing to show
/// yet; `None` when the terminal area should draw as usual.
pub fn render(
    state: &Entity<AppState>,
    terminal: Option<AnyElement>,
    cx: &App,
) -> Option<AnyElement> {
    let app = state.read(cx);
    let card = match &app.setup.stage {
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
        Stage::Done => done(state, cx),
        Stage::Needed(check) => needed(state, check, terminal.is_some(), cx),
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
    let ConnectionStatus::Reconnecting { reason } = app.connection() else {
        return None;
    };
    if app.snapshot().generation != 0 {
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
fn needed(state: &Entity<AppState>, check: &Check, fix_ran: bool, cx: &App) -> AnyElement {
    let config = state.read(cx).herdr_config.clone();
    let command = check
        .fix()
        .zip(config.as_ref())
        .and_then(|(fix, config)| config.fix_command(fix));
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
    let description = if fix_ran {
        format!("{description} That didn't fix it; the terminal says why.")
    } else {
        description
    };
    let mut actions = v_flex().gap_3().items_center();
    if let Some(fix) = check.fix().filter(|_| command.is_some()) {
        actions = actions.child(fix_button(state, fix));
    }
    if let Some(command) = command.filter(|_| !fix_ran) {
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

fn done(state: &Entity<AppState>, cx: &App) -> AnyElement {
    let state = state.clone();
    let finish = Button::new("herdr-setup-finish")
        .primary()
        .label("Finish Setup")
        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.finish_setup(cx)));
    Empty::new()
        .header(header(
            Icon::new(IconName::Check).text_color(cx.theme().success),
            "herdr is ready",
            "Everything grasslands needs is set up.",
        ))
        .content(EmptyContent::new().child(finish))
        .into_any_element()
}

fn fix_button(state: &Entity<AppState>, fix: Fix) -> Button {
    let label = match fix {
        Fix::Install => "Install herdr",
        Fix::Update(_) => "Update herdr",
        Fix::StopServer => "Restart herdr",
    };
    let state = state.clone();
    Button::new("herdr-setup-fix")
        .primary()
        .label(label)
        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.run_fix(fix, cx)))
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
