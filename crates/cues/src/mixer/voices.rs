//! The audio thread's side: live voices in preallocated slots, their fades,
//! and the soft clip on the summed output. Port of the voice fades in
//! cuelume's `render.ts` and `silence` and the clip in `output.ts`.
//!
//! Nothing here locks, allocates or frees once built. Voices arrive whole
//! over a bounded `std::sync::mpsc` channel and leave the same way to be
//! dropped by the worker. Both channels are array-backed: `try_recv` and
//! `try_send` only take a lock to wake a blocked peer, and neither side ever
//! blocks on them.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};

use crate::Stereo;

/// Gain a handed-off voice fades to before it ends: 80 dB down, as cuelume's exponential ramp.
const HANDOFF_FLOOR: f32 = 0.0001;
/// How long a silenced voice takes to fade, in seconds: heard as at once, long enough not to click.
const SILENCE_FADE: f64 = 0.015;
/// Where the soft clip starts bending the summed output.
const CLIP_KNEE: f32 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct VoiceId(pub(super) u64);

/// One play's samples, sounding.
#[derive(Debug)]
pub(super) struct Voice {
    id: VoiceId,
    stereo: Stereo,
    gain: f32,
    position: usize,
    /// Fade multiplier on top of `gain`, 1 until a fade starts.
    level: f32,
    fade: Fade,
}

#[derive(Clone, Copy, Debug)]
enum Fade {
    None,
    /// Multiplies `level` by `ratio` each sample.
    Exponential {
        ratio: f32,
        left: usize,
    },
    /// Takes `step` off `level` each sample.
    Linear {
        step: f32,
        left: usize,
    },
    /// Faded out; the voice is over.
    Done,
}

impl Voice {
    pub(super) fn new(id: VoiceId, stereo: Stereo, gain: f32) -> Self {
        Self {
            id,
            stereo,
            gain,
            position: 0,
            level: 1.0,
            fade: Fade::None,
        }
    }

    #[cfg(test)]
    pub(super) fn id(&self) -> VoiceId {
        self.id
    }

    #[cfg(test)]
    pub(super) fn gain(&self) -> f32 {
        self.gain
    }

    pub(super) fn len(&self) -> usize {
        self.stereo.left.len().min(self.stereo.right.len())
    }

    fn is_over(&self) -> bool {
        matches!(self.fade, Fade::Done) || self.position >= self.len()
    }

    /// From the current level to [`HANDOFF_FLOOR`] along an exponential curve, then over.
    fn fade_exponential(&mut self, samples: usize) {
        self.fade = if samples == 0 || self.level <= HANDOFF_FLOOR {
            Fade::Done
        } else {
            Fade::Exponential {
                ratio: (HANDOFF_FLOOR / self.level).powf(1.0 / samples as f32),
                left: samples,
            }
        };
    }

    /// From the current level to silence along a straight line, then over.
    fn fade_linear(&mut self, samples: usize) {
        self.fade = if samples == 0 {
            Fade::Done
        } else {
            Fade::Linear {
                step: self.level / samples as f32,
                left: samples,
            }
        };
    }

    fn next(&mut self) -> Option<(f32, f32)> {
        if self.is_over() {
            return None;
        }
        let gain = self.gain * self.level;
        let frame = (
            self.stereo.left[self.position] * gain,
            self.stereo.right[self.position] * gain,
        );
        self.position += 1;
        self.fade = match self.fade {
            Fade::Exponential { ratio, left } => {
                self.level *= ratio;
                countdown(left, |left| Fade::Exponential { ratio, left })
            }
            Fade::Linear { step, left } => {
                self.level = (self.level - step).max(0.0);
                countdown(left, |left| Fade::Linear { step, left })
            }
            fade => fade,
        };
        Some(frame)
    }
}

fn countdown(left: usize, fade: impl FnOnce(usize) -> Fade) -> Fade {
    match left - 1 {
        0 => Fade::Done,
        left => fade(left),
    }
}

/// A cue's last voice to fade as its next one starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Handoff {
    pub(super) id: VoiceId,
    pub(super) samples: usize,
}

/// A voice to start at the next buffer, and the voice it takes over from.
#[derive(Debug)]
pub(super) struct Start {
    pub(super) voice: Voice,
    pub(super) handoff: Option<Handoff>,
}

#[derive(Debug)]
pub(super) enum ToAudio {
    Start(Start),
    /// Fade everything sounding, for a mute heard at once.
    Silence,
}

/// The audio callback's state.
pub(super) struct Mixer {
    slots: Box<[Option<Voice>]>,
    /// A start that arrived with every slot taken; it waits for one to free.
    pending: Option<Start>,
    silence_samples: usize,
}

impl Mixer {
    pub(super) fn new(slots: usize, rate: u32) -> Self {
        Self {
            slots: (0..slots).map(|_| None).collect(),
            pending: None,
            silence_samples: (SILENCE_FADE * f64::from(rate)).round() as usize,
        }
    }

    /// One audio callback: take what the worker sent, fill `out` with
    /// interleaved frames of `channels`, and hand spent voices back.
    pub(super) fn process(
        &mut self,
        incoming: &Receiver<ToAudio>,
        out: &mut [f32],
        channels: usize,
        spent: &SyncSender<Voice>,
    ) {
        self.receive(incoming);
        self.mix(out, channels);
        self.hand_back(spent);
    }

    fn receive(&mut self, incoming: &Receiver<ToAudio>) {
        loop {
            if let Some(start) = self.pending.take()
                && let Err(start) = self.place(start)
            {
                // keep the rest queued behind it, in order
                self.pending = Some(start);
                return;
            }
            match incoming.try_recv() {
                Ok(ToAudio::Start(start)) => self.pending = Some(start),
                Ok(ToAudio::Silence) => self.silence(),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    fn place(&mut self, start: Start) -> Result<(), Start> {
        let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) else {
            return Err(start);
        };
        *slot = Some(start.voice);
        if let Some(handoff) = start.handoff
            && let Some(last) = self.live_mut().find(|voice| voice.id == handoff.id)
        {
            last.fade_exponential(handoff.samples);
        }
        Ok(())
    }

    fn silence(&mut self) {
        let samples = self.silence_samples;
        for voice in self.live_mut() {
            voice.fade_linear(samples);
        }
    }

    fn live_mut(&mut self) -> impl Iterator<Item = &mut Voice> {
        self.slots
            .iter_mut()
            .flatten()
            .filter(|voice| !voice.is_over())
    }

    fn mix(&mut self, out: &mut [f32], channels: usize) {
        for frame in out.chunks_mut(channels.max(1)) {
            let (mut left, mut right) = (0.0, 0.0);
            for voice in self.slots.iter_mut().flatten() {
                if let Some((l, r)) = voice.next() {
                    left += l;
                    right += r;
                }
            }
            let (left, right) = (soft_clip(left), soft_clip(right));
            match frame {
                [mono] => *mono = 0.5 * (left + right),
                [l, r, rest @ ..] => {
                    *l = left;
                    *r = right;
                    rest.fill(0.0);
                }
                [] => {}
            }
        }
    }

    fn hand_back(&mut self, spent: &SyncSender<Voice>) {
        for slot in self.slots.iter_mut() {
            if !slot.as_ref().is_some_and(Voice::is_over) {
                continue;
            }
            let Some(voice) = slot.take() else { continue };
            match spent.try_send(voice) {
                Ok(()) => {}
                // the worker is behind (or gone, and the stream with it): keep
                // it in its slot rather than free it here, and try next callback
                Err(TrySendError::Full(voice) | TrySendError::Disconnected(voice)) => {
                    *slot = Some(voice);
                    return;
                }
            }
        }
    }

    #[cfg(test)]
    fn occupied(&self) -> usize {
        self.slots.iter().flatten().count()
    }
}

/// cuelume's soft clip: straight below the knee, a parabolic shoulder above
/// it, flat at ±0.85 from full scale (the wave shaper's curve ends at ±1).
pub(super) fn soft_clip(level: f32) -> f32 {
    let over = level.abs().min(1.0) - CLIP_KNEE;
    if over <= 0.0 {
        return level;
    }
    let shoulder = 1.0 - CLIP_KNEE;
    level.signum() * (CLIP_KNEE + over - over * over / (2.0 * shoulder))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

    use super::*;

    const RATE: u32 = 1000;

    fn constant(id: u64, len: usize, value: f32, gain: f32) -> Voice {
        Voice::new(
            VoiceId(id),
            Stereo {
                left: vec![value; len],
                right: vec![value; len],
            },
            gain,
        )
    }

    fn start(voice: Voice, handoff: Option<Handoff>) -> ToAudio {
        ToAudio::Start(Start { voice, handoff })
    }

    struct Rig {
        mixer: Mixer,
        to_audio: SyncSender<ToAudio>,
        incoming: Receiver<ToAudio>,
        spent_tx: SyncSender<Voice>,
        spent: Receiver<Voice>,
    }

    impl Rig {
        fn new(slots: usize) -> Self {
            let (to_audio, incoming) = sync_channel(16);
            let (spent_tx, spent) = sync_channel(16);
            Self {
                mixer: Mixer::new(slots, RATE),
                to_audio,
                incoming,
                spent_tx,
                spent,
            }
        }

        fn send(&self, message: ToAudio) {
            self.to_audio.try_send(message).unwrap();
        }

        /// Runs `frames` stereo frames and returns the left channel.
        fn run(&mut self, frames: usize) -> Vec<f32> {
            let mut out = vec![0.0; frames * 2];
            self.mixer
                .process(&self.incoming, &mut out, 2, &self.spent_tx);
            out.iter().step_by(2).copied().collect()
        }

        fn spent_ids(&self) -> Vec<u64> {
            self.spent.try_iter().map(|voice| voice.id().0).collect()
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn soft_clip_passes_below_the_knee() {
        assert_eq!(soft_clip(0.69), 0.69);
    }

    #[test]
    fn soft_clip_bends_above_the_knee() {
        assert!(close(soft_clip(0.85), 0.8125));
    }

    #[test]
    fn soft_clip_tops_out_at_full_scale() {
        assert!(close(soft_clip(1.0), 0.85));
    }

    #[test]
    fn soft_clip_holds_past_full_scale() {
        assert!(close(soft_clip(4.0), 0.85));
    }

    #[test]
    fn soft_clip_is_symmetric() {
        assert_eq!(soft_clip(-0.9), -soft_clip(0.9));
    }

    #[test]
    fn voices_sum_with_their_gain() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 4, 0.2, 0.5), None));
        rig.send(start(constant(1, 4, 0.3, 1.0), None));
        assert!(close(rig.run(1)[0], 0.4));
    }

    #[test]
    fn summed_output_is_soft_clipped() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 4, 0.6, 1.0), None));
        rig.send(start(constant(1, 4, 0.6, 1.0), None));
        assert!(close(rig.run(1)[0], 0.85));
    }

    #[test]
    fn voice_plays_its_samples_then_silence() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 2, 0.5, 1.0), None));
        assert_eq!(rig.run(3), [0.5, 0.5, 0.0]);
    }

    #[test]
    fn mono_output_averages_the_channels() {
        let mut rig = Rig::new(4);
        let voice = Voice::new(
            VoiceId(0),
            Stereo {
                left: vec![0.4],
                right: vec![0.2],
            },
            1.0,
        );
        rig.send(start(voice, None));
        let mut out = [0.0];
        rig.mixer.process(&rig.incoming, &mut out, 1, &rig.spent_tx);
        assert!(close(out[0], 0.3));
    }

    #[test]
    fn extra_channels_are_silent() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 1, 0.5, 1.0), None));
        let mut out = [9.0; 4];
        rig.mixer.process(&rig.incoming, &mut out, 4, &rig.spent_tx);
        assert_eq!(out, [0.5, 0.5, 0.0, 0.0]);
    }

    #[test]
    fn finished_voices_are_handed_back() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(7, 2, 0.5, 1.0), None));
        rig.run(2);
        assert_eq!(rig.spent_ids(), [7]);
    }

    #[test]
    fn handoff_fades_exponentially_to_the_floor() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 1000, 0.5, 1.0), None));
        rig.run(1);
        let handoff = Handoff {
            id: VoiceId(0),
            samples: 100,
        };
        rig.send(start(constant(1, 1000, 0.0, 1.0), Some(handoff)));
        let out = rig.run(51);
        // halfway down an exponential ramp from 1 to 1e-4 is 1e-2
        assert!(close(out[50], 0.5 * 0.01), "{}", out[50]);
    }

    #[test]
    fn handed_off_voice_ends_when_its_fade_does() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 1000, 0.5, 1.0), None));
        let handoff = Handoff {
            id: VoiceId(0),
            samples: 80,
        };
        rig.send(start(constant(1, 1000, 0.0, 1.0), Some(handoff)));
        rig.run(80);
        assert_eq!(rig.spent_ids(), [0]);
    }

    #[test]
    fn handoff_leaves_other_voices_alone() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 1000, 0.5, 1.0), None));
        let handoff = Handoff {
            id: VoiceId(9),
            samples: 10,
        };
        rig.send(start(constant(1, 1000, 0.0, 1.0), Some(handoff)));
        assert_eq!(rig.run(20)[19], 0.5);
    }

    #[test]
    fn silence_fades_linearly_over_15_ms() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 1000, 0.5, 1.0), None));
        rig.run(1);
        rig.send(ToAudio::Silence);
        let out = rig.run(16);
        assert_eq!(
            (close(out[5], 0.5 * 10.0 / 15.0), out[15]),
            (true, 0.0),
            "{out:?}"
        );
    }

    #[test]
    fn silence_ends_every_voice() {
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 1000, 0.5, 1.0), None));
        rig.send(start(constant(1, 1000, 0.5, 1.0), None));
        rig.send(ToAudio::Silence);
        rig.run(15);
        assert_eq!(rig.mixer.occupied(), 0);
    }

    #[test]
    fn silence_spares_voices_that_start_after_it() {
        let mut rig = Rig::new(4);
        rig.send(ToAudio::Silence);
        rig.send(start(constant(0, 100, 0.5, 1.0), None));
        assert_eq!(rig.run(20)[19], 0.5);
    }

    #[test]
    fn start_waits_for_a_free_slot() {
        let mut rig = Rig::new(1);
        rig.send(start(constant(0, 2, 0.1, 1.0), None));
        rig.send(start(constant(1, 2, 0.3, 1.0), None));
        rig.run(2);
        assert_eq!(rig.run(1), [0.3]);
    }

    #[test]
    fn spent_voice_waits_when_the_worker_is_behind() {
        let (spent_tx, spent) = sync_channel(1);
        let mut rig = Rig::new(4);
        rig.send(start(constant(0, 1, 0.5, 1.0), None));
        rig.send(start(constant(1, 1, 0.5, 1.0), None));
        let mut out = [0.0; 2];
        rig.mixer.process(&rig.incoming, &mut out, 2, &spent_tx);
        let first = spent.try_iter().count();
        rig.mixer.process(&rig.incoming, &mut out, 2, &spent_tx);
        assert_eq!((first, spent.try_iter().count()), (1, 1));
    }
}
