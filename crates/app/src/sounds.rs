//! Interface and status sounds through `cues`, the cuelume port. One
//! app-wide [`Cues`], created on first use; the on/off, theme and volume picked in
//! Settings → Sounds, saved under `"sounds"` in the settings file.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cues::{Cue, Cues, Emphasis, PlayOptions, Theme};
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

/// Plays `cue` on the shared output, if there is one. The cue to use for a
/// moment is in `crates/cues/AGENTS.md`.
/// Silent in tests and script runs, which drive the same code paths.
pub fn play(cue: Cue, options: PlayOptions) {
    #[cfg(debug_assertions)]
    PLAYED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(cue);
    if cfg!(test) || std::env::var_os("RT_SCRIPT").is_some() {
        return;
    }
    if let Some(cues) = cues() {
        cues.play(cue, options);
    }
}

/// Every cue asked for so far, played or not (debug builds), so script
/// runs can check the sounds they would have made.
#[cfg(debug_assertions)]
static PLAYED: std::sync::Mutex<Vec<Cue>> = std::sync::Mutex::new(Vec::new());

#[cfg(debug_assertions)]
pub fn played() -> Vec<Cue> {
    PLAYED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Options for a frequent action.
pub fn subtle() -> PlayOptions {
    PlayOptions::default().emphasis(Emphasis::Subtle)
}

/// Options for a rare action that matters.
pub fn strong() -> PlayOptions {
    PlayOptions::default().emphasis(Emphasis::Strong)
}

/// Whether the user has sounds on.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The Theme dropdown's choices: settings key, label.
pub const THEMES: [(Theme, &str, &str); 4] = [
    (Theme::Default, "default", "Default"),
    (Theme::Mech, "mech", "Mech"),
    (Theme::Bubble, "bubble", "Bubble"),
    (Theme::Press, "press", "Press"),
];

pub fn theme_key(theme: Theme) -> &'static str {
    THEMES
        .iter()
        .find(|(t, ..)| *t == theme)
        .map_or("default", |(_, key, _)| key)
}

pub fn theme_from_key(key: &str) -> Option<Theme> {
    THEMES
        .iter()
        .find(|(_, k, _)| *k == key)
        .map(|(theme, ..)| *theme)
}

/// The saved sound choice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundSettings {
    pub enabled: bool,
    pub theme: Theme,
    /// 0–1.
    pub volume: f32,
}

impl Global for SoundSettings {}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: Theme::Default,
            volume: 1.0,
        }
    }
}

impl SoundSettings {
    /// Reads the `"sounds"` object from a settings file's JSON, keeping the
    /// defaults for anything missing or malformed. A `"theme": "off"` from
    /// before the on/off switch reads as off.
    fn from_json(json: &Map<String, Value>) -> Self {
        let defaults = Self::default();
        let sounds = json.get("sounds");
        let field = |key: &str| sounds.and_then(|sounds| sounds.get(key));
        let theme = field("theme").and_then(Value::as_str);
        Self {
            enabled: field("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(theme != Some("off")),
            theme: theme.and_then(theme_from_key).unwrap_or(defaults.theme),
            volume: field("volume")
                .and_then(Value::as_f64)
                .map_or(defaults.volume, |volume| volume.clamp(0.0, 1.0) as f32),
        }
    }

    fn write_json(&self, json: &mut Map<String, Value>) {
        json.insert(
            "sounds".into(),
            json!({ "enabled": self.enabled, "theme": theme_key(self.theme), "volume": self.volume }),
        );
    }

    fn apply(&self) {
        ENABLED.store(self.enabled, Ordering::Relaxed);
        let Some(cues) = cues() else { return };
        cues.set_theme(self.theme);
        cues.set_volume(self.volume);
        cues.set_enabled(self.enabled);
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

/// Turns sounds on or off and saves it; turning on plays a `Toggle`.
pub fn set_enabled(enabled: bool, cx: &mut App) {
    update(cx, |settings| settings.enabled = enabled);
    if let Some(cues) = cues().filter(|_| enabled) {
        cues.play(Cue::Toggle, PlayOptions::default());
    }
}

/// Picks a theme, saves it, and previews it: `Ready`, then `Attention`.
pub fn set_theme(theme: Theme, cx: &mut App) {
    update(cx, |settings| settings.theme = theme);
    let preview = cues().map(|cues| {
        cues.play(Cue::Ready, PlayOptions::default());
        let gap = cx.background_executor().timer(PREVIEW_GAP);
        cx.background_executor().spawn(async move {
            gap.await;
            cues.play(Cue::Attention, PlayOptions::default());
        })
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
    use super::{SoundSettings, Theme};
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
            enabled: false,
            theme: Theme::Press,
            volume: 0.5,
        };
        let mut json = Map::new();
        settings.write_json(&mut json);
        assert_eq!(SoundSettings::from_json(&json), settings);
    }

    #[test]
    fn theme_off_from_before_the_switch_reads_as_off() {
        let json = object(json!({ "sounds": { "theme": "off", "volume": 0.4 } }));
        let settings = SoundSettings::from_json(&json);
        assert_eq!((settings.enabled, settings.theme), (false, Theme::Default));
    }

    #[test]
    fn missing_or_malformed_keys_keep_defaults() {
        let json =
            object(json!({ "sounds": { "enabled": "yes", "theme": "kazoo", "volume": "loud" } }));
        assert_eq!(SoundSettings::from_json(&json), SoundSettings::default());
    }

    #[test]
    fn volume_is_clamped() {
        let json = object(json!({ "sounds": { "volume": 4.0 } }));
        assert_eq!(SoundSettings::from_json(&json).volume, 1.0);
    }
}
