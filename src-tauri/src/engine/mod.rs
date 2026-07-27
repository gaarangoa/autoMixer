//! Real-time audio engine: cpal output thread, lock-free command queue,
//! click-free smoothed parameters. Step 1 of the engine build-out:
//! plumbing is in place but no source data is wired yet, so playback
//! is silence until the decode/prefetch layers (steps 2 and 3).

pub mod automation;
pub mod commands;
pub mod dsp;
pub mod events;
pub mod mixer;
pub mod render;
pub mod shared;
pub mod smoothed;
pub mod source;
pub mod telemetry;
pub mod thread;

use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use rtrb::{Producer, RingBuffer};

use commands::EngineCommand;
use events::EngineEvent;
use shared::{EngineShared, TrackClipSource, TrackSource};
use source::cache::read_cache_all;
use thread::{AudioThreadConfig, AudioThreadDeps, AudioThreadHandle};

use crate::model::MixSession;

const COMMAND_QUEUE_CAPACITY: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CacheFileStamp {
    len: u64,
    modified_nanos: u128,
}

#[derive(Clone)]
struct DecodedSource {
    stamp: CacheFileStamp,
    channels: u16,
    sample_rate: u32,
    frames: u64,
    buffer: Arc<[f32]>,
}

/// Top-level engine handle used by the UI / Tauri command surface.
pub struct AudioEngine {
    command_tx: Producer<EngineCommand>,
    events_rx: Receiver<EngineEvent>,
    shared: Arc<EngineShared>,
    audio_thread: Option<AudioThreadHandle>,
    playing_session: Option<String>,
    decoded_sources: HashMap<PathBuf, DecodedSource>,
    bound_source_signature: Option<u64>,
}

impl AudioEngine {
    pub fn new(block_size: u32) -> Self {
        let (producer, consumer) = RingBuffer::<EngineCommand>::new(COMMAND_QUEUE_CAPACITY);
        let (events_tx, events_rx): (Sender<EngineEvent>, Receiver<EngineEvent>) = unbounded();
        let shared = Arc::new(EngineShared::new());

        let audio_thread = AudioThreadHandle::spawn(AudioThreadDeps {
            commands: consumer,
            events: events_tx,
            shared: shared.clone(),
            config: AudioThreadConfig {
                preferred_block_size: block_size,
            },
        });

        Self {
            command_tx: producer,
            events_rx,
            shared,
            audio_thread: Some(audio_thread),
            playing_session: None,
            decoded_sources: HashMap::new(),
            bound_source_signature: None,
        }
    }

    pub fn shared(&self) -> Arc<EngineShared> {
        self.shared.clone()
    }

    pub fn drain_events(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn send(&mut self, command: EngineCommand) {
        // If the queue is full we drop the oldest behavior: this is not
        // expected with a 4096-slot queue, but we never block the UI.
        if self.command_tx.push(command).is_err() {
            // Make room by popping one slot and retrying. This is a UI
            // thread, so allocation/blocking is fine; we just ensure no
            // command is permanently lost.
            // (rtrb doesn't expose pop on the producer; in practice we
            // should never get here. Log and drop.)
            eprintln!("[engine] command queue full, dropping command {command:?}");
        }
    }

    pub fn play(&mut self, session_id: String) {
        self.playing_session = Some(session_id);
        if let Some(thread) = self.audio_thread.as_ref() {
            thread.start();
        }
        self.send(EngineCommand::Play);
    }

    pub fn pause(&mut self) {
        self.send(EngineCommand::Pause);
    }

    pub fn stop(&mut self) {
        self.send(EngineCommand::Stop);
        self.playing_session = None;
    }

    pub fn seek(&mut self, sample: u64) {
        self.send(EngineCommand::Seek { sample });
    }

    pub fn set_metronome(
        &mut self,
        enabled: bool,
        bpm: f32,
        numerator: u8,
        denominator: u8,
        volume_db: f32,
    ) {
        self.send(EngineCommand::SetMetronome {
            enabled,
            bpm,
            numerator,
            denominator,
            volume_db,
        });
    }

    /// Load each track's cached audio and bind it to the matching engine slot.
    /// Slots beyond the session's track count are cleared.
    pub fn bind_session_sources(&mut self, session: &MixSession) -> Result<(), String> {
        let signature = source_binding_signature(session)?;
        if self.bound_source_signature == Some(signature) {
            return Ok(());
        }

        let by_id: HashMap<&str, &crate::model::SourceFile> = session
            .source_files
            .iter()
            .map(|s| (s.id.as_str(), s))
            .collect();
        let mut used_paths = HashSet::new();

        for (i, track) in session.tracks.iter().enumerate() {
            if i >= self.shared.source_slots.len() {
                break;
            }
            let mut clips = Vec::new();
            if track.clips.is_empty() && !track.clips_materialized {
                if let Some(src) = by_id.get(track.source_file_id.as_str()) {
                    let path = Path::new(&src.cache_path);
                    used_paths.insert(path.to_path_buf());
                    let decoded = self.decoded_source(path)?;
                    clips.push(TrackClipSource {
                        start_sample: track.start_sample,
                        duration_samples: decoded.frames,
                        source_offset_sample: 0,
                        gain_db: 0.0,
                        channels: decoded.channels,
                        sample_rate: decoded.sample_rate,
                        buffer: decoded.buffer,
                    });
                }
            } else {
                for clip in &track.clips {
                    let source_id = clip
                        .source_file_id
                        .as_deref()
                        .unwrap_or(track.source_file_id.as_str());
                    let Some(src) = by_id.get(source_id) else {
                        continue;
                    };
                    let path = Path::new(&src.cache_path);
                    used_paths.insert(path.to_path_buf());
                    let decoded = self.decoded_source(path)?;
                    clips.push(TrackClipSource {
                        start_sample: clip.start_sample,
                        duration_samples: decoded
                            .frames
                            .saturating_sub(clip.source_offset_sample)
                            .min(clip.end_sample.saturating_sub(clip.start_sample)),
                        source_offset_sample: clip.source_offset_sample,
                        gain_db: clip.gain_db,
                        channels: decoded.channels,
                        sample_rate: decoded.sample_rate,
                        buffer: decoded.buffer,
                    });
                }
            }
            if clips.is_empty() {
                self.shared.source_slots[i].store(None);
            } else {
                self.shared.source_slots[i].store(Some(Arc::new(TrackSource { clips })));
            }
        }
        self.send(EngineCommand::SetSessionRate {
            rate: session.sample_rate,
        });
        for i in session.tracks.len()..self.shared.source_slots.len() {
            self.shared.source_slots[i].store(None);
        }
        self.decoded_sources
            .retain(|path, _| used_paths.contains(path));
        self.bound_source_signature = Some(signature);
        Ok(())
    }

    fn decoded_source(&mut self, path: &Path) -> Result<DecodedSource, String> {
        let stamp = cache_file_stamp(path)?;
        if let Some(cached) = self.decoded_sources.get(path) {
            if cached.stamp == stamp {
                return Ok(cached.clone());
            }
        }
        let (header, samples) = read_cache_all(path)?;
        let decoded = DecodedSource {
            stamp,
            channels: header.channels,
            sample_rate: header.sample_rate,
            frames: header.frames,
            buffer: samples.into(),
        };
        self.decoded_sources
            .insert(path.to_path_buf(), decoded.clone());
        Ok(decoded)
    }

    pub fn unbind_all_sources(&mut self) {
        for slot in &self.shared.source_slots {
            slot.store(None);
        }
        self.decoded_sources.clear();
        self.bound_source_signature = None;
    }

    pub fn publish_automation(&self, session: &MixSession) {
        let snapshot = automation::build_snapshot(session);
        self.shared.automation.store(Arc::new(snapshot));
    }
}

fn cache_file_stamp(path: &Path) -> Result<CacheFileStamp, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("stat {}: {error}", path.display()))?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(CacheFileStamp {
        len: metadata.len(),
        modified_nanos,
    })
}

fn source_binding_signature(session: &MixSession) -> Result<u64, String> {
    let mut hasher = DefaultHasher::new();
    session.id.hash(&mut hasher);
    session.sample_rate.hash(&mut hasher);
    session.tracks.len().hash(&mut hasher);

    let source_by_id: HashMap<&str, &crate::model::SourceFile> = session
        .source_files
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect();
    let mut hashed_sources = HashSet::new();
    for track in &session.tracks {
        track.id.hash(&mut hasher);
        track.source_file_id.hash(&mut hasher);
        track.start_sample.hash(&mut hasher);
        track.clips_materialized.hash(&mut hasher);
        track.clips.len().hash(&mut hasher);
        for clip in &track.clips {
            clip.id.hash(&mut hasher);
            clip.source_file_id.hash(&mut hasher);
            clip.start_sample.hash(&mut hasher);
            clip.end_sample.hash(&mut hasher);
            clip.source_offset_sample.hash(&mut hasher);
            clip.gain_db.to_bits().hash(&mut hasher);
        }

        let source_ids = std::iter::once(track.source_file_id.as_str()).chain(
            track
                .clips
                .iter()
                .filter_map(|clip| clip.source_file_id.as_deref()),
        );
        for source_id in source_ids {
            if !hashed_sources.insert(source_id) {
                continue;
            }
            let Some(source) = source_by_id.get(source_id) else {
                continue;
            };
            source.id.hash(&mut hasher);
            source.cache_path.hash(&mut hasher);
            let stamp = cache_file_stamp(Path::new(&source.cache_path))?;
            stamp.len.hash(&mut hasher);
            stamp.modified_nanos.hash(&mut hasher);
        }
    }
    Ok(hasher.finish())
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new(0)
    }
}
