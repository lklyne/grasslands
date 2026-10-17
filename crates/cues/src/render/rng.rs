//! cuelume's seeded generator (`output.ts`'s `seeded`, a mulberry32) and `sound.mjs`'s `seedOf`,
//! with JavaScript's `Math.imul` and `>>>` as wrapping `u32` arithmetic.

pub(super) struct Random {
    state: u32,
}

impl Random {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    /// The next value in [0, 1).
    pub fn next(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let state = self.state;
        let mut mixed = (state ^ (state >> 15)).wrapping_mul(1 | state);
        mixed = mixed.wrapping_add((mixed ^ (mixed >> 7)).wrapping_mul(61 | mixed)) ^ mixed;
        f64::from(mixed ^ (mixed >> 14)) / 4_294_967_296.0
    }
}

/// `sound.mjs`'s FNV-1a string hash.
pub(super) fn seed_of(text: &str) -> u32 {
    text.encode_utf16().fold(2_166_136_261, |hash: u32, unit| {
        (hash ^ u32::from(unit)).wrapping_mul(16_777_619)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From Node: `seeded(1)` four times, as `output.ts` builds the noise.
    #[test]
    fn random_should_match_javascript_for_seed_one() {
        let mut random = Random::new(1);
        let values: Vec<f64> = (0..4).map(|_| random.next()).collect();
        assert_eq!(values, JS_SEEDED_1);
    }

    /// From Node: `seeded(0xdeadbeef)`, a seed with the top bit set.
    #[test]
    fn random_should_match_javascript_for_a_high_seed() {
        let mut random = Random::new(0xdead_beef);
        let values: Vec<f64> = (0..3).map(|_| random.next()).collect();
        assert_eq!(
            values,
            [0.9413696140982211, 0.26719574979506433, 0.772033357527107]
        );
    }

    /// From Node: `seedOf(...) >>> 0`.
    #[test]
    fn seed_of_should_match_javascript() {
        assert_eq!(seed_of("default.ready.normal.both.0"), 1_873_209_539);
        assert_eq!(seed_of("press.tap.strong.release.2"), 2_082_882_955);
    }

    const JS_SEEDED_1: [f64; 4] = [
        0.6270739405881613,
        0.002735721180215478,
        0.5274470399599522,
        0.9810509674716741,
    ];
}
