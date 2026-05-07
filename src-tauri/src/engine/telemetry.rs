//! Polls audio-thread atomics and emits engine events to the UI.
//! Runs on the tokio runtime; never blocks the audio thread.

use std::{sync::Arc, time::Duration};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::time::interval;

use super::shared::EngineShared;
use super::mixer::MAX_TRACKS;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlayheadEvent {
    pub sample: u64,
    pub running: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MetersEvent {
    pub master_peak: f32,
    pub track_peaks: Vec<f32>,
}

const POLL_HZ: u64 = 30;

pub fn spawn_telemetry(app: AppHandle, shared: Arc<EngineShared>) {
    tauri::async_runtime::spawn(async move {
        let mut tick = interval(Duration::from_millis(1000 / POLL_HZ));
        let mut last_playhead = u64::MAX;
        let mut last_running = false;
        loop {
            tick.tick().await;
            let running = shared.running.load(std::sync::atomic::Ordering::Relaxed);
            let playhead = shared.playhead.load(std::sync::atomic::Ordering::Relaxed);
            if playhead != last_playhead || running != last_running {
                let _ = app.emit(
                    "engine:playhead",
                    PlayheadEvent { sample: playhead, running },
                );
                last_playhead = playhead;
                last_running = running;
            }
            let master_peak = shared.master_peak.load(std::sync::atomic::Ordering::Relaxed) as f32
                / 1_000_000.0;
            let track_peaks: Vec<f32> = (0..MAX_TRACKS)
                .map(|i| {
                    shared.track_peaks[i].load(std::sync::atomic::Ordering::Relaxed) as f32
                        / 1_000_000.0
                })
                .collect();
            let _ = app.emit(
                "engine:meters",
                MetersEvent { master_peak, track_peaks },
            );
        }
    });
}
