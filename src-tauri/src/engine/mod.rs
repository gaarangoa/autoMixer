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

use std::{path::Path, sync::Arc};

use crossbeam_channel::{unbounded, Receiver, Sender};
use rtrb::{Producer, RingBuffer};

use commands::EngineCommand;
use events::EngineEvent;
use shared::{EngineShared, TrackClipSource, TrackSource};
use source::cache::read_cache_all;
use thread::{AudioThreadConfig, AudioThreadDeps, AudioThreadHandle};

use crate::model::MixSession;

const COMMAND_QUEUE_CAPACITY: usize = 4096;

/// Top-level engine handle used by the UI / Tauri command surface.
pub struct AudioEngine {
    command_tx: Producer<EngineCommand>,
    events_rx: Receiver<EngineEvent>,
    shared: Arc<EngineShared>,
    audio_thread: Option<AudioThreadHandle>,
    playing_session: Option<String>,
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
            config: AudioThreadConfig { preferred_block_size: block_size },
        });

        Self {
            command_tx: producer,
            events_rx,
            shared,
            audio_thread: Some(audio_thread),
            playing_session: None,
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
        self.send(EngineCommand::Play);
        if let Some(thread) = self.audio_thread.as_ref() {
            thread.start();
        }
    }

    pub fn pause(&mut self) {
        self.send(EngineCommand::Pause);
        if let Some(thread) = self.audio_thread.as_ref() {
            thread.stop();
        }
    }

    pub fn stop(&mut self) {
        self.send(EngineCommand::Stop);
        self.playing_session = None;
        if let Some(thread) = self.audio_thread.as_ref() {
            thread.stop();
        }
    }

    pub fn seek(&mut self, sample: u64) {
        self.send(EngineCommand::Seek { sample });
    }

    /// Load each track's cached audio and bind it to the matching engine slot.
    /// Slots beyond the session's track count are cleared.
    pub fn bind_session_sources(&mut self, session: &MixSession) -> Result<(), String> {
        use std::collections::HashMap;
        let by_id: HashMap<&str, &crate::model::SourceFile> =
            session.source_files.iter().map(|s| (s.id.as_str(), s)).collect();

        for (i, track) in session.tracks.iter().enumerate() {
            if i >= self.shared.source_slots.len() {
                break;
            }
            let mut clips = Vec::new();
            if track.clips.is_empty() {
                if let Some(src) = by_id.get(track.source_file_id.as_str()) {
                    let (header, samples) = read_cache_all(Path::new(&src.cache_path))?;
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
                    clips.push(TrackClipSource {
                        start_sample: clip.start_sample,
                        duration_samples: header.frames
                            .saturating_sub(clip.source_offset_sample)
                            .min(clip.end_sample.saturating_sub(clip.start_sample)),
                        source_offset_sample: clip.source_offset_sample,
                        gain_db: clip.gain_db,
                        channels: header.channels,
                        sample_rate: header.sample_rate,
                        buffer: samples,
                    });
                }
            }
            if clips.is_empty() {
                self.shared.source_slots[i].store(None);
            } else {
                self.shared.source_slots[i].store(Some(Arc::new(TrackSource { clips })));
            }
        }
        self.send(EngineCommand::SetSessionRate { rate: session.sample_rate });
        for i in session.tracks.len()..self.shared.source_slots.len() {
            self.shared.source_slots[i].store(None);
        }
        Ok(())
    }

    pub fn unbind_all_sources(&self) {
        for slot in &self.shared.source_slots {
            slot.store(None);
        }
    }

    pub fn publish_automation(&self, session: &MixSession) {
        let snapshot = automation::build_snapshot(session);
        self.shared.automation.store(Arc::new(snapshot));
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new(0)
    }
}
