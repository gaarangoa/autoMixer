//! Multi-resolution peak files for waveform rendering.
//! Each level is samples-per-peak; output is min/max pairs (interleaved as f32).

use serde::{Deserialize, Serialize};

pub const ZOOM_LEVELS: [usize; 4] = [256, 1024, 4096, 16384];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeakLevel {
    pub samples_per_peak: usize,
    /// Pairs of (min, max) interleaved.
    pub mins: Vec<f32>,
    pub maxs: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeakFile {
    pub channels: u16,
    pub sample_rate: u32,
    pub frames: u64,
    pub levels: Vec<PeakLevel>,
    /// Coarse single-row preview for dense UI usage.
    pub preview: Vec<f32>,
}

pub fn build_peaks(samples: &[f32], channels: u16, sample_rate: u32) -> PeakFile {
    let channels = channels.max(1) as usize;
    let frames = samples.len() / channels;
    let mut levels = Vec::with_capacity(ZOOM_LEVELS.len());
    for &spp in ZOOM_LEVELS.iter() {
        levels.push(build_level(samples, channels, frames, spp));
    }
    let preview = build_preview(samples, channels, frames, 512);
    PeakFile {
        channels: channels as u16,
        sample_rate,
        frames: frames as u64,
        levels,
        preview,
    }
}

fn build_level(samples: &[f32], channels: usize, frames: usize, spp: usize) -> PeakLevel {
    let bins = (frames + spp - 1) / spp;
    let mut mins = Vec::with_capacity(bins);
    let mut maxs = Vec::with_capacity(bins);
    for bin in 0..bins {
        let start = bin * spp;
        let end = ((bin + 1) * spp).min(frames);
        let mut lo = 0.0_f32;
        let mut hi = 0.0_f32;
        for f in start..end {
            for c in 0..channels {
                let s = samples[f * channels + c];
                if s < lo {
                    lo = s;
                }
                if s > hi {
                    hi = s;
                }
            }
        }
        mins.push(lo);
        maxs.push(hi);
    }
    PeakLevel { samples_per_peak: spp, mins, maxs }
}

fn build_preview(samples: &[f32], channels: usize, frames: usize, bins: usize) -> Vec<f32> {
    if frames == 0 {
        return vec![0.02; bins];
    }
    let step = ((frames + bins - 1) / bins).max(1);
    (0..bins)
        .map(|bin| {
            let start = bin * step;
            let end = ((bin + 1) * step).min(frames);
            let mut peak = 0.0_f32;
            for f in start..end {
                for c in 0..channels {
                    let s = samples[f * channels + c].abs();
                    if s > peak {
                        peak = s;
                    }
                }
            }
            peak
        })
        .collect()
}
