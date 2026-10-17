//! One play of a cue as samples. Owned by the synthesis port; the signatures
//! below are the contract the mixer codes against.

use std::time::Duration;

use crate::{Context, Cue, Emphasis, Part, Theme};

/// One play's rendered samples, room included, before volume and the output's soft clip.
#[derive(Clone, Debug, Default)]
pub struct Stereo {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// Everything one play decided (cuelume's `Played` plus the part).
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub theme: Theme,
    pub cue: Cue,
    pub emphasis: Emphasis,
    pub context: Context,
    /// Time since this cue last played, for cadence; `None` plays it as the first.
    pub since_last: Option<Duration>,
    /// Place in the cue's walk; see [`walk_len`].
    pub step: usize,
    pub part: Part,
    /// Per-play variation (cuelume's strike and jitter). Off renders as `sound.mjs` does.
    pub vary: bool,
    /// Seeds the noise offsets and the variation.
    pub seed: u32,
}

/// Renders `request` at `rate` Hz: master gain and output gain applied, room and subsonic cut included.
pub fn render(request: &Request, rate: u32) -> Stereo {
    let _ = (request, rate);
    todo!("synthesis port")
}

/// How many steps the cue's walk has in `theme`; 0 when it has none.
pub fn walk_len(theme: Theme, cue: Cue) -> usize {
    let _ = (theme, cue);
    todo!("synthesis port")
}

/// Whether the cue in `theme` has a release to play as a gesture lets go.
pub fn has_release(theme: Theme, cue: Cue) -> bool {
    let _ = (theme, cue);
    todo!("synthesis port")
}

/// How long the cue's last play takes to fade when it plays again: 400 ms with note layers, else 80 ms.
pub fn handoff(theme: Theme, cue: Cue) -> Duration {
    let _ = (theme, cue);
    todo!("synthesis port")
}
