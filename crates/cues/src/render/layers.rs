//! cuelume's `layers.ts`: the layer model and the builders themes spell materials with.

use crate::Emphasis;

/// A noise layer's filter, read as Web Audio's `BiquadFilterNode` reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Filter {
    Lowpass,
    Highpass,
    Bandpass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// A sine. cuelume's recipes use no other waveform and no detune.
    Tone,
    Noise(Filter),
}

/// One tone or one burst of filtered noise under an attack/decay envelope.
#[derive(Clone, Copy, Debug)]
pub(super) struct Layer {
    pub source: Source,
    /// A tone's frequency, or a noise layer's filter frequency.
    pub frequency: f64,
    /// A noise layer's filter Q: linear for bandpass, dB for lowpass and highpass.
    pub q: f64,
    pub offset: f64,
    pub attack: f64,
    /// Seconds to fall 60 dB once the attack ends.
    pub decay: f64,
    pub peak: f64,
    pub glide_to: Option<f64>,
    /// Defaults to attack + decay.
    pub glide_time: Option<f64>,
    /// The lowest emphasis this layer plays at.
    pub from: Emphasis,
    /// Lasts as long as a `count`, not only starts in step with it.
    pub stretch: bool,
    /// Moves with the walk and lets go slowly when the cue plays again.
    pub note: bool,
}

/// Web Audio's default Q, which `sound.mjs` also falls back to.
const DEFAULT_Q: f64 = 1.0;

pub(super) const fn tone(frequency: f64, attack: f64, decay: f64, peak: f64) -> Layer {
    Layer {
        source: Source::Tone,
        frequency,
        q: DEFAULT_Q,
        offset: 0.0,
        attack,
        decay,
        peak,
        glide_to: None,
        glide_time: None,
        from: Emphasis::Subtle,
        stretch: false,
        note: false,
    }
}

pub(super) const fn noise(
    filter: Filter,
    frequency: f64,
    q: f64,
    attack: f64,
    decay: f64,
    peak: f64,
) -> Layer {
    Layer {
        source: Source::Noise(filter),
        q,
        ..tone(frequency, attack, decay, peak)
    }
}

impl Layer {
    pub const fn glide(self, to: f64, time: f64) -> Self {
        Self {
            glide_to: Some(to),
            glide_time: Some(time),
            ..self
        }
    }

    pub const fn from(self, from: Emphasis) -> Self {
        Self { from, ..self }
    }

    pub const fn attack(self, attack: f64) -> Self {
        Self { attack, ..self }
    }

    pub const fn q(self, q: f64) -> Self {
        Self { q, ..self }
    }

    pub const fn stretch(self) -> Self {
        Self {
            stretch: true,
            ..self
        }
    }
}

/// Moves a whole group to a higher emphasis. For `struck` and `note` this matches passing `from`
/// to the builder: every mode or partial then takes it.
pub(super) fn from(layers: Vec<Layer>, from: Emphasis) -> Vec<Layer> {
    layers.into_iter().map(|layer| layer.from(from)).collect()
}

/// How a cue rings into the room when its result lands.
pub(super) const LANDED: f64 = 2.0;

const KNOCK_DROP: f64 = 1.6;
const KNOCK_DROP_TIME: f64 = 0.018;

/// A sine struck sharp that drops to `frequency` in 18 ms.
pub(super) const fn knock(frequency: f64, decay: f64, peak: f64) -> Layer {
    tone(frequency * KNOCK_DROP, 0.001, decay, peak).glide(frequency, KNOCK_DROP_TIME)
}

/// A struck object's modes as (ratio to the fundamental, level).
pub(super) type Material = &'static [(f64, f64)];

pub(super) const BAR: Material = &[(1.0, 1.0), (2.76, 0.3), (5.4, 0.1)];
pub(super) const MALLET: Material = &[(1.0, 1.0), (3.99, 0.2)];

const MODE_CEILING: f64 = 5000.0;
const STRIKE_ATTACK: f64 = 0.003;
const SHORTEST_MODE: f64 = 0.028;
const MODE_DAMPING: f64 = 0.75;

/// One sine per mode of `material`, the higher modes dying sooner and left out of subtle.
pub(super) fn struck(frequency: f64, decay: f64, peak: f64, material: Material) -> Vec<Layer> {
    material
        .iter()
        .filter(|(ratio, _)| frequency * ratio <= MODE_CEILING)
        .enumerate()
        .map(|(mode, &(ratio, level))| {
            let layer = tone(
                frequency * ratio,
                STRIKE_ATTACK,
                SHORTEST_MODE.max(decay / ratio.powf(MODE_DAMPING)),
                peak * level,
            );
            if mode == 0 {
                layer
            } else {
                layer.from(Emphasis::Normal)
            }
        })
        .collect()
}

/// A lowpass or highpass Q in dB that gives a flat corner.
pub(super) const FLAT_Q: f64 = -3.0;

/// The mallet meeting the bar: 3 ms of noise, low-passed at the mallet's hardness.
pub(super) const fn contact(hardness: f64, peak: f64) -> Layer {
    noise(Filter::Lowpass, hardness, FLAT_Q, 0.001, 0.0035, peak)
}

/// One narrow resonance of a switch's click.
pub(super) const fn ring(frequency: f64, peak: f64) -> Layer {
    noise(Filter::Bandpass, frequency, 8.0, 0.001, 0.0055, peak)
}

const CLICK_BODY: f64 = 0.675;

/// A tight ring over a softer, wider body below it.
pub(super) fn click(frequency: f64, peak: f64) -> Vec<Layer> {
    vec![
        ring(frequency, peak),
        ring(frequency * CLICK_BODY, peak * 0.5).q(2.0),
    ]
}

/// Filtered noise sweeping from `from` to `to` Hz.
pub(super) const fn air(from: f64, to: f64, decay: f64, peak: f64) -> Layer {
    noise(Filter::Bandpass, from, 1.5, 0.008, decay, peak).glide(to, decay)
}

/// A part hitting its stop.
pub(super) const fn thud(frequency: f64, decay: f64, peak: f64) -> Layer {
    noise(Filter::Lowpass, frequency, 0.7, 0.001, decay, peak)
}

/// (multiple, share of the ring, level) for each of a note's partials.
const NOTE_PARTIALS: [(f64, f64, f64); 4] = [
    (1.0, 1.0, 1.0),
    (2.0, 0.9, 0.5),
    (3.0, 1.0, 0.04),
    (4.0, 0.73, 0.056),
];
const NOTE_SHARP: f64 = 1.09;
const NOTE_SETTLE: f64 = 0.015;
const NOTE_SWELL: f64 = 0.11;

/// A deep note that swells in under a click and rings on; its upper partials are ornament.
pub(super) fn note(frequency: f64, decay: f64, peak: f64) -> Vec<Layer> {
    let attack = NOTE_SWELL.min(decay / 2.0);
    NOTE_PARTIALS
        .iter()
        .enumerate()
        .map(|(partial, &(multiple, length, level))| {
            let layer = Layer {
                note: true,
                ..tone(
                    frequency * multiple * NOTE_SHARP,
                    attack,
                    decay * length,
                    peak * level,
                )
                .glide(frequency * multiple, NOTE_SETTLE)
            };
            if partial < 2 {
                layer
            } else {
                layer.from(Emphasis::Normal)
            }
        })
        .collect()
}
