//! One play of a cue as samples. Owned by the synthesis port; the signatures
//! below are the contract the mixer codes against.
//!
//! A port of cuelume's `render.ts` and `scripts/sound.mjs`: which layers play comes from
//! `layersFor`, and the samples from `sound.mjs`'s offline renderer, which matches them sample for
//! sample with the same seed.

mod dsp;
mod layers;
mod rng;
mod shape;
mod themes;

use std::f64::consts::LN_10;
use std::time::Duration;

use self::dsp::{Biquad, ROOM_SECONDS, ROOM_SEEDS, ROOM_SEND, Spectrum};
use self::layers::{FLAT_Q, Filter, Layer, Source};
use self::rng::Random;
use self::shape::Jitter;
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

/// Web Audio sources stop this long after their envelope's nominal end.
const SOURCE_STOP_PADDING: f64 = 0.05;
const DECAY_DB: f64 = 60.0;
const OUTPUT_GAIN: f64 = 5.8;
const SUBSONIC_HZ: f64 = 40.0;
/// `sound.mjs` renders this long past the room's tail.
const RENDER_MARGIN: f64 = 0.05;
const HANDOFF_TIME: Duration = Duration::from_millis(80);
const NOTE_HANDOFF_TIME: Duration = Duration::from_millis(400);
/// Moves the variation's generator off the noise's, so `vary` leaves the noise offsets alone.
const VARY_SEED: u32 = 0x9e37_79b9;

/// The time constant that takes `decay` seconds to fall `DECAY_DB`.
fn fall_time(decay: f64) -> f64 {
    decay / ((DECAY_DB / 20.0) * LN_10)
}

/// Renders `request` at `rate` Hz: master gain and output gain applied, room and subsonic cut included.
pub fn render(request: &Request, rate: u32) -> Stereo {
    let recipe = themes::recipe(request.theme, request.cue);
    let shape = shape::shape_for(
        request.cue,
        &request.context,
        request.emphasis,
        request.since_last,
    );
    let mut vary_random = Random::new(request.seed ^ VARY_SEED);
    let jitter = recipe.vary.filter(|_| request.vary).map(|vary| Jitter {
        vary,
        random: &mut vary_random,
    });
    let layers = shape::layers_for(
        &recipe,
        request.emphasis,
        &shape,
        request.step,
        request.part,
        jitter,
    );
    if layers.is_empty() {
        return Stereo::default();
    }

    let rate = f64::from(rate);
    let end = layers
        .iter()
        .map(|layer| layer.offset + layer.attack + layer.decay + SOURCE_STOP_PADDING)
        .fold(f64::NEG_INFINITY, f64::max)
        + ROOM_SECONDS
        + RENDER_MARGIN;
    let mut dry = vec![0.0; (end * rate).ceil() as usize];
    let noise = dsp::noise(rate);
    let mut random = Random::new(request.seed);
    for layer in &layers {
        add_layer(&mut dry, layer, &noise, &mut random, rate);
    }

    let gain = recipe.master_gain * OUTPUT_GAIN;
    let send = ROOM_SEND * recipe.room;
    let rooms = ROOM_SEEDS.map(|seed| dsp::room(rate, seed));
    let size = dsp::convolution_size(dry.len(), rooms[0].len());
    let dry_spectrum = Spectrum::new(&dry, size);
    let subsonic = dsp::biquad(Filter::Highpass, SUBSONIC_HZ, FLAT_Q, rate);
    let [left, right] = rooms.map(|room| {
        let wet = dsp::convolve(&dry_spectrum, &Spectrum::new(&room, size), dry.len());
        let mut out: Vec<f64> = dry
            .iter()
            .zip(&wet)
            .map(|(dry, wet)| (dry + wet * send) * gain)
            .collect();
        dsp::filter(&mut out, &subsonic);
        out.into_iter().map(|v| v as f32).collect()
    });
    Stereo { left, right }
}

/// Web Audio's `exponentialRampToValueAtTime` from `v0` at 0 to `v1` at `t1`.
fn exp_at(v0: f64, v1: f64, t1: f64, t: f64) -> f64 {
    if t <= 0.0 {
        v0
    } else if t >= t1 {
        v1
    } else {
        v0 * (v1 / v0).powf(t / t1)
    }
}

/// Mixes one layer into `out`: a phase-accumulated sine, or the shared noise from a seeded point
/// through a biquad recomputed every 32 samples, under a straight attack and exponential fall.
fn add_layer(out: &mut [f64], layer: &Layer, noise: &[f32], random: &mut Random, rate: f64) {
    let start = (layer.offset * rate).round() as usize;
    let n = ((layer.attack + layer.decay + SOURCE_STOP_PADDING) * rate).round() as usize;
    let noise_from = (random.next() * noise.len() as f64).floor() as usize;
    let fall = fall_time(layer.decay);
    let glide_time = layer.glide_time.unwrap_or(layer.attack + layer.decay);
    let pitch_at = |t: f64| match layer.glide_to {
        Some(to) => exp_at(layer.frequency, to, glide_time, t),
        None => layer.frequency,
    };
    let envelope_at = |t: f64| {
        if t < layer.attack {
            (layer.peak * t) / layer.attack
        } else {
            layer.peak * ((layer.attack - t) / fall).exp()
        }
    };

    let mut phase = 0.0;
    let mut state = Biquad::default();
    let mut k = [0.0; 5];
    for (i, sample) in out.iter_mut().skip(start).take(n).enumerate() {
        let t = i as f64 / rate;
        let g = envelope_at(t);
        match layer.source {
            Source::Tone => {
                phase += (2.0 * std::f64::consts::PI * pitch_at(t)) / rate;
                *sample += g * phase.sin();
            }
            Source::Noise(filter) => {
                if i % 32 == 0 {
                    k = dsp::biquad(filter, pitch_at(t), layer.q, rate);
                }
                let x = f64::from(noise[(noise_from + i) % noise.len()]);
                *sample += g * state.step(&k, x);
            }
        }
    }
}

/// How many steps the cue's walk has in `theme`; 0 when it has none.
pub fn walk_len(theme: Theme, cue: Cue) -> usize {
    themes::recipe(theme, cue).walk.len()
}

/// Whether the cue in `theme` has a release to play as a gesture lets go.
pub fn has_release(theme: Theme, cue: Cue) -> bool {
    themes::recipe(theme, cue).release.is_some()
}

/// How long the cue's last play takes to fade when it plays again: 400 ms with note layers, else 80 ms.
pub fn handoff(theme: Theme, cue: Cue) -> Duration {
    if themes::recipe(theme, cue)
        .layers
        .iter()
        .any(|layer| layer.note)
    {
        NOTE_HANDOFF_TIME
    } else {
        HANDOFF_TIME
    }
}

/// The seed `scripts/sound.mjs` renders take `take` of a play with, for checking against it. A
/// [`Request`] with this seed, no context, no `since_last` and `vary` off renders what it does.
pub fn reference_seed(theme: Theme, cue: Cue, emphasis: Emphasis, part: Part, take: u32) -> u32 {
    rng::seed_of(&format!(
        "{}.{}.{}.{}.{take}",
        theme_name(theme),
        cue_name(cue),
        emphasis_name(emphasis),
        part_name(part)
    ))
}

const fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Default => "default",
        Theme::Mech => "mech",
        Theme::Bubble => "bubble",
        Theme::Press => "press",
    }
}

const fn cue_name(cue: Cue) -> &'static str {
    match cue {
        Cue::Tap => "tap",
        Cue::Type => "type",
        Cue::Select => "select",
        Cue::Toggle => "toggle",
        Cue::Open => "open",
        Cue::Close => "close",
        Cue::Navigate => "navigate",
        Cue::Success => "success",
        Cue::Warning => "warning",
        Cue::Error => "error",
        Cue::Loading => "loading",
        Cue::Ready => "ready",
        Cue::Attention => "attention",
        Cue::Count => "count",
    }
}

const fn emphasis_name(emphasis: Emphasis) -> &'static str {
    match emphasis {
        Emphasis::Subtle => "subtle",
        Emphasis::Normal => "normal",
        Emphasis::Strong => "strong",
    }
}

const fn part_name(part: Part) -> &'static str {
    match part {
        Part::Press => "press",
        Part::Release => "release",
        Part::Both => "both",
    }
}
