//! Getting herdr ready: the setup screen the terminal area shows until the
//! app can connect, and the fixes it runs (install, update, restart).
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

use crate::chrome::Confirmation;
use crate::herdr::{self, Check, ConnectionStatus, Fix, InstallMethod, MIN_VERSION};
use crate::logging;
use crate::state::AppState;

/// Lines of a fix's output kept for the screen.
const OUTPUT_LINES: usize = 400;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Stage {
    /// `herdr status` is running.
    Checking,
    /// herdr needs something before the app can use it.
    Needed(Check),
    /// A fix is running; its output is in [`Setup::run`].
    Fixing(Fix),
    /// Nothing to do: the app connects (or is connected).
    #[default]
    Ready,
}

#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub stage: Stage,
    /// The last fix that ran, shown until the next one starts.
    pub run: Option<FixRun>,
}

#[derive(Debug, Clone)]
pub struct FixRun {
    /// The command as a person would type it.
    pub command: String,
    pub output: Vec<String>,
    /// Why it failed, once it has.
    pub failure: Option<String>,
}

impl FixRun {
    pub fn new(command: String) -> Self {
        Self {
            command,
            output: Vec::new(),
            failure: None,
        }
    }

    pub fn push(&mut self, line: String) {
        if self.output.len() == OUTPUT_LINES {
            self.output.remove(0);
        }
        self.output.push(line);
    }
}

impl Setup {
    pub fn is_ready(&self) -> bool {
        self.stage == Stage::Ready
    }
}

/// The setup screen for `state`, or the connection problem while there is
/// nothing to show yet; `None` when the terminal area should draw as usual.
pub fn render(state: &Entity<AppState>, cx: &App) -> Option<AnyElement> {
    let app = state.read(cx);
    let body = match &app.setup.stage {
        Stage::Ready => return connection_problem(app, cx),
        Stage::Checking => progress("Checking herdr…", None, cx),
        Stage::Fixing(_) => {
            let command = app.setup.run.as_ref().map(|run| run.command.clone());
            progress("Working on herdr…", command, cx)
        }
        Stage::Needed(check) => needed(state, check, cx),
    };
    let run = app.setup.run.as_ref().map(|run| output(run, cx));
    Some(
        v_flex()
            .id("herdr-setup")
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .p_6()
            .overflow_y_scroll()
            .child(body)
            .children(run)
            .into_any_element(),
    )
}

/// "Can't reach herdr" while the first read hasn't arrived and the last
/// attempt failed. Before that, the plain connecting message is enough.
fn connection_problem(app: &AppState, cx: &App) -> Option<AnyElement> {
    let ConnectionStatus::Reconnecting { reason } = &app.connection else {
        return None;
    };
    if app.snapshot.generation != 0 {
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
                        IconName::TriangleAlert,
                        "Can't reach herdr",
                        format!("{reason}. Retrying…"),
                        cx,
                    ))
                    .content(EmptyContent::new().child(open_log_button())),
            )
            .into_any_element(),
    )
}

fn needed(state: &Entity<AppState>, check: &Check, cx: &App) -> AnyElement {
    let config = state.read(cx).herdr_config.clone();
    let command = check
        .fix()
        .zip(config.as_ref())
        .and_then(|(fix, config)| config.fix_command(fix));
    let (title, description) = match check {
        Check::Ready => return div().into_any_element(),
        Check::Missing => (
            "herdr isn't installed".to_owned(),
            "grasslands runs on herdr, the agent multiplexer. It's installed the same way \
             you'd use it in a terminal."
                .to_owned(),
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
                text.push_str(" Update it through your Nix profile or flake, then check again.");
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
    let mut actions = h_flex().gap_2().justify_center();
    if let (Some(fix), Some(command)) = (check.fix(), command.as_ref()) {
        actions = actions.child(fix_button(state, fix));
        let shown = command.display().to_owned();
        actions = actions.child(command_line(shown, cx));
    }
    let check_again = {
        let state = state.clone();
        Button::new("herdr-setup-check")
            .label("Check Again")
            .on_click(move |_, _, cx| state.update(cx, |state, cx| state.check_herdr(cx)))
    };
    Empty::new()
        .header(header(IconName::TriangleAlert, title, description, cx))
        .content(
            EmptyContent::new()
                .child(actions)
                .child(h_flex().gap_2().child(check_again).child(open_log_button())),
        )
        .into_any_element()
}

fn fix_button(state: &Entity<AppState>, fix: Fix) -> Button {
    let label = match fix {
        Fix::Install => "Install herdr",
        Fix::Update(_) => "Update herdr",
        Fix::StopServer => "Restart herdr",
    };
    let state = state.clone();
    let button = Button::new("herdr-setup-fix").label(label);
    if fix != Fix::StopServer {
        return button
            .primary()
            .on_click(move |_, _, cx| state.update(cx, |state, cx| state.run_fix(fix, cx)));
    }
    button.danger().on_click(move |_, window, cx| {
        let agents = state.read(cx).snapshot.agents.len();
        let mut message =
            "Restarting herdr closes every pane in the session; whatever runs in them stops."
                .to_owned();
        if agents > 0 {
            message.push_str(&format!(
                " {agents} agent{} will stop.",
                if agents == 1 { "" } else { "s" }
            ));
        }
        let confirmation = Confirmation {
            title: "Restart herdr?".into(),
            message,
            confirm_label: "Restart herdr".into(),
        };
        let state = state.clone();
        let on_confirm = move |_: &mut Window, cx: &mut App| {
            state.update(cx, |state, cx| state.run_fix(Fix::StopServer, cx));
        };
        if let Some(chrome) = crate::chrome::get(window, cx) {
            chrome.update(cx, |chrome, cx| {
                chrome.confirm(confirmation, on_confirm, window, cx)
            });
        }
    })
}

/// The command a fix runs, with a copy button, for anyone who'd rather run
/// it in their own terminal.
fn command_line(command: String, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .gap_1()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_sm()
                .child(command.clone()),
        )
        .child(
            Clipboard::new("herdr-setup-copy")
                .tooltip("Copy command")
                .value(command),
        )
}

fn progress(title: &str, command: Option<String>, cx: &App) -> AnyElement {
    let header = EmptyHeader::new()
        .media(EmptyMedia::new().child(Spinner::new()))
        .title(EmptyTitle::new().child(title.to_owned()));
    let header = match command {
        Some(command) => header.description(
            EmptyDescription::new().child(
                div()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(command),
            ),
        ),
        None => header,
    };
    Empty::new().header(header).into_any_element()
}

/// The last fix's output, newest at the bottom, with its failure if it failed.
fn output(run: &FixRun, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let lines = run.output.iter().map(|line| div().child(line.clone()));
    let failure = run
        .failure
        .as_ref()
        .map(|failure| div().text_color(theme.danger).child(failure.clone()));
    v_flex()
        .id("herdr-setup-output")
        .w_full()
        .max_w(px(720.))
        .max_h(px(280.))
        .overflow_y_scroll()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(
            div()
                .text_color(theme.foreground)
                .child(format!("$ {}", run.command)),
        )
        .children(lines)
        .children(failure)
}

fn header(
    icon: IconName,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &App,
) -> EmptyHeader {
    EmptyHeader::new()
        .media(EmptyMedia::new().child(Icon::new(icon).text_color(cx.theme().warning)))
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
