//! Parity with the WAVs cuelume ships for its Claude Code plugin (`claude-code/sounds`, MIT). `sound.mjs
//! plugin` wrote each as one `stereo(theme, cue)` play, normal, both parts, take 0, with no gap:
//! 16-bit stereo, each sample `round(v * 32767)` clamped. Ours go through the same quantiser.

mod common;

use std::path::PathBuf;

use cues::{Context, Cue, Emphasis, Part, Request, reference_seed, render};

/// 16-bit fixtures: the residual must sit at least this far below each channel's energy.
const PASS_DB: f64 = 85.0;
const HEADER: usize = 44;
const FULL_SCALE: f64 = 32767.0;

/// `sound.mjs`'s `q`: JavaScript's `Math.round`, which rounds halves up.
fn quantise(v: f32) -> i16 {
    (f64::from(v) * FULL_SCALE + 0.5)
        .floor()
        .clamp(-32768.0, 32767.0) as i16
}

/// The fixture's left and right channels in full-scale units.
fn read_wav(theme: &str, cue: &str) -> [Vec<f64>; 2] {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/plugin/{theme}/{cue}.wav"));
    let bytes = std::fs::read(&path).expect("fixture");
    assert_eq!(
        &bytes[36..40],
        b"data",
        "{path:?} has the 44-byte header sound.mjs writes"
    );
    let frames: Vec<[i16; 2]> = bytes[HEADER..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|f| {
            [
                i16::from_le_bytes([f[0], f[1]]),
                i16::from_le_bytes([f[2], f[3]]),
            ]
        })
        .collect();
    [0, 1].map(|channel| {
        frames
            .iter()
            .map(|f| f64::from(f[channel]) / FULL_SCALE)
            .collect()
    })
}

#[test]
fn render_should_match_cuelumes_plugin_wavs() {
    let mut failures = Vec::new();
    for theme_name in ["default", "mech", "bubble", "press"] {
        for (cue_name, cue) in [("ready", Cue::Ready), ("attention", Cue::Attention)] {
            let theme = common::theme(theme_name);
            let stereo = render(
                &Request {
                    theme,
                    cue,
                    emphasis: Emphasis::Normal,
                    context: Context::default(),
                    since_last: None,
                    step: 0,
                    part: Part::Both,
                    vary: false,
                    seed: reference_seed(theme, cue, Emphasis::Normal, Part::Both, 0),
                },
                cues::SAMPLE_RATE,
            );
            let reference = read_wav(theme_name, cue_name);
            for (channel, (ours, reference)) in [&stereo.left, &stereo.right]
                .into_iter()
                .zip(&reference)
                .enumerate()
            {
                let ours: Vec<f64> = ours
                    .iter()
                    .map(|&v| f64::from(quantise(v)) / FULL_SCALE)
                    .collect();
                let db = common::residual_db(reference, &ours);
                println!("{theme_name}/{cue_name} channel {channel}: {db:.1} dB down");
                if ours.len() != reference.len() || db < PASS_DB {
                    failures.push(format!(
                        "{theme_name}/{cue_name} channel {channel}: {db:.1} dB, length {} vs ours {}",
                        reference.len(),
                        ours.len()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
