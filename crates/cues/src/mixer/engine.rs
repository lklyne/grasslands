//! What to play: cadence, the walk, held presses and handing a cue's last
//! play off to its next. Port of cuelume's `engine.ts` and the voice
//! bookkeeping in `render.ts`. No device and no clock of its own, so tests
//! drive it with their own instants and a fake [`Source`].

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use super::PlayOptions;
use super::voices::{Handoff, Start, Voice, VoiceId};
use crate::{Cue, Part, Request, Stereo, Theme};

/// Repeats closer than this walk on to the next step; a longer pause starts the walk again.
pub(super) const WALK_GAP: Duration = Duration::from_millis(1000);

/// Where the samples and recipe facts come from: [`Synth`] in the app, a fake in tests.
pub(super) trait Source {
    fn render(&mut self, request: &Request, rate: u32) -> Stereo;
    fn walk_len(&self, theme: Theme, cue: Cue) -> usize;
    fn has_release(&self, theme: Theme, cue: Cue) -> bool;
    fn handoff(&self, theme: Theme, cue: Cue) -> Duration;
}

/// The synthesis in [`crate::render`].
pub(super) struct Synth;

impl Source for Synth {
    fn render(&mut self, request: &Request, rate: u32) -> Stereo {
        crate::render(request, rate)
    }

    fn walk_len(&self, theme: Theme, cue: Cue) -> usize {
        crate::walk_len(theme, cue)
    }

    fn has_release(&self, theme: Theme, cue: Cue) -> bool {
        crate::has_release(theme, cue)
    }

    fn handoff(&self, theme: Theme, cue: Cue) -> Duration {
        crate::handoff(theme, cue)
    }
}

/// Everything one press decided, so its release sounds like the same press.
#[derive(Clone, Copy, Debug)]
pub(super) struct Played {
    request: Request,
    volume: f32,
}

/// Presses waiting for their release: one per cue, or one per gesture when the caller names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Holding {
    cue: Cue,
    gesture: Option<Box<str>>,
}

/// A cue's latest voice and how long it takes to let go.
#[derive(Clone, Copy, Debug)]
struct Latest {
    id: VoiceId,
    fade: Duration,
}

pub(super) struct Engine<S> {
    source: S,
    enabled: bool,
    volume: f32,
    theme: Theme,
    last_played: HashMap<Cue, Instant>,
    steps: HashMap<Cue, usize>,
    held: HashMap<Holding, Played>,
    latest: HashMap<Cue, Latest>,
    next_id: u64,
    seed: u32,
    /// Cues whose render panicked in a theme, so each is logged once.
    panicked: HashSet<(Cue, Theme)>,
}

impl<S: Source> Engine<S> {
    pub(super) fn new(source: S) -> Self {
        Self {
            source,
            enabled: true,
            volume: 1.0,
            theme: Theme::default(),
            last_played: HashMap::new(),
            steps: HashMap::new(),
            held: HashMap::new(),
            latest: HashMap::new(),
            next_id: 0,
            // truncation is fine: any 32 bits of a random hash make a seed
            seed: RandomState::new().hash_one(0u8) as u32,
            panicked: HashSet::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn source(&self) -> &S {
        &self.source
    }

    /// Turns sound on or off. Returns whether what's ringing should be silenced:
    /// off is heard at once, and a press waiting for its release is forgotten.
    pub(super) fn set_enabled(&mut self, enabled: bool) -> bool {
        self.enabled = enabled;
        if enabled {
            return false;
        }
        self.held.clear();
        self.latest.clear();
        true
    }

    pub(super) fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub(super) fn set_volume(&mut self, volume: f32) {
        self.volume = normalize_volume(volume, self.volume);
    }

    /// Decides one play of `cue` made at `at`, or `None` when it plays nothing:
    /// sound is off, the volume is 0, or a release has no press waiting.
    pub(super) fn decide(
        &mut self,
        cue: Cue,
        options: &PlayOptions,
        part: Part,
        gesture: Option<Box<str>>,
        at: Instant,
    ) -> Option<Played> {
        if !self.enabled {
            return None;
        }
        let holding = Holding { cue, gesture };
        if part == Part::Release {
            return self.held.remove(&holding);
        }
        // the press before this one is over, even if this play turns out silent
        self.held.remove(&holding);
        let volume = self.volume * options.volume.map_or(1.0, |v| normalize_volume(v, 1.0));
        if volume == 0.0 {
            return None;
        }
        let theme = options.theme.unwrap_or(self.theme);
        let since_last = self.since_last_play(cue, at);
        let step = self.step_for(theme, cue, since_last);
        let played = Played {
            request: Request {
                theme,
                cue,
                emphasis: options.emphasis,
                context: options.context,
                since_last,
                step,
                part,
                vary: true,
                seed: self.next_seed(),
            },
            volume,
        };
        if part == Part::Press {
            self.held.insert(holding, played);
        }
        Some(played)
    }

    /// Renders `part` of a decided play at `rate` and hands the cue's last play
    /// off to it. `None` when the part has no layers to sound.
    pub(super) fn start(&mut self, played: Played, part: Part, rate: u32) -> Option<Start> {
        let Played {
            mut request,
            volume,
        } = played;
        let (theme, cue) = (request.theme, request.cue);
        if part == Part::Release && !self.source.has_release(theme, cue) {
            return None;
        }
        request.part = part;
        // a bug in one recipe drops that play rather than every sound after it
        let source = &mut self.source;
        let stereo = match panic::catch_unwind(AssertUnwindSafe(|| source.render(&request, rate))) {
            Ok(stereo) => stereo,
            Err(_) => {
                if self.panicked.insert((cue, theme)) {
                    log::error!(
                        "cues: rendering {cue:?} in {theme:?} panicked; dropping its plays"
                    );
                }
                return None;
            }
        };
        if stereo.left.is_empty() {
            return None;
        }
        let id = VoiceId(self.next_id);
        self.next_id += 1;
        // a release rings with its press rather than replacing it
        let handoff = if part == Part::Release {
            None
        } else {
            let fade = self.source.handoff(theme, cue);
            self.latest
                .insert(cue, Latest { id, fade })
                .map(|last| Handoff {
                    id: last.id,
                    samples: samples(last.fade, rate),
                })
        };
        Some(Start {
            voice: Voice::new(id, stereo, volume),
            handoff,
        })
    }

    fn since_last_play(&mut self, cue: Cue, at: Instant) -> Option<Duration> {
        self.last_played
            .insert(cue, at)
            .map(|last| at.saturating_duration_since(last))
    }

    /// The next step of the cue's walk: on while repeats come quickly, back to the first after a pause.
    fn step_for(&mut self, theme: Theme, cue: Cue, since_last: Option<Duration>) -> usize {
        let len = self.source.walk_len(theme, cue);
        let quick = since_last.is_some_and(|since| since < WALK_GAP);
        let step = match self.steps.get(&cue) {
            Some(last) if len > 0 && quick => (last + 1) % len,
            _ => 0,
        };
        self.steps.insert(cue, step);
        step
    }

    /// A splitmix-style step: plays need distinct seeds, not good randomness.
    fn next_seed(&mut self) -> u32 {
        self.seed = self.seed.wrapping_add(0x9e37_79b9);
        let mut z = self.seed;
        z = (z ^ (z >> 16)).wrapping_mul(0x85eb_ca6b);
        z = (z ^ (z >> 13)).wrapping_mul(0xc2b2_ae35);
        z ^ (z >> 16)
    }
}

fn normalize_volume(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

pub(super) fn samples(duration: Duration, rate: u32) -> usize {
    (duration.as_secs_f64() * f64::from(rate)).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Emphasis};

    const RATE: u32 = 1000;

    /// Renders every play as `len` samples of 1.0 and records what it was asked for.
    #[derive(Default)]
    struct Fake {
        requests: Vec<Request>,
        walks: HashMap<Cue, usize>,
        no_release: Vec<Cue>,
        silent: Vec<Cue>,
        panics: Vec<Cue>,
    }

    impl Source for Fake {
        fn render(&mut self, request: &Request, _rate: u32) -> Stereo {
            self.requests.push(*request);
            assert!(!self.panics.contains(&request.cue), "fake render panic");
            let len = if self.silent.contains(&request.cue) {
                0
            } else {
                10
            };
            Stereo {
                left: vec![1.0; len],
                right: vec![1.0; len],
            }
        }

        fn walk_len(&self, _theme: Theme, cue: Cue) -> usize {
            self.walks.get(&cue).copied().unwrap_or(0)
        }

        fn has_release(&self, _theme: Theme, cue: Cue) -> bool {
            !self.no_release.contains(&cue)
        }

        fn handoff(&self, theme: Theme, _cue: Cue) -> Duration {
            match theme {
                Theme::Bubble => Duration::from_millis(400),
                _ => Duration::from_millis(80),
            }
        }
    }

    struct Rig {
        engine: Engine<Fake>,
        t0: Instant,
    }

    impl Rig {
        fn new() -> Self {
            Self::with(Fake::default())
        }

        fn with(fake: Fake) -> Self {
            Self {
                engine: Engine::new(fake),
                t0: Instant::now(),
            }
        }

        fn run(
            &mut self,
            cue: Cue,
            options: PlayOptions,
            part: Part,
            gesture: Option<&str>,
            ms: u64,
        ) -> Option<Start> {
            let at = self.t0 + Duration::from_millis(ms);
            let played = self
                .engine
                .decide(cue, &options, part, gesture.map(Into::into), at)?;
            self.engine.start(played, part, RATE)
        }

        fn play(&mut self, cue: Cue, ms: u64) -> Option<Start> {
            self.run(cue, PlayOptions::default(), Part::Both, None, ms)
        }

        fn press(&mut self, cue: Cue, gesture: Option<&str>, ms: u64) -> Option<Start> {
            self.run(cue, PlayOptions::default(), Part::Press, gesture, ms)
        }

        fn release(&mut self, cue: Cue, gesture: Option<&str>, ms: u64) -> Option<Start> {
            self.run(cue, PlayOptions::default(), Part::Release, gesture, ms)
        }

        fn requests(&self) -> &[Request] {
            &self.engine.source().requests
        }

        fn last(&self) -> Request {
            *self.requests().last().unwrap()
        }
    }

    fn walking(cue: Cue, len: usize) -> Fake {
        Fake {
            walks: HashMap::from([(cue, len)]),
            ..Fake::default()
        }
    }

    #[test]
    fn play_renders_both_parts_in_one_voice() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0).unwrap();
        assert_eq!(rig.last().part, Part::Both);
    }

    #[test]
    fn first_play_has_no_cadence() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0);
        assert_eq!(rig.last().since_last, None);
    }

    #[test]
    fn repeat_reports_time_since_last_play() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0);
        rig.play(Cue::Tap, 120);
        assert_eq!(rig.last().since_last, Some(Duration::from_millis(120)));
    }

    #[test]
    fn cadence_is_per_cue() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0);
        rig.play(Cue::Type, 50);
        assert_eq!(rig.last().since_last, None);
    }

    #[test]
    fn quick_repeats_walk_on_and_wrap() {
        let mut rig = Rig::with(walking(Cue::Type, 3));
        for ms in [0, 100, 200, 300, 400] {
            rig.play(Cue::Type, ms);
        }
        let steps: Vec<_> = rig.requests().iter().map(|r| r.step).collect();
        assert_eq!(steps, [0, 1, 2, 0, 1]);
    }

    #[test]
    fn pause_restarts_the_walk() {
        let mut rig = Rig::with(walking(Cue::Type, 3));
        rig.play(Cue::Type, 0);
        rig.play(Cue::Type, 100);
        rig.play(Cue::Type, 100 + WALK_GAP.as_millis() as u64);
        assert_eq!(rig.last().step, 0);
    }

    #[test]
    fn cue_without_walk_stays_on_first_step() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0);
        rig.play(Cue::Tap, 10);
        assert_eq!(rig.last().step, 0);
    }

    #[test]
    fn release_sounds_like_its_press() {
        let mut rig = Rig::with(walking(Cue::Tap, 4));
        rig.play(Cue::Tap, 0);
        let options = PlayOptions {
            emphasis: Emphasis::Strong,
            ..PlayOptions::default()
        };
        rig.run(Cue::Tap, options, Part::Press, None, 50).unwrap();
        let press = rig.last();
        rig.release(Cue::Tap, None, 140).unwrap();
        let release = rig.last();
        assert_eq!(
            (
                release.part,
                release.seed,
                release.step,
                release.emphasis,
                release.since_last
            ),
            (
                Part::Release,
                press.seed,
                press.step,
                Emphasis::Strong,
                press.since_last
            )
        );
    }

    #[test]
    fn release_without_press_plays_nothing() {
        let mut rig = Rig::new();
        assert!(rig.release(Cue::Tap, None, 0).is_none());
    }

    #[test]
    fn release_lets_go_only_once() {
        let mut rig = Rig::new();
        rig.press(Cue::Tap, None, 0);
        rig.release(Cue::Tap, None, 90);
        assert!(rig.release(Cue::Tap, None, 100).is_none());
    }

    #[test]
    fn release_without_release_layers_renders_nothing() {
        let mut rig = Rig::with(Fake {
            no_release: vec![Cue::Tap],
            ..Fake::default()
        });
        rig.press(Cue::Tap, None, 0);
        rig.release(Cue::Tap, None, 90);
        assert_eq!(rig.requests().len(), 1);
    }

    #[test]
    fn gestures_hold_their_own_presses() {
        let mut rig = Rig::new();
        rig.press(Cue::Type, Some("KeyA"), 0);
        rig.press(Cue::Type, Some("KeyB"), 10);
        let a = rig.release(Cue::Type, Some("KeyA"), 20);
        let b = rig.release(Cue::Type, Some("KeyB"), 30);
        assert!(a.is_some() && b.is_some());
    }

    #[test]
    fn release_without_gesture_ignores_gesture_presses() {
        let mut rig = Rig::new();
        rig.press(Cue::Type, Some("KeyA"), 0);
        assert!(rig.release(Cue::Type, None, 10).is_none());
    }

    #[test]
    fn new_press_replaces_the_held_one() {
        let mut rig = Rig::new();
        rig.press(Cue::Tap, None, 0);
        rig.press(Cue::Tap, None, 10);
        let second = rig.last().seed;
        rig.release(Cue::Tap, None, 20);
        assert_eq!(rig.last().seed, second);
    }

    #[test]
    fn play_forgets_the_held_press() {
        let mut rig = Rig::new();
        rig.press(Cue::Tap, None, 0);
        rig.play(Cue::Tap, 10);
        assert!(rig.release(Cue::Tap, None, 20).is_none());
    }

    #[test]
    fn silent_play_still_forgets_the_held_press() {
        let mut rig = Rig::new();
        rig.press(Cue::Tap, None, 0);
        let mute = PlayOptions {
            volume: Some(0.0),
            ..PlayOptions::default()
        };
        rig.run(Cue::Tap, mute, Part::Press, None, 10);
        assert!(rig.release(Cue::Tap, None, 20).is_none());
    }

    #[test]
    fn zero_volume_plays_nothing_and_keeps_cadence() {
        let mut rig = Rig::new();
        rig.engine.set_volume(0.0);
        assert!(rig.play(Cue::Tap, 0).is_none());
        rig.engine.set_volume(1.0);
        rig.play(Cue::Tap, 10);
        assert_eq!(rig.last().since_last, None);
    }

    #[test]
    fn per_play_volume_scales_global_volume() {
        let mut rig = Rig::new();
        rig.engine.set_volume(0.5);
        let options = PlayOptions {
            volume: Some(0.5),
            ..PlayOptions::default()
        };
        let start = rig.run(Cue::Tap, options, Part::Both, None, 0).unwrap();
        assert_eq!(start.voice.gain(), 0.25);
    }

    #[test]
    fn volume_is_clamped() {
        let mut rig = Rig::new();
        let options = PlayOptions {
            volume: Some(3.0),
            ..PlayOptions::default()
        };
        let start = rig.run(Cue::Tap, options, Part::Both, None, 0).unwrap();
        assert_eq!(start.voice.gain(), 1.0);
    }

    #[test]
    fn non_finite_volume_keeps_the_last() {
        let mut rig = Rig::new();
        rig.engine.set_volume(0.5);
        rig.engine.set_volume(f32::NAN);
        assert_eq!(rig.play(Cue::Tap, 0).unwrap().voice.gain(), 0.5);
    }

    #[test]
    fn disabled_plays_nothing() {
        let mut rig = Rig::new();
        rig.engine.set_enabled(false);
        assert!(rig.play(Cue::Tap, 0).is_none());
    }

    #[test]
    fn disabling_asks_for_silence() {
        let mut rig = Rig::new();
        assert!(rig.engine.set_enabled(false));
    }

    #[test]
    fn disabling_forgets_held_presses() {
        let mut rig = Rig::new();
        rig.press(Cue::Tap, None, 0);
        rig.engine.set_enabled(false);
        rig.engine.set_enabled(true);
        assert!(rig.release(Cue::Tap, None, 10).is_none());
    }

    #[test]
    fn disabling_forgets_the_voices_to_hand_off() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0);
        rig.engine.set_enabled(false);
        rig.engine.set_enabled(true);
        assert!(rig.play(Cue::Tap, 10).unwrap().handoff.is_none());
    }

    #[test]
    fn replay_fades_the_last_voice_over_its_handoff() {
        let mut rig = Rig::new();
        let first = rig.play(Cue::Tap, 0).unwrap();
        let second = rig.play(Cue::Tap, 10).unwrap();
        assert_eq!(
            second.handoff,
            Some(Handoff {
                id: first.voice.id(),
                samples: samples(Duration::from_millis(80), RATE),
            })
        );
    }

    #[test]
    fn handoff_uses_the_theme_the_last_voice_played() {
        let mut rig = Rig::new();
        let bubble = PlayOptions {
            theme: Some(Theme::Bubble),
            ..PlayOptions::default()
        };
        rig.run(Cue::Tap, bubble, Part::Both, None, 0);
        let second = rig.play(Cue::Tap, 10).unwrap();
        assert_eq!(
            second.handoff.unwrap().samples,
            samples(Duration::from_millis(400), RATE)
        );
    }

    #[test]
    fn other_cues_ring_on() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0);
        assert!(rig.play(Cue::Type, 10).unwrap().handoff.is_none());
    }

    #[test]
    fn release_rings_alongside_its_press() {
        let mut rig = Rig::new();
        rig.press(Cue::Tap, None, 0);
        assert!(rig.release(Cue::Tap, None, 90).unwrap().handoff.is_none());
    }

    #[test]
    fn release_does_not_become_the_voice_to_hand_off() {
        let mut rig = Rig::new();
        let press = rig.press(Cue::Tap, None, 0).unwrap();
        rig.release(Cue::Tap, None, 90);
        let next = rig.press(Cue::Tap, None, 200).unwrap();
        assert_eq!(next.handoff.unwrap().id, press.voice.id());
    }

    #[test]
    fn silent_render_hands_nothing_off() {
        let mut rig = Rig::with(Fake {
            silent: vec![Cue::Type],
            ..Fake::default()
        });
        rig.play(Cue::Tap, 0);
        assert!(rig.play(Cue::Type, 10).is_none());
    }

    fn panicking(cue: Cue) -> Rig {
        Rig::with(Fake {
            panics: vec![cue],
            ..Fake::default()
        })
    }

    #[test]
    fn panicking_render_drops_the_play() {
        let mut rig = panicking(Cue::Error);
        assert!(rig.play(Cue::Error, 0).is_none());
    }

    #[test]
    fn panicking_render_leaves_other_cues_playing() {
        let mut rig = panicking(Cue::Error);
        rig.play(Cue::Error, 0);
        assert!(rig.play(Cue::Tap, 10).is_some());
    }

    #[test]
    fn panicking_render_is_noted_once_per_cue_and_theme() {
        let mut rig = panicking(Cue::Error);
        rig.play(Cue::Error, 0);
        rig.play(Cue::Error, 10);
        let mech = PlayOptions::default().theme(Theme::Mech);
        rig.run(Cue::Error, mech, Part::Both, None, 20);
        assert_eq!(rig.engine.panicked.len(), 2);
    }

    #[test]
    fn panicking_render_hands_nothing_off() {
        let mut rig = panicking(Cue::Error);
        let first = rig.play(Cue::Error, 0);
        rig.engine.source.panics.clear();
        let next = rig.play(Cue::Error, 10).unwrap();
        assert_eq!((first.is_none(), next.handoff), (true, None));
    }

    #[test]
    fn per_play_theme_leaves_active_theme() {
        let mut rig = Rig::new();
        rig.engine.set_theme(Theme::Mech);
        let press = PlayOptions {
            theme: Some(Theme::Press),
            ..PlayOptions::default()
        };
        rig.run(Cue::Tap, press, Part::Both, None, 0);
        rig.play(Cue::Tap, 10);
        assert_eq!(rig.last().theme, Theme::Mech);
    }

    #[test]
    fn options_reach_the_request() {
        let mut rig = Rig::new();
        let context = Context {
            input: Some(crate::Input::Keyboard),
            ..Context::default()
        };
        let options = PlayOptions {
            context,
            emphasis: Emphasis::Subtle,
            ..PlayOptions::default()
        };
        rig.run(Cue::Tap, options, Part::Both, None, 0);
        let request = rig.last();
        assert_eq!(
            (request.context, request.emphasis, request.vary),
            (context, Emphasis::Subtle, true)
        );
    }

    #[test]
    fn decisions_advance_without_a_device() {
        let mut rig = Rig::new();
        let at = rig.t0;
        rig.engine
            .decide(Cue::Tap, &PlayOptions::default(), Part::Both, None, at);
        rig.play(Cue::Tap, 30);
        assert_eq!(rig.last().since_last, Some(Duration::from_millis(30)));
    }

    #[test]
    fn plays_get_distinct_seeds() {
        let mut rig = Rig::new();
        rig.play(Cue::Tap, 0);
        rig.play(Cue::Tap, 10);
        assert_ne!(rig.requests()[0].seed, rig.requests()[1].seed);
    }
}
