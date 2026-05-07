//! Source-file pipeline: decode → resample → analyze → cache → peaks.
//! All work happens at import time; playback reads only the cache.

pub mod analysis;
pub mod cache;
pub mod decode;
pub mod peaks;
pub mod resample;

use std::path::Path;

use cache::{write_cache, CacheHeader};
use decode::decode_file;
use peaks::{build_peaks, PeakFile};
use resample::resample_interleaved;

pub struct ImportedAudio {
    pub channels: u16,
    pub sample_rate: u32,
    pub frames: u64,
    pub samples: Vec<f32>,
    pub peaks: PeakFile,
}

pub fn import_to_session_rate(
    source_path: &Path,
    target_sample_rate: u32,
) -> Result<ImportedAudio, String> {
    let decoded = decode_file(source_path)?;
    let samples = if decoded.sample_rate == target_sample_rate {
        decoded.samples
    } else {
        resample_interleaved(
            &decoded.samples,
            decoded.channels,
            decoded.sample_rate,
            target_sample_rate,
        )?
    };
    let frames = (samples.len() / decoded.channels as usize) as u64;
    let peaks = build_peaks(&samples, decoded.channels, target_sample_rate);
    Ok(ImportedAudio {
        channels: decoded.channels,
        sample_rate: target_sample_rate,
        frames,
        samples,
        peaks,
    })
}

pub fn write_to_cache(path: &Path, audio: &ImportedAudio) -> Result<(), String> {
    write_cache(
        path,
        &CacheHeader {
            channels: audio.channels,
            sample_rate: audio.sample_rate,
            frames: audio.frames,
        },
        &audio.samples,
    )
}
