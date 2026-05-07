//! Stereo biquad filter using RBJ audio cookbook coefficients.

use std::f32::consts::PI;

#[derive(Clone, Copy, Debug)]
pub enum BiquadKind {
    LowPass,
    HighPass,
    LowShelf,
    HighShelf,
    Peak,
}

#[derive(Clone, Copy, Debug)]
pub struct BiquadCoefs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoefs {
    pub const IDENTITY: Self = Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    pub fn design(kind: BiquadKind, fs: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        let freq = freq.clamp(10.0, fs * 0.49);
        let q = q.max(0.05);
        let omega = 2.0 * PI * freq / fs;
        let cos_w = omega.cos();
        let sin_w = omega.sin();
        let alpha = sin_w / (2.0 * q);
        let a = 10.0_f32.powf(gain_db / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::LowPass => {
                let b0 = (1.0 - cos_w) * 0.5;
                let b1 = 1.0 - cos_w;
                let b2 = (1.0 - cos_w) * 0.5;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadKind::HighPass => {
                let b0 = (1.0 + cos_w) * 0.5;
                let b1 = -(1.0 + cos_w);
                let b2 = (1.0 + cos_w) * 0.5;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadKind::Peak => {
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w;
                let b2 = 1.0 - alpha * a;
                let a0 = 1.0 + alpha / a;
                let a1 = -2.0 * cos_w;
                let a2 = 1.0 - alpha / a;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadKind::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha);
                let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w);
                let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha);
                let a0 = (a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha;
                let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w);
                let a2 = (a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadKind::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha);
                let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w);
                let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha);
                let a0 = (a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha;
                let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w);
                let a2 = (a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
        };
        let inv = 1.0 / a0;
        Self { b0: b0 * inv, b1: b1 * inv, b2: b2 * inv, a1: a1 * inv, a2: a2 * inv }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct BiquadState {
    z1: f32,
    z2: f32,
}

impl BiquadState {
    #[inline]
    pub fn process(&mut self, coefs: &BiquadCoefs, x: f32) -> f32 {
        // Direct Form II Transposed - low-noise, denormal-resistant.
        let y = coefs.b0 * x + self.z1;
        self.z1 = coefs.b1 * x - coefs.a1 * y + self.z2;
        self.z2 = coefs.b2 * x - coefs.a2 * y;
        if !y.is_finite() {
            self.z1 = 0.0;
            self.z2 = 0.0;
            return 0.0;
        }
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_passes_signal() {
        let mut state = BiquadState::default();
        let coefs = BiquadCoefs::IDENTITY;
        for i in 0..100 {
            let x = (i as f32 / 100.0).sin();
            let y = state.process(&coefs, x);
            assert!((y - x).abs() < 1e-6);
        }
    }

    #[test]
    fn lowpass_attenuates_high_frequency() {
        let coefs = BiquadCoefs::design(BiquadKind::LowPass, 48_000.0, 1_000.0, 0.7, 0.0);
        let mut state = BiquadState::default();
        // 10 kHz tone should be heavily attenuated.
        let mut energy = 0.0_f32;
        for n in 0..2400 {
            let x = (2.0 * std::f32::consts::PI * 10_000.0 * n as f32 / 48_000.0).sin();
            let y = state.process(&coefs, x);
            energy += y * y;
        }
        let energy_input = 1200.0; // half from sin^2 average
        assert!(energy < energy_input * 0.05);
    }
}
