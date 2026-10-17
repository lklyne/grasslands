//! The cpal output stream, owned by the worker: opened on the first play,
//! paused once nothing has sounded for a while, rebuilt after it fails.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat, SupportedStreamConfig};

use super::voices::{Mixer, Start, ToAudio, Voice};

/// Voices that can sound at once.
const SLOTS: usize = 64;
/// Messages that can wait for the next audio callback.
const QUEUE: usize = 64;
/// How long the stream runs on with nothing sounding before it pauses.
const IDLE_PAUSE: Duration = Duration::from_secs(2);
/// How long to wait before looking for a device again after none was found.
const RETRY: Duration = Duration::from_secs(5);

pub(super) struct Output {
    open: Option<Open>,
    retry_at: Option<Instant>,
    /// Whether the current outage has been logged.
    warned: bool,
}

struct Open {
    stream: cpal::Stream,
    rate: u32,
    to_audio: SyncSender<ToAudio>,
    spent: Receiver<Voice>,
    failed: Arc<AtomicBool>,
    playing: bool,
    /// When the last voice sent will have finished.
    busy_until: Instant,
}

impl Output {
    pub(super) fn new() -> Self {
        Self {
            open: None,
            retry_at: None,
            warned: false,
        }
    }

    /// The rate to render at, opening the stream if it isn't. `None` while there's no output.
    pub(super) fn rate(&mut self, now: Instant) -> Option<u32> {
        if self.open.is_none() && self.retry_at.is_none_or(|at| now >= at) {
            match open(now) {
                Ok(open) => {
                    log::info!("cues: output open at {} Hz", open.rate);
                    self.open = Some(open);
                    self.retry_at = None;
                    self.warned = false;
                }
                Err(error) => {
                    if !self.warned {
                        log::warn!("cues: no audio output, staying silent: {error}");
                        self.warned = true;
                    }
                    self.retry_at = Some(now + RETRY);
                }
            }
        }
        self.open.as_ref().map(|open| open.rate)
    }

    pub(super) fn start(&mut self, start: Start, now: Instant) {
        let Some(open) = &mut self.open else { return };
        let length = Duration::from_secs_f64(start.voice.len() as f64 / f64::from(open.rate));
        if !open.playing {
            if let Err(error) = open.stream.play() {
                log::warn!("cues: output failed to start, will reopen: {error}");
                self.open = None;
                return;
            }
            open.playing = true;
        }
        match open.to_audio.try_send(ToAudio::Start(start)) {
            Ok(()) => open.busy_until = open.busy_until.max(now + length),
            Err(TrySendError::Full(_)) => log::debug!("cues: output queue full, dropped a play"),
            Err(TrySendError::Disconnected(_)) => self.open = None,
        }
    }

    /// Fades whatever is sounding.
    pub(super) fn silence(&mut self) {
        if let Some(open) = &self.open
            && open.playing
            && let Err(TrySendError::Full(_)) = open.to_audio.try_send(ToAudio::Silence)
        {
            log::debug!("cues: output queue full, dropped a silence");
        }
    }

    /// Drops spent voices, notices a failed stream, and pauses an idle one.
    pub(super) fn maintain(&mut self, now: Instant) {
        let Some(open) = &mut self.open else { return };
        open.spent.try_iter().for_each(drop);
        if open.failed.load(Ordering::Relaxed) {
            log::info!("cues: output failed, will reopen on the next play");
            self.open = None;
            return;
        }
        if !open.playing || now < open.busy_until + IDLE_PAUSE {
            return;
        }
        match open.stream.pause() {
            Ok(()) => open.playing = false,
            // some backends can't pause; the callback writes silence instead
            Err(error) if error.kind() == ErrorKind::UnsupportedOperation => open.playing = false,
            Err(error) => {
                log::warn!("cues: output failed to pause, will reopen: {error}");
                self.open = None;
            }
        }
    }
}

fn open(now: Instant) -> Result<Open, cpal::Error> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| cpal::Error::new(ErrorKind::DeviceNotAvailable))?;
    let config = choose_config(&device)?;
    let rate = config.sample_rate();
    let channels = usize::from(config.channels());
    let (to_audio, incoming) = sync_channel(QUEUE);
    // room for every voice the audio side can hold, so handing one back never waits long
    let (spent_tx, spent) = sync_channel(SLOTS + QUEUE);
    let failed = Arc::new(AtomicBool::new(false));
    let mut mixer = Mixer::new(SLOTS, rate);
    let flag = Arc::clone(&failed);
    let stream = device.build_output_stream::<f32, _, _>(
        config.config(),
        move |data, _| mixer.process(&incoming, data, channels, &spent_tx),
        move |error| {
            if needs_rebuild(error.kind()) {
                log::warn!("cues: output stream error: {error}");
                flag.store(true, Ordering::Relaxed);
            }
        },
        None,
    )?;
    Ok(Open {
        stream,
        rate,
        to_audio,
        spent,
        failed,
        playing: false,
        busy_until: now,
    })
}

/// 32-bit float at 48 kHz, stereo if the device has it; else its own default if that's float.
fn choose_config(device: &cpal::Device) -> Result<SupportedStreamConfig, cpal::Error> {
    let floats: Vec<_> = device
        .supported_output_configs()?
        .filter(|range| {
            range.sample_format() == SampleFormat::F32 && range.contains_rate(crate::SAMPLE_RATE)
        })
        .collect();
    let preferred = floats
        .iter()
        .find(|range| range.channels() >= 2)
        .or_else(|| floats.first());
    if let Some(range) = preferred {
        return Ok(range.with_sample_rate(crate::SAMPLE_RATE));
    }
    let fallback = device.default_output_config()?;
    if fallback.sample_format() == SampleFormat::F32 {
        Ok(fallback)
    } else {
        Err(cpal::Error::with_message(
            ErrorKind::UnsupportedConfig,
            "the output device has no 32-bit float format",
        ))
    }
}

/// Whether the stream is dead. A rerouted stream, an xrun or a refused
/// real-time promotion all leave it playing.
fn needs_rebuild(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::DeviceChanged | ErrorKind::Xrun | ErrorKind::RealtimeDenied
    )
}
