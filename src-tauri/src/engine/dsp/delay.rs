//! Stereo feedback delay with fractional sample interpolation.
//! Low-pass in the feedback path tames buildup.

use crate::engine::smoothed::SmoothedParam;

const SMOOTH_MS: f32 = 50.0;
const MAX_DELAY_SECONDS: f32 = 4.0;

pub struct StereoDelay {
    fs: f32,
    pub time_ms: SmoothedParam,
    pub feedback: SmoothedParam,
    pub mix: SmoothedParam,
    /// Stereo offset in samples (negative = swap).
    pub spread: f32,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write: usize,
    lp_l: f32,
    lp_r: f32,
    damp_coef: f32,
}

impl StereoDelay {
    pub fn new(fs: f32) -> Self {
        let max = (MAX_DELAY_SECONDS * fs) as usize + 64;
        Self {
            fs,
            time_ms: SmoothedParam::new(350.0, fs, SMOOTH_MS),
            feedback: SmoothedParam::new(0.35, fs, SMOOTH_MS),
            mix: SmoothedParam::new(0.5, fs, SMOOTH_MS),
            spread: 0.0,
            buf_l: vec![0.0; max],
            buf_r: vec![0.0; max],
            write: 0,
            lp_l: 0.0,
            lp_r: 0.0,
            damp_coef: 0.4,
        }
    }

    pub fn set(&mut self, time_ms: f32, feedback: f32, mix: f32) {
        self.time_ms
            .set_target(time_ms.clamp(1.0, MAX_DELAY_SECONDS * 1000.0));
        self.feedback.set_target(feedback.clamp(0.0, 0.95));
        self.mix.set_target(mix.clamp(0.0, 1.0));
    }

    #[inline]
    pub fn process(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let time_samples = (self.time_ms.next() * 0.001 * self.fs).max(1.0);
        let fb = self.feedback.next();
        let mix = self.mix.next();
        let len = self.buf_l.len() as f32;

        let read_pos = (self.write as f32 + len - time_samples) % len;
        let i0 = read_pos as usize % self.buf_l.len();
        let i1 = (i0 + 1) % self.buf_l.len();
        let frac = read_pos - i0 as f32;

        let dl = lerp(self.buf_l[i0], self.buf_l[i1], frac);
        let dr = lerp(self.buf_r[i0], self.buf_r[i1], frac);

        // LP in feedback path for tame repeats.
        self.lp_l = self.damp_coef * self.lp_l + (1.0 - self.damp_coef) * dl;
        self.lp_r = self.damp_coef * self.lp_r + (1.0 - self.damp_coef) * dr;

        let wl = in_l + self.lp_r * fb; // ping-pong feedback
        let wr = in_r + self.lp_l * fb;
        self.buf_l[self.write] = if wl.is_finite() { wl } else { 0.0 };
        self.buf_r[self.write] = if wr.is_finite() { wr } else { 0.0 };
        self.write = (self.write + 1) % self.buf_l.len();

        let dry = 1.0 - mix;
        (in_l * dry + dl * mix, in_r * dry + dr * mix)
    }

    pub fn pull_wet(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let (l, r) = self.process(in_l, in_r);
        let m = self.mix.current().max(1.0e-6);
        let dry = 1.0 - m;
        ((l - in_l * dry) / m, (r - in_r * dry) / m)
    }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
