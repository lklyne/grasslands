//! UI pieces for herdr's saved SSH machines (docs/decisions/0019-remote-machines.md):
//! the sidebar's group header and its menu, the Add Machine dialog, the
//! terminal area's offline screen and the file panel's empty state. Each takes
//! plain data and callbacks, so the app wires them to its machine connections.

mod add_dialog;
pub mod ssh_config;

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chrome::{self, Callback, Confirmation, MenuCommand, MenuEntry};

pub use add_dialog::open_add_machine;
pub use ssh_config::SshHost;

/// What a machine's header and screens show about its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineState {
    Connected,
    Connecting,
    NeedsSignIn,
    NeedsSetup,
    Offline,
    Disabled,
}

impl MachineState {
    /// herdr-client's status, plus its reason for the screens' detail line.
    pub fn of(status: &crate::herdr::MachineStatus) -> (Self, Option<&str>) {
        use crate::herdr::MachineStatus as Status;
        match status {
            Status::Connecting => (Self::Connecting, None),
            Status::Connected => (Self::Connected, None),
            Status::NeedsSignIn(reason) => (Self::NeedsSignIn, Some(reason)),
            Status::NeedsSetup(reason) => (Self::NeedsSetup, Some(reason)),
            Status::Offline(reason) => (Self::Offline, Some(reason)),
            Status::Disabled(reason) => (Self::Disabled, Some(reason)),
        }
    }

    /// The status dot: green connected, yellow connecting, orange when it
    /// needs you, gray offline or disabled.
    pub fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Connected => theme.success,
            Self::Connecting => theme.warning,
            // The theme has no orange; between its warning and danger.
            Self::NeedsSignIn | Self::NeedsSetup => theme.warning.blend(theme.danger.opacity(0.5)),
            Self::Offline | Self::Disabled => theme.muted_foreground.opacity(0.6),
        }
    }

    /// The short note beside the label, when the dot alone isn't enough.
    pub fn note(self) -> Option<&'static str> {
        match self {
            Self::Connected | Self::Connecting => None,
            Self::NeedsSignIn => Some("Needs sign-in"),
            Self::NeedsSetup => Some("Needs setup"),
            Self::Offline => Some("Offline"),
            Self::Disabled => Some("Disabled"),
        }
    }

    /// Whether its workspaces draw dimmed, their input off.
    pub fn is_dimmed(self) -> bool {
        !matches!(self, Self::Connected)
    }
}

type PointCallback = Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;
/// A UI callback with the user's answer: a new name, or enabled or not.
pub type AnswerCallback<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;

/// A sidebar group's header: chevron, status dot, label, status note and
/// a ··· button. Build with [`machine_header`].
#[derive(IntoElement)]
pub struct MachineHeader {
    id: SharedString,
    label: SharedString,
    state: MachineState,
    collapsed: bool,
    on_toggle: Option<Callback>,
    on_menu: Option<PointCallback>,
}

/// The header for the machine `id` (any string unique among the headers).
pub fn machine_header(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    state: MachineState,
) -> MachineHeader {
    MachineHeader {
        id: id.into(),
        label: label.into(),
        state,
        collapsed: false,
        on_toggle: None,
        on_menu: None,
    }
}

impl MachineHeader {
    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    /// Clicking the header folds or unfolds its group.
    pub fn on_toggle(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }

    /// Shows the ··· button; `f` gets the click's window position, where
    /// [`open_machine_menu`] anchors the menu. Right-click calls it too.
    pub fn on_menu(mut self, f: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static) -> Self {
        self.on_menu = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for MachineHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let id = self.id;
        let chevron = Icon::new(if self.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        })
        .xsmall()
        .text_color(theme.muted_foreground);
        let dot = div()
            .flex_shrink_0()
            .size(px(7.))
            .rounded_full()
            .bg(self.state.color(cx));
        let note = self.state.note().map(|note| {
            div()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(note)
        });
        let more = self.on_menu.clone().map(|on_menu| {
            Button::new(SharedString::from(format!("machine-more-{id}")))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_menu(event.position(), window, cx);
                })
        });
        let on_toggle = self.on_toggle;
        let on_right_click = self.on_menu;
        h_flex()
            .id(SharedString::from(format!("machine-header-{id}")))
            .h(px(24.))
            .mt_1()
            .pl(px(2.))
            .gap_1p5()
            .text_sm()
            .rounded(theme.radius)
            .hover(|this| this.bg(theme.list_hover))
            .child(chevron)
            .child(dot)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground.opacity(0.85))
                    .child(self.label),
            )
            .children(note)
            .child(div().flex_1())
            .children(more)
            .when_some(on_toggle, |this, on_toggle| {
                this.cursor_pointer()
                    .on_click(move |_, window, cx| on_toggle(window, cx))
            })
            .when_some(on_right_click, |this, on_menu| {
                this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    cx.stop_propagation();
                    on_menu(event.position, window, cx);
                })
            })
    }
}

/// What a machine's menu entries do. Rename and Remove ask first (a name
/// prompt, a confirmation) and call back with the answer.
#[derive(Clone)]
pub struct MachineActions {
    pub on_reconnect: Callback,
    /// Sign In… (needs sign-in) or Set Up… (needs setup).
    pub on_fix: Callback,
    pub on_rename: AnswerCallback<String>,
    /// With the new value: `false` disables.
    pub on_set_enabled: AnswerCallback<bool>,
    pub on_remove: Callback,
}

/// The ··· menu for the machine `label`: Reconnect, Sign In… or Set Up…
/// when that's the problem, Rename…, Disable or Enable, Remove….
pub fn machine_menu(label: &str, state: MachineState, actions: MachineActions) -> Vec<MenuEntry> {
    let callback = |f: Callback| MenuCommand::Callback(f);
    let mut entries = vec![MenuEntry::Item {
        label: "Reconnect".into(),
        command: callback(actions.on_reconnect),
        disabled: state == MachineState::Disabled,
    }];
    let fix = match state {
        MachineState::NeedsSignIn => Some("Sign In…"),
        MachineState::NeedsSetup => Some("Set Up…"),
        _ => None,
    };
    if let Some(fix) = fix {
        entries.push(MenuEntry::item(fix, callback(actions.on_fix)));
    }
    entries.push(MenuEntry::Separator);

    let current = label.to_owned();
    let on_rename = actions.on_rename;
    entries.push(MenuEntry::item(
        "Rename…",
        callback(Rc::new(move |window, cx| {
            let on_rename = on_rename.clone();
            crate::prompt::ask_name(
                "Rename Machine",
                "Rename",
                &current,
                move |name, window, cx| {
                    let name = name.trim();
                    if !name.is_empty() {
                        on_rename(name.to_owned(), window, cx);
                    }
                },
                window,
                cx,
            );
        })),
    ));
    let enable = state == MachineState::Disabled;
    let on_set_enabled = actions.on_set_enabled;
    entries.push(MenuEntry::item(
        if enable { "Enable" } else { "Disable" },
        callback(Rc::new(move |window, cx| {
            on_set_enabled(enable, window, cx)
        })),
    ));
    entries.push(MenuEntry::Separator);

    let label = label.to_owned();
    let on_remove = actions.on_remove;
    entries.push(MenuEntry::item(
        "Remove…",
        callback(Rc::new(move |window, cx| {
            let confirmation = Confirmation {
                title: format!("Remove {label}?"),
                message: format!(
                    "herdr forgets this machine. Whatever runs on {label} keeps running; \
                     add it again to get back to it."
                ),
                confirm_label: "Remove".into(),
            };
            let on_remove = on_remove.clone();
            if let Some(chrome) = chrome::get(window, cx) {
                chrome.update(cx, |chrome, cx| {
                    chrome.confirm(confirmation, move |w, cx| on_remove(w, cx), window, cx)
                });
            }
        })),
    ));
    entries
}

/// Opens `entries` (from [`machine_menu`]) as a context menu at `position`
/// in the main window `window`.
pub fn open_machine_menu(
    entries: Vec<MenuEntry>,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(chrome) = chrome::get(window, cx) {
        chrome.update(cx, |chrome, cx| chrome.menu(entries, position, window, cx));
    }
}

/// The terminal area for a machine that isn't connected: what's wrong, with
/// Reconnect and, when given, Sign In… or Set Up…. `detail` is the
/// connection's own reason (e.g. ssh's error), shown under the title.
pub fn offline_screen(
    label: &str,
    state: MachineState,
    detail: Option<&str>,
    on_reconnect: Callback,
    on_fix: Option<Callback>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    // ssh's reasons often end in a period of their own.
    let detail = detail
        .map(|d| d.trim().trim_end_matches('.'))
        .filter(|d| !d.is_empty());
    let with_detail = |text: String| match detail {
        Some(detail) => format!("{detail}. {text}"),
        None => text,
    };
    let muted = |icon: Icon| icon.text_color(theme.muted_foreground);
    let (media, title, description, fix_label) = match state {
        MachineState::Connected | MachineState::Connecting => (
            EmptyMedia::new().child(Spinner::new()),
            format!("Connecting to {label}…"),
            None,
            None,
        ),
        MachineState::Offline => (
            EmptyMedia::new().child(muted(Icon::new(assets::IconName::WifiOff))),
            format!("{label} is offline"),
            Some(with_detail(
                "Your panes keep running there; grasslands reconnects on its own.".into(),
            )),
            Some("Sign In…"),
        ),
        MachineState::NeedsSignIn => (
            EmptyMedia::new()
                .child(Icon::new(assets::IconName::KeyRound).text_color(theme.warning)),
            format!("{label} needs you to sign in"),
            Some(with_detail(
                "SSH wants a password, passphrase or host key approval. Sign in from a \
                 terminal and grasslands connects."
                    .into(),
            )),
            Some("Sign In…"),
        ),
        MachineState::NeedsSetup => (
            EmptyMedia::new().child(Icon::new(IconName::TriangleAlert).text_color(theme.warning)),
            format!("herdr on {label} needs setup"),
            Some(with_detail(
                "herdr's setup runs in a terminal so you can answer its prompts.".into(),
            )),
            Some("Set Up…"),
        ),
        MachineState::Disabled => (
            EmptyMedia::new().child(muted(Icon::new(assets::IconName::ServerOff))),
            format!("{label} is disabled"),
            Some("Enable it from its menu in the sidebar to connect.".to_owned()),
            None,
        ),
    };
    let mut header = EmptyHeader::new()
        .media(media)
        .title(EmptyTitle::new().child(title));
    if let Some(description) = description {
        header = header.description(EmptyDescription::new().child(description));
    }
    let fix_first = matches!(state, MachineState::NeedsSignIn | MachineState::NeedsSetup);
    let fix = fix_label.zip(on_fix).map(|(label, on_fix)| {
        Button::new("machine-offline-fix")
            .label(label)
            .when(fix_first, |b| b.primary())
            .on_click(move |_, window, cx| on_fix(window, cx))
    });
    let reconnect = (!matches!(
        state,
        MachineState::Disabled | MachineState::Connected | MachineState::Connecting
    ))
    .then(|| {
        Button::new("machine-offline-reconnect")
            .label("Reconnect")
            .when(!fix_first, |b| b.primary())
            .on_click(move |_, window, cx| on_reconnect(window, cx))
    });
    let mut buttons = h_flex().gap_2();
    buttons = if fix_first {
        buttons.children(fix).children(reconnect)
    } else {
        buttons.children(reconnect).children(fix)
    };
    v_flex()
        .id("machine-offline")
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            Empty::new()
                .header(header)
                .content(EmptyContent::new().child(buttons)),
        )
        .into_any_element()
}

/// The file panel while the active workspace is on the machine `label`:
/// the file tree and editor read this Mac's disk only.
pub fn remote_files_empty(label: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_4()
        .child(
            Empty::new().header(
                EmptyHeader::new()
                    .media(
                        EmptyMedia::new()
                            .child(Icon::new(IconName::Folder).text_color(theme.muted_foreground)),
                    )
                    .title(
                        EmptyTitle::new().child(format!("Files on {label} aren't available yet")),
                    )
                    .description(EmptyDescription::new().child(format!(
                        "The file tree and editor read this Mac's disk. Use the terminal for \
                         files on {label} for now."
                    ))),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{MachineActions, MachineState, machine_menu};
    use crate::chrome::MenuEntry;

    fn labels(state: MachineState) -> Vec<String> {
        let noop = Rc::new(|_: &mut gpui_kit::Window, _: &mut gpui_kit::App| {});
        let actions = MachineActions {
            on_reconnect: noop.clone(),
            on_fix: noop.clone(),
            on_rename: Rc::new(|_, _, _| {}),
            on_set_enabled: Rc::new(|_, _, _| {}),
            on_remove: noop,
        };
        machine_menu("box", state, actions)
            .into_iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item { label, .. } => Some(label.to_string()),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn a_machine_needing_setup_offers_set_up() {
        assert_eq!(
            labels(MachineState::NeedsSetup),
            ["Reconnect", "Set Up…", "Rename…", "Disable", "Remove…"]
        );
    }

    #[test]
    fn a_disabled_machine_offers_enable() {
        assert_eq!(
            labels(MachineState::Disabled),
            ["Reconnect", "Rename…", "Enable", "Remove…"]
        );
    }
}
