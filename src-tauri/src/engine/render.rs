//! Offline render through the same DSP graph as live playback.
//!
//! Construction: load each track's f32 cache, build an EngineShared with
//! source slots populated, instantiate a Mixer, push the session state in,
//! then process blocks until the longest track ends; write WAV.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use crossbeam_channel::unbounded;
use hound::{SampleFormat, WavSpec, WavWriter};
use rtrb::RingBuffer;

use crate::model::{MixSession, SourceFile};

use super::{
    automation::build_snapshot,
    commands::EngineCommand,
    mixer::{Mixer, MAX_TRACKS},
    shared::{EngineShared, TrackClipSource, TrackSource},
    source::cache::read_cache_all,
};

const RENDER_BLOCK: usize = 1024;

pub struct RenderProgress {
    pub frames_done: u64,
    pub frames_total: u64,
}

pub struct RenderedMix {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

/// Run the offline mixer and return interleaved stereo f32 PCM in memory.
/// Used by the assistant for analysis (e.g. critique) without writing a WAV.
pub fn render_session_to_buffer(session: &MixSession) -> Result<RenderedMix, String> {
    render_session_to_buffer_with_bypass(session, false)
}

/// Render using the same source-only bypass path as the live MIX/ORIG toggle.
pub fn render_session_to_buffer_with_bypass(
    session: &MixSession,
    master_bypass: bool,
) -> Result<RenderedMix, String> {
    let (mut mixer, total_with_tail, channels, _sample_rate) = build_render_mixer(session, master_bypass)?;
    let mut block = vec![0.0_f32; RENDER_BLOCK * channels as usize];
    let mut produced: u64 = 0;
    let mut out: Vec<f32> = Vec::with_capacity((total_with_tail as usize) * channels as usize);
    while produced < total_with_tail {
        mixer.render(&mut block);
        let frames_this_block = block.len() / channels as usize;
        let to_write = ((total_with_tail - produced) as usize).min(frames_this_block);
        out.extend_from_slice(&block[..to_write * channels as usize]);
        produced += to_write as u64;
    }
    Ok(RenderedMix { samples: out, channels, sample_rate: session.sample_rate })
}

fn build_render_mixer(session: &MixSession, master_bypass: bool) -> Result<(Mixer, u64, u16, f32), String> {
    // Load all source caches up front.
    let by_id: HashMap<&str, &SourceFile> =
        session.source_files.iter().map(|s| (s.id.as_str(), s)).collect();

    let shared = Arc::new(EngineShared::new());
    let mut total_frames: u64 = 0;
    for (i, track) in session.tracks.iter().enumerate() {
        if i >= MAX_TRACKS {
            break;
        }
        let mut clips = Vec::new();
        if track.clips.is_empty() {
            if let Some(src) = by_id.get(track.source_file_id.as_str()) {
                let (header, samples) = read_cache_all(Path::new(&src.cache_path))?;
                let end = track.start_sample + header.frames;
                total_frames = total_frames.max(end);
                clips.push(TrackClipSource {
                    start_sample: track.start_sample,
                    duration_samples: header.frames,
                    source_offset_sample: 0,
                    gain_db: 0.0,
                    channels: header.channels,
                    sample_rate: header.sample_rate,
                    buffer: samples,
                });
            }
        } else {
            for clip in &track.clips {
                let source_id = clip.source_file_id.as_deref().unwrap_or(track.source_file_id.as_str());
                let Some(src) = by_id.get(source_id) else {
                    continue;
                };
                let (header, samples) = read_cache_all(Path::new(&src.cache_path))?;
                let duration = header.frames
                    .saturating_sub(clip.source_offset_sample)
                    .min(clip.end_sample.saturating_sub(clip.start_sample));
                total_frames = total_frames.max(clip.start_sample + duration);
                clips.push(TrackClipSource {
                    start_sample: clip.start_sample,
                    duration_samples: duration,
                    source_offset_sample: clip.source_offset_sample,
                    gain_db: clip.gain_db,
                    channels: header.channels,
                    sample_rate: header.sample_rate,
                    buffer: samples,
                });
            }
        }
        if !clips.is_empty() {
            shared.source_slots[i].store(Some(Arc::new(TrackSource { clips })));
        }
    }
    if total_frames == 0 {
        total_frames = session.sample_rate as u64;
    }
    shared.automation.store(Arc::new(build_snapshot(session)));

    // Build a Mixer; dispatch session state through the command queue.
    let (mut producer, consumer) = RingBuffer::<EngineCommand>::new(4096);
    let (events_tx, _events_rx) = unbounded();
    let sample_rate = session.sample_rate as f32;
    let channels: u16 = 2;
    let mixer = Mixer::new(sample_rate, channels, consumer, events_tx, shared.clone());

    // Push session state.
    let _ = producer.push(EngineCommand::SetMasterGainDb(session.master.gain_db));
    let _ = producer.push(EngineCommand::SetMasterCeilingDb(session.master.limiter.ceiling_db));
    let _ = producer.push(EngineCommand::SetMasterBypass { enabled: master_bypass });
    for (i, track) in session.tracks.iter().enumerate() {
        if i >= MAX_TRACKS {
            break;
        }
        let slot = i as u32;
        let _ = producer.push(EngineCommand::SetTrackActive { slot, active: true });
        let _ = producer.push(EngineCommand::SetTrackGainDb { slot, db: track.gain_db });
        let _ = producer.push(EngineCommand::SetTrackPan { slot, pan: track.pan });
        let _ = producer.push(EngineCommand::SetTrackMuted { slot, muted: track.muted });
        let _ = producer.push(EngineCommand::SetTrackSolo { slot, solo: track.solo });
        let _ = producer.push(EngineCommand::SetTrackReverbSendDb {
            slot,
            db: track.sends.reverb_db,
        });
        let _ = producer.push(EngineCommand::SetTrackDelaySendDb {
            slot,
            db: track.sends.delay_db,
        });
        let _ = producer.push(EngineCommand::SetTrackHighPass {
            slot,
            enabled: track.chain.high_pass.enabled,
            frequency_hz: track.chain.high_pass.frequency_hz,
            slope_db_oct: track.chain.high_pass.slope_db_oct,
        });
        let _ = producer.push(EngineCommand::SetTrackLowPass {
            slot,
            enabled: track.chain.low_pass.enabled,
            frequency_hz: track.chain.low_pass.frequency_hz,
            slope_db_oct: track.chain.low_pass.slope_db_oct,
        });
        for (band_idx, band) in track.chain.eq.iter().enumerate().take(4) {
            let _ = producer.push(EngineCommand::SetTrackEqBand {
                slot,
                band: band_idx as u8,
                frequency_hz: band.frequency_hz,
                gain_db: band.gain_db,
                q: band.q,
            });
        }
        let comp = &track.chain.compressor;
        let _ = producer.push(EngineCommand::SetTrackCompressor {
            slot,
            enabled: comp.enabled,
            threshold_db: comp.threshold_db,
            ratio: comp.ratio,
            attack_ms: comp.attack_ms,
            release_ms: comp.release_ms,
            knee_db: comp.knee_db,
            makeup_db: comp.makeup_db,
        });
    }
    let _ = producer.push(EngineCommand::Play);

    let tail_seconds = 2.0_f32;
    let tail_frames = (tail_seconds * sample_rate) as u64;
    let total_with_tail = total_frames + tail_frames;
    Ok((mixer, total_with_tail, channels, sample_rate))
}

pub fn render_session(session: &MixSession, output_path: &Path) -> Result<PathBuf, String> {
    let path = if output_path.extension().is_some() {
        output_path.to_path_buf()
    } else {
        output_path.with_extension("wav")
    };
    let (mut mixer, total_with_tail, channels, _sample_rate) = build_render_mixer(session, false)?;
    let spec = WavSpec {
        channels,
        sample_rate: session.sample_rate,
        bits_per_sample: 24,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
    let mut block = vec![0.0_f32; RENDER_BLOCK * channels as usize];
    let mut produced: u64 = 0;
    while produced < total_with_tail {
        mixer.render(&mut block);
        let frames_this_block = block.len() / channels as usize;
        let to_write = ((total_with_tail - produced) as usize).min(frames_this_block);
        for f in 0..to_write {
            for c in 0..channels as usize {
                let s = block[f * channels as usize + c].clamp(-1.0, 1.0);
                let i32_sample = (s * 8_388_607.0) as i32;
                writer.write_sample(i32_sample).map_err(|e| e.to_string())?;
            }
        }
        produced += to_write as u64;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(path)
}
