//! Themes: the Kit's default light and dark, plus the theme files GPUI Kit
//! keeps in its repo (not in the published crate), vendored in
//! `crates/app/themes`. The user picks a light theme, a dark theme and an
//! appearance (follow the system, or always light or dark) in the Settings
//! window; the choice is saved to `~/.config/grasslands/settings.json`.
//!
//! The UI base size (the rem every `text_*`, spacing and rem-sized control
//! scales from) is 14px rather than the Kit's 16, for desktop density:
//! `text_xs` 10.5, `text_sm` 12.25, `text_base` 14.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use serde_json::{Map, Value};

use crate::actions::ToggleTheme;

/// The UI base size. A theme reload resets it from the theme's tokens, so
/// every load goes through [`apply_base`].
const BASE: Pixels = px(14.);

/// GPUI Kit's theme files at v0.7.3 (github.com/longbridge/gpui-kit, `themes/`).
const BUNDLED: &[&str] = &[
    include_str!("../themes/adventure.json"),
    include_str!("../themes/alduin.json"),
    include_str!("../themes/asciinema.json"),
    include_str!("../themes/aurora.json"),
    include_str!("../themes/ayu.json"),
    include_str!("../themes/catppuccin.json"),
    include_str!("../themes/everforest.json"),
    include_str!("../themes/fahrenheit.json"),
    include_str!("../themes/flexoki.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/harper.json"),
    include_str!("../themes/hybrid.json"),
    include_str!("../themes/jellybeans.json"),
    include_str!("../themes/kibble.json"),
    include_str!("../themes/macos-classic.json"),
    include_str!("../themes/mellifluous.json"),
    include_str!("../themes/molokai.json"),
    include_str!("../themes/solarized.json"),
    include_str!("../themes/spaceduck.json"),
    include_str!("../themes/tokyonight.json"),
    include_str!("../themes/twilight.json"),
];

/// Light or dark: following the system, or fixed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Match System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|appearance| appearance.key() == key)
    }
}

/// The saved theme choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeSettings {
    pub appearance: Appearance,
    /// Theme names, as in the theme files. An unknown name falls back to the
    /// Kit's default for that mode.
    pub light: SharedString,
    pub dark: SharedString,
}

impl Global for ThemeSettings {}

impl ThemeSettings {
    fn defaults(cx: &App) -> Self {
        let registry = ThemeRegistry::global(cx);
        Self {
            appearance: Appearance::System,
            light: registry.default_light_theme().name.clone(),
            dark: registry.default_dark_theme().name.clone(),
        }
    }

    /// Reads the keys this module owns from a settings file's JSON object,
    /// keeping `defaults` for any that are missing or malformed.
    fn from_json(json: &Map<String, Value>, defaults: Self) -> Self {
        let name = |key: &str| {
            json.get(key)
                .and_then(Value::as_str)
                .map(SharedString::from)
        };
        Self {
            appearance: json
                .get("appearance")
                .and_then(Value::as_str)
                .and_then(Appearance::from_key)
                .unwrap_or(defaults.appearance),
            light: name("light_theme").unwrap_or(defaults.light),
            dark: name("dark_theme").unwrap_or(defaults.dark),
        }
    }

    fn write_json(&self, json: &mut Map<String, Value>) {
        json.insert("appearance".into(), self.appearance.key().into());
        json.insert("light_theme".into(), self.light.to_string().into());
        json.insert("dark_theme".into(), self.dark.to_string().into());
    }
}

pub fn init(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for file in BUNDLED {
        if let Err(error) = registry.load_themes_from_str(file) {
            tracing::warn!("bundled theme file: {error:#}");
        }
    }
    let defaults = ThemeSettings::defaults(cx);
    cx.set_global(ThemeSettings::from_json(&read_settings_file(), defaults));

    cx.on_action(|_: &ToggleTheme, cx| {
        let (appearance, direction) = if cx.theme().is_dark() {
            (Appearance::Light, cues::Direction::Forward)
        } else {
            (Appearance::Dark, cues::Direction::Back)
        };
        crate::sounds::play(
            cues::Cue::Toggle,
            cues::PlayOptions::default().direction(direction),
        );
        update_settings(cx, |settings| settings.appearance = appearance);
    });
}

pub fn settings(cx: &App) -> &ThemeSettings {
    cx.global::<ThemeSettings>()
}

/// Changes the theme choice, applies it and saves it.
pub fn update_settings(cx: &mut App, edit: impl FnOnce(&mut ThemeSettings)) {
    edit(cx.global_mut::<ThemeSettings>());
    apply(cx.window_appearance(), cx);
    save(settings(cx));
}

/// Applies the theme choice for `window`'s appearance. Called when the main
/// window opens and whenever the system appearance changes.
pub fn sync_with_system(window: &mut Window, cx: &mut App) {
    // The window's appearance rather than the app's: the Kit notes the app's
    // is wrong on Linux (longbridge/gpui-kit#104).
    apply(window.appearance(), cx);
}

fn apply(system: WindowAppearance, cx: &mut App) {
    let settings = settings(cx).clone();
    let registry = ThemeRegistry::global(cx);
    let find = |name: &SharedString, fallback: &Rc<ThemeConfig>| {
        registry.themes().get(name).unwrap_or(fallback).clone()
    };
    let light = find(&settings.light, registry.default_light_theme());
    let dark = find(&settings.dark, registry.default_dark_theme());
    let mode = match settings.appearance {
        Appearance::System => ThemeMode::from(system),
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    Theme::change(mode, None, cx);
    apply_base(cx);
}

fn apply_base(cx: &mut App) {
    Theme::update(cx, |theme| theme.font_size = BASE);
}

/// `~/.config/grasslands/settings.json`. Not under `XDG_CONFIG_HOME`, which
/// the dev scripts point at herdr's isolated config.
fn settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".config/grasslands/settings.json"))
}

/// The settings file's object; empty when there is no file yet.
fn read_json(path: &Path) -> anyhow::Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(error.into()),
    };
    match serde_json::from_str(&text)? {
        Value::Object(json) => Ok(json),
        _ => anyhow::bail!("{} is not a JSON object", path.display()),
    }
}

/// Writes the theme keys into the settings file, keeping any other keys.
fn save(settings: &ThemeSettings) {
    write_settings_file(|json| settings.write_json(json));
}

/// The settings file's object, for modules that keep their own keys in it.
/// Empty when there is no file or it can't be read (logged).
pub fn read_settings_file() -> Map<String, Value> {
    match settings_path().map(|path| read_json(&path)) {
        Some(Ok(json)) => json,
        Some(Err(error)) => {
            tracing::warn!("settings: {error:#}");
            Map::new()
        }
        None => Map::new(),
    }
}

/// Lets `write` set its keys in the settings file, keeping any other keys.
/// Script runs toggle settings to check the app follows; they must not
/// change the user's saved choices.
pub fn write_settings_file(write: impl FnOnce(&mut Map<String, Value>)) {
    if std::env::var_os("RT_SCRIPT").is_some() {
        return;
    }
    let Some(path) = settings_path() else {
        return;
    };
    let result = (|| -> anyhow::Result<()> {
        let mut json = read_json(&path)?;
        write(&mut json);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&json)? + "\n")?;
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!("saving {}: {error:#}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, ThemeSettings};
    use serde_json::{Map, Value, json};

    fn defaults() -> ThemeSettings {
        ThemeSettings {
            appearance: Appearance::System,
            light: "Default Light".into(),
            dark: "Default Dark".into(),
        }
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = ThemeSettings {
            appearance: Appearance::Dark,
            light: "Ayu Light".into(),
            dark: "Tokyo Night".into(),
        };
        let mut json = Map::new();
        settings.write_json(&mut json);
        assert_eq!(ThemeSettings::from_json(&json, defaults()), settings);
    }

    #[test]
    fn missing_or_malformed_keys_keep_defaults() {
        let json = object(json!({ "appearance": "sepia", "dark_theme": 3 }));
        assert_eq!(ThemeSettings::from_json(&json, defaults()), defaults());
    }

    #[test]
    fn writing_keeps_other_keys() {
        let mut json = object(json!({ "font": "Berkeley Mono" }));
        defaults().write_json(&mut json);
        assert_eq!(json.get("font"), Some(&json!("Berkeley Mono")));
    }
}
