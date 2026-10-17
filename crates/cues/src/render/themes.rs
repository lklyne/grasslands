//! The four themes' recipes, transcribed from cuelume's `recipes.ts`, `mech.ts`, `bubble.ts` and
//! `press.ts`. Every number is cuelume's; see those files for what each layer is.

use super::layers::{
    BAR, Filter, LANDED, Layer, MALLET, air, click, contact, from, knock, noise, note, ring,
    struck, thud, tone,
};
use crate::{Cue, Emphasis, Theme};

use Emphasis::{Normal, Strong};
use Filter::{Bandpass, Lowpass};

/// Per-play randomness, as fractions: the strike's pitch and level move by up to ± these.
#[derive(Clone, Copy, Debug)]
pub(super) struct Variation {
    pub pitch: f64,
    pub level: f64,
}

#[derive(Clone, Debug)]
pub(super) struct Recipe {
    pub master_gain: f64,
    pub layers: Vec<Layer>,
    /// How strongly the cue rings into the room, as a multiple of the shared send.
    pub room: f64,
    pub vary: Option<Variation>,
    pub release: Option<Vec<Layer>>,
    /// Semitones the cue's note layers step through on quick repeats.
    pub walk: &'static [f64],
}

impl Recipe {
    fn new(master_gain: f64, layers: Vec<Layer>) -> Self {
        Self {
            master_gain,
            layers,
            room: 1.0,
            vary: None,
            release: None,
            walk: &[],
        }
    }

    fn vary(self, pitch: f64, level: f64) -> Self {
        Self {
            vary: Some(Variation { pitch, level }),
            ..self
        }
    }

    fn room(self, room: f64) -> Self {
        Self { room, ..self }
    }

    fn release(self, release: Vec<Layer>) -> Self {
        Self {
            release: Some(release),
            ..self
        }
    }

    fn walk(self, walk: &'static [f64]) -> Self {
        Self { walk, ..self }
    }
}

pub(super) fn recipe(theme: Theme, cue: Cue) -> Recipe {
    match theme {
        Theme::Default => default(cue),
        Theme::Mech => mech(cue),
        Theme::Bubble => bubble(cue),
        Theme::Press => press(cue),
    }
}

fn default(cue: Cue) -> Recipe {
    match cue {
        Cue::Tap => Recipe::new(
            0.39,
            vec![
                noise(Bandpass, 4500.0, 1.2, 0.001, 0.00242, 0.03).from(Normal),
                tone(1174.66, 0.001, 0.18, 0.0135),
                tone(1180.0, 0.001, 0.164, 0.008),
                tone(3242.0, 0.001, 0.0827, 0.0055),
                knock(300.0, 0.0261, 0.02).from(Normal),
                tone(587.33, 0.002, 0.246, 0.009).from(Strong),
                knock(150.0, 0.0489, 0.014).from(Strong),
            ],
        )
        .vary(0.012, 0.12),
        Cue::Type => Recipe::new(
            0.375,
            vec![
                noise(Bandpass, 3900.0, 0.8, 0.001, 0.00517, 0.08),
                noise(Bandpass, 1550.0, 3.5, 0.001, 0.0144, 0.22).from(Normal),
                noise(Bandpass, 310.0, 2.5, 0.002, 0.0254, 0.35),
                noise(Bandpass, 140.0, 2.0, 0.002, 0.037, 0.45).from(Strong),
            ],
        )
        .vary(0.07, 0.2),
        Cue::Select => Recipe::new(
            0.306,
            vec![
                noise(Bandpass, 2800.0, 2.2, 0.001, 0.00749, 0.16).from(Normal),
                knock(415.0, 0.0194, 0.03),
                knock(208.0, 0.0313, 0.025).from(Strong),
            ],
        ),
        Cue::Toggle => Recipe::new(
            0.24,
            vec![
                noise(Bandpass, 1830.0, 1.6, 0.001, 0.0156, 0.12),
                knock(330.0, 0.0339, 0.045),
                noise(Bandpass, 3150.0, 1.6, 0.001, 0.0124, 0.08).from(Normal),
                knock(165.0, 0.0375, 0.025).from(Strong),
            ],
        ),
        Cue::Open => Recipe::new(
            0.335,
            vec![
                noise(Bandpass, 600.0, 1.4, 0.05, 0.0558, 0.168).glide(1500.0, 0.1),
                tone(659.25, 0.05, 0.165, 0.015),
                tone(1318.51, 0.05, 0.0883, 0.005).from(Normal),
                noise(Lowpass, 250.0, 0.7, 0.04, 0.0779, 0.12).from(Strong),
            ],
        ),
        Cue::Close => Recipe::new(
            0.258,
            vec![
                noise(Bandpass, 1400.0, 1.4, 0.02, 0.054, 0.216).glide(540.0, 0.07),
                tone(261.63, 0.003, 0.104, 0.02),
                noise(Bandpass, 350.0, 2.0, 0.001, 0.0273, 0.2).from(Normal),
                noise(Bandpass, 165.0, 2.0, 0.001, 0.0345, 0.3).from(Strong),
            ],
        ),
        Cue::Success => Recipe::new(
            0.566,
            [
                vec![contact(1500.0, 0.04)],
                struck(523.25, 0.333, 0.022, MALLET),
                struck(783.99, 0.391, 0.02, MALLET),
                struck(1318.51, 0.335, 0.014, MALLET),
                from(struck(261.63, 0.469, 0.02, MALLET), Strong),
            ]
            .concat(),
        )
        .room(LANDED),
        Cue::Error => Recipe::new(
            0.501,
            [
                vec![contact(900.0, 0.05)],
                struck(293.66, 0.138, 0.04, MALLET),
                struck(698.46, 0.106, 0.018, MALLET),
                vec![noise(Bandpass, 250.0, 1.5, 0.001, 0.0273, 0.2).from(Strong)],
            ]
            .concat(),
        ),
        Cue::Navigate => Recipe::new(
            0.322,
            vec![
                noise(Bandpass, 400.0, 1.2, 0.12, 0.111, 0.18).glide(1100.0, 0.21),
                noise(Lowpass, 540.0, 0.7, 0.09, 0.109, 0.045).from(Normal),
                noise(Lowpass, 230.0, 0.7, 0.1, 0.144, 0.1).from(Strong),
            ],
        ),
        Cue::Warning => Recipe::new(
            0.437,
            [
                vec![contact(1200.0, 0.04)],
                struck(440.0, 0.163, 0.038, MALLET),
                struck(659.25, 0.145, 0.03, MALLET),
                from(struck(220.0, 0.261, 0.02, MALLET), Strong),
            ]
            .concat(),
        ),
        Cue::Loading => Recipe::new(
            0.106,
            vec![
                tone(196.0, 0.35, 0.969, 0.03),
                tone(392.0, 0.35, 0.9, 0.01),
                noise(Lowpass, 500.0, 0.7, 0.35, 0.844, 0.006).from(Normal),
                tone(98.0, 0.35, 1.04, 0.02).from(Strong),
            ],
        ),
        Cue::Ready => Recipe::new(
            0.467,
            [
                vec![contact(1500.0, 0.02)],
                struck(392.0, 0.497, 0.026, BAR),
                vec![tone(393.2, 0.003, 0.462, 0.012)],
                from(struck(196.0, 0.612, 0.016, BAR), Strong),
            ]
            .concat(),
        )
        .room(LANDED),
        Cue::Attention => Recipe::new(
            0.551,
            [
                vec![contact(2500.0, 0.02)],
                struck(880.0, 0.599, 0.018, BAR),
                vec![tone(882.0, 0.003, 0.57, 0.01)],
                struck(1318.51, 0.559, 0.014, BAR),
                from(struck(440.0, 0.649, 0.012, BAR), Strong),
            ]
            .concat(),
        ),
        Cue::Count => Recipe::new(
            0.336,
            vec![
                noise(Bandpass, 700.0, 1.2, 0.15, 0.611, 0.05)
                    .glide(1400.0, 0.6)
                    .stretch(),
                tone(1046.5, 0.15, 0.721, 0.012).stretch(),
                noise(Bandpass, 2000.0, 2.0, 0.15, 0.652, 0.02)
                    .glide(3200.0, 0.6)
                    .stretch()
                    .from(Normal),
                tone(261.63, 0.15, 0.758, 0.015).stretch().from(Strong),
            ],
        )
        .vary(0.02, 0.12),
    }
}

fn mech(cue: Cue) -> Recipe {
    match cue {
        Cue::Tap => Recipe::new(
            0.964,
            vec![
                noise(Bandpass, 2650.0, 1.5, 0.001, 0.00413, 0.08),
                knock(455.0, 0.0156, 0.02),
                noise(Bandpass, 4600.0, 2.0, 0.001, 0.00222, 0.05).from(Normal),
                noise(Bandpass, 215.0, 2.0, 0.001, 0.0216, 0.3).from(Strong),
            ],
        )
        .vary(0.02, 0.1),
        Cue::Type => Recipe::new(
            0.484,
            vec![
                noise(Bandpass, 3000.0, 1.2, 0.001, 0.003, 0.1),
                noise(Bandpass, 2150.0, 5.0, 0.001, 0.00737, 0.18).from(Normal),
                noise(Bandpass, 750.0, 3.0, 0.001, 0.00898, 0.22),
                noise(Bandpass, 330.0, 2.5, 0.001, 0.0173, 0.3).from(Strong),
            ],
        )
        .vary(0.05, 0.15),
        Cue::Select => Recipe::new(
            0.295,
            vec![
                noise(Bandpass, 3650.0, 3.0, 0.001, 0.00273, 0.2).from(Normal),
                noise(Bandpass, 1500.0, 8.0, 0.001, 0.00762, 0.35),
                tone(1480.0, 0.001, 0.0433, 0.012),
                knock(250.0, 0.0188, 0.025).from(Strong),
            ],
        ),
        Cue::Toggle => Recipe::new(
            0.293,
            vec![
                noise(Bandpass, 1650.0, 2.0, 0.001, 0.00381, 0.14),
                knock(330.0, 0.0288, 0.04),
                noise(Bandpass, 2650.0, 3.0, 0.001, 0.00553, 0.18).from(Normal),
                noise(Bandpass, 250.0, 2.0, 0.001, 0.0259, 0.3).from(Strong),
            ],
        ),
        Cue::Open => Recipe::new(
            0.869,
            vec![
                noise(Bandpass, 2150.0, 3.0, 0.001, 0.00375, 0.16),
                noise(Bandpass, 1160.0, 2.0, 0.02, 0.031, 0.08).glide(2150.0, 0.05),
                noise(Bandpass, 1330.0, 5.0, 0.001, 0.00763, 0.14).from(Normal),
                noise(Bandpass, 290.0, 2.0, 0.001, 0.0265, 0.25).from(Strong),
            ],
        ),
        Cue::Close => Recipe::new(
            0.96,
            vec![
                noise(Bandpass, 2150.0, 2.0, 0.015, 0.0288, 0.04).glide(1080.0, 0.04),
                noise(Bandpass, 1000.0, 4.0, 0.001, 0.00898, 0.22),
                noise(Bandpass, 3300.0, 2.0, 0.001, 0.003, 0.1).from(Normal),
                noise(Bandpass, 180.0, 2.0, 0.001, 0.0302, 0.3).from(Strong),
            ],
        ),
        Cue::Success => Recipe::new(
            0.662,
            [
                vec![contact(2200.0, 0.05)],
                struck(698.46, 0.121, 0.03, BAR),
                struck(1046.5, 0.149, 0.026, BAR),
                struck(1760.0, 0.106, 0.018, BAR),
                vec![noise(Bandpass, 4150.0, 2.0, 0.001, 0.00324, 0.06).from(Strong)],
            ]
            .concat(),
        ),
        Cue::Error => Recipe::new(
            0.59,
            [
                vec![noise(Bandpass, 430.0, 4.0, 0.001, 0.0173, 0.3)],
                struck(293.66, 0.0678, 0.045, BAR),
                struck(698.46, 0.0522, 0.02, BAR),
                vec![
                    noise(Bandpass, 1830.0, 3.0, 0.001, 0.006, 0.1).from(Normal),
                    noise(Bandpass, 150.0, 2.0, 0.001, 0.0345, 0.3).from(Strong),
                ],
            ]
            .concat(),
        ),
        Cue::Navigate => Recipe::new(
            0.804,
            vec![
                noise(Bandpass, 750.0, 1.8, 0.05, 0.096, 0.1).glide(1400.0, 0.13),
                noise(Bandpass, 2500.0, 4.0, 0.001, 0.003, 0.1).from(Normal),
                noise(Bandpass, 250.0, 1.5, 0.04, 0.109, 0.12).from(Strong),
            ],
        ),
        Cue::Warning => Recipe::new(
            0.484,
            [
                vec![contact(2000.0, 0.05)],
                struck(587.33, 0.0922, 0.04, BAR),
                struck(880.0, 0.0969, 0.03, BAR),
                vec![noise(Bandpass, 300.0, 2.0, 0.001, 0.0265, 0.25).from(Strong)],
            ]
            .concat(),
        ),
        Cue::Loading => Recipe::new(
            0.223,
            vec![
                noise(Bandpass, 500.0, 1.0, 0.35, 0.848, 0.03),
                tone(233.08, 0.35, 0.965, 0.015),
                noise(Bandpass, 1000.0, 2.0, 0.35, 0.883, 0.005).from(Normal),
                noise(Lowpass, 250.0, 0.7, 0.35, 0.807, 0.04).from(Strong),
            ],
        ),
        Cue::Ready => Recipe::new(
            0.546,
            [
                vec![contact(2000.0, 0.05)],
                struck(523.25, 0.178, 0.05, BAR),
                from(struck(261.63, 0.242, 0.03, BAR), Strong),
            ]
            .concat(),
        ),
        Cue::Attention => Recipe::new(
            0.481,
            [
                vec![contact(2500.0, 0.04)],
                struck(880.0, 0.231, 0.04, BAR),
                struck(1318.51, 0.242, 0.03, BAR),
                vec![noise(Bandpass, 300.0, 2.0, 0.001, 0.0265, 0.25).from(Strong)],
            ]
            .concat(),
        ),
        Cue::Count => Recipe::new(
            0.654,
            vec![
                noise(Bandpass, 500.0, 2.0, 0.1, 0.648, 0.06)
                    .glide(900.0, 0.6)
                    .stretch(),
                noise(Bandpass, 1900.0, 4.0, 0.1, 0.606, 0.03)
                    .glide(3300.0, 0.6)
                    .stretch()
                    .from(Normal),
                noise(Bandpass, 220.0, 2.0, 0.1, 0.5, 0.1)
                    .stretch()
                    .from(Strong),
            ],
        )
        .vary(0.03, 0.1),
    }
}

/// A sine that slides from `from` to `to` Hz in `time` seconds.
const fn slide(from: f64, to: f64, time: f64, decay: f64, peak: f64) -> Layer {
    tone(from, 0.002, decay, peak).glide(to, time)
}

/// A bubble surfacing: a sine that rises by half in 40 ms.
const fn bloop(frequency: f64, decay: f64, peak: f64) -> Layer {
    slide(frequency, frequency * 1.5, 0.04, decay, peak)
}

/// A drop falling from a tap: a quick flick upward, done in 12 ms.
const fn drip(frequency: f64, decay: f64, peak: f64) -> Layer {
    slide(frequency, frequency * 1.5, 0.012, decay, peak).attack(0.001)
}

/// A bubble bursting: 3 ms of low-passed noise.
const fn pop(frequency: f64, peak: f64) -> Layer {
    noise(Lowpass, frequency, 0.9, 0.001, 0.0032, peak)
}

/// bubble's LIVELY and STEADY variations.
const LIVELY: (f64, f64) = (0.06, 0.1);
const STEADY: (f64, f64) = (0.0, 0.08);

fn bubble(cue: Cue) -> Recipe {
    match cue {
        Cue::Tap => Recipe::new(
            0.411,
            vec![
                knock(300.0, 0.0216, 0.06),
                bloop(600.0, 0.0489, 0.014).attack(0.012),
                knock(600.0, 0.0165, 0.015).from(Normal),
                knock(150.0, 0.0389, 0.05).from(Strong),
            ],
        )
        .vary(0.08, 0.1),
        Cue::Type => Recipe::new(
            0.383,
            vec![
                knock(415.0, 0.0151, 0.06),
                pop(2500.0, 0.03).from(Normal),
                knock(208.0, 0.0278, 0.05).from(Strong),
            ],
        )
        .vary(0.1, 0.2),
        Cue::Select => Recipe::new(
            0.214,
            vec![
                drip(700.0, 0.0278, 0.05),
                pop(3000.0, 0.075).from(Normal),
                bloop(350.0, 0.0606, 0.03).from(Strong),
            ],
        )
        .vary(0.03, 0.1),
        Cue::Toggle => Recipe::new(
            0.175,
            vec![
                noise(Lowpass, 900.0, 0.9, 0.001, 0.00779, 0.12),
                slide(330.0, 660.0, 0.06, 0.0791, 0.045).attack(0.003),
                slide(990.0, 1485.0, 0.02, 0.045, 0.01)
                    .from(Normal)
                    .attack(0.03),
                slide(165.0, 330.0, 0.06, 0.0969, 0.03).from(Strong),
            ],
        )
        .vary(0.04, 0.1),
        Cue::Open => Recipe::new(
            0.221,
            vec![
                slide(220.0, 520.0, 0.14, 0.0692, 0.04).attack(0.1),
                slide(330.0, 780.0, 0.14, 0.0866, 0.012)
                    .from(Normal)
                    .attack(0.1),
                slide(110.0, 260.0, 0.14, 0.0969, 0.03)
                    .from(Strong)
                    .attack(0.1),
            ],
        )
        .vary(LIVELY.0, LIVELY.1),
        Cue::Close => Recipe::new(
            0.357,
            vec![
                slide(700.0, 220.0, 0.07, 0.0905, 0.045).attack(0.004),
                pop(1200.0, 0.06).from(Normal),
                slide(350.0, 110.0, 0.07, 0.121, 0.03)
                    .from(Strong)
                    .attack(0.004),
            ],
        )
        .vary(LIVELY.0, LIVELY.1),
        Cue::Success => Recipe::new(
            0.52,
            vec![
                slide(523.25, 783.99, 0.08, 0.323, 0.04).attack(0.003),
                slide(1046.5, 1567.98, 0.08, 0.12, 0.01)
                    .from(Normal)
                    .attack(0.003),
                slide(261.63, 392.0, 0.08, 0.391, 0.02)
                    .from(Strong)
                    .attack(0.003),
            ],
        )
        .room(LANDED)
        .vary(STEADY.0, STEADY.1),
        Cue::Error => Recipe::new(
            0.387,
            vec![
                slide(349.23, 261.63, 0.14, 0.226, 0.045).attack(0.004),
                pop(900.0, 0.12).from(Normal),
                knock(147.0, 0.0556, 0.05).from(Strong),
            ],
        )
        .vary(STEADY.0, STEADY.1),
        Cue::Navigate => Recipe::new(
            0.554,
            vec![
                slide(300.0, 1500.0, 0.09, 0.0943, 0.035).attack(0.01),
                noise(Bandpass, 1500.0, 1.0, 0.02, 0.0778, 0.05)
                    .glide(3000.0, 0.09)
                    .from(Normal),
                slide(150.0, 600.0, 0.1, 0.125, 0.025)
                    .from(Strong)
                    .attack(0.01),
            ],
        )
        .vary(LIVELY.0, LIVELY.1),
        Cue::Warning => Recipe::new(
            0.436,
            vec![
                slide(500.0, 440.0, 0.05, 0.181, 0.045),
                pop(1800.0, 0.09).from(Normal),
                tone(220.0, 0.003, 0.225, 0.025).from(Strong),
            ],
        )
        .vary(STEADY.0, STEADY.1),
        Cue::Loading => Recipe::new(
            0.194,
            vec![
                noise(Lowpass, 900.0, 0.7, 0.35, 0.727, 0.03),
                slide(400.0, 600.0, 0.35, 0.953, 0.016).attack(0.35),
                noise(Bandpass, 1800.0, 1.5, 0.35, 0.749, 0.004).from(Normal),
                noise(Lowpass, 400.0, 0.7, 0.35, 0.807, 0.04).from(Strong),
            ],
        )
        .vary(STEADY.0, STEADY.1),
        Cue::Ready => Recipe::new(
            0.532,
            vec![
                slide(1400.0, 700.0, 0.012, 0.0242, 0.03).attack(0.001),
                slide(392.0, 587.33, 0.06, 0.288, 0.04).attack(0.012),
                pop(2000.0, 0.03).from(Normal),
                slide(196.0, 294.0, 0.08, 0.391, 0.02)
                    .from(Strong)
                    .attack(0.012),
            ],
        )
        .room(LANDED)
        .vary(STEADY.0, STEADY.1),
        Cue::Attention => Recipe::new(
            0.666,
            vec![
                slide(659.25, 880.0, 0.14, 0.295, 0.035).attack(0.003),
                pop(2200.0, 0.12).from(Normal),
                bloop(329.63, 0.261, 0.02).from(Strong),
            ],
        )
        .vary(STEADY.0, STEADY.1),
        Cue::Count => Recipe::new(
            0.267,
            vec![
                noise(Lowpass, 900.0, 0.7, 0.15, 0.634, 0.04)
                    .glide(1800.0, 0.6)
                    .stretch(),
                slide(587.33, 880.0, 0.6, 0.717, 0.02)
                    .attack(0.15)
                    .stretch(),
                pop(1600.0, 0.05).from(Normal),
                slide(293.66, 440.0, 0.6, 0.717, 0.02)
                    .attack(0.15)
                    .from(Strong)
                    .stretch(),
            ],
        )
        .vary(LIVELY.0, LIVELY.1),
    }
}

const C2: f64 = 65.41;
const E2: f64 = 82.41;
const A2: f64 = 110.0;
const G3: f64 = 196.0;
const A3: f64 = 220.0;
const C4: f64 = 261.63;
const E4: f64 = 329.63;
const G4: f64 = 392.0;
const A4: f64 = 440.0;
const C5: f64 = 523.25;
const E5: f64 = 659.25;
const A5: f64 = 880.0;

const PRESS_HZ: f64 = 4000.0;
const RELEASE_HZ: f64 = 4800.0;
const WALK: &[f64] = &[0.0, 4.0, 9.0];

fn press(cue: Cue) -> Recipe {
    match cue {
        Cue::Tap => Recipe::new(
            0.0431,
            [
                click(PRESS_HZ, 0.53),
                note(C2, 1.36, 0.12),
                vec![thud(200.0, 0.0472, 0.15).from(Strong)],
            ]
            .concat(),
        )
        .release(from(click(RELEASE_HZ, 0.28), Normal))
        .walk(WALK)
        .vary(STEADY.0, STEADY.1),
        Cue::Type => Recipe::new(
            0.297,
            [
                click(4200.0, 0.25),
                vec![
                    knock(305.0, 0.0288, 0.04),
                    knock(150.0, 0.0424, 0.03).from(Strong),
                ],
            ]
            .concat(),
        )
        .release(vec![
            ring(4100.0, 0.08).from(Normal),
            ring(1000.0, 0.06).q(2.0).from(Normal),
            knock(414.0, 0.0216, 0.012).from(Normal),
        ])
        .vary(0.08, 0.15),
        Cue::Select => Recipe::new(
            0.442,
            [
                click(RELEASE_HZ, 0.12),
                vec![
                    knock(700.0, 0.0196, 0.02),
                    knock(350.0, 0.0326, 0.02).from(Strong),
                ],
            ]
            .concat(),
        )
        .release(vec![ring(RELEASE_HZ, 0.03).from(Normal)])
        .vary(0.02, 0.1),
        Cue::Toggle => Recipe::new(
            0.0571,
            [
                click(PRESS_HZ, 0.245),
                vec![knock(330.0, 0.0389, 0.05)],
                note(E2, 0.324, 0.06),
                vec![knock(165.0, 0.05, 0.05).from(Strong)],
            ]
            .concat(),
        )
        .release(from(click(RELEASE_HZ, 0.1), Normal))
        .vary(STEADY.0, STEADY.1),
        Cue::Open => Recipe::new(
            0.0857,
            [
                click(RELEASE_HZ, 0.15),
                vec![knock(440.0, 0.0363, 0.03), air(1600.0, 3200.0, 0.035, 0.1)],
                note(A2, 0.827, 0.08),
                vec![thud(260.0, 0.039, 0.12).from(Strong)],
            ]
            .concat(),
        )
        .release(vec![ring(RELEASE_HZ, 0.05).from(Normal)])
        .walk(WALK)
        .vary(STEADY.0, STEADY.1),
        Cue::Close => Recipe::new(
            0.099,
            [
                click(3200.0, 0.204),
                vec![
                    knock(130.81, 0.0432, 0.06),
                    air(1800.0, 700.0, 0.04, 0.1).from(Normal),
                ],
                note(C2, 0.258, 0.08),
                vec![thud(180.0, 0.0364, 0.2).from(Strong)],
            ]
            .concat(),
        )
        .vary(STEADY.0, STEADY.1),
        Cue::Success => Recipe::new(
            0.102,
            [
                note(C4, 1.47, 0.072),
                note(G4, 1.51, 0.06),
                note(E5, 1.36, 0.045),
                from(note(C4 / 2.0, 1.64, 0.036), Strong),
            ]
            .concat(),
        )
        .room(LANDED),
        Cue::Error => Recipe::new(
            0.0708,
            [
                note(A2, 0.487, 0.12),
                note(C5, 0.461, 0.04),
                vec![
                    thud(200.0, 0.0545, 0.2).attack(0.01),
                    thud(120.0, 0.0727, 0.2).from(Strong),
                ],
            ]
            .concat(),
        ),
        Cue::Navigate => Recipe::new(
            0.837,
            [
                click(RELEASE_HZ, 0.15),
                vec![
                    air(600.0, 1400.0, 0.0877, 0.12).q(1.0),
                    knock(587.33, 0.0207, 0.015).from(Normal),
                    thud(300.0, 0.0487, 0.12).from(Strong),
                ],
            ]
            .concat(),
        )
        .vary(STEADY.0, STEADY.1),
        Cue::Warning => Recipe::new(
            0.0716,
            [
                note(A3, 0.905, 0.096),
                note(E4, 0.84, 0.072),
                from(note(A3 / 2.0, 1.01, 0.048), Strong),
            ]
            .concat(),
        ),
        Cue::Loading => Recipe::new(
            0.0661,
            vec![
                tone(G3, 0.35, 0.889, 0.05),
                tone(G3 * 2.0, 0.35, 0.9, 0.01).from(Normal),
                tone(G3 / 2.0, 0.35, 0.969, 0.03).from(Strong),
            ],
        ),
        Cue::Ready => Recipe::new(
            0.0983,
            [
                note(G3, 1.78, 0.108),
                from(note(G3 / 2.0, 1.98, 0.054), Strong),
            ]
            .concat(),
        )
        .room(LANDED),
        Cue::Attention => Recipe::new(
            0.125,
            [
                note(A4, 2.34, 0.067),
                note(E5, 2.24, 0.048),
                note(A5, 1.89, 0.024),
                from(note(A4 / 2.0, 2.61, 0.034), Strong),
            ]
            .concat(),
        ),
        Cue::Count => Recipe::new(
            0.362,
            vec![
                noise(Bandpass, 900.0, 2.0, 0.15, 0.568, 0.08)
                    .glide(1800.0, 0.6)
                    .stretch(),
                noise(Lowpass, 200.0, 0.7, 0.15, 0.594, 0.06)
                    .glide(500.0, 0.6)
                    .stretch(),
                noise(Bandpass, 2600.0, 4.0, 0.15, 0.587, 0.02)
                    .glide(3600.0, 0.6)
                    .stretch()
                    .from(Normal),
                noise(Lowpass, 150.0, 0.7, 0.15, 0.568, 0.08)
                    .glide(300.0, 0.6)
                    .stretch()
                    .from(Strong),
            ],
        )
        .vary(0.02, 0.1),
    }
}
