//! Interaction and status sounds, synthesized at play time. A port of
//! [cuelume](https://github.com/danielwh2/cuelume) (MIT) at commit `2a1ba4f`.
//!
//! - [`render`] turns one play of a cue into 48 kHz stereo samples: the
//!   recipe's layers, shaped by emphasis and context, through cuelume's room
//!   and subsonic cut. Pure DSP, no I/O.
//! - [`Cues`] owns the output stream and decides what to play: cadence, the
//!   walk, held presses, and fading a cue's last play when it plays again.

mod mixer;
mod render;

pub use mixer::{Cues, Error, PlayOptions};
pub use render::{Request, Stereo, handoff, has_release, render, walk_len};

/// The rate [`render`] writes at unless asked otherwise.
pub const SAMPLE_RATE: u32 = 48_000;

/// A finished material for every cue. A theme changes how a cue sounds, never what it means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Theme {
    #[default]
    Default,
    Mech,
    Bubble,
    Press,
}

/// One interface job. See cuelume's README for which cue fits which moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cue {
    Tap,
    Type,
    Select,
    Toggle,
    Open,
    Close,
    Navigate,
    Success,
    Warning,
    Error,
    Loading,
    Ready,
    Attention,
    Count,
}

/// How much the action matters: subtle drops ornament layers, strong adds its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Emphasis {
    Subtle,
    #[default]
    Normal,
    Strong,
}

/// Which part of a gesture to play.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Part {
    Press,
    Release,
    #[default]
    Both,
}

/// What did the activating. Shapes `tap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Input {
    Mouse,
    Touch,
    Pen,
    Keyboard,
}

/// Which key was typed. Shapes `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyRole {
    Printable,
    Space,
    Delete,
    Enter,
}

/// Which way the interaction moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Forward,
    Back,
}

/// What the caller knows about the interaction. Missing context plays the canonical sound.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Context {
    pub input: Option<Input>,
    pub key: Option<KeyRole>,
    pub direction: Option<Direction>,
    /// How long a `count` runs, clamped to 300–2000 ms. Other cues ignore it.
    pub duration: Option<std::time::Duration>,
}
