//! Feedforward compressor with log-domain detector.
//! Soft-knee, separate attack/release, makeup gain.

use crate::engine::smoothed::{db_to_gain, SmoothedParam};

const SMOOTH_MS: f32 = 25.0;

pub struct Compressor {
    pub enabled: bool,
    pub threshold_db: SmoothedParam,
    pub ratio: SmoothedParam,
    pub knee_db: SmoothedParam,
    pub makeup_db: SmoothedParam,
    pub attack_coef: f32,
    pub release_coef: f32,
    /// Detector state (smoothed envelope, dB).
    env_db: f32,
    fs: f32,
}

impl Compressor {
    pub fn new(fs: f32) -> Self {
        let mut c = Self {
            enabled: false,
            threshold_db: SmoothedParam::new(-18.0, fs, SMOOTH_MS),
            ratio: SmoothedParam::new(2.0, fs, SMOOTH_MS),
            knee_db: SmoothedParam::new(6.0, fs, SMOOTH_MS),
            makeup_db: SmoothedParam::new(0.0, fs, SMOOTH_MS),
            attack_coef: 0.0,
            release_coef: 0.0,
            env_db: -120.0,
            fs,
        };
        c.set_attack_release(20.0, 160.0);
        c
    }

    pub fn set(
        &mut self,
        enabled: bool,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        knee_db: f32,
        makeup_db: f32,
    ) {
        self.enabled = enabled;
        self.threshold_db.set_target(threshold_db);
        self.ratio.set_target(ratio.max(1.0));
        self.knee_db.set_target(knee_db.max(0.0));
        self.makeup_db.set_target(makeup_db);
        self.set_attack_release(attack_ms, release_ms);
    }

    fn set_attack_release(&mut self, attack_ms: f32, release_ms: f32) {
        // One-pole: y[n] = a*y[n-1] + (1-a)*x[n]
        // Time constant -> coefficient:
        let attack = (-1.0 / (self.fs * (attack_ms.max(0.1) * 0.001))).exp();
        let release = (-1.0 / (self.fs * (release_ms.max(1.0) * 0.001))).exp();
        self.attack_coef = attack;
        self.release_coef = release;
    }

    #[cfg(test)]
    pub fn force_envelope_db(&mut self, db: f32) {
        self.env_db = db;
    }

    /// Compute gain reduction (in linear) for the current input level.
    /// Caller multiplies the input signal by this factor.
    #[inline]
    pub fn process_link(&mut self, l: f32, r: f32) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let threshold = self.threshold_db.next();
        let ratio = self.ratio.next();
        let knee = self.knee_db.next();
        let makeup = self.makeup_db.next();

        // Stereo-linked detector: max of |L|, |R| in dB.
        let peak = l.abs().max(r.abs());
        let level_db = if peak <= 1.0e-7 {
            -140.0
        } else {
            20.0 * peak.log10()
        };

        // Smooth the detector with attack/release.
        let coef = if level_db > self.env_db {
            self.attack_coef
        } else {
            self.release_coef
        };
        self.env_db = coef * self.env_db + (1.0 - coef) * level_db;

        // Static curve: soft knee.
        let over = self.env_db - threshold;
        let gain_reduction_db = if knee > 0.0 && over.abs() <= knee * 0.5 {
            // Quadratic spline through the knee.
            let x = over + knee * 0.5;
            let scaled = (1.0 / ratio - 1.0) * x * x / (2.0 * knee);
            scaled
        } else if over > 0.0 {
            (1.0 / ratio - 1.0) * over
        } else {
            0.0
        };

        let total_db = gain_reduction_db + makeup;
        db_to_gain(total_db).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_threshold_unity_gain() {
        let mut c = Compressor::new(48_000.0);
        c.set(true, -10.0, 4.0, 5.0, 50.0, 0.0, 0.0);
        // Run a few samples to seed envelope below threshold.
        for _ in 0..1000 {
            c.process_link(0.05, 0.05);
        }
        let g = c.process_link(0.05, 0.05);
        assert!((g - 1.0).abs() < 0.05, "expected ~1.0, got {g}");
    }

    #[test]
    fn above_threshold_attenuates() {
        let mut c = Compressor::new(48_000.0);
        c.set(true, -20.0, 4.0, 1.0, 100.0, 0.0, 0.0);
        // Force envelope to a known high value so we get full static-curve gain reduction.
        c.force_envelope_db(0.0); // 20 dB over threshold, ratio 4 -> 15 dB reduction.
        let g = c.process_link(1.0, 1.0);
        let g_db = 20.0 * g.log10();
        // Allow some tolerance for one-block envelope drift.
        assert!(g_db < -5.0, "expected attenuation, got {g_db} dB");
    }
}
