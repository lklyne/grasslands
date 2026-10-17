//! cuelume's `context.ts`: how emphasis and context bend a recipe, and render.ts's `layersFor`.

use std::time::Duration;

use super::layers::{Layer, Source};
use super::rng::Random;
use super::themes::{Recipe, Variation};
use crate::{Context, Cue, Direction, Emphasis, Input, KeyRole, Part};

/// Scales for a recipe's pitch, level and length, its bright layers and its tail. `sweep` is -1 to
/// play glides backwards; `time` moves when layers start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Shape {
    pitch: f64,
    level: f64,
    length: f64,
    bright: f64,
    tail: f64,
    sweep: f64,
    time: f64,
}

/// Every factor 1. Multiplying by 1 is exact, so a full `Shape` stands in for context.ts's partial ones.
const IDENTITY: Shape = Shape {
    pitch: 1.0,
    level: 1.0,
    length: 1.0,
    bright: 1.0,
    tail: 1.0,
    sweep: 1.0,
    time: 1.0,
};
const BRIGHT_HZ: f64 = 2500.0;

const fn emphasis_shape(emphasis: Emphasis) -> Shape {
    match emphasis {
        Emphasis::Subtle => Shape {
            level: 0.8,
            length: 0.85,
            bright: 0.75,
            ..IDENTITY
        },
        Emphasis::Normal => IDENTITY,
        Emphasis::Strong => Shape {
            pitch: 0.98,
            level: 1.08,
            length: 1.2,
            bright: 1.1,
            ..IDENTITY
        },
    }
}

pub(super) const fn rank(emphasis: Emphasis) -> u8 {
    match emphasis {
        Emphasis::Subtle => 0,
        Emphasis::Normal => 1,
        Emphasis::Strong => 2,
    }
}

const fn key_shape(key: KeyRole) -> Shape {
    match key {
        KeyRole::Printable => IDENTITY,
        KeyRole::Space => Shape {
            pitch: 0.86,
            level: 1.08,
            length: 1.15,
            ..IDENTITY
        },
        KeyRole::Delete => Shape {
            pitch: 0.92,
            level: 0.9,
            bright: 0.8,
            ..IDENTITY
        },
        KeyRole::Enter => Shape {
            pitch: 0.8,
            level: 1.12,
            length: 1.25,
            ..IDENTITY
        },
    }
}

const fn input_shape(input: Input) -> Shape {
    match input {
        Input::Mouse => IDENTITY,
        Input::Touch => Shape {
            pitch: 0.97,
            level: 0.92,
            bright: 0.45,
            ..IDENTITY
        },
        Input::Pen => Shape {
            pitch: 1.02,
            bright: 1.3,
            ..IDENTITY
        },
        Input::Keyboard => Shape {
            pitch: 0.94,
            length: 0.85,
            bright: 0.7,
            ..IDENTITY
        },
    }
}

const DIRECTION_STEP: f64 = 0.05;
const FAST_MS: f64 = 70.0;
const SLOW_MS: f64 = 220.0;
const COUNT_MS: f64 = 800.0;
const COUNT_MIN_MS: f64 = 300.0;
const COUNT_MAX_MS: f64 = 2000.0;

const fn has_cadence(cue: Cue) -> bool {
    matches!(
        cue,
        Cue::Type | Cue::Select | Cue::Tap | Cue::Toggle | Cue::Navigate
    )
}

const fn reversible(cue: Cue) -> bool {
    matches!(cue, Cue::Navigate | Cue::Toggle | Cue::Count)
}

fn cadence(since_last_ms: f64) -> Shape {
    let speed = ((SLOW_MS - since_last_ms) / (SLOW_MS - FAST_MS)).clamp(0.0, 1.0);
    Shape {
        level: 1.0 - 0.22 * speed,
        length: 1.0 - 0.25 * speed,
        bright: 1.0 - 0.2 * speed,
        tail: 1.0 - 0.8 * speed,
        ..IDENTITY
    }
}

impl Shape {
    fn times(self, by: Shape) -> Shape {
        Shape {
            pitch: self.pitch * by.pitch,
            level: self.level * by.level,
            length: self.length * by.length,
            bright: self.bright * by.bright,
            tail: self.tail * by.tail,
            sweep: self.sweep * by.sweep,
            time: self.time * by.time,
        }
    }
}

/// The combined shape for one play of `cue`. `since_last` of `None` is the first play.
pub(super) fn shape_for(
    cue: Cue,
    context: &Context,
    emphasis: Emphasis,
    since_last: Option<Duration>,
) -> Shape {
    let direction = context.direction.map(|direction| match direction {
        Direction::Forward => 1.0,
        Direction::Back => -1.0,
    });
    let mut shape = IDENTITY.times(emphasis_shape(emphasis));
    if has_cadence(cue) {
        let since_last_ms = since_last.map_or(f64::INFINITY, |since| since.as_secs_f64() * 1000.0);
        shape = shape.times(cadence(since_last_ms));
    }
    if let (Cue::Type, Some(key)) = (cue, context.key) {
        shape = shape.times(key_shape(key));
    }
    if let (Cue::Tap, Some(input)) = (cue, context.input) {
        shape = shape.times(input_shape(input));
    }
    if let (Cue::Select, Some(direction)) = (cue, direction) {
        shape = shape.times(Shape {
            pitch: 1.0 + DIRECTION_STEP * direction,
            ..IDENTITY
        });
    }
    if let (Cue::Count, Some(duration)) = (cue, context.duration) {
        let ms = (duration.as_secs_f64() * 1000.0).clamp(COUNT_MIN_MS, COUNT_MAX_MS);
        shape = shape.times(Shape {
            time: ms / COUNT_MS,
            ..IDENTITY
        });
    }
    if reversible(cue) && direction == Some(-1.0) {
        shape = shape.times(Shape {
            sweep: -1.0,
            ..IDENTITY
        });
    }
    shape
}

struct Factors {
    pitch: f64,
    gain: f64,
    length: f64,
}

/// What `shape` does to one layer, clamped so no context runs away with a cue.
fn layer_factors(layer: &Layer, shape: &Shape) -> Factors {
    let bright = if layer.frequency >= BRIGHT_HZ {
        shape.bright
    } else {
        1.0
    };
    let tail = if layer.offset > 0.0 { shape.tail } else { 1.0 };
    Factors {
        pitch: shape.pitch.clamp(0.75, 1.25),
        gain: (shape.level * bright * tail).clamp(0.0, 1.5),
        length: shape.length.clamp(0.6, 1.6),
    }
}

/// One play's strike: how high the object sounds and how hard it was hit, shared by every layer.
#[derive(Clone, Copy)]
struct Strike {
    pitch: f64,
    force: f64,
}

const STEADY_STRIKE: Strike = Strike {
    pitch: 1.0,
    force: 1.0,
};
const DECAY_JITTER: f64 = 0.1;
const MODE_JITTER: f64 = 0.005;

/// The play's variation and the generator it draws from, when the play varies.
pub(super) struct Jitter<'a> {
    pub vary: Variation,
    pub random: &'a mut Random,
}

impl Jitter<'_> {
    fn nudge(&mut self, amount: f64) -> f64 {
        1.0 + (self.random.next() * 2.0 - 1.0) * amount
    }
}

/// The press lets go this long after it presses when a play has no real let-go.
pub(super) const RELEASE_GAP: f64 = 0.09;

fn shaped(
    layer: &Layer,
    shape: &Shape,
    strike: Strike,
    jitter: &mut Option<Jitter<'_>>,
    walk: f64,
) -> Layer {
    let factors = layer_factors(layer, shape);
    let tune = match (layer.source, jitter.as_mut()) {
        (Source::Tone, Some(jitter)) => jitter.nudge(MODE_JITTER),
        (Source::Tone, None) => 1.0,
        (Source::Noise(_), _) => strike.force,
    };
    let pitch = factors.pitch * strike.pitch * tune * if layer.note { walk } else { 1.0 };
    let peak = layer.peak * factors.gain * strike.force;
    let stretch = if layer.stretch { shape.time } else { 1.0 };
    let ring = jitter
        .as_mut()
        .map_or(1.0, |jitter| jitter.nudge(DECAY_JITTER));
    let decay = layer.decay * factors.length * stretch * ring;
    let (from, to) = match layer.glide_to {
        Some(glide_to) if shape.sweep < 0.0 => (glide_to, Some(layer.frequency)),
        glide_to => (layer.frequency, glide_to),
    };
    Layer {
        frequency: from * pitch,
        glide_to: to.map(|to| to * pitch),
        glide_time: layer.glide_time.map(|time| time * stretch),
        offset: layer.offset * shape.time,
        attack: layer.attack * stretch,
        peak,
        decay,
        ..*layer
    }
}

/// The layers `part` of one play sounds: arranged for its emphasis, bent by its shape, strike and
/// walk, the release `RELEASE_GAP` after the press when both play.
pub(super) fn layers_for(
    recipe: &Recipe,
    emphasis: Emphasis,
    shape: &Shape,
    step: usize,
    part: Part,
    mut jitter: Option<Jitter<'_>>,
) -> Vec<Layer> {
    let mut parts: Vec<(&[Layer], f64)> = Vec::with_capacity(2);
    if part != Part::Release {
        parts.push((&recipe.layers, 0.0));
    }
    if part != Part::Press
        && let Some(release) = &recipe.release
    {
        parts.push((release, if part == Part::Both { RELEASE_GAP } else { 0.0 }));
    }
    let walk = 2f64.powf(recipe.walk.get(step).copied().unwrap_or(0.0) / 12.0);
    let strike = match jitter.as_mut() {
        Some(jitter) => {
            let Variation { pitch, level } = jitter.vary;
            Strike {
                pitch: jitter.nudge(pitch),
                force: jitter.nudge(level),
            }
        }
        None => STEADY_STRIKE,
    };
    let mut layers = Vec::new();
    for (part_layers, delay) in parts {
        for layer in part_layers
            .iter()
            .filter(|layer| rank(emphasis) >= rank(layer.from))
        {
            let bent = shaped(layer, shape, strike, &mut jitter, walk);
            layers.push(Layer {
                offset: bent.offset + delay,
                ..bent
            });
        }
    }
    layers
}
