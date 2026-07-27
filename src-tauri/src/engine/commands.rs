//! Small `Copy` command structs sent UI -> audio thread via lock-free queue.
//!
//! Track-level commands address tracks by an engine-side `slot` index.
//! The session store maintains the slot mapping; the audio thread never
//! looks at session JSON or strings.

#[derive(Debug, Clone, Copy)]
pub enum EngineCommand {
    SetMasterGainDb(f32),
    SetMasterCeilingDb(f32),
    SetTrackGainDb {
        slot: u32,
        db: f32,
    },
    SetTrackPan {
        slot: u32,
        pan: f32,
    },
    SetTrackMuted {
        slot: u32,
        muted: bool,
    },
    SetTrackSolo {
        slot: u32,
        solo: bool,
    },
    SetTrackActive {
        slot: u32,
        active: bool,
    },
    SetTrackHighPass {
        slot: u32,
        enabled: bool,
        frequency_hz: f32,
        slope_db_oct: u16,
    },
    SetTrackLowPass {
        slot: u32,
        enabled: bool,
        frequency_hz: f32,
        slope_db_oct: u16,
    },
    SetTrackEqBand {
        slot: u32,
        band: u8,
        frequency_hz: f32,
        gain_db: f32,
        q: f32,
    },
    SetTrackCompressor {
        slot: u32,
        enabled: bool,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        knee_db: f32,
        makeup_db: f32,
    },
    SetTrackReverbSendDb {
        slot: u32,
        db: f32,
    },
    SetTrackDelaySendDb {
        slot: u32,
        db: f32,
    },
    Play,
    Pause,
    Stop,
    Seek {
        sample: u64,
    },
    SetSessionRate {
        rate: u32,
    },
    SetMasterBypass {
        enabled: bool,
    },
    SetMetronome {
        enabled: bool,
        bpm: f32,
        numerator: u8,
        denominator: u8,
        volume_db: f32,
    },
}
