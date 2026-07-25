use uuid::Uuid;

use crate::model::{
    CompressorState, EqBand, EqBandType, FilterState, LimiterState, MasterChannel, Sends, Track,
    TrackChain, TrackKind,
};

const COLORS: [&str; 7] = [
    "#4f8cff", "#d95f5f", "#2f9e6e", "#c28a2c", "#8b6bd8", "#3aa6a6", "#d26ca3",
];

pub fn default_eq() -> Vec<EqBand> {
    vec![
        EqBand { band_type: EqBandType::LowShelf, frequency_hz: 100.0, gain_db: 0.0, q: 0.7 },
        EqBand { band_type: EqBandType::Peak, frequency_hz: 400.0, gain_db: 0.0, q: 1.0 },
        EqBand { band_type: EqBandType::Peak, frequency_hz: 2500.0, gain_db: 0.0, q: 1.0 },
        EqBand { band_type: EqBandType::HighShelf, frequency_hz: 8000.0, gain_db: 0.0, q: 0.7 },
    ]
}

pub fn default_compressor() -> CompressorState {
    CompressorState {
        enabled: false,
        threshold_db: -18.0,
        ratio: 2.0,
        attack_ms: 20.0,
        release_ms: 160.0,
        knee_db: 6.0,
        makeup_db: 0.0,
    }
}

pub fn default_chain() -> TrackChain {
    TrackChain {
        high_pass: FilterState { enabled: false, frequency_hz: 40.0, slope_db_oct: 12 },
        low_pass: FilterState { enabled: false, frequency_hz: 18000.0, slope_db_oct: 12 },
        eq: default_eq(),
        compressor: default_compressor(),
    }
}

pub fn default_master() -> MasterChannel {
    MasterChannel {
        gain_db: 0.0,
        limiter: LimiterState { enabled: true, ceiling_db: -1.0 },
    }
}

pub fn make_track(source_file_id: String, name: String, index: usize) -> Track {
    Track {
        id: Uuid::new_v4().to_string(),
        kind: TrackKind::Audio,
        name: name.clone(),
        role: infer_role(&name),
        source_file_id,
        start_sample: 0,
        gain_db: 0.0,
        pan: 0.0,
        muted: false,
        solo: false,
        ai_generated: false,
        input_latency_ms: 0,
        color: COLORS[index % COLORS.len()].to_string(),
        chain: default_chain(),
        sends: Sends { reverb_db: -60.0, delay_db: -60.0 },
        automation: Vec::new(),
        clips_materialized: false,
        clips: Vec::new(),
        video_clips: Vec::new(),
        camera_device_id: None,
        record_camera_audio: false,
    }
}

pub fn infer_role(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    if lower.contains("kick") {
        Some("kick".into())
    } else if lower.contains("snare") {
        Some("snare".into())
    } else if lower.contains("bass") {
        Some("bass".into())
    } else if lower.contains("vocal") || lower.contains("vox") || lower.contains("lead") {
        Some("lead_vocal".into())
    } else if lower.contains("guitar") || lower.contains("gtr") {
        Some("guitar".into())
    } else if lower.contains("drum") {
        Some("drums".into())
    } else if lower.contains("keys") || lower.contains("piano") || lower.contains("synth") {
        Some("keys".into())
    } else {
        None
    }
}
