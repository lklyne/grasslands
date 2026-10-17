//! The output and playback decisions. [`Cues`] only queues commands; a
//! worker thread decides what plays ([`engine`]), renders it, and hands the
//! samples to the audio callback ([`voices`]) over the stream in [`output`].

mod engine;
mod output;
mod voices;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::{Context, Cue, Direction, Emphasis, Input, KeyRole, Part, Theme};
use engine::{Engine, Synth};
use output::Output;

/// How often the worker tidies up the output while no command arrives.
const TICK: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("couldn't start the sound worker: {0}")]
    Spawn(#[from] std::io::Error),
}

/// Per-play options, like cuelume's `PlayOptions`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayOptions {
    pub emphasis: Emphasis,
    pub context: Context,
    /// Multiplier for this play only, clamped to 0–1; `None` is 1.
    pub volume: Option<f32>,
    /// Material for this play only; `None` plays the active theme.
    pub theme: Option<Theme>,
}

impl PlayOptions {
    pub fn emphasis(mut self, emphasis: Emphasis) -> Self {
        self.emphasis = emphasis;
        self
    }

    pub fn volume(mut self, volume: f32) -> Self {
        self.volume = Some(volume);
        self
    }

    pub fn theme(mut self, theme: Theme) -> Self {
        self.theme = Some(theme);
        self
    }

    pub fn direction(mut self, direction: Direction) -> Self {
        self.context.direction = Some(direction);
        self
    }

    pub fn key(mut self, key: KeyRole) -> Self {
        self.context.key = Some(key);
        self
    }

    pub fn input(mut self, input: Input) -> Self {
        self.context.input = Some(input);
        self
    }

    pub fn duration(mut self, duration: Duration) -> Self {
        self.context.duration = Some(duration);
        self
    }

    /// Multiplies the cue's pitch, within an octave either way.
    pub fn pitch(mut self, pitch: f64) -> Self {
        self.context.pitch = Some(pitch);
        self
    }
}

enum Command {
    Play {
        cue: Cue,
        options: PlayOptions,
        part: Part,
        gesture: Option<Box<str>>,
        at: Instant,
    },
    Enable(bool),
    Theme(Theme),
    Volume(f32),
}

/// The app's sound output. Cheap to clone and safe to call from any thread.
/// Every call returns at once; a worker thread renders and plays.
#[derive(Clone)]
pub struct Cues {
    commands: mpsc::Sender<Command>,
}

impl Cues {
    /// Starts the worker. Opens no device: the output starts on the first play.
    pub fn new() -> Result<Self, Error> {
        let (commands, incoming) = mpsc::channel();
        thread::Builder::new()
            .name("cues".into())
            .spawn(move || run(&incoming))?;
        Ok(Self { commands })
    }

    /// Plays the press and, `RELEASE_GAP` later, the release.
    pub fn play(&self, cue: Cue, options: PlayOptions) {
        self.queue_play(cue, options, Part::Both, None);
    }

    /// Plays the press of a gesture that lets go later; [`Cues::release`] lets go.
    /// A cue holds one press at a time.
    pub fn press(&self, cue: Cue, options: PlayOptions) {
        self.queue_play(cue, options, Part::Press, None);
    }

    /// Lets go of the cue's last [`Cues::press`]. Plays nothing if it already let go.
    pub fn release(&self, cue: Cue) {
        self.queue_play(cue, PlayOptions::default(), Part::Release, None);
    }

    /// [`Cues::press`] held under `gesture`, such as a key's code, so presses
    /// that overlap each let go of their own.
    pub fn press_gesture(&self, cue: Cue, gesture: &str, options: PlayOptions) {
        self.queue_play(cue, options, Part::Press, Some(gesture.into()));
    }

    /// Lets go of the press [`Cues::press_gesture`] held under `gesture`.
    pub fn release_gesture(&self, cue: Cue, gesture: &str) {
        self.queue_play(
            cue,
            PlayOptions::default(),
            Part::Release,
            Some(gesture.into()),
        );
    }

    /// Turns sound on or off. Off is heard at once: what's ringing fades in
    /// 15 ms and presses waiting for their release are forgotten.
    pub fn set_enabled(&self, enabled: bool) {
        self.send(Command::Enable(enabled));
    }

    /// Switches the material of future plays; what's ringing finishes as it started.
    pub fn set_theme(&self, theme: Theme) {
        self.send(Command::Theme(theme));
    }

    /// Sets the volume of future plays, clamped to 0–1. Non-finite values are ignored.
    pub fn set_volume(&self, volume: f32) {
        self.send(Command::Volume(volume));
    }

    fn queue_play(&self, cue: Cue, options: PlayOptions, part: Part, gesture: Option<Box<str>>) {
        self.send(Command::Play {
            cue,
            options,
            part,
            gesture,
            at: Instant::now(),
        });
    }

    fn send(&self, command: Command) {
        // the worker only stops if it panicked; sound is best-effort, so play on silently
        let _ = self.commands.send(command);
    }
}

/// The worker: runs until every [`Cues`] is dropped.
fn run(incoming: &mpsc::Receiver<Command>) {
    let mut engine = Engine::new(Synth);
    let mut output = Output::new();
    loop {
        match incoming.recv_timeout(TICK) {
            Ok(command) => handle(command, &mut engine, &mut output),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        output.maintain(Instant::now());
    }
}

fn handle(command: Command, engine: &mut Engine<Synth>, output: &mut Output) {
    match command {
        Command::Play {
            cue,
            options,
            part,
            gesture,
            at,
        } => {
            let Some(played) = engine.decide(cue, &options, part, gesture, at) else {
                return;
            };
            let now = Instant::now();
            let Some(rate) = output.rate(now) else { return };
            if let Some(start) = engine.start(played, part, rate) {
                output.start(start, now);
            }
        }
        Command::Enable(enabled) => {
            if engine.set_enabled(enabled) {
                output.silence();
            }
        }
        Command::Theme(theme) => engine.set_theme(theme),
        Command::Volume(volume) => engine.set_volume(volume),
    }
}
