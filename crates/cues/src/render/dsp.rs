//! Web Audio's half of cuelume, done by hand as `scripts/sound.mjs` does it: RBJ biquads with Web
//! Audio's formulas, the shared noise, the room impulse and an FFT convolution.

use std::f64::consts::{LN_10, PI};

use super::layers::{FLAT_Q, Filter};
use super::rng::Random;

/// Normalised biquad coefficients `[b0, b1, b2, a1, a2]`.
pub(super) type Coefficients = [f64; 5];

/// Web Audio's biquad for `filter` at `frequency`, its Q in dB for lowpass and highpass.
pub(super) fn biquad(filter: Filter, frequency: f64, q: f64, rate: f64) -> Coefficients {
    let w = (2.0 * PI * frequency.min(rate / 2.0 - 1.0)) / rate;
    let (c, s) = (w.cos(), w.sin());
    if filter == Filter::Bandpass {
        let a = s / (2.0 * q);
        let a0 = 1.0 + a;
        return [a / a0, 0.0, -a / a0, (-2.0 * c) / a0, (1.0 - a) / a0];
    }
    let a = s / (2.0 * 10f64.powf(q / 20.0));
    let b = if filter == Filter::Lowpass {
        [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0]
    } else {
        [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0]
    };
    let a0 = 1.0 + a;
    [
        b[0] / a0,
        b[1] / a0,
        b[2] / a0,
        (-2.0 * c) / a0,
        (1.0 - a) / a0,
    ]
}

/// A direct-form-I biquad's running state.
#[derive(Default)]
pub(super) struct Biquad {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    pub fn step(&mut self, k: &Coefficients, x: f64) -> f64 {
        let y = k[0] * x + k[1] * self.x1 + k[2] * self.x2 - k[3] * self.y1 - k[4] * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Runs `x` through one fixed biquad, in place.
pub(super) fn filter(x: &mut [f64], k: &Coefficients) {
    let mut state = Biquad::default();
    for v in x {
        *v = state.step(k, *v);
    }
}

pub(super) const NOISE_SECONDS: f64 = 2.0;
const NOISE_SEED: u32 = 1;

/// The shared noise every noise layer loops from, stored as `f32` as cuelume's buffer is.
pub(super) fn noise(rate: f64) -> Vec<f32> {
    let mut random = Random::new(NOISE_SEED);
    let length = ((NOISE_SECONDS * rate).floor() as usize).max(1);
    (0..length)
        .map(|_| (2.0 * random.next() - 1.0) as f32)
        .collect()
}

pub(super) const ROOM_SECONDS: f64 = 0.25;
const ROOM_PREDELAY: f64 = 0.008;
const ROOM_LOWPASS: f64 = 3500.0;
pub(super) const ROOM_SEND: f64 = 0.08;
const ROOM_DECAY_DB: f64 = 60.0;
pub(super) const ROOM_SEEDS: [u32; 2] = [3110, 5639];
const ROOM_REFLECTIONS_PER_SECOND: f64 = 48_000.0;

/// `output.ts`'s `fillRoom`: a dense burst of reflections dying 60 dB over `ROOM_SECONDS`,
/// unit energy, spread onto `rate`'s grid. Stored as `f32`, as Web Audio's buffer is.
fn fill_room(data: &mut [f32], rate: f64, seed: u32) {
    let mut random = Random::new(seed);
    let fall = (ROOM_DECAY_DB / 20.0) * LN_10;
    let count = (ROOM_SECONDS * ROOM_REFLECTIONS_PER_SECOND).floor() as usize;
    let reflections: Vec<f32> = (0..count)
        .map(|i| {
            let t = i as f64 / ROOM_REFLECTIONS_PER_SECOND;
            if t < ROOM_PREDELAY {
                0.0
            } else {
                ((2.0 * random.next() - 1.0) * ((-fall * t) / ROOM_SECONDS).exp()) as f32
            }
        })
        .collect();
    let energy: f64 = reflections
        .iter()
        .map(|&r| f64::from(r) * f64::from(r))
        .sum();
    let scale = 1.0 / energy.sqrt();
    for (i, &reflection) in reflections.iter().enumerate() {
        let at = (i as f64 / ROOM_REFLECTIONS_PER_SECOND) * rate;
        let sample = at.floor();
        let late = at - sample;
        let sample = sample as usize;
        let reflection = f64::from(reflection) * scale;
        if let Some(v) = data.get_mut(sample) {
            *v = (f64::from(*v) + reflection * (1.0 - late)) as f32;
        }
        if let Some(v) = data.get_mut(sample + 1) {
            *v = (f64::from(*v) + reflection * late) as f32;
        }
    }
}

/// One channel's room: the impulse through the walls' lowpass, as `sound.mjs` prefilters it.
pub(super) fn room(rate: f64, seed: u32) -> Vec<f64> {
    let mut impulse = vec![0f32; ((ROOM_SECONDS * rate).floor() as usize).max(1)];
    fill_room(&mut impulse, rate, seed);
    let mut room: Vec<f64> = impulse.into_iter().map(f64::from).collect();
    filter(
        &mut room,
        &biquad(Filter::Lowpass, ROOM_LOWPASS, FLAT_Q, rate),
    );
    room
}

/// A spectrum ready to convolve with: `x` zero-padded to `size` and transformed.
pub(super) struct Spectrum {
    re: Vec<f64>,
    im: Vec<f64>,
}

impl Spectrum {
    pub fn new(x: &[f64], size: usize) -> Self {
        let mut re = vec![0.0; size];
        re[..x.len()].copy_from_slice(x);
        let mut im = vec![0.0; size];
        fft(&mut re, &mut im);
        Self { re, im }
    }
}

/// The smallest power of two `sound.mjs` convolves `x` and `h` at.
pub(super) fn convolution_size(x: usize, h: usize) -> usize {
    (x + h).next_power_of_two()
}

/// `x` convolved with `h`, the first `length` samples, both spectra the same size.
pub(super) fn convolve(x: &Spectrum, h: &Spectrum, length: usize) -> Vec<f64> {
    let n = x.re.len();
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    for i in 0..n {
        re[i] = x.re[i] * h.re[i] - x.im[i] * h.im[i];
        im[i] = -(x.re[i] * h.im[i] + x.im[i] * h.re[i]);
    }
    // the forward transform of the conjugate, conjugated again below by reading only the real part
    fft(&mut re, &mut im);
    re.truncate(length);
    for v in &mut re {
        *v /= n as f64;
    }
    re
}

/// `sound.mjs`'s in-place radix-2 FFT, its twiddles computed once per stage.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut b = n >> 1;
        while j & b != 0 {
            j ^= b;
            b >>= 1;
        }
        j ^= b;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut twiddles = Vec::with_capacity(n / 2);
    let mut len = 2;
    while len <= n {
        let a = (-2.0 * PI) / len as f64;
        let half = len / 2;
        twiddles.clear();
        twiddles.extend((0..half).map(|k| {
            let angle = a * k as f64;
            (angle.cos(), angle.sin())
        }));
        for i in (0..n).step_by(len) {
            for (k, &(c, s)) in twiddles.iter().enumerate() {
                let (u, v) = (i + k, i + k + half);
                let tr = re[v] * c - im[v] * s;
                let ti = re[v] * s + im[v] * c;
                re[v] = re[u] - tr;
                im[v] = im[u] - ti;
                re[u] += tr;
                im[u] += ti;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From Node, `sound.mjs`'s `biquad` at 48 kHz.
    #[test]
    fn biquad_should_match_sound_mjs() {
        let cases = [
            (Filter::Lowpass, 3500.0, FLAT_Q, JS_LOWPASS),
            (Filter::Highpass, 40.0, FLAT_Q, JS_HIGHPASS),
            (Filter::Bandpass, 4000.0, 8.0, JS_BANDPASS),
        ];
        for (filter, frequency, q, expected) in cases {
            let k = biquad(filter, frequency, q, 48_000.0);
            for (ours, theirs) in k.iter().zip(expected) {
                assert!(
                    (ours - theirs).abs() <= 1e-15,
                    "{filter:?}: {k:?} vs {expected:?}"
                );
            }
        }
    }

    #[test]
    fn convolve_should_match_direct_convolution() {
        let x = [1.0, 2.0, 3.0, 0.0, 0.0];
        let h = [0.5, -1.0];
        let size = convolution_size(x.len(), h.len());
        let y = convolve(&Spectrum::new(&x, size), &Spectrum::new(&h, size), x.len());
        for (ours, theirs) in y.iter().zip([0.5, 0.0, -0.5, -3.0, 0.0]) {
            assert!((ours - theirs).abs() < 1e-12, "{y:?}");
        }
    }

    const JS_LOWPASS: Coefficients = [
        0.03929032586258175,
        0.0785806517251635,
        0.03929032586258175,
        -1.3667937185882215,
        0.5239550220385486,
    ];
    const JS_HIGHPASS: Coefficients = [
        0.9963087984317135,
        -1.992617596863427,
        0.9963087984317135,
        -1.9926039396153263,
        0.9926312541115275,
    ];
    const JS_BANDPASS: Coefficients = [
        0.0303030303030303,
        0.0,
        -0.0303030303030303,
        -1.6795644194607295,
        0.9393939393939394,
    ];
}
