//! What every play at a rate shares, built once: the noise buffer and the rooms' spectra. cuelume
//! builds these once per `AudioContext`; here they are kept per rate, and the rooms per
//! convolution size, for as long as the process runs.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use super::dsp::{self, ROOM_SEEDS, Spectrum};

type Rooms = [Spectrum; 2];
type Cache<K, V> = LazyLock<Mutex<HashMap<K, Arc<V>>>>;

static NOISE: Cache<u32, [f32]> = LazyLock::new(Mutex::default);
/// Keyed by rate and convolution size.
static ROOMS: Cache<(u32, usize), Rooms> = LazyLock::new(Mutex::default);

/// The shared noise at `rate`.
pub(super) fn noise(rate: u32) -> Arc<[f32]> {
    let mut cache = NOISE.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(
        cache
            .entry(rate)
            .or_insert_with(|| dsp::noise(f64::from(rate)).into()),
    )
}

/// Both channels' room impulses at `rate`, transformed at `size`, the size a play of `dry`
/// samples convolves at.
pub(super) fn rooms(rate: u32, dry: usize) -> Arc<Rooms> {
    let size = dsp::convolution_size(dry, dsp::room_len(f64::from(rate)));
    let mut cache = ROOMS.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(cache.entry((rate, size)).or_insert_with(|| {
        Arc::new(ROOM_SEEDS.map(|seed| Spectrum::new(&dsp::room(f64::from(rate), seed), size)))
    }))
}
