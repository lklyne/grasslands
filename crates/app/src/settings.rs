//! The Settings window (cmd-,): GPUI Kit's `Settings` page in a window of
//! its own, so it never sits under the main window's native terminal views.
//! One window at a time; opening it again brings the open one forward.

use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{ActiveTheme as _, IconName, ThemeMode, ThemeRegistry, TitleBar, v_flex};
use gpui_kit::*;

use crate::actions::OpenSettings;
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
    let (handle, _) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| SettingsView))?;
    cx.set_global(SettingsWindow(Some(handle)));
    Ok(())
}

struct SettingsView;

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
                        .page(sounds_page()),
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
            .item(
                SettingItem::new(
                    "Sound",
                    SettingField::switch(|cx| sounds::settings(cx).enabled, sounds::set_enabled),
                )
                .description("Interface and agent sounds."),
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
