//! What the parity tests share: cuelume's theme names and the residual measure.

use cues::Theme;

pub fn theme(name: &str) -> Theme {
    match name {
        "default" => Theme::Default,
        "mech" => Theme::Mech,
        "bubble" => Theme::Bubble,
        "press" => Theme::Press,
        _ => panic!("unknown theme {name}"),
    }
}

/// How far below the reference's energy the residual (reference − ours) sits, in dB. Infinite
/// when they match exactly, silence included.
pub fn residual_db(reference: &[f64], ours: &[f64]) -> f64 {
    let signal: f64 = reference.iter().map(|v| v * v).sum();
    let residual: f64 = reference
        .iter()
        .zip(ours)
        .map(|(r, o)| (r - o) * (r - o))
        .sum();
    if residual == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (signal / residual).log10()
    }
}
