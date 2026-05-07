//! FFT-based audio analysis. Run once at import time.
//!
//! Outputs:
//! - Peak / RMS / LUFS (BS.1770 K-weighted, mean-square approximation)
//! - Spectral centroid (Hz)
//! - Band energy fractions (low <250 Hz, mid 250-4000 Hz, high >4000 Hz)
//! - Silence percent
//! - Dynamic range (peak - rms in dB)

use realfft::RealFftPlanner;

pub struct AudioAnalysis {
    pub peak_db: f32,
    pub rms_db: f32,
    pub lufs: f32,
    pub spectral_centroid_hz: f32,
    pub low_energy: f32,
    pub mid_energy: f32,
    pub high_energy: f32,
    pub silence_percent: f32,
    pub dynamic_range_db: f32,
}

pub fn analyze(samples: &[f32], channels: u16, sample_rate: u32) -> AudioAnalysis {
    let channels = channels.max(1) as usize;
    if samples.is_empty() {
        return AudioAnalysis {
            peak_db: -120.0,
            rms_db: -120.0,
            lufs: -120.0,
            spectral_centroid_hz: 0.0,
            low_energy: 0.33,
            mid_energy: 0.34,
            high_energy: 0.33,
            silence_percent: 100.0,
            dynamic_range_db: 0.0,
        };
    }

    // Mono mix for spectral analysis.
    let frames = samples.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc = 0.0;
        for c in 0..channels {
            acc += samples[f * channels + c];
        }
        mono.push(acc / channels as f32);
    }

    let peak = mono.iter().fold(0.0_f32, |a, s| a.max(s.abs()));
    let sum_sq: f64 = mono.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    let rms = (sum_sq / mono.len() as f64).sqrt() as f32;
    let silent = mono.iter().filter(|s| s.abs() < 0.0001).count();
    let silence_percent = silent as f32 / mono.len() as f32 * 100.0;

    // K-weighted approximation: high-pass at ~38 Hz then high-shelf at 1.5 kHz +4 dB.
    // We approximate by applying a 1st-order high-pass and a high-shelf via
    // simple recursive filters on the mono signal, then compute mean-square.
    let kw = k_weighted_mean_square(&mono, sample_rate as f32);
    let lufs = if kw > 0.0 { (-0.691 + 10.0 * kw.log10()) as f32 } else { -120.0 };

    let (centroid, low, mid, high) = spectral_features(&mono, sample_rate as f32);

    AudioAnalysis {
        peak_db: amp_to_db(peak),
        rms_db: amp_to_db(rms),
        lufs,
        spectral_centroid_hz: centroid,
        low_energy: low,
        mid_energy: mid,
        high_energy: high,
        silence_percent,
        dynamic_range_db: (amp_to_db(peak) - amp_to_db(rms)).max(0.0),
    }
}

fn amp_to_db(v: f32) -> f32 {
    if v <= 1.0e-7 {
        -120.0
    } else {
        20.0 * v.log10()
    }
}

fn k_weighted_mean_square(mono: &[f32], fs: f32) -> f64 {
    // Simplified 2-stage filter — not bit-exact BS.1770 but provides a usable
    // loudness proxy for prompting the assistant.
    let hp_alpha = (-2.0 * std::f32::consts::PI * 38.0 / fs).exp();
    let mut hp_y = 0.0_f32;
    let mut hp_x_prev = 0.0_f32;

    // High-shelf at ~1500 Hz, +4 dB approximated as a one-pole shelving boost.
    let shelf_freq = 1500.0;
    let shelf_alpha = (-2.0 * std::f32::consts::PI * shelf_freq / fs).exp();
    let shelf_gain = 10.0_f32.powf(4.0 / 20.0);
    let mut shelf_lp = 0.0_f32;

    let mut sum_sq = 0.0_f64;
    for &x in mono {
        // High-pass.
        hp_y = hp_alpha * (hp_y + x - hp_x_prev);
        hp_x_prev = x;
        // Shelf: split into LP component and HP component, add HP gain.
        shelf_lp = shelf_alpha * shelf_lp + (1.0 - shelf_alpha) * hp_y;
        let high = hp_y - shelf_lp;
        let shelved = shelf_lp + high * shelf_gain;
        sum_sq += (shelved as f64) * (shelved as f64);
    }
    sum_sq / mono.len() as f64
}

fn spectral_features(mono: &[f32], fs: f32) -> (f32, f32, f32, f32) {
    const N: usize = 4096;
    if mono.len() < N {
        return (0.0, 0.33, 0.34, 0.33);
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();

    // Average spectrum across non-overlapping windows.
    let hop = N;
    let mut acc = vec![0.0_f32; output.len()];
    let mut window_count = 0_usize;
    let mut pos = 0;
    while pos + N <= mono.len() {
        for i in 0..N {
            // Hann window.
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (N - 1) as f32).cos();
            input[i] = mono[pos + i] * w;
        }
        if fft.process(&mut input, &mut output).is_err() {
            break;
        }
        for (a, c) in acc.iter_mut().zip(output.iter()) {
            *a += (c.re * c.re + c.im * c.im).sqrt();
        }
        window_count += 1;
        pos += hop;
        if window_count >= 64 {
            break;
        }
    }
    if window_count == 0 {
        return (0.0, 0.33, 0.34, 0.33);
    }
    for v in acc.iter_mut() {
        *v /= window_count as f32;
    }

    let bin_hz = fs / N as f32;
    let mut total = 0.0_f32;
    let mut weighted_sum = 0.0_f32;
    let mut low = 0.0_f32;
    let mut mid = 0.0_f32;
    let mut high = 0.0_f32;
    for (i, mag) in acc.iter().enumerate() {
        let freq = i as f32 * bin_hz;
        weighted_sum += freq * mag;
        total += mag;
        if freq < 250.0 {
            low += mag;
        } else if freq < 4000.0 {
            mid += mag;
        } else {
            high += mag;
        }
    }

    let centroid = if total > 0.0 { weighted_sum / total } else { 0.0 };
    let denom = (low + mid + high).max(1.0e-6);
    (centroid, low / denom, mid / denom, high / denom)
}
