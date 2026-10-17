//! The render contract the mixer relies on.

use std::time::Duration;

use cues::{Context, Cue, Emphasis, Part, Request, Theme, handoff, has_release, render, walk_len};

fn request(theme: Theme, cue: Cue, part: Part) -> Request {
    Request {
        theme,
        cue,
        emphasis: Emphasis::Normal,
        context: Context::default(),
        since_last: None,
        step: 0,
        part,
        vary: false,
        seed: 7,
    }
}

/// `sound.mjs` renders the release 90 ms (`RELEASE_GAP`) after the press when both play.
const RELEASE_GAP_SAMPLES: usize = 4320;

#[test]
fn both_should_hold_the_press_and_the_release_after_it() {
    let both = render(
        &request(Theme::Press, Cue::Tap, Part::Both),
        cues::SAMPLE_RATE,
    );
    let press = render(
        &request(Theme::Press, Cue::Tap, Part::Press),
        cues::SAMPLE_RATE,
    );
    let release = render(
        &request(Theme::Press, Cue::Tap, Part::Release),
        cues::SAMPLE_RATE,
    );
    assert!(!release.left.is_empty());

    // up to where the release starts, both is the press alone
    let shared = RELEASE_GAP_SAMPLES.min(press.left.len());
    let before: f64 = (0..shared)
        .map(|i| f64::from((both.left[i] - press.left[i]).abs()))
        .fold(0.0, f64::max);
    assert!(
        before < 1e-6,
        "both differs from the press before the release: {before}"
    );

    // after it, the release has joined
    let after: f64 = (RELEASE_GAP_SAMPLES..press.left.len())
        .map(|i| f64::from((both.left[i] - press.left[i]).abs()))
        .fold(0.0, f64::max);
    assert!(after > 1e-4, "both has no release in it: {after}");
}

#[test]
fn a_part_with_no_layers_should_render_empty() {
    let mut subtle_release = request(Theme::Press, Cue::Tap, Part::Release);
    subtle_release.emphasis = Emphasis::Subtle;
    let stereo = render(&subtle_release, cues::SAMPLE_RATE);
    assert!(stereo.left.is_empty() && stereo.right.is_empty());
}

#[test]
fn a_release_should_render_empty_for_a_cue_without_one() {
    let stereo = render(
        &request(Theme::Default, Cue::Tap, Part::Release),
        cues::SAMPLE_RATE,
    );
    assert!(stereo.left.is_empty());
}

#[test]
fn render_should_scale_its_length_with_the_rate() {
    let at_48k = render(&request(Theme::Default, Cue::Ready, Part::Both), 48_000);
    let at_44k = render(&request(Theme::Default, Cue::Ready, Part::Both), 44_100);
    let expected = at_48k.left.len() as f64 * 44_100.0 / 48_000.0;
    assert!(
        (at_44k.left.len() as f64 - expected).abs() <= 2.0,
        "{} vs {expected}",
        at_44k.left.len()
    );
}

#[test]
fn vary_should_change_a_cue_that_varies() {
    let steady = render(
        &request(Theme::Default, Cue::Tap, Part::Both),
        cues::SAMPLE_RATE,
    );
    let varied = render(
        &Request {
            vary: true,
            ..request(Theme::Default, Cue::Tap, Part::Both)
        },
        cues::SAMPLE_RATE,
    );
    assert_ne!(steady.left, varied.left);
}

#[test]
fn vary_should_leave_a_cue_without_variation_alone() {
    let steady = render(
        &request(Theme::Default, Cue::Select, Part::Both),
        cues::SAMPLE_RATE,
    );
    let varied = render(
        &Request {
            vary: true,
            ..request(Theme::Default, Cue::Select, Part::Both)
        },
        cues::SAMPLE_RATE,
    );
    assert_eq!(steady.left, varied.left);
}

#[test]
fn only_press_tap_and_open_should_walk() {
    assert_eq!(walk_len(Theme::Press, Cue::Tap), 3);
    assert_eq!(walk_len(Theme::Press, Cue::Open), 3);
    assert_eq!(walk_len(Theme::Default, Cue::Tap), 0);
}

#[test]
fn only_press_inputs_should_have_releases() {
    assert!(has_release(Theme::Press, Cue::Type));
    assert!(!has_release(Theme::Press, Cue::Close));
    assert!(!has_release(Theme::Default, Cue::Tap));
}

#[test]
fn handoff_should_be_slow_for_cues_with_notes() {
    assert_eq!(handoff(Theme::Press, Cue::Tap), Duration::from_millis(400));
    assert_eq!(handoff(Theme::Default, Cue::Tap), Duration::from_millis(80));
}
