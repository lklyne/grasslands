//! The Settings window (cmd-,): GPUI Kit's `Settings` page in a window of
//! its own, so it never sits under the main window's native terminal views.
//! One window at a time; opening it again brings the open one forward.
//!
//! Appearance and Sounds are grasslands' own (`settings.json`); the herdr
//! page shows and edits herdr's `config.toml` (`crate::herdr_prefs`).

use gpui_kit::component::button::Button;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, ThemeMode, ThemeRegistry,
    TitleBar, h_flex, v_flex,
};
use gpui_kit::*;

use crate::actions::OpenSettings;
use crate::herdr::config::{
    self as herdr_config, AgentPanelSort, AgentSound, Edit, MAX_TOAST_DELAY, SOUND_AGENTS,
    StatusIndicators, ToastDelivery,
};
use crate::herdr_prefs::{self, HerdrPrefs};
use crate::sounds;
use crate::theme::{self, Appearance};

/// The open Settings window, if any.
#[derive(Default)]
struct SettingsWindow(Option<AnyWindowHandle>);

impl Global for SettingsWindow {}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| {
        if let Err(error) = open(cx) {
            tracing::error!("could not open settings: {error:#}");
        }
    });
}

fn open(cx: &mut App) -> anyhow::Result<()> {
    let open = cx.default_global::<SettingsWindow>().0;
    if let Some(handle) = open
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return Ok(());
    }
    let mut options = TitleBar::window_options();
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some("Settings".into());
    }
    // Open on the display the user is working on, not the main one.
    let display_id = cx.active_window().and_then(|window| {
        window
            .update(cx, |_, window, cx| {
                window.display(cx).map(|display| display.id())
            })
            .ok()
            .flatten()
    });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            display_id,
            size(px(760.), px(520.)),
            cx,
        ))),
        display_id,
        window_min_size: Some(size(px(520.), px(360.))),
        ..options
    };
    let (handle, _) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            // herdr's config can change under the window (an editor, herdr,
            // the agents sort toggle); show it as it is.
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    herdr_prefs::reload(cx);
                }
            })
            .detach();
            SettingsView {
                _prefs: cx.observe_global::<HerdrPrefs>(|_, cx| cx.notify()),
            }
        })
    })?;
    cx.set_global(SettingsWindow(Some(handle)));
    Ok(())
}

struct SettingsView {
    _prefs: Subscription,
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new().child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Settings"),
                ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new("settings")
                        .sidebar_width(px(180.))
                        .page(appearance_page(cx))
                        .page(sounds_page())
                        .page(herdr_page(cx)),
                ),
            )
    }
}

fn appearance_page(cx: &App) -> SettingPage {
    let appearances = Appearance::ALL
        .iter()
        .map(|a| (a.key().into(), a.label().into()))
        .collect();
    SettingPage::new("Appearance")
        .icon(IconName::Palette)
        .default_open(true)
        .group(
            SettingGroup::new()
                .title("Theme")
                .item(
                    SettingItem::new(
                        "Appearance",
                        SettingField::dropdown(
                            appearances,
                            |cx| theme::settings(cx).appearance.key().into(),
                            |key: SharedString, cx| {
                                if let Some(appearance) = Appearance::from_key(&key) {
                                    theme::update_settings(cx, |s| s.appearance = appearance);
                                }
                            },
                        ),
                    )
                    .description("Follow the system's light or dark mode, or keep one."),
                )
                .item(
                    SettingItem::new(
                        "Light theme",
                        SettingField::scrollable_dropdown(
                            theme_options(ThemeMode::Light, cx),
                            |cx| theme::settings(cx).light.clone(),
                            |name, cx| pick(ThemeMode::Light, name, cx),
                        ),
                    )
                    .description("Used in light mode."),
                )
                .item(
                    SettingItem::new(
                        "Dark theme",
                        SettingField::scrollable_dropdown(
                            theme_options(ThemeMode::Dark, cx),
                            |cx| theme::settings(cx).dark.clone(),
                            |name, cx| pick(ThemeMode::Dark, name, cx),
                        ),
                    )
                    .description("Used in dark mode."),
                ),
        )
}

fn sounds_page() -> SettingPage {
    let themes = sounds::THEMES
        .iter()
        .map(|(_, key, label)| ((*key).into(), (*label).into()))
        .collect();
    let percent = NumberFieldOptions {
        min: 0.,
        max: 100.,
        step: 10.,
    };
    SettingPage::new("Sounds").icon(IconName::Bell).group(
        SettingGroup::new()
            .title("grasslands sounds")
            .description(
                "The sounds grasslands makes: interface cues, and agent sounds when herdr's \
                 agent notification settings (the herdr page) allow them.",
            )
            .item(
                SettingItem::new(
                    "Sound",
                    SettingField::switch(|cx| sounds::settings(cx).enabled, sounds::set_enabled),
                )
                .description("Every sound grasslands plays. Off silences agent sounds too."),
            )
            .item(
                SettingItem::new(
                    "Theme",
                    SettingField::dropdown(
                        themes,
                        |cx| sounds::theme_key(sounds::settings(cx).theme).into(),
                        |key: SharedString, cx| {
                            if let Some(theme) = sounds::theme_from_key(&key) {
                                sounds::set_theme(theme, cx);
                            }
                        },
                    ),
                )
                .description("The material the sounds are made of."),
            )
            .item(
                SettingItem::new(
                    "Volume",
                    SettingField::number_input(
                        percent,
                        |cx| (f64::from(sounds::settings(cx).volume) * 100.).round(),
                        |percent, cx| sounds::set_volume((percent / 100.) as f32, cx),
                    ),
                )
                .description("Percent."),
            ),
    )
}

/// herdr's own preferences, read from and written to its `config.toml`.
/// Every change goes into the file (comments kept) and Local's herdr reloads
/// it, so herdr's own clients follow too.
fn herdr_page(cx: &App) -> SettingPage {
    let prefs = herdr_prefs::get(cx);
    let custom: Vec<String> = [
        ("all", &prefs.sound.path),
        ("finished", &prefs.sound.done_path),
        ("needs input", &prefs.sound.request_path),
    ]
    .into_iter()
    .filter_map(|(what, path)| Some(format!("{what}: {}", path.as_ref()?.display())))
    .collect();
    let mut notifications = SettingGroup::new()
        .title("Agent notifications")
        .description(
            "When an agent finishes or needs input. Like herdr, nothing sounds for the \
             workspace you're looking at, and no toast shows for the tab you're looking at.",
        )
        .item(
            SettingItem::new(
                "Agent sounds",
                SettingField::switch(
                    |cx| herdr_prefs::get(cx).sound.enabled,
                    |on, cx| {
                        herdr_prefs::set(Edit::SoundEnabled(on), cx);
                        sounds::play(cues::Cue::Toggle, cues::PlayOptions::default());
                    },
                ),
            )
            .description(if herdr_config::sound_disabled_by_env() {
                "ui.sound.enabled. HERDR_DISABLE_SOUND is set, so agents are silent anyway."
            } else {
                "ui.sound.enabled: a sound for agents in background workspaces."
            }),
        )
        .item(
            SettingItem::new(
                "Toasts",
                SettingField::dropdown(
                    ToastDelivery::ALL
                        .iter()
                        .map(|d| (d.key().into(), toast_label(*d).into()))
                        .collect(),
                    |cx| herdr_prefs::get(cx).toast.delivery.key().into(),
                    |key: SharedString, cx| {
                        if let Some(delivery) = ToastDelivery::from_key(&key) {
                            herdr_prefs::set(Edit::ToastDelivery(delivery), cx);
                            sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                        }
                    },
                ),
            )
            .description(
                "ui.toast.delivery. Off: none. herdr: a grasslands toast. Terminal: a toast, \
                 plus macOS while grasslands is in the background. System: a toast, or only \
                 macOS while grasslands is in the background.",
            ),
        )
        .item(
            SettingItem::new(
                "Toast delay",
                SettingField::number_input(
                    NumberFieldOptions {
                        min: 0.,
                        max: MAX_TOAST_DELAY as f64,
                        step: 1.,
                    },
                    |cx| herdr_prefs::get(cx).toast.delay_seconds as f64,
                    |seconds, cx| {
                        let seconds = seconds.round().clamp(0., MAX_TOAST_DELAY as f64) as u64;
                        herdr_prefs::set(Edit::ToastDelay(seconds), cx);
                        sounds::play(cues::Cue::Select, sounds::subtle());
                    },
                ),
            )
            .description(
                "ui.toast.delay_seconds: seconds to wait. Only an agent still in the same \
                 state then gets its sound and toast.",
            ),
        );
    if !custom.is_empty() {
        let custom = custom.join("\n");
        notifications = notifications.item(
            SettingItem::new(
                "Custom sounds",
                SettingField::render(move |_, _, cx| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(custom.clone())
                }),
            )
            .layout(Axis::Vertical)
            .description(
                "[ui.sound] path, done_path and request_path. They play in place of the \
                 agent cues; edit them in the config file.",
            ),
        );
    }

    let running = herdr_prefs::running_agents(cx);
    let agents = SettingGroup::new()
        .title("Sound per agent")
        .description("[ui.sound.agents]: override the sound for one kind of agent.")
        .items(SOUND_AGENTS.iter().map(|agent| {
            let agent = *agent;
            let default_label = if herdr_config::muted_by_default(agent) {
                "Default (off)"
            } else {
                "Default (on)"
            };
            let options = AgentSound::ALL
                .iter()
                .map(|sound| {
                    let label = match sound {
                        AgentSound::Default => default_label,
                        AgentSound::On => "On",
                        AgentSound::Off => "Off",
                    };
                    (sound.key().into(), label.into())
                })
                .collect();
            let item = SettingItem::new(
                agent,
                SettingField::dropdown(
                    options,
                    move |cx| herdr_prefs::get(cx).sound.agent(agent).key().into(),
                    move |key: SharedString, cx| {
                        if let Some(sound) = AgentSound::from_key(&key) {
                            let agent = agent.to_owned();
                            herdr_prefs::set(Edit::AgentSound { agent, sound }, cx);
                            sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                        }
                    },
                ),
            );
            if running.contains(agent) {
                item.description("Running now.")
            } else {
                item
            }
        }));

    let sidebar = SettingGroup::new()
        .title("Agents panel")
        .item(
            SettingItem::new(
                "Order",
                SettingField::dropdown(
                    AgentPanelSort::ALL
                        .iter()
                        .map(|sort| {
                            let label = match sort {
                                AgentPanelSort::Spaces => "Grouped by space",
                                AgentPanelSort::Priority => "Priority",
                            };
                            (sort.key().into(), label.into())
                        })
                        .collect(),
                    |cx| herdr_prefs::get(cx).agent_panel_sort.key().into(),
                    |key: SharedString, cx| {
                        if let Some(sort) = AgentPanelSort::from_key(&key) {
                            herdr_prefs::set(Edit::AgentPanelSort(sort), cx);
                            sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                        }
                    },
                ),
            )
            .description("ui.agent_panel_sort; the sidebar's sort toggle sets it too."),
        )
        .item(
            SettingItem::new(
                "Status indicators",
                SettingField::dropdown(
                    StatusIndicators::ALL
                        .iter()
                        .map(|indicators| {
                            let label = match indicators {
                                StatusIndicators::Dots => "Dots",
                                StatusIndicators::Symbols => "Symbols",
                            };
                            (indicators.key().into(), label.into())
                        })
                        .collect(),
                    |cx| herdr_prefs::get(cx).status_indicators.key().into(),
                    |key: SharedString, cx| {
                        if let Some(indicators) = StatusIndicators::from_key(&key) {
                            herdr_prefs::set(Edit::StatusIndicators(indicators), cx);
                            sounds::play(cues::Cue::Select, cues::PlayOptions::default());
                        }
                    },
                ),
            )
            .description("ui.status_indicators: color dots, or a distinct symbol per state."),
        );

    let dialogs = SettingGroup::new()
        .title("Dialogs")
        .item(herdr_switch(
            "Confirm closing a workspace",
            "ui.confirm_close. Closing a pane or tab still asks when it would stop an agent.",
            |prefs| prefs.confirm_close,
            Edit::ConfirmClose,
        ))
        .item(herdr_switch(
            "Name new tabs",
            "ui.prompt_new_tab_name: ask for a name before making a tab.",
            |prefs| prefs.prompt_new_tab_name,
            Edit::PromptNewTabName,
        ))
        .item(herdr_switch(
            "Name new workspaces",
            "ui.prompt_new_workspace_name: ask for a name before making a workspace.",
            |prefs| prefs.prompt_new_workspace_name,
            Edit::PromptNewWorkspaceName,
        ));

    let file = SettingGroup::new().title("Config file").item(
        SettingItem::new(
            "herdr's config.toml",
            SettingField::render(|_, _, cx| config_file(cx)),
        )
        .layout(Axis::Vertical)
        .description(
            "Local herdr's settings live here; other machines keep their own. Edits from \
             anywhere show up here, and the changes above keep the file's comments.",
        ),
    );

    SettingPage::new("herdr")
        .icon(Icon::new(assets::IconName::SquareTerminal))
        .description("herdr's own settings, from its config.toml. herdr applies them too.")
        .groups([notifications, agents, sidebar, dialogs, file])
}

/// A switch for one of herdr's boolean preferences.
fn herdr_switch(
    title: &'static str,
    description: &'static str,
    value: fn(&herdr_config::Prefs) -> bool,
    edit: fn(bool) -> Edit,
) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::switch(
            move |cx| value(herdr_prefs::get(cx)),
            move |on, cx| {
                herdr_prefs::set(edit(on), cx);
                sounds::play(cues::Cue::Toggle, cues::PlayOptions::default());
            },
        ),
    )
    .description(description)
}

fn toast_label(delivery: ToastDelivery) -> &'static str {
    match delivery {
        ToastDelivery::Off => "Off",
        ToastDelivery::Herdr => "herdr",
        ToastDelivery::Terminal => "Terminal",
        ToastDelivery::System => "System",
    }
}

/// The config file's path, Open / Reveal / Reload, and what herdr's
/// `config check` (or the last write or reload) found wrong.
fn config_file(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Some(path) = herdr_prefs::path(cx).map(std::path::Path::to_path_buf) else {
        return div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("No config path: HOME is not set.")
            .into_any_element();
    };
    let exists = path.exists();
    let problems = herdr_prefs::problems(cx);
    let (open_path, reveal_path) = (path.clone(), path.clone());
    v_flex()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .child(if exists {
                    path.display().to_string()
                } else {
                    format!("{} (not created yet: herdr's defaults)", path.display())
                }),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("herdr-config-open")
                        .small()
                        .label("Open")
                        .disabled(!exists)
                        .on_click(move |_, _, cx| {
                            sounds::play(cues::Cue::Tap, cues::PlayOptions::default());
                            cx.open_with_system(&open_path);
                        }),
                )
                .child(
                    Button::new("herdr-config-reveal")
                        .small()
                        .label("Reveal in Finder")
                        .disabled(!exists)
                        .on_click(move |_, _, cx| {
                            sounds::play(cues::Cue::Tap, cues::PlayOptions::default());
                            cx.reveal_path(&reveal_path);
                        }),
                )
                .child(
                    Button::new("herdr-config-reload")
                        .small()
                        .label("Reload in herdr")
                        .tooltip("herdr server reload-config")
                        .on_click(|_, _, cx| {
                            sounds::play(cues::Cue::Tap, cues::PlayOptions::default());
                            herdr_prefs::reload_server(cx);
                        }),
                ),
        )
        .children((!problems.is_empty()).then(|| {
            div()
                .text_xs()
                .text_color(theme.warning)
                .font_family(theme.mono_font_family.clone())
                .child(problems.join("\n"))
        }))
        .into_any_element()
}

/// Every registered theme of `mode`, the Kit's default first, then by name.
fn theme_options(mode: ThemeMode, cx: &App) -> Vec<(SharedString, SharedString)> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|theme| theme.mode == mode)
        .map(|theme| (theme.name.clone(), theme.name.clone()))
        .collect()
}

/// Sets the theme for `mode`. With a fixed appearance of the other mode the
/// pick would not show, so the appearance follows it; under Match System it
/// stays.
pub fn pick(mode: ThemeMode, name: SharedString, cx: &mut App) {
    theme::update_settings(cx, |settings| {
        match mode {
            ThemeMode::Light => settings.light = name,
            ThemeMode::Dark => settings.dark = name,
        }
        let fixed = match mode {
            ThemeMode::Light => Appearance::Light,
            ThemeMode::Dark => Appearance::Dark,
        };
        if settings.appearance != Appearance::System {
            settings.appearance = fixed;
        }
    });
}

/// The open Settings window, for script checks.
pub fn window(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<SettingsWindow>()?.0
}
