//! The output stream and playback decisions. Owned by the mixer work; the
//! signatures below are the contract the app codes against.

use crate::{Context, Cue, Emphasis, Theme};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no audio output device")]
    NoDevice,
}

/// Per-play options, like cuelume's `PlayOptions`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayOptions {
    pub emphasis: Emphasis,
    pub context: Context,
    /// Multiplier for this play only, clamped to 0–1; `None` is 1.
    pub volume: Option<f32>,
    /// Material for this play only; `None` plays the active theme.
    pub theme: Option<Theme>,
}

/// The app's sound output. Cheap to clone and safe to call from any thread.
#[derive(Clone)]
pub struct Cues {}

impl Cues {
    pub fn new() -> Result<Self, Error> {
        todo!("mixer")
    }

    /// Plays the press and, `RELEASE_GAP` later, the release.
    pub fn play(&self, cue: Cue, options: PlayOptions) {
        let _ = (cue, options);
        todo!("mixer")
    }
}
