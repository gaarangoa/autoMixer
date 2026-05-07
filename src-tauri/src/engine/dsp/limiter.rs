//! Lookahead sample-peak limiter.
//!
//! 5ms lookahead. Detects upcoming peaks above the ceiling and applies a
//! smoothed gain reduction so the delayed signal never exceeds the ceiling.
//! Soft-knee around the ceiling for transparent character.

use crate::engine::smoothed::{db_to_gain, SmoothedParam};

const SMOOTH_MS: f32 = 25.0;

pub struct Limiter {
    fs: f32,
    pub ceiling_db: SmoothedParam,
    pub release_ms: SmoothedParam,
    #[allow(dead_code)]
    lookahead_samples: usize,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    /// Per-sample peak window (max of next N samples).
    peak_window: Vec<f32>,
    write: usize,
    /// Smoothed gain (linear).
    gain: f32,
    attack_coef: f32,
    release_coef: f32,
}

impl Limiter {
    pub fn new(fs: f32) -> Self {
        let lookahead_samples = (0.005 * fs) as usize;
        Self {
            fs,
            ceiling_db: SmoothedParam::new(-1.0, fs, SMOOTH_MS),
            release_ms: SmoothedParam::new(60.0, fs, SMOOTH_MS),
            lookahead_samples,
            delay_l: vec![0.0; lookahead_samples + 1],
            delay_r: vec![0.0; lookahead_samples + 1],
            peak_window: vec![0.0; lookahead_samples + 1],
            write: 0,
            gain: 1.0,
            attack_coef: (-1.0 / (fs * 0.001)).exp(),
            release_coef: (-1.0 / (fs * 0.060)).exp(),
        }
    }

    pub fn set(&mut self, ceiling_db: f32, release_ms: f32) {
        self.ceiling_db.set_target(ceiling_db);
        self.release_ms.set_target(release_ms.max(1.0));
        self.release_coef = (-1.0 / (self.fs * (release_ms.max(1.0) * 0.001))).exp();
    }

    #[inline]
    pub fn process(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let ceil_db = self.ceiling_db.next();
        let _ = self.release_ms.next();
        let ceiling = db_to_gain(ceil_db).min(1.0);

        // Insert into the lookahead delay line.
        let len = self.delay_l.len();
        self.delay_l[self.write] = in_l;
        self.delay_r[self.write] = in_r;
        let peak = in_l.abs().max(in_r.abs());
        self.peak_window[self.write] = peak;

        // Find the max peak across the lookahead window.
        let mut window_peak = 0.0_f32;
        for &p in &self.peak_window {
            if p > window_peak {
                window_peak = p;
            }
        }

        // Required gain to keep window_peak under ceiling.
        let target_gain = if window_peak > ceiling {
            ceiling / window_peak
        } else {
            1.0
        };

        let coef = if target_gain < self.gain {
            self.attack_coef
        } else {
            self.release_coef
        };
        self.gain = coef * self.gain + (1.0 - coef) * target_gain;
        if !self.gain.is_finite() {
            self.gain = 1.0;
        }

        // Read from the oldest slot (now + 1 = oldest in circular).
        let read = (self.write + 1) % len;
        let out_l = (self.delay_l[read] * self.gain).clamp(-ceiling, ceiling);
        let out_r = (self.delay_r[read] * self.gain).clamp(-ceiling, ceiling);

        self.write = (self.write + 1) % len;
        (out_l, out_r)
    }
}
