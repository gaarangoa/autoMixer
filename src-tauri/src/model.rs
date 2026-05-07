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
    pub start_sample: u64,
    pub end_sample: u64,
    pub gain_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: Id,
    pub name: String,
    pub role: Option<String>,
    pub source_file_id: Id,
    pub start_sample: u64,
    pub gain_db: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub color: String,
    pub chain: TrackChain,
    pub sends: Sends,
    pub automation: Vec<AutomationLane>,
    pub clips: Vec<ClipRegion>,
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
pub struct MixSession {
    pub id: Id,
    pub name: String,
    pub sample_rate: u32,
    pub bpm: Option<f32>,
    pub source_files: Vec<SourceFile>,
    pub tracks: Vec<Track>,
    pub buses: Vec<Bus>,
    pub master: MasterChannel,
    pub regions: Vec<Region>,
    pub markers: Vec<Marker>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum MixAction {
    CreateRegion { name: String, start_sample: u64, end_sample: u64, track_ids: Option<Vec<Id>> },
    DeleteTrack { track_id: Id },
    SetTrackGain { track_id: Id, gain_db: f32 },
    AdjustTrackGain { track_id: Id, delta_db: f32 },
    SetTrackPan { track_id: Id, pan: f32 },
    MuteTrack { track_id: Id, muted: bool },
    SoloTrack { track_id: Id, solo: bool },
    SetHighPass { track_id: Id, frequency_hz: f32, slope_db_oct: u16 },
    SetLowPass { track_id: Id, frequency_hz: f32, slope_db_oct: u16 },
    SetEqBand { track_id: Id, band: usize, frequency_hz: f32, gain_db: f32, q: f32 },
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
    pub issues: Vec<CritiqueIssue>,
    #[serde(default)]
    pub strengths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixCritique {
    pub mix_score: f32,
    pub summary: String,
    pub headroom_db: f32,
    pub integrated_lufs_estimate: f32,
    pub true_peak_db_estimate: f32,
    pub mix_issues: Vec<CritiqueIssue>,
    pub per_track: Vec<TrackCritique>,
    #[serde(default)]
    pub recommended_next_steps: Vec<String>,
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
