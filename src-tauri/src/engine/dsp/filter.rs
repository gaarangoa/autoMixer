//! HP / LP filters with selectable slope (12 or 24 dB/oct via cascaded biquads).

use crate::engine::smoothed::SmoothedParam;

use super::biquad::{BiquadCoefs, BiquadKind, BiquadState};

const SMOOTH_MS: f32 = 25.0;

#[derive(Clone, Copy)]
pub enum FilterMode {
    HighPass,
    LowPass,
}

pub struct CascadeFilter {
    mode: FilterMode,
    pub enabled: bool,
    pub freq: SmoothedParam,
    /// Number of biquad stages (1 = 12 dB/oct, 2 = 24 dB/oct).
    pub stages: u8,
    coefs: BiquadCoefs,
    left: [BiquadState; 2],
    right: [BiquadState; 2],
}

impl CascadeFilter {
    pub fn new(mode: FilterMode, fs: f32, freq: f32) -> Self {
        let kind = match mode {
            FilterMode::HighPass => BiquadKind::HighPass,
            FilterMode::LowPass => BiquadKind::LowPass,
        };
        Self {
            mode,
            enabled: false,
            freq: SmoothedParam::new(freq, fs, SMOOTH_MS),
            stages: 1,
            coefs: BiquadCoefs::design(kind, fs, freq, 0.707, 0.0),
            left: [BiquadState::default(); 2],
            right: [BiquadState::default(); 2],
        }
    }

    pub fn set(&mut self, enabled: bool, freq: f32, slope_db_oct: u16) {
        self.enabled = enabled;
        self.freq.set_target(freq.clamp(20.0, 20_000.0));
        self.stages = if slope_db_oct >= 24 { 2 } else { 1 };
    }

    pub fn refresh(&mut self, fs: f32) {
        let f = self.freq.next();
        let kind = match self.mode {
            FilterMode::HighPass => BiquadKind::HighPass,
            FilterMode::LowPass => BiquadKind::LowPass,
        };
        // Butterworth Q = 0.707 for 2nd-order; cascaded stages give Linkwitz-Riley-ish steepness.
        self.coefs = BiquadCoefs::design(kind, fs, f, 0.707, 0.0);
    }

    #[inline]
    pub fn process_stereo(&mut self, mut l: f32, mut r: f32) -> (f32, f32) {
        if !self.enabled {
            return (l, r);
        }
        for s in 0..self.stages as usize {
            l = self.left[s].process(&self.coefs, l);
            r = self.right[s].process(&self.coefs, r);
        }
        (l, r)
    }
}
