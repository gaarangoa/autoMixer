//! 8-line Feedback Delay Network reverb.
//!
//! Eight delay lines with prime-length tap times feed a normalized Hadamard
//! mixing matrix. A one-pole low-pass in each feedback path provides damping.
//! Stereo output is constructed from two complementary tap mixes.
//!
//! This is a textbook FDN suitable for plate-style ambience and short halls.
//! Pre-allocated; no allocations in `process`.

use crate::engine::smoothed::SmoothedParam;

const N: usize = 8;
const SMOOTH_MS: f32 = 50.0;

/// Prime-ish delay lengths in milliseconds.
const BASE_DELAY_MS: [f32; N] = [29.7, 37.1, 41.3, 43.7, 53.9, 59.3, 67.1, 73.9];

pub struct Reverb {
    fs: f32,
    pub mix: SmoothedParam,     // 0..1 wet level
    pub size: SmoothedParam,    // 0..1 size scalar
    pub damping: SmoothedParam, // 0..1 high-frequency damping
    delays: [Vec<f32>; N],
    write_idx: [usize; N],
    delay_samples: [usize; N],
    feedback: f32,
    lp_state: [f32; N],
}

impl Reverb {
    pub fn new(fs: f32) -> Self {
        let max_delay =
            (BASE_DELAY_MS.iter().cloned().fold(0.0_f32, f32::max) * 0.001 * fs) as usize + 64;
        let delays: [Vec<f32>; N] = std::array::from_fn(|_| vec![0.0; max_delay]);
        let mut r = Self {
            fs,
            mix: SmoothedParam::new(0.5, fs, SMOOTH_MS),
            size: SmoothedParam::new(0.7, fs, SMOOTH_MS),
            damping: SmoothedParam::new(0.4, fs, SMOOTH_MS),
            delays,
            write_idx: [0; N],
            delay_samples: [0; N],
            feedback: 0.85,
            lp_state: [0.0; N],
        };
        r.recompute_delays(0.7);
        r
    }

    fn recompute_delays(&mut self, size: f32) {
        let scale = 0.5 + size.clamp(0.0, 1.0) * 1.5; // 0.5x..2.0x
        for (i, &ms) in BASE_DELAY_MS.iter().enumerate() {
            let samples = ((ms * scale * 0.001) * self.fs) as usize;
            self.delay_samples[i] = samples.max(1).min(self.delays[i].len() - 1);
        }
    }

    pub fn set(&mut self, mix: f32, size: f32, damping: f32) {
        self.mix.set_target(mix.clamp(0.0, 1.0));
        self.size.set_target(size.clamp(0.0, 1.0));
        self.damping.set_target(damping.clamp(0.0, 1.0));
    }

    /// Mix wet into the input. Returns post-mix stereo.
    #[inline]
    pub fn process(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let mix = self.mix.next();
        let size = self.size.next();
        let damping = self.damping.next();
        // Recompute delays only if the rounded length changed; cheap check.
        let want_first = ((BASE_DELAY_MS[0] * (0.5 + size * 1.5) * 0.001) * self.fs) as usize;
        if want_first != self.delay_samples[0] {
            self.recompute_delays(size);
        }

        // Read taps.
        let mut s = [0.0_f32; N];
        for i in 0..N {
            let r_idx = (self.write_idx[i] + self.delays[i].len() - self.delay_samples[i])
                % self.delays[i].len();
            s[i] = self.delays[i][r_idx];
        }

        // Hadamard-like mixing matrix (normalized 8x8 orthogonal pattern).
        let mixed = hadamard_mix(s);

        // Damping low-pass + feedback.
        let damp_coef = damping.clamp(0.0, 0.99);
        for i in 0..N {
            self.lp_state[i] = damp_coef * self.lp_state[i] + (1.0 - damp_coef) * mixed[i];
        }

        // Inputs distributed across lines.
        let in_avg = (in_l + in_r) * 0.5;
        let inputs = [
            in_l,
            in_r,
            in_avg,
            in_avg,
            in_l * 0.7,
            in_r * 0.7,
            in_avg * 0.7,
            in_avg * 0.7,
        ];

        // Write back: input + feedback.
        for i in 0..N {
            let v = inputs[i] + self.lp_state[i] * self.feedback;
            self.delays[i][self.write_idx[i]] = if v.is_finite() { v } else { 0.0 };
            self.write_idx[i] = (self.write_idx[i] + 1) % self.delays[i].len();
        }

        // Stereo wet sum.
        let wet_l = (s[0] + s[2] + s[4] + s[6]) * 0.25;
        let wet_r = (s[1] + s[3] + s[5] + s[7]) * 0.25;

        let dry = 1.0 - mix;
        let wet = mix;
        (in_l * dry + wet_l * wet, in_r * dry + wet_r * wet)
    }

    pub fn pull_wet(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let (l, r) = self.process(in_l, in_r);
        // Subtract dry to expose only the wet component for bus mixing.
        let mix = self.mix.current().max(1.0e-6);
        let dry = 1.0 - mix;
        ((l - in_l * dry) / mix, (r - in_r * dry) / mix)
    }
}

#[inline]
fn hadamard_mix(s: [f32; 8]) -> [f32; 8] {
    // 8x8 Hadamard transform (unnormalized), divided by sqrt(8) for unity.
    let h = 1.0 / (8.0_f32).sqrt();
    [
        (s[0] + s[1] + s[2] + s[3] + s[4] + s[5] + s[6] + s[7]) * h,
        (s[0] - s[1] + s[2] - s[3] + s[4] - s[5] + s[6] - s[7]) * h,
        (s[0] + s[1] - s[2] - s[3] + s[4] + s[5] - s[6] - s[7]) * h,
        (s[0] - s[1] - s[2] + s[3] + s[4] - s[5] - s[6] + s[7]) * h,
        (s[0] + s[1] + s[2] + s[3] - s[4] - s[5] - s[6] - s[7]) * h,
        (s[0] - s[1] + s[2] - s[3] - s[4] + s[5] - s[6] + s[7]) * h,
        (s[0] + s[1] - s[2] - s[3] - s[4] - s[5] + s[6] + s[7]) * h,
        (s[0] - s[1] - s[2] + s[3] - s[4] + s[5] + s[6] - s[7]) * h,
    ]
}
