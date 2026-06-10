use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Id = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAnalysis {
    pub peak_db: f32,
    pub rms_db: f32,
    pub lufs_estimate: f32,
    pub spectral_centroid_hz: f32,
    pub low_energy: f32,
    pub mid_energy: f32,
    pub high_energy: f32,
    pub silence_percent: f32,
    pub dynamic_range_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    pub id: Id,
    pub original_name: String,
    pub cache_path: String,
    pub peak_path: String,
    pub duration_samples: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub analysis: TrackAnalysis,
    pub peak_preview: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSourceFile {
    pub id: Id,
    pub original_name: String,
    pub path: String,
    pub mime_type: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqBandType {
    LowShelf,
    Peak,
    HighShelf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EqBand {
    #[serde(rename = "type")]
    pub band_type: EqBandType,
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterState {
    pub enabled: bool,
    pub frequency_hz: f32,
    pub slope_db_oct: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressorState {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackChain {
    pub high_pass: FilterState,
    pub low_pass: FilterState,
    pub eq: Vec<EqBand>,
    pub compressor: CompressorState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sends {
    pub reverb_db: f32,
    pub delay_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutomatableParam {
    GainDb,
    Pan,
    #[serde(rename = "highPass.frequencyHz")]
    HighPassFrequencyHz,
    #[serde(rename = "lowPass.frequencyHz")]
    LowPassFrequencyHz,
    #[serde(rename = "sends.reverbDb")]
    SendsReverbDb,
    #[serde(rename = "sends.delayDb")]
    SendsDelayDb,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPoint {
    pub sample: u64,
    pub value: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CurveType {
    Linear,
    Exponential,
    Hold,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationLane {
    pub id: Id,
    pub param: AutomatableParam,
    pub region_id: Option<Id>,
    pub points: Vec<AutomationPoint>,
    pub curve: CurveType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipRegion {
    pub id: Id,
    #[serde(default)]
    pub source_file_id: Option<Id>,
    #[serde(default)]
    pub name: Option<String>,
    pub start_sample: u64,
    pub end_sample: u64,
    #[serde(default)]
    pub source_offset_sample: u64,
    pub gain_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoClipRegion {
    pub id: Id,
    pub video_source_file_id: Id,
    #[serde(default)]
    pub name: Option<String>,
    pub start_sample: u64,
    pub end_sample: u64,
    #[serde(default)]
    pub source_offset_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<VideoLayout>,
    // Backup of the source-id, offset and duration BEFORE the first effects render.
    // Set on the first call to replace_track_video and never overwritten afterwards.
    // Lets `revert_clip_video` restore the original recording without any grade/effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pristine_video_source_file_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pristine_source_offset_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pristine_duration_samples: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoLayout {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub crop_top: f32,
    pub crop_right: f32,
    pub crop_bottom: f32,
    pub crop_left: f32,
    pub opacity: f32,
    pub rotation: f32,
    pub z_index: i32,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub blur: f32,
    pub preset: VideoFilterPreset,
}

impl Default for VideoLayout {
    /// A full-frame layer: fills the canvas with no crop or color adjustments.
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            crop_top: 0.0,
            crop_right: 0.0,
            crop_bottom: 0.0,
            crop_left: 0.0,
            opacity: 1.0,
            rotation: 0.0,
            z_index: 0,
            brightness: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            blur: 0.0,
            preset: VideoFilterPreset::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoCanvas {
    pub width: u32,
    pub height: u32,
    pub background: String,
}

impl Default for VideoCanvas {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            background: "#000000".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoFilterPreset {
    None,
    Warm,
    Cool,
    Mono,
    Punch,
    Dream,
    /// Cinematic teal-orange grade.
    Cinema,
    /// High-contrast black & white.
    Noir,
    /// Dark, contrasty, slightly cool — "sad / moody".
    Moody,
    /// Sepia-leaning vintage / film look.
    Vintage,
    /// Warm golden-hour push.
    Golden,
    /// Strong cool blue tint.
    Cold,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    Audio,
    Video,
}

fn default_track_kind() -> TrackKind {
    TrackKind::Audio
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: Id,
    #[serde(default = "default_track_kind")]
    pub kind: TrackKind,
    pub name: String,
    pub role: Option<String>,
    pub source_file_id: Id,
    pub start_sample: u64,
    pub gain_db: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    #[serde(default)]
    pub ai_generated: bool,
    /// Recording latency compensation in milliseconds. When a recording is finalised on
    /// this track, the placed clip's start is shifted earlier by this many ms so what was
    /// recorded lines up with what was playing. Default 0.
    #[serde(default)]
    pub input_latency_ms: i32,
    pub color: String,
    pub chain: TrackChain,
    pub sends: Sends,
    pub automation: Vec<AutomationLane>,
    pub clips: Vec<ClipRegion>,
    #[serde(default)]
    pub video_clips: Vec<VideoClipRegion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_device_id: Option<String>,
    #[serde(default)]
    pub record_camera_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub id: Id,
    pub name: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub track_ids: Option<Vec<Id>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub id: Id,
    pub name: String,
    pub sample: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimiterState {
    pub enabled: bool,
    pub ceiling_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterChannel {
    pub gain_db: f32,
    pub limiter: LimiterState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bus {
    pub id: Id,
    pub name: String,
    pub gain_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerProfile {
    /// Free-form id for known presets; "custom" if user edited fields directly.
    pub preset_id: String,
    /// Subtle (≤1 dB moves preferred) / moderate / bold.
    pub aggressiveness: String,
    /// corrective_only / tonal_shaping / sculpting
    pub eq_philosophy: String,
    /// transparent_glue / character / aggressive
    pub compression_philosophy: String,
    /// narrow / natural / wide
    pub stereo_treatment: String,
    /// dry / tasteful / lush
    pub space: String,
    /// broadcast / streaming / loud — drives target LUFS.
    pub loudness_target: String,
    /// Optional genre tag (rock / electronic / acoustic / hip-hop / pop / cinematic / …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// Optional reference engineer flavor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_engineer: Option<String>,
    /// Free-form user notes appended to the preamble.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_notes: Option<String>,
}

impl Default for MixerProfile {
    fn default() -> Self {
        Self {
            preset_id: "balanced".into(),
            aggressiveness: "moderate".into(),
            eq_philosophy: "tonal_shaping".into(),
            compression_philosophy: "transparent_glue".into(),
            stereo_treatment: "natural".into(),
            space: "tasteful".into(),
            loudness_target: "streaming".into(),
            genre: None,
            reference_engineer: None,
            custom_notes: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixSession {
    pub id: Id,
    pub name: String,
    pub sample_rate: u32,
    pub bpm: Option<f32>,
    pub source_files: Vec<SourceFile>,
    #[serde(default)]
    pub video_source_files: Vec<VideoSourceFile>,
    pub tracks: Vec<Track>,
    pub buses: Vec<Bus>,
    pub master: MasterChannel,
    pub regions: Vec<Region>,
    pub markers: Vec<Marker>,
    #[serde(default)]
    pub sections: Vec<MixSection>,
    #[serde(default)]
    pub mixer_profile: MixerProfile,
    #[serde(default)]
    pub video_canvas: VideoCanvas,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixSection {
    pub start: f32,
    pub end: f32,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<SectionAnalysis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionAnalysis {
    pub peak_db: f32,
    pub rms_db: f32,
    pub lufs: f32,
    pub spectral_centroid_hz: f32,
    pub low_energy: f32,
    pub mid_energy: f32,
    pub high_energy: f32,
    pub dynamic_range_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonPatchOp {
    pub op: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistorySource {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: Id,
    pub timestamp: i64,
    pub source: HistorySource,
    pub explanation: Option<String>,
    pub forward_patch: Vec<JsonPatchOp>,
    pub inverse_patch: Vec<JsonPatchOp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixProject {
    pub session: MixSession,
    pub history: Vec<HistoryEntry>,
    pub redo_stack: Vec<HistoryEntry>,
    #[serde(default)]
    pub chat_messages: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum MixAction {
    CreateRegion { name: String, start_sample: u64, end_sample: u64, track_ids: Option<Vec<Id>> },
    DeleteTrack { track_id: Id },
    RenameTrack { track_id: Id, name: String },
    SetTrackRole { track_id: Id, role: Option<String> },
    SetTrackGain { track_id: Id, gain_db: f32 },
    AdjustTrackGain { track_id: Id, delta_db: f32 },
    SetTrackPan { track_id: Id, pan: f32 },
    MuteTrack { track_id: Id, muted: bool },
    SoloTrack { track_id: Id, solo: bool },
    SetTrackAiGenerated { track_id: Id, ai_generated: bool },
    SetHighPass {
        track_id: Id,
        #[serde(
            alias = "frequency",
            alias = "frequency_hz",
            alias = "frequencyHZ",
            alias = "freqHz",
            alias = "freq_hz",
            alias = "freq",
            alias = "hz"
        )]
        frequency_hz: f32,
        #[serde(alias = "slope", alias = "slope_db_oct", alias = "slopeDbPerOctave", alias = "slopeDbOctave")]
        slope_db_oct: u16,
    },
    SetLowPass {
        track_id: Id,
        #[serde(
            alias = "frequency",
            alias = "frequency_hz",
            alias = "frequencyHZ",
            alias = "freqHz",
            alias = "freq_hz",
            alias = "freq",
            alias = "hz"
        )]
        frequency_hz: f32,
        #[serde(alias = "slope", alias = "slope_db_oct", alias = "slopeDbPerOctave", alias = "slopeDbOctave")]
        slope_db_oct: u16,
    },
    SetEqBand {
        track_id: Id,
        band: usize,
        #[serde(
            alias = "frequency",
            alias = "frequency_hz",
            alias = "frequencyHZ",
            alias = "freqHz",
            alias = "freq_hz",
            alias = "freq",
            alias = "hz"
        )]
        frequency_hz: f32,
        #[serde(alias = "gain", alias = "gain_db", alias = "db")]
        gain_db: f32,
        q: f32,
    },
    SetCompressor {
        track_id: Id,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        knee_db: f32,
        makeup_db: f32,
    },
    SetReverbSend { track_id: Id, level_db: f32 },
    SetDelaySend { track_id: Id, level_db: f32 },
    SetMasterGain { gain_db: f32 },
    AdjustMasterGain { delta_db: f32 },
    SetProcessorParam { target_id: Id, processor_id: String, param_id: String, value: f32 },
    SetRegionGain { region_id: Id, track_id: Id, gain_db: f32 },
    ApplySectionAutomation { region_id: Id, track_id: Id, param: AutomatableParam, value: f32 },
    Undo,
    Redo,
    RenderMix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCard {
    pub skill_id: String,
    pub display_name: String,
    pub when_to_use: String,
    pub musical_intents: Vec<String>,
    pub summary_actions: Vec<String>,
    pub required_context: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalog {
    pub skills: Vec<SkillCard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRequest {
    pub session_id: Id,
    pub user_text: String,
    pub selected_track_ids: Vec<Id>,
    pub selected_region_ids: Vec<Id>,
    pub ollama_base_url: Option<String>,
    pub ollama_model: Option<String>,
    #[serde(default)]
    pub recent_critique: Option<MixCritique>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CritiqueSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CritiqueIssue {
    pub category: String,
    pub severity: CritiqueSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackCritique {
    pub track_id: Id,
    pub track_name: String,
    pub rating: f32,
    #[serde(deserialize_with = "deserialize_issues")]
    pub issues: Vec<CritiqueIssue>,
    #[serde(default)]
    pub strengths: Vec<String>,
}

/// Accept either `[{category, severity, message, ...}]` or a shorthand
/// `["text", ...]` from the model — the latter gets wrapped as a generic
/// medium-severity issue so the critique survives.
fn deserialize_issues<'de, D>(de: D) -> Result<Vec<CritiqueIssue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor};
    use std::fmt;

    struct IssuesVisitor;

    impl<'de> Visitor<'de> for IssuesVisitor {
        type Value = Vec<CritiqueIssue>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a list of CritiqueIssue objects or strings")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut out = Vec::new();
            while let Some(value) = seq.next_element::<Value>()? {
                match value {
                    Value::String(s) => out.push(CritiqueIssue {
                        category: "general".into(),
                        severity: CritiqueSeverity::Medium,
                        message: s,
                        suggested_skills: None,
                    }),
                    other => {
                        let issue: CritiqueIssue =
                            serde_json::from_value(other).map_err(de::Error::custom)?;
                        out.push(issue);
                    }
                }
            }
            Ok(out)
        }
    }

    de.deserialize_seq(IssuesVisitor)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixCritique {
    pub mix_score: f32,
    pub summary: String,
    pub headroom_db: f32,
    pub integrated_lufs_estimate: f32,
    pub true_peak_db_estimate: f32,
    #[serde(deserialize_with = "deserialize_issues")]
    pub mix_issues: Vec<CritiqueIssue>,
    pub per_track: Vec<TrackCritique>,
    #[serde(default)]
    pub recommended_next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokens {
    pub prompt: u32,
    pub response: u32,
    pub elapsed_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum AssistantResponse {
    #[serde(rename_all = "camelCase")]
    Ok {
        explanation: String,
        actions: Vec<MixAction>,
        warnings: Vec<String>,
        selected_skills: Vec<String>,
        session: MixSession,
        history: Vec<HistoryEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rationale: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        per_action_notes: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<TurnTokens>,
    },
    #[serde(rename_all = "camelCase")]
    Clarification {
        question: String,
        reason: String,
    },
    #[serde(rename_all = "camelCase")]
    Critique {
        critique: MixCritique,
        selected_skills: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Err {
        kind: String,
        message: String,
        raw_model_output: Option<String>,
    },
}
