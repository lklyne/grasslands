//! Interface and status sounds through `cues`, the cuelume port. One
//! app-wide [`Cues`], created on first use; the theme and volume picked in
//! Settings → Sounds, saved under `"sounds"` in the settings file.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cues::{Cue, Cues, PlayOptions, Theme};
use gpui_kit::*;
use serde_json::{Map, Value, json};

use crate::theme;

/// How long the theme preview waits between its two cues.
const PREVIEW_GAP: Duration = Duration::from_millis(600);

/// Whether sounds are on, for callers that fall back to system sounds when
/// there is no [`Cues`].
static ENABLED: AtomicBool = AtomicBool::new(true);

/// The shared output, or `None` if it couldn't start (logged once).
pub fn cues() -> Option<&'static Cues> {
    static CUES: OnceLock<Option<Cues>> = OnceLock::new();
    CUES.get_or_init(|| {
        Cues::new()
            .inspect_err(|error| tracing::warn!("sounds: {error}; using system sounds"))
            .ok()
    })
    .as_ref()
}

/// Whether the user has sounds on.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A choice in the Theme dropdown: one of cuelume's materials, or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundTheme(Option<Theme>);

impl SoundTheme {
    pub const ALL: [Self; 5] = [
        Self(Some(Theme::Default)),
        Self(Some(Theme::Mech)),
        Self(Some(Theme::Bubble)),
        Self(Some(Theme::Press)),
        Self(None),
    ];

    pub fn key(self) -> &'static str {
        match self.0 {
            Some(Theme::Default) => "default",
            Some(Theme::Mech) => "mech",
            Some(Theme::Bubble) => "bubble",
            Some(Theme::Press) => "press",
            None => "off",
        }
    }

    pub fn label(self) -> &'static str {
        match self.0 {
            Some(Theme::Default) => "Default",
            Some(Theme::Mech) => "Mech",
            Some(Theme::Bubble) => "Bubble",
            Some(Theme::Press) => "Press",
            None => "Off",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.key() == key)
    }
}

/// The saved sound choice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundSettings {
    pub theme: SoundTheme,
    /// 0–1.
    pub volume: f32,
}

impl Global for SoundSettings {}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            theme: SoundTheme(Some(Theme::Default)),
            volume: 1.0,
        }
    }
}

impl SoundSettings {
    /// Reads the `"sounds"` object from a settings file's JSON, keeping the
    /// defaults for anything missing or malformed.
    fn from_json(json: &Map<String, Value>) -> Self {
        let defaults = Self::default();
        let sounds = json.get("sounds");
        let field = |key: &str| sounds.and_then(|sounds| sounds.get(key));
        Self {
            theme: field("theme")
                .and_then(Value::as_str)
                .and_then(SoundTheme::from_key)
                .unwrap_or(defaults.theme),
            volume: field("volume")
                .and_then(Value::as_f64)
                .map_or(defaults.volume, |volume| volume.clamp(0.0, 1.0) as f32),
        }
    }

    fn write_json(&self, json: &mut Map<String, Value>) {
        json.insert(
            "sounds".into(),
            json!({ "theme": self.theme.key(), "volume": self.volume }),
        );
    }

    fn apply(&self) {
        ENABLED.store(self.theme.0.is_some(), Ordering::Relaxed);
        let Some(cues) = cues() else { return };
        if let Some(theme) = self.theme.0 {
            cues.set_theme(theme);
        }
        cues.set_volume(self.volume);
        cues.set_enabled(self.theme.0.is_some());
    }
}

/// The pending second cue of a theme preview; a newer preview replaces it.
struct Preview(Option<Task<()>>);

impl Global for Preview {}

pub fn init(cx: &mut App) {
    let settings = SoundSettings::from_json(&theme::read_settings_file());
    settings.apply();
    cx.set_global(settings);
}

pub fn settings(cx: &App) -> SoundSettings {
    *cx.global::<SoundSettings>()
}

/// Picks a theme, saves it, and previews it: `Ready`, then `Attention`.
pub fn set_theme(theme: SoundTheme, cx: &mut App) {
    update(cx, |settings| settings.theme = theme);
    let preview = theme.0.and_then(|theme| {
        let cues = cues()?;
        cues.play(Cue::Ready, PlayOptions::default().theme(theme));
        let gap = cx.background_executor().timer(PREVIEW_GAP);
        Some(cx.background_executor().spawn(async move {
            gap.await;
            cues.play(Cue::Attention, PlayOptions::default().theme(theme));
        }))
    });
    cx.set_global(Preview(preview));
}

/// Sets the volume (0–1), saves it, and plays a `Tap` at the new level.
pub fn set_volume(volume: f32, cx: &mut App) {
    update(cx, |settings| settings.volume = volume.clamp(0.0, 1.0));
    if let Some(cues) = cues() {
        cues.play(Cue::Tap, PlayOptions::default());
    }
}

fn update(cx: &mut App, edit: impl FnOnce(&mut SoundSettings)) {
    let mut settings = settings(cx);
    edit(&mut settings);
    settings.apply();
    cx.set_global(settings);
    theme::write_settings_file(|json| settings.write_json(json));
}

#[cfg(test)]
mod tests {
    use super::{SoundSettings, SoundTheme, Theme};
    use serde_json::{Map, Value, json};

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = SoundSettings {
            theme: SoundTheme(Some(Theme::Press)),
            volume: 0.5,
        };
        let mut json = Map::new();
        settings.write_json(&mut json);
        assert_eq!(SoundSettings::from_json(&json), settings);
    }

    #[test]
    fn off_is_saved_as_off() {
        let mut json = Map::new();
        SoundSettings {
            theme: SoundTheme(None),
            volume: 1.0,
        }
        .write_json(&mut json);
        assert_eq!(json["sounds"]["theme"], json!("off"));
    }

    #[test]
    fn missing_or_malformed_keys_keep_defaults() {
        let json = object(json!({ "sounds": { "theme": "kazoo", "volume": "loud" } }));
        assert_eq!(SoundSettings::from_json(&json), SoundSettings::default());
    }

    #[test]
    fn volume_is_clamped() {
        let json = object(json!({ "sounds": { "volume": 4.0 } }));
        assert_eq!(SoundSettings::from_json(&json).volume, 1.0);
    }
}
