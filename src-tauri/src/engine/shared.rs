use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64},
    Arc,
};

use arc_swap::{ArcSwap, ArcSwapOption};

use super::automation::AutomationSnapshot;
use super::mixer::MAX_TRACKS;

/// Audio data for one clip on a track. Immutable once published.
pub struct TrackClipSource {
    pub start_sample: u64,
    pub duration_samples: u64,
    pub source_offset_sample: u64,
    pub gain_db: f32,
    pub channels: u16,
    pub sample_rate: u32,
    /// Decoded PCM is shared by every clip that references the same cache file.
    /// Keeping an `Arc` here avoids copying an entire take for repeated regions.
    pub buffer: Arc<[f32]>,
}

/// Audio data bound to a track slot. Immutable once published; rebinding is
/// done by storing a new `Arc<TrackSource>` into the slot's `ArcSwapOption`.
pub struct TrackSource {
    pub clips: Vec<TrackClipSource>,
}

/// State shared between UI thread and audio thread.
pub struct EngineShared {
    pub playhead: AtomicU64,
    pub master_peak: AtomicU32,
    pub running: AtomicBool,
    /// Per-slot source binding. Writes are lock-free from UI; reads are
    /// wait-free on the audio thread.
    pub source_slots: Vec<Arc<ArcSwapOption<TrackSource>>>,
    /// Per-slot peak meter (fixed point: amplitude * 1_000_000).
    pub track_peaks: Vec<AtomicU32>,
    /// Lock-free automation snapshot. UI publishes; audio thread reads.
    pub automation: ArcSwap<AutomationSnapshot>,
}

impl EngineShared {
    pub fn new() -> Self {
        let source_slots = (0..MAX_TRACKS)
            .map(|_| Arc::new(ArcSwapOption::<TrackSource>::from(None)))
            .collect();
        let track_peaks = (0..MAX_TRACKS).map(|_| AtomicU32::new(0)).collect();
        Self {
            playhead: AtomicU64::new(0),
            master_peak: AtomicU32::new(0),
            running: AtomicBool::new(false),
            source_slots,
            track_peaks,
            automation: ArcSwap::from_pointee(AutomationSnapshot::default()),
        }
    }
}

impl Default for EngineShared {
    fn default() -> Self {
        Self::new()
    }
}
