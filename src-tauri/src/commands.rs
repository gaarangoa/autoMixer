use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::{
    ab_judge::{AbJudgeOptions, AbJudgeResponse},
    actions::{apply_actions, record_patch, redo, undo},
    assistant,
    audio,
    engine::commands::EngineCommand,
    model::{AssistantRequest, AssistantResponse, HistorySource, JsonPatchOp, MixAction, MixProject, MixSection, MixSession, MixerProfile, SectionAnalysis, SkillCatalog, VideoFilterPreset, VideoLayout},
    store::SessionStore,
    AppState,
};
use serde::Deserialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConfig {
    ollama_base_url: String,
    ollama_model: String,
}

#[derive(Serialize)]
pub struct ModelsResponse {
    models: Vec<String>,
    /// Human-readable label of the detected server protocol ("Ollama" or
    /// "OpenAI-compatible (vLLM / llama.cpp)").
    provider: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDevicesResponse {
    devices: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingMetersResponse {
    peaks: Vec<f32>,
    /// Most recent per-channel peaks, used to show a live L/R meter in the inspector.
    /// Empty when no meter has been received yet.
    channel_peaks: Vec<f32>,
}

#[derive(Serialize)]
pub struct RenderResponse {
    path: String,
}

/// Response from the single-clip direct-edit path. The frontend receives the
/// updated project (so the swapped-in clip shows up) and a description of what
/// the agent actually applied, for the chat summary.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyClipEffectsResponse {
    project: MixProject,
    look_preset: Option<crate::model::VideoFilterPreset>,
    color_grade: Option<AgentColorGrade>,
    video_effects: Option<AgentVideoEffects>,
    // Short label of what the source for each field was: "llm", "keywords", or "none".
    // Lets the UI tell the user how much the vision model contributed.
    source_summary: String,
}

/// Per-clip effects JSON returned by the single-shot vision call. All fields
/// optional — the LLM can leave them out and we fall back to keyword detection.
#[derive(Deserialize)]
struct ClipEffectsChoice {
    look_preset: Option<String>,
    color_grade: Option<AgentColorGrade>,
    video_effects: Option<AgentVideoEffects>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentVideoEditResponse {
    pub path: String,
    pub script: Vec<AgentVideoScriptEntry>,
    // Color-look preset the agent inferred from the user's instructions (e.g. "cinema",
    // "warm", "moody"), so the frontend can sync its Look chip and reuse it on re-renders.
    // Serialized as `lookPreset` (camelCase) for the TS client. None = no preset applied.
    pub look_preset: Option<crate::model::VideoFilterPreset>,
    // Free-form color grade derived from the user's instructions. Used as the actual
    // render filter when present; otherwise the named look_preset (or nothing) is used.
    pub color_grade: Option<AgentColorGrade>,
    // Whole-edit effects (fade in/out, speed) applied after the cuts + grade.
    pub video_effects: Option<AgentVideoEffects>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AgentVideoScriptCandidate {
    image_number: usize,
    track_index: usize,
    track_name: String,
    timeline_seconds: f64,
    angle_label: Option<String>,
    note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AgentVideoScriptEntry {
    window_index: u32,
    total_windows: u32,
    start_seconds: f64,
    end_seconds: f64,
    decision: String,
    candidates: Vec<AgentVideoScriptCandidate>,
    chosen_track_index: Option<usize>,
    chosen_track_name: Option<String>,
    reason: String,
    data_provided: Vec<String>,
    model_choice: Option<usize>,
    variety_override: bool,
    source_offset_seconds: Option<f64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentAudioWindowFeatures {
    peak_db: f32,
    rms_db: f32,
    lufs_estimate: f32,
    loudness: String,
    transient_density: f32,
    transient_activity: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AgentVideoProgress {
    stage: String,
    message: String,
    current: u32,
    total: u32,
    elapsed_seconds: f32,
}

#[derive(Clone)]
struct VideoRenderClip {
    track_id: String,
    track_index: usize,
    track_name: String,
    path: PathBuf,
    start_sample: u64,
    end_sample: u64,
    source_offset_ms: u64,
    layout: VideoLayout,
}

struct AutoEditSegment {
    input_index: usize,
    timeline_start: u64,
    timeline_end: u64,
    source_offset_ms: u64,
}

struct RenderedAudioAnalysis {
    samples: Vec<f32>,
    channels: usize,
    sample_rate: u32,
    /// Timeline sample where frame 0 of `samples` begins. 0 for a full-session render,
    /// `range_start` for a range render — used by `audio_features_for_window` to look
    /// up the right frames without rendering the unused leading section.
    timeline_start_sample: u64,
}

const MAX_DYNAMIC_HOLD_WINDOWS: u32 = 4;
const MIN_WINDOWS_BEFORE_COVERAGE_CUT: u32 = 4;
const MIN_USAGE_GAP_FOR_COVERAGE_CUT: u32 = 2;

#[derive(Serialize)]
struct OllamaChatRequest {
    model: String,
    stream: bool,
    messages: Vec<OllamaChatMessage>,
}

#[derive(Serialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaChatResponseMessage,
}

#[derive(Deserialize)]
struct OllamaChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct AgentShotChoice {
    choice: usize,
    decision: Option<String>,
    reason: Option<String>,
    edit_intent: Option<String>,
    continuity_plan: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)] // kept alongside analyze_agent_window_frames for the old two-call path
struct AgentWindowFrameAnalysis {
    candidate_labels: Option<Vec<String>>,
    candidate_notes: Option<Vec<String>>,
    window_summary: Option<String>,
}

/// Combined describe-and-decide output. Used by the merged single-call agent
/// pipeline (one vision-model HTTP request per window instead of one vision +
/// one edit request). Backwards-compatible decoding: missing fields fall back
/// the same way the two-call path did.
#[derive(Deserialize)]
struct AgentMergedChoice {
    candidate_labels: Option<Vec<String>>,
    candidate_notes: Option<Vec<String>>,
    window_summary: Option<String>,
    choice: usize,
    decision: Option<String>,
    reason: Option<String>,
    edit_intent: Option<String>,
    continuity_plan: Option<String>,
    // Color-look preset the model picked from the user's instructions for THIS window.
    // Aggregated across windows (majority vote) to pick the final render look. One of:
    // none, warm, cool, mono, punch, dream, cinema, noir, moody, vintage, golden, cold.
    look_preset: Option<String>,
    // Free-form custom color grade. Lets the model express any look (not just the 12
    // presets) by emitting clamped numeric parameters. Backend validates each field and
    // builds a safe ffmpeg filter chain. We use the first non-empty grade across windows
    // — same user prompt drives them all, so no aggregation needed.
    color_grade: Option<AgentColorGrade>,
    // Whole-edit effects (fades, speed) requested by the user. Same first-non-empty-wins
    // aggregation across windows. Keyword pre-detection in Rust seeds a fallback so the
    // user always gets effects even when the vision model ignores this field.
    video_effects: Option<AgentVideoEffects>,
}

/// Free-form color-grade parameters the agent can emit per user instructions.
/// All fields optional; the backend clamps each one to a safe range before building
/// the ffmpeg filter chain. No raw filter strings are accepted from the model — only
/// numeric parameters — so there's no filter-injection risk.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentColorGrade {
    pub name: Option<String>,
    // Short rationale (1-2 sentences) explaining how the model mapped the user's words
    // to these parameters. Surfaced in the chat so the user can see and refine. Pure
    // text, no validation needed beyond the trim/empty filter.
    pub reason: Option<String>,
    pub brightness: Option<f32>,
    pub contrast: Option<f32>,
    pub saturation: Option<f32>,
    pub gamma: Option<f32>,
    pub rgb_mix: Option<RgbMix>,
    pub hue_shift: Option<f32>,
    pub vignette: Option<f32>,
    pub blur: Option<f32>,
    pub sharpen: Option<f32>,
    pub grain: Option<f32>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RgbMix {
    pub rr: Option<f32>,
    pub gg: Option<f32>,
    pub bb: Option<f32>,
}

/// Whole-edit video effects applied AFTER the cuts + color grade. Fades touch both
/// video and audio together so transitions feel correct. All fields optional; the
/// backend clamps each one before building the ffmpeg filter chain. Like the color
/// grade, no raw filter strings are accepted from the model.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentVideoEffects {
    // Free-text rationale (1-2 sentences). Shown in the chat under "Why:".
    pub reason: Option<String>,
    // Fade-in length in seconds. Applied to video and audio together starting at 0.
    pub fade_in_seconds: Option<f32>,
    // Fade-out length in seconds. Ends at the very end of the rendered clip.
    pub fade_out_seconds: Option<f32>,
    // Playback-rate multiplier (1.0 = normal, 0.5 = half speed, 2.0 = double).
    // Affects video PTS and audio tempo together so they stay in sync.
    pub speed_factor: Option<f32>,
}

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> UiConfig {
    UiConfig {
        ollama_base_url: state.config.ollama_base_url.clone(),
        ollama_model: state.config.ollama_model.clone(),
    }
}

#[tauri::command]
pub fn get_skill_catalog() -> SkillCatalog {
    crate::capabilities::skill_catalog()
}

#[tauri::command]
pub async fn list_ollama_models(base_url: String) -> Result<ModelsResponse, String> {
    let (provider, models) = assistant::list_models(base_url).await?;
    Ok(ModelsResponse { models, provider: provider.label().to_string() })
}

#[tauri::command]
pub fn list_input_devices() -> Result<InputDevicesResponse, String> {
    Ok(InputDevicesResponse { devices: crate::recorder::input_devices()? })
}

#[tauri::command]
pub fn list_input_device_channels(input_device: Option<String>) -> Result<u32, String> {
    Ok(crate::recorder::input_device_channel_count(input_device)?)
}

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>, album_id: String) -> Result<Vec<MixSession>, String> {
    state.store.lock().map_err(|error| error.to_string())?.list_sessions(&album_id)
}

#[tauri::command]
pub fn create_session(state: State<'_, AppState>, album_id: String, name: String) -> Result<MixProject, String> {
    state.store.lock().map_err(|error| error.to_string())?.create_session(&album_id, name)
}

// ---- Native menu (File submenu with album/song switching) ----

#[derive(serde::Deserialize)]
pub struct MenuEntry {
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub fn set_file_menu(
    app: AppHandle,
    albums: Vec<MenuEntry>,
    sessions: Vec<MenuEntry>,
    current_album_id: String,
    current_session_id: String,
) -> Result<(), String> {
    // Menu mutation must run on the main thread (macOS).
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = crate::build_and_set_menu(&handle, &albums, &sessions, &current_album_id, &current_session_id);
    })
    .map_err(|error| error.to_string())
}

// ---- Albums (projects) ----

#[tauri::command]
pub fn list_albums(state: State<'_, AppState>) -> Result<Vec<crate::model::MixAlbum>, String> {
    state.store.lock().map_err(|error| error.to_string())?.list_albums()
}

#[tauri::command]
pub fn create_album(state: State<'_, AppState>, name: String) -> Result<crate::model::MixAlbum, String> {
    state.store.lock().map_err(|error| error.to_string())?.create_album(name)
}

#[tauri::command]
pub fn get_album(state: State<'_, AppState>, album_id: String) -> Result<crate::model::MixAlbum, String> {
    state.store.lock().map_err(|error| error.to_string())?.get_album(&album_id)
}

#[tauri::command]
pub fn rename_album(state: State<'_, AppState>, album_id: String, name: String) -> Result<crate::model::MixAlbum, String> {
    state.store.lock().map_err(|error| error.to_string())?.rename_album(&album_id, name)
}

#[tauri::command]
pub fn delete_album(state: State<'_, AppState>, album_id: String) -> Result<(), String> {
    state.store.lock().map_err(|error| error.to_string())?.delete_album(&album_id)
}

#[tauri::command]
pub fn get_project(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)
}

#[tauri::command]
pub fn import_audio_files(state: State<'_, AppState>, session_id: String, paths: Vec<String>) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut latest = None;
    for path in paths {
        latest = Some(store.add_source_file(&session_id, Path::new(&path))?);
    }
    latest.map_or_else(|| store.get_project(&session_id), Ok)
}

#[tauri::command]
pub fn create_recording_track(state: State<'_, AppState>, session_id: String, channels: Option<u16>) -> Result<MixProject, String> {
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .create_recording_track(&session_id, channels.unwrap_or(1))?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn create_video_track(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .create_video_track(&session_id)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

/// Swap the rendered video file behind an existing agent-edit track's clip in place.
/// Keeps the track id, clip id, layout and start position — only the underlying source
/// file (and clip length) changes.
#[tauri::command]
pub fn replace_rendered_video_track(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    clip_id: String,
    video_path: String,
    duration_ms: u64,
) -> Result<MixProject, String> {
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .replace_track_video(&session_id, &track_id, &clip_id, Path::new(&video_path), duration_ms.max(1))?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn add_rendered_video_track(
    state: State<'_, AppState>,
    session_id: String,
    video_path: String,
    name: String,
    start_sample: u64,
    duration_ms: u64,
) -> Result<MixProject, String> {
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .add_rendered_video_track(&session_id, Path::new(&video_path), name, start_sample, duration_ms)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

/// Upsert the single canonical agent-edit track (replace in place instead of
/// stacking a new copy every run) and re-sync the audio engine. Used by the
/// Hermes `edit_video` control path.
pub fn upsert_agent_video_track(
    state: &AppState,
    session_id: &str,
    video_path: &Path,
    start_sample: u64,
    duration_ms: u64,
) -> Result<MixProject, String> {
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .upsert_agent_video_track(session_id, video_path, start_sample, duration_ms)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

/// Hard-restart the whole app. Kept as a last-resort recovery path; the normal way
/// to stop an agent run is `cancel_agent`.
#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

/// Cancel the in-flight agent run (chat turn, auto-mix pipeline, or video agent edit)
/// without touching playback or the rest of the app. The pending command returns
/// promptly with `assistant::CANCELLED_MESSAGE`.
#[tauri::command]
pub fn cancel_agent() {
    assistant::cancel_agent_run();
}

/// Apply a batch of actions to a session and bring the live audio engine + saved
/// store into agreement, returning the updated project. This is the single mutation
/// path shared by the Tauri command (`apply_mix_actions`) and the in-process control
/// surface (`control.rs`) that the Hermes agent drives, so an external tool call is
/// indistinguishable from a UI edit at the store/engine level.
///
/// Locking order is always store-then-audio (never the reverse) to avoid deadlock.
pub fn apply_and_sync(
    state: &AppState,
    session_id: &str,
    actions: &[MixAction],
    source: HistorySource,
    explanation: Option<String>,
) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(session_id)?;
    apply_actions(&mut project, actions, source, explanation)?;
    store.save(&project)?;
    push_engine_commands(state, &project.session, actions);
    if let Ok(mut audio) = state.audio.lock() {
        // Defensive re-sync: makes sure the engine's per-slot gain/mute/pan/etc. mirror the
        // saved project regardless of which actions were in the batch (e.g. a `rename_track`
        // has no engine command of its own; without this, the slot's controls drift if a
        // previous batch was missed).
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

/// Step backward in history and re-sync the engine. Shared by command + control surface.
pub fn undo_and_sync(state: &AppState, session_id: &str) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(session_id)?;
    undo(&mut project)?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

/// Step forward in history and re-sync the engine. Shared by command + control surface.
pub fn redo_and_sync(state: &AppState, session_id: &str) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(session_id)?;
    redo(&mut project)?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn apply_mix_actions(
    state: State<'_, AppState>,
    session_id: String,
    actions: Vec<MixAction>,
    explanation: Option<String>,
) -> Result<MixProject, String> {
    apply_and_sync(state.inner(), &session_id, &actions, HistorySource::User, explanation)
}

#[tauri::command]
pub fn undo_mix_action(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    undo_and_sync(state.inner(), &session_id)
}

#[tauri::command]
pub fn redo_mix_action(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    redo_and_sync(state.inner(), &session_id)
}

#[tauri::command]
pub fn reset_session(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    project.session.tracks.clear();
    project.session.source_files.clear();
    project.session.regions.clear();
    project.session.markers.clear();
    project.history.clear();
    project.redo_stack.clear();
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn apply_recorded_patch(
    state: State<'_, AppState>,
    session_id: String,
    forward_patch: Vec<JsonPatchOp>,
    inverse_patch: Vec<JsonPatchOp>,
    explanation: Option<String>,
) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    record_patch(&mut project, forward_patch, inverse_patch, HistorySource::User, explanation)?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

struct TauriLlmObserver {
    app: tauri::AppHandle,
}

impl assistant::LlmObserver for TauriLlmObserver {
    fn chunk(&self, phase: &str, text: &str) {
        let _ = self.app.emit(
            "llm:chunk",
            serde_json::json!({ "phase": phase, "text": text }),
        );
    }
    fn stats(&self, phase: &str, stats: &assistant::LlmCallStats) {
        let _ = self.app.emit(
            "llm:stats",
            serde_json::json!({
                "phase": phase,
                "promptTokens": stats.prompt_tokens,
                "responseTokens": stats.response_tokens,
                "elapsedMs": stats.elapsed_ms,
            }),
        );
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesModel {
    pub base_url: String,
    pub model: String,
    pub provider: String,
}

fn hermes_bin() -> PathBuf {
    if let Ok(p) = std::env::var("AUTOMIXER_HERMES_BIN") {
        return PathBuf::from(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(&home).join(".local/bin/hermes");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("hermes")
}

/// Parse the Hermes agent's orchestration model (base URL, model, provider) out of
/// `~/.hermes/config.yaml`. This is the single source of truth for the text model —
/// the chat agent and the auto-mix pipeline both use it, so there's only ever one.
fn read_hermes_model() -> HermesModel {
    let (mut base_url, mut model, mut provider) = (String::new(), String::new(), String::new());
    // Read AutoMixer's DEDICATED Hermes home (isolated config), not the shared ~/.hermes.
    {
        let path = crate::hermes_service::automixer_hermes_home().join("config.yaml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut in_model = false;
            for line in text.lines() {
                if line.starts_with("model:") {
                    in_model = true;
                    continue;
                }
                if in_model {
                    if !line.starts_with(' ') && !line.trim().is_empty() {
                        break;
                    }
                    let trimmed = line.trim();
                    if let Some(v) = trimmed.strip_prefix("base_url:") {
                        base_url = v.trim().to_string();
                    } else if let Some(v) = trimmed.strip_prefix("default:") {
                        model = v.trim().to_string();
                    } else if let Some(v) = trimmed.strip_prefix("provider:") {
                        provider = v.trim().to_string();
                    }
                }
            }
        }
    }
    HermesModel { base_url, model, provider }
}

/// Read the Hermes agent's current orchestration model so the settings UI can display it.
#[tauri::command]
pub fn get_hermes_model() -> Result<HermesModel, String> {
    Ok(read_hermes_model())
}

/// Clear the chat: forget the agent's conversation for this session so the next turn
/// starts fresh (no stale context carrying over, e.g. a previously-applied look).
#[tauri::command]
pub async fn clear_chat(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.hermes_service.reset_session(&session_id).await
}

/// Warm up the models at app startup so the first agent turn / video edit is fast:
/// pre-spawn the agent's tool server, and prime the video model's vision encoder with a
/// tiny throwaway frame (the "first window is slow" cost). All best-effort — failures
/// (model not up yet, text-only endpoint) are ignored.
pub async fn warm_up_models(
    video_base: String,
    video_model: String,
    hermes: std::sync::Arc<crate::hermes_service::HermesService>,
) {
    // Give the sidecar + model server a moment to come up.
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    if hermes.wait_ready(std::time::Duration::from_secs(20)).await {
        let _ = hermes.warmup().await;
    }
    let base = video_base.trim_end_matches('/').to_string();
    if !base.is_empty() && !video_model.trim().is_empty() {
        if let Ok(frame) = warmup_frame_b64() {
            let _ = call_ollama_chat(&base, &video_model, "Reply with the single word: ok.".to_string(), Some(vec![frame])).await;
        }
    }
}

/// A tiny 64×64 JPEG to prime the vision encoder. Returns base64 (no data URL prefix).
fn warmup_frame_b64() -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("automixer-warmup-{}.jpg", uuid::Uuid::new_v4()));
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=gray:s=64x64", "-frames:v", "1"])
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("ffmpeg warmup frame failed".into());
    }
    let bytes = fs::read(&path).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&path);
    Ok(BASE64_STANDARD.encode(bytes))
}

/// Point the Hermes agent at any OpenAI-compatible endpoint. We use the bare
/// `custom` provider (which needs no registry entry, unlike `custom:<name>`),
/// write the base URL + model, then restart the sidecar so `hermes acp` reloads
/// its config.
#[tauri::command]
pub async fn set_hermes_model(
    state: State<'_, AppState>,
    base_url: String,
    model: String,
) -> Result<(), String> {
    let hermes = hermes_bin();
    // Write to AutoMixer's DEDICATED Hermes home (HERMES_HOME), so model changes land in
    // the isolated config and never touch the shared ~/.hermes / desktop app.
    let home = crate::hermes_service::bootstrap_hermes_home();
    let set = |key: &str, value: &str| -> Result<(), String> {
        let output = Command::new(&hermes)
            .env("HERMES_HOME", &home)
            .arg("config")
            .arg("set")
            .arg(key)
            .arg(value)
            .output()
            .map_err(|e| format!("could not run hermes config set: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    };
    set("model.provider", "custom")?;
    set("model.base_url", base_url.trim())?;
    set("model.default", model.trim())?;
    // Relaunch the sidecar so the new model takes effect, then wait for it.
    state.hermes_service.restart();
    state
        .hermes_service
        .wait_ready(std::time::Duration::from_secs(30))
        .await;
    // Re-warm the new model in the background (the restart dropped the prior warm-up),
    // so the user's first turn on the freshly-set model reuses a cached system prompt
    // instead of paying the ~40s cold prefill.
    let hermes_for_warm = state.hermes_service.clone();
    tauri::async_runtime::spawn(async move {
        let _ = hermes_for_warm.warmup().await;
    });
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEndpoint {
    pub base_url: String,
    pub model: String,
}

/// Push the user's current track selection so the video-edit skill defaults to
/// only the selected video tracks (and the monitor can mirror it).
#[tauri::command]
pub fn set_video_selection(session_id: String, track_ids: Vec<String>) {
    crate::control::set_selection(&session_id, track_ids);
}

/// Read the stored selection — the monitor reads this on open so it's correct even
/// if it missed the live selection event.
#[tauri::command]
pub fn get_video_selection(session_id: String) -> Vec<String> {
    crate::control::get_selection(&session_id)
}

/// Read the video/vision VLM endpoint the video-edit skill uses.
#[tauri::command]
pub fn get_video_model() -> Result<ModelEndpoint, String> {
    let config = crate::config::Config::load();
    Ok(ModelEndpoint {
        base_url: config.video_base_url,
        model: config.video_model,
    })
}

/// Point the video-edit skill at any OpenAI-compatible / Ollama vision endpoint
/// (e.g. Qwen3-VL on the DGX Spark). Persisted so the skill reads it per request.
#[tauri::command]
pub fn set_video_model(base_url: String, model: String) -> Result<(), String> {
    let mut config = crate::config::Config::load();
    config.video_base_url = base_url.trim().to_string();
    config.video_model = model.trim().to_string();
    config.save()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoMixSummary {
    pub stages_run: usize,
    pub total_actions: usize,
    pub stages: Vec<serde_json::Value>,
}

/// Run the auto-mix pipeline to completion (awaitable), emitting the same
/// `auto-mix:*` progress events the UI already renders, and returning a summary.
/// Unlike the fire-and-forget `start_auto_mix` command, this is what the Hermes
/// "auto_mix" skill calls so the agent can report the outcome.
pub async fn run_auto_mix_blocking(
    app: &tauri::AppHandle,
    session_id: &str,
    stage_ids: Vec<String>,
) -> Result<AutoMixSummary, String> {
    use crate::auto_mix::AutoMixStage;
    let all = [
        AutoMixStage::RawSessionPrep,
        AutoMixStage::PrepIntent,
        AutoMixStage::StaticBalance,
        AutoMixStage::CleanupFilters,
        AutoMixStage::SubtractiveEq,
        AutoMixStage::Dynamics,
        AutoMixStage::TonalEnhancement,
        AutoMixStage::DepthSpace,
        AutoMixStage::SectionAutomation,
        AutoMixStage::MixBusLoudness,
    ];
    let stages: Vec<AutoMixStage> = if stage_ids.is_empty() {
        all.to_vec()
    } else {
        all.into_iter().filter(|s| stage_ids.iter().any(|id| id == s.id())).collect()
    };
    if stages.is_empty() {
        return Err("No valid auto-mix stages selected.".into());
    }

    assistant::reset_agent_cancel();
    let mut config = crate::config::Config::load();
    // Use the SAME model as the chat agent (one single text model), not a separate
    // gpt-oss/ollama setting. Falls back to config.ollama_* only if Hermes has none.
    let agent = read_hermes_model();
    if !agent.base_url.is_empty() {
        config.ollama_base_url = agent.base_url;
    }
    if !agent.model.is_empty() {
        config.ollama_model = agent.model;
    }
    let store = std::sync::Arc::new(std::sync::Mutex::new(SessionStore::new(config.data_dir.clone())));
    let observer: std::sync::Arc<dyn assistant::LlmObserver> = std::sync::Arc::new(assistant::NoopObserver);

    let _ = app.emit(
        "auto-mix:start",
        serde_json::json!({ "stages": stages.iter().map(|s| s.id()).collect::<Vec<_>>() }),
    );
    let mut total_actions = 0usize;
    let mut summaries = Vec::new();
    for (i, stage) in stages.iter().enumerate() {
        if assistant::agent_cancelled() {
            break;
        }
        let _ = app.emit(
            "auto-mix:stage-start",
            serde_json::json!({ "index": i, "stageId": stage.id(), "displayName": stage.display_name() }),
        );
        let report = crate::auto_mix::run_stage(&config, store.clone(), session_id, *stage, observer.clone()).await?;
        let action_count = report.action_count;
        let status = report.status.clone();
        let stage_id = report.stage_id.clone();
        let _ = app.emit("auto-mix:stage-done", serde_json::json!(report));
        let _ = sync_audio_from_app(app, session_id);
        total_actions += action_count;
        summaries.push(serde_json::json!({ "stageId": stage_id, "status": status, "actionCount": action_count }));
        if status == "error" || status == "cancelled" {
            break;
        }
    }
    Ok(AutoMixSummary { stages_run: summaries.len(), total_actions, stages: summaries })
}

#[derive(Deserialize)]
struct CropChoice {
    #[serde(default, alias = "cropTop")]
    crop_top: Option<f32>,
    #[serde(default, alias = "cropRight")]
    crop_right: Option<f32>,
    #[serde(default, alias = "cropBottom")]
    crop_bottom: Option<f32>,
    #[serde(default, alias = "cropLeft")]
    crop_left: Option<f32>,
}

/// Vision auto-crop: extract a frame from the clip, ask the configured video model
/// for edge-crop percentages that satisfy `instructions`, and write them to the
/// clip's layout. Shared by the `auto_crop` control endpoint / agent tool.
pub async fn auto_crop_clip(
    app: &tauri::AppHandle,
    session_id: &str,
    track_id: &str,
    clip_id: &str,
    instructions: &str,
) -> Result<MixProject, String> {
    use tauri::Manager;
    assistant::reset_agent_cancel();
    let config = crate::config::Config::load();

    let mut project = { app.state::<AppState>().store.lock().map_err(|e| e.to_string())?.get_project(session_id)? };

    // Locate the clip's source video + a representative timestamp (its midpoint).
    let (source_path, frame_time) = {
        let track = project.session.tracks.iter().find(|t| t.id == track_id).ok_or("Track not found")?;
        if !matches!(track.kind, crate::model::TrackKind::Video) {
            return Err("That track is not a video track.".into());
        }
        let clip = track.video_clips.iter().find(|c| c.id == clip_id).ok_or("Clip not found on the track")?;
        let source = project
            .session
            .video_source_files
            .iter()
            .find(|s| s.id == clip.video_source_file_id)
            .ok_or("The clip's source video file is missing")?;
        let offset_s = clip.source_offset_ms as f64 / 1000.0;
        let dur_s = clip.end_sample.saturating_sub(clip.start_sample) as f64 / project.session.sample_rate as f64;
        (PathBuf::from(&source.path), offset_s + dur_s.max(0.0) / 2.0)
    };
    if !source_path.exists() {
        return Err("The clip's source video file is no longer on disk.".into());
    }

    let temp_dir = config.data_dir.join("auto-crop").join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).map_err(|e| format!("Could not prepare temp dir: {e}"))?;
    let frame_path = temp_dir.join("frame.jpg");
    let _ = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .arg("-ss")
        .arg(format!("{frame_time:.3}"))
        .arg("-i")
        .arg(&source_path)
        .args(["-frames:v", "1", "-q:v", "6", "-vf", "scale=768:768:force_original_aspect_ratio=decrease"])
        .arg(&frame_path)
        .status();

    let frame_read = fs::read(&frame_path);
    let _ = fs::remove_dir_all(&temp_dir);
    let bytes = frame_read.map_err(|e| format!("Could not extract a frame at {frame_time:.1}s: {e}"))?;
    let prompt = format!(
        "You are reframing a video. Look at this frame and decide how to crop it so: {instructions}. \
         Reply with ONLY a JSON object giving the percentage to REMOVE from each edge (each 0-45): \
         {{\"cropTop\":0,\"cropRight\":0,\"cropBottom\":0,\"cropLeft\":0}}. \
         Crop conservatively and keep the main subject fully in frame."
    );
    let base_url = config.video_base_url.trim_end_matches('/').to_string();
    let resp = call_ollama_chat(&base_url, &config.video_model, prompt, Some(vec![BASE64_STANDARD.encode(bytes)]))
        .await
        .map_err(|e| format!("Video model call failed ({base_url} / {}): {e}", config.video_model))?;
    let text = resp.message.content;
    let json = crate::assistant::extract_json_object(&text).unwrap_or_else(|| text.clone());
    let crop: CropChoice = serde_json::from_str(&json)
        .map_err(|e| format!("Video model returned an unparseable crop ({e}). Raw: {}", text.chars().take(180).collect::<String>()))?;

    // Apply as a reversible history entry so the auto-crop can be undone (⌘Z).
    let ti = project.session.tracks.iter().position(|t| t.id == track_id).ok_or("Track not found")?;
    let ci = project.session.tracks[ti].video_clips.iter().position(|c| c.id == clip_id).ok_or("Clip not found")?;
    let old_layout = project.session.tracks[ti].video_clips[ci].layout.clone().unwrap_or_default();
    let mut layout = old_layout.clone();
    if let Some(v) = crop.crop_top { layout.crop_top = v; }
    if let Some(v) = crop.crop_right { layout.crop_right = v; }
    if let Some(v) = crop.crop_bottom { layout.crop_bottom = v; }
    if let Some(v) = crop.crop_left { layout.crop_left = v; }
    let new_layout = normalized_video_layout(&layout);
    let path = format!("/tracks/{ti}/videoClips/{ci}/layout");
    let forward = vec![crate::model::JsonPatchOp { op: "replace".into(), path: path.clone(), value: Some(serde_json::to_value(&new_layout).map_err(|e| e.to_string())?) }];
    let inverse = vec![crate::model::JsonPatchOp { op: "replace".into(), path, value: Some(serde_json::to_value(&old_layout).map_err(|e| e.to_string())?) }];
    crate::actions::record_patch(&mut project, forward, inverse, crate::model::HistorySource::Assistant, Some("Auto-crop".to_string()))?;
    app.state::<AppState>().store.lock().map_err(|e| e.to_string())?.save(&project)?;
    Ok(project)
}

/// Apply fade-in / fade-out / playback-speed to an existing video clip by re-encoding
/// its rendered video with the ffmpeg fade/setpts filter and swapping the clip's source
/// in place. Fast (one ffmpeg pass) — so a simple "fade in 2s, fade out 10s" doesn't
/// need a full multicam re-edit. Re-encodes from the PRISTINE source when one exists, so
/// changing the fade values re-renders from the original instead of stacking effects.
/// Shared by the `apply_video_effects` control endpoint / agent tool.
pub fn apply_video_effects(
    app: &tauri::AppHandle,
    session_id: &str,
    track_id: &str,
    clip_id: &str,
    fade_in_seconds: Option<f32>,
    fade_out_seconds: Option<f32>,
    speed_factor: Option<f32>,
) -> Result<MixProject, String> {
    use tauri::Manager;
    let state = app.state::<AppState>();
    let project = state.store.lock().map_err(|e| e.to_string())?.get_project(session_id)?;

    // Resolve the clip's source video (pristine when available) + its region length.
    let (source_path, region_seconds) = {
        let track = project.session.tracks.iter().find(|t| t.id == track_id).ok_or("Track not found")?;
        if !matches!(track.kind, crate::model::TrackKind::Video) {
            return Err("That track is not a video track.".into());
        }
        let clip = track.video_clips.iter().find(|c| c.id == clip_id).ok_or("Clip not found on the track")?;
        let source_id = clip.pristine_video_source_file_id.as_ref().unwrap_or(&clip.video_source_file_id);
        let source = project.session.video_source_files.iter().find(|s| &s.id == source_id)
            .ok_or("The clip's source video file is missing")?;
        let region = clip.end_sample.saturating_sub(clip.start_sample) as f64 / project.session.sample_rate as f64;
        (PathBuf::from(&source.path), region)
    };
    if !source_path.exists() {
        return Err("The clip's video file is missing on disk.".into());
    }

    let effects = AgentVideoEffects { reason: None, fade_in_seconds, fade_out_seconds, speed_factor };
    // Total duration for fade-out placement: probe the actual source, fall back to the
    // clip region length.
    let total_s = probe_video_duration(&source_path).unwrap_or(region_seconds).max(0.05);
    let (video_chain, audio_chain, _speed) = build_effects_filters(&effects, total_s);
    if video_chain.is_none() && audio_chain.is_none() {
        return Err("Nothing to apply — set a fade-in, fade-out, or speed.".into());
    }

    let renders_dir = state.store.lock().map_err(|e| e.to_string())?.renders_dir();
    fs::create_dir_all(&renders_dir).map_err(|e| e.to_string())?;
    let out_path = renders_dir.join(format!("fx-{}.mp4", uuid::Uuid::new_v4()));
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error").arg("-i").arg(&source_path);
    if let Some(v) = video_chain.as_ref() { cmd.arg("-vf").arg(v); }
    if let Some(a) = audio_chain.as_ref() { cmd.arg("-af").arg(a); }
    cmd.arg("-c:v").arg("libx264").arg("-preset").arg("medium").arg("-crf").arg("18")
        .arg("-pix_fmt").arg("yuv420p").arg("-movflags").arg("+faststart")
        .arg("-c:a").arg("aac").arg("-b:a").arg("256k");
    let output = cmd.arg(&out_path).output().map_err(|e| format!("Could not run ffmpeg: {e}"))?;
    if !output.status.success() {
        return Err(format!("ffmpeg effects render failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }

    let new_duration_ms = (probe_video_duration(&out_path).unwrap_or(total_s) * 1000.0).round() as u64;
    let project = state.store.lock().map_err(|e| e.to_string())?
        .replace_track_video(session_id, track_id, clip_id, &out_path, new_duration_ms)?;
    if let Ok(mut audio) = state.audio.lock() {
        let _ = audio.bind_session_sources(&project.session);
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    let _ = app.emit("session:externally-updated", serde_json::json!({ "sessionId": session_id, "project": project }));
    Ok(project)
}

/// Run a chat turn through the embedded Hermes agent sidecar.
///
/// The agent owns the loop (reasoning, tool execution, memory). Its tool calls
/// flow through the in-process control surface (`control.rs`), which mutates the
/// live session and refreshes the UI via `session:externally-updated` — so faders
/// move mid-turn. We stream the agent's tokens/thoughts/tool events onto the same
/// `llm:turn-start`/`llm:chunk`/`llm:turn-end` events the chat UI already renders.
#[tauri::command]
pub async fn assistant_request(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: AssistantRequest,
) -> Result<AssistantResponse, String> {
    assistant::reset_agent_cancel();
    let hermes = state.hermes_service.clone();
    if !hermes.wait_ready(std::time::Duration::from_secs(30)).await {
        return Err(format!(
            "Hermes agent sidecar at {} is not responding. Check that `uv` is on PATH and the model endpoint is reachable.",
            hermes.base_url()
        ));
    }

    let _ = app.emit("llm:turn-start", serde_json::json!({ "userText": request.user_text }));

    // Accumulate the visible assistant message so the finished turn persists in the
    // chat log (the live bubble is cleared on turn-end).
    let message = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let app_cb = app.clone();
    let message_cb = message.clone();
    let result = hermes
        .chat(&request.session_id, &request.user_text, move |event| {
            use crate::hermes_service::ChatEvent;
            match event {
                ChatEvent::Chunk { text } => {
                    if let Ok(mut buf) = message_cb.lock() {
                        buf.push_str(&text);
                    }
                    let _ = app_cb.emit("llm:chunk", serde_json::json!({ "phase": "action", "text": text }));
                }
                ChatEvent::Thought { text } => {
                    let _ = app_cb.emit("llm:chunk", serde_json::json!({ "phase": "think", "text": text }));
                }
                ChatEvent::Tool { name, status, .. } => {
                    let label = if status.is_empty() || status == "None" {
                        format!("{name}\n")
                    } else {
                        format!("{name} [{status}]\n")
                    };
                    let _ = app_cb.emit("llm:chunk", serde_json::json!({ "phase": "tool", "text": label }));
                }
                ChatEvent::Usage { output_tokens, thought_tokens, turns_since_compaction, compact_after } => {
                    // Estimated token usage + how full the conversation is before the
                    // next auto-compaction (which resets the context).
                    let _ = app_cb.emit("agent:usage", serde_json::json!({
                        "outputTokens": output_tokens,
                        "thoughtTokens": thought_tokens,
                        "turnsSinceCompaction": turns_since_compaction,
                        "compactAfter": compact_after,
                    }));
                }
                ChatEvent::Done { .. } => {}
                ChatEvent::Error { message } => {
                    let _ = app_cb.emit("llm:chunk", serde_json::json!({ "phase": "error", "text": message }));
                }
            }
        }, assistant::agent_cancelled)
        .await;

    let _ = app.emit("llm:turn-end", serde_json::json!({}));
    if assistant::agent_cancelled() {
        // The user pressed Stop — the sidecar was disconnected and cancelled the
        // ACP turn. Whatever tool calls already landed stay applied (they're real
        // edits); we just report the cancellation to the chat.
        return Err(assistant::CANCELLED_MESSAGE.into());
    }
    result?;

    // Reload the (tool-mutated) project to return the fresh session + history. The
    // actions were already applied and synced to the engine via the control surface.
    let project = {
        let store = state.store.lock().map_err(|error| error.to_string())?;
        store.get_project(&request.session_id)?
    };
    let explanation = message.lock().map(|buf| buf.clone()).unwrap_or_default();
    Ok(AssistantResponse::Ok {
        explanation,
        actions: vec![],
        warnings: vec![],
        selected_skills: vec![],
        session: project.session,
        history: project.history,
        rationale: None,
        per_action_notes: None,
        tokens: None,
    })
}

#[tauri::command]
pub fn transport_play(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    let mut audio = state.audio.lock().map_err(|error| error.to_string())?;
    audio.bind_session_sources(&project.session)?;
    sync_session_to_engine(&mut audio, &project.session);
    audio.publish_automation(&project.session);
    audio.play(session_id);
    Ok(())
}

#[tauri::command]
pub fn transport_pause(state: State<'_, AppState>) -> Result<(), String> {
    state.audio.lock().map_err(|error| error.to_string())?.pause();
    Ok(())
}

#[tauri::command]
pub fn transport_stop(state: State<'_, AppState>) -> Result<(), String> {
    state.audio.lock().map_err(|error| error.to_string())?.stop();
    Ok(())
}

#[tauri::command]
pub fn transport_seek(state: State<'_, AppState>, sample: u64) -> Result<(), String> {
    state.audio.lock().map_err(|error| error.to_string())?.seek(sample);
    Ok(())
}

#[tauri::command]
pub fn start_recording(
    state: State<'_, AppState>,
    session_id: String,
    start_sample: u64,
    target_track_id: Option<String>,
    input_device: Option<String>,
    input_gain_db: Option<f32>,
    input_channels: Option<Vec<u16>>,
) -> Result<(), String> {
    let session = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .get_project(&session_id)?
        .session;
    let safe_start = start_sample.min(session_duration_samples(&session));
    if let Some(handle) = state.input_monitor.lock().map_err(|error| error.to_string())?.take() {
        handle.stop()?;
    }
    let mut recorder = state.recorder.lock().map_err(|error| error.to_string())?;
    if let Some(handle) = recorder.take() {
        let _ = handle.stop();
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    let path = state
        .config
        .data_dir
        .join("recordings")
        .join(format!("recording-{stamp}.wav"));
    if let Some(track_id) = target_track_id.as_deref() {
        if !session.tracks.iter().any(|track| track.id == track_id) {
            return Err(format!("Unknown target track {track_id}"));
        }
    }
    // Take the target track's preferred channel count (1 = mono, 2 = stereo) from the
    // placeholder source created at track-add time. Falls back to mono.
    let target_channels: u16 = target_track_id
        .as_deref()
        .and_then(|track_id| session.tracks.iter().find(|track| track.id == track_id))
        .and_then(|track| session.source_files.iter().find(|source| source.id == track.source_file_id))
        .map(|source| source.channels.max(1).min(2))
        .unwrap_or(1);
    // dB -> linear gain factor. Clamp to a sane range so we can't massively boost noise.
    let gain_db = input_gain_db.unwrap_or(0.0).clamp(-60.0, 24.0);
    let gain_factor = 10f32.powf(gain_db / 20.0);
    let handle = crate::recorder::start_recording(path, safe_start, target_track_id, input_device, target_channels, gain_factor, input_channels)?;
    if let Err(error) = handle.wait_until_ready(Duration::from_secs(3)) {
        let _ = handle.stop();
        return Err(error);
    }
    *recorder = Some(handle);
    Ok(())
}

#[tauri::command]
pub fn poll_recording_meters(state: State<'_, AppState>) -> Result<RecordingMetersResponse, String> {
    let mut recorder = state.recorder.lock().map_err(|error| error.to_string())?;
    if let Some(error) = recorder.as_ref().and_then(|handle| handle.startup_error()) {
        let _ = recorder.take();
        return Err(error);
    }
    let drained = recorder
        .as_ref()
        .map(|handle| handle.drain_meters())
        .unwrap_or_default();
    let channel_peaks = drained.last().map(|m| m.channel_peaks.clone()).unwrap_or_default();
    let peaks = drained.into_iter().map(|meter| meter.peak).collect();
    Ok(RecordingMetersResponse { peaks, channel_peaks })
}

#[tauri::command]
pub fn stop_recording(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    let handle = state
        .recorder
        .lock()
        .map_err(|error| error.to_string())?
        .take()
        .ok_or_else(|| "No recording is active.".to_string())?;
    let start_sample = handle.start_sample;
    let target_track_id = handle.target_track_id.clone();
    let path = handle.stop()?;
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let project = if let Some(track_id) = target_track_id {
        store.add_recording_clip(&session_id, &track_id, &path, start_sample)?
    } else {
        store.add_source_file_at(&session_id, &path, start_sample)?
    };
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn start_input_monitor(state: State<'_, AppState>, input_device: Option<String>) -> Result<(), String> {
    if state.recorder.lock().map_err(|error| error.to_string())?.is_some() {
        return Ok(());
    }
    let mut monitor = state.input_monitor.lock().map_err(|error| error.to_string())?;
    if let Some(handle) = monitor.take() {
        handle.stop()?;
    }
    let handle = crate::recorder::start_input_monitor(input_device)?;
    if let Err(error) = handle.wait_until_ready(Duration::from_secs(3)) {
        let _ = handle.stop();
        return Err(error);
    }
    *monitor = Some(handle);
    Ok(())
}

#[tauri::command]
pub fn poll_input_monitor_meters(state: State<'_, AppState>) -> Result<RecordingMetersResponse, String> {
    let mut monitor = state.input_monitor.lock().map_err(|error| error.to_string())?;
    if let Some(error) = monitor.as_ref().and_then(|handle| handle.startup_error()) {
        let _ = monitor.take();
        return Err(error);
    }
    let drained = monitor
        .as_ref()
        .map(|handle| handle.drain_meters())
        .unwrap_or_default();
    let channel_peaks = drained.last().map(|m| m.channel_peaks.clone()).unwrap_or_default();
    let peaks = drained.into_iter().map(|meter| meter.peak).collect();
    Ok(RecordingMetersResponse { peaks, channel_peaks })
}

#[tauri::command]
pub fn stop_input_monitor(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(handle) = state.input_monitor.lock().map_err(|error| error.to_string())?.take() {
        handle.stop()?;
    }
    Ok(())
}

#[tauri::command]
pub fn delete_clip(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    clip_id: String,
) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let project = store.delete_clip(&session_id, &track_id, &clip_id)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn delete_clip_range(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    start_sample: u64,
    end_sample: u64,
) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let project = store.delete_clip_range(&session_id, &track_id, start_sample, end_sample)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn set_master_gain(state: State<'_, AppState>, session_id: String, gain_db: f32) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    project.session.master.gain_db = gain_db.clamp(-24.0, 12.0);
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.send(EngineCommand::SetMasterGainDb(project.session.master.gain_db));
    }
    Ok(project)
}

#[tauri::command]
pub fn set_master_bypass(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state.audio
        .lock()
        .map_err(|error| error.to_string())?
        .send(EngineCommand::SetMasterBypass { enabled });
    Ok(())
}

#[tauri::command]
pub fn save_chat_messages(
    state: State<'_, AppState>,
    session_id: String,
    messages: Vec<serde_json::Value>,
) -> Result<(), String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    project.chat_messages = messages;
    store.save(&project)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePreset {
    pub id: String,
    pub display_name: String,
    pub summary: String,
    pub profile: MixerProfile,
}

#[tauri::command]
pub fn list_mixer_profiles() -> Vec<ProfilePreset> {
    use MixerProfile as P;
    vec![
        ProfilePreset {
            id: "balanced".into(),
            display_name: "Balanced (default)".into(),
            summary: "Modest moves, tasteful EQ + glue compression. Streaming loudness.".into(),
            profile: P::default(),
        },
        ProfilePreset {
            id: "scheps_minimalist".into(),
            display_name: "Scheps minimalist".into(),
            summary: "Less is more. Tiny cuts over boosts, almost no compression, dry space.".into(),
            profile: P {
                preset_id: "scheps_minimalist".into(),
                aggressiveness: "subtle".into(),
                eq_philosophy: "corrective_only".into(),
                compression_philosophy: "transparent_glue".into(),
                stereo_treatment: "natural".into(),
                space: "dry".into(),
                loudness_target: "streaming".into(),
                reference_engineer: Some("Andrew Scheps".into()),
                ..P::default()
            },
        },
        ProfilePreset {
            id: "cla_punch".into(),
            display_name: "CLA punch".into(),
            summary: "Big drums, narrow surgical EQ, characterful comp, loud master.".into(),
            profile: P {
                preset_id: "cla_punch".into(),
                aggressiveness: "bold".into(),
                eq_philosophy: "sculpting".into(),
                compression_philosophy: "aggressive".into(),
                stereo_treatment: "wide".into(),
                space: "tasteful".into(),
                loudness_target: "loud".into(),
                reference_engineer: Some("Chris Lord-Alge".into()),
                ..P::default()
            },
        },
        ProfilePreset {
            id: "modern_pop".into(),
            display_name: "Modern pop".into(),
            summary: "Wide stereo, aggressive sidechain feel, loud streaming target.".into(),
            profile: P {
                preset_id: "modern_pop".into(),
                aggressiveness: "moderate".into(),
                eq_philosophy: "tonal_shaping".into(),
                compression_philosophy: "character".into(),
                stereo_treatment: "wide".into(),
                space: "tasteful".into(),
                loudness_target: "loud".into(),
                genre: Some("pop".into()),
                ..P::default()
            },
        },
        ProfilePreset {
            id: "acoustic_natural".into(),
            display_name: "Acoustic natural".into(),
            summary: "Preserve dynamics, minimal compression, broad EQ, tasteful room.".into(),
            profile: P {
                preset_id: "acoustic_natural".into(),
                aggressiveness: "subtle".into(),
                eq_philosophy: "corrective_only".into(),
                compression_philosophy: "transparent_glue".into(),
                stereo_treatment: "natural".into(),
                space: "tasteful".into(),
                loudness_target: "broadcast".into(),
                genre: Some("acoustic".into()),
                ..P::default()
            },
        },
        ProfilePreset {
            id: "electronic_loud".into(),
            display_name: "Electronic / club".into(),
            summary: "Wide, sidechained, loud. Aggressive sculpting on synths.".into(),
            profile: P {
                preset_id: "electronic_loud".into(),
                aggressiveness: "bold".into(),
                eq_philosophy: "sculpting".into(),
                compression_philosophy: "aggressive".into(),
                stereo_treatment: "wide".into(),
                space: "lush".into(),
                loudness_target: "loud".into(),
                genre: Some("electronic".into()),
                ..P::default()
            },
        },
    ]
}

#[tauri::command]
pub fn set_mixer_profile(
    state: State<'_, AppState>,
    session_id: String,
    profile: MixerProfile,
) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    project.session.mixer_profile = profile;
    store.save(&project)?;
    Ok(project)
}

#[tauri::command]
pub fn rename_session(state: State<'_, AppState>, session_id: String, name: String) -> Result<MixProject, String> {
    state.store.lock().map_err(|error| error.to_string())?.rename_session(&session_id, name)
}

#[tauri::command]
pub fn delete_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.store.lock().map_err(|error| error.to_string())?.delete_session(&session_id)
}

#[tauri::command]
pub fn export_project_bundle(state: State<'_, AppState>, session_id: String, bundle_dir: String) -> Result<(), String> {
    state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .export_project_bundle(&session_id, Path::new(&bundle_dir))
}

#[tauri::command]
pub fn import_project_bundle(state: State<'_, AppState>, bundle_dir: String) -> Result<MixProject, String> {
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .import_project_bundle(Path::new(&bundle_dir))?;
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoMixOptions {
    pub stages: Vec<String>,
    pub ollama_base_url: Option<String>,
    pub ollama_model: Option<String>,
}

#[tauri::command]
pub async fn start_auto_mix(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    options: AutoMixOptions,
) -> Result<(), String> {
    use crate::auto_mix::AutoMixStage;
    let stages: Vec<AutoMixStage> = options
        .stages
        .iter()
        .filter_map(|s| match s.as_str() {
            "raw_session_prep" => Some(AutoMixStage::RawSessionPrep),
            "prep_intent" => Some(AutoMixStage::PrepIntent),
            "static_balance" => Some(AutoMixStage::StaticBalance),
            "cleanup_filters" => Some(AutoMixStage::CleanupFilters),
            "subtractive_eq" => Some(AutoMixStage::SubtractiveEq),
            "dynamics" => Some(AutoMixStage::Dynamics),
            "tonal_enhancement" => Some(AutoMixStage::TonalEnhancement),
            "depth_space" => Some(AutoMixStage::DepthSpace),
            "section_automation" => Some(AutoMixStage::SectionAutomation),
            "mix_bus_loudness" => Some(AutoMixStage::MixBusLoudness),
            // Legacy IDs kept so older frontend sessions/buttons do not break during reloads.
            "gain_staging" => Some(AutoMixStage::StaticBalance),
            "corrective_eq" => Some(AutoMixStage::SubtractiveEq),
            "tonal_shaping" => Some(AutoMixStage::TonalEnhancement),
            "space_glue" => Some(AutoMixStage::DepthSpace),
            "master_balance" => Some(AutoMixStage::MixBusLoudness),
            _ => None,
        })
        .collect();
    if stages.is_empty() {
        return Err("No stages selected.".into());
    }

    let observer: std::sync::Arc<dyn assistant::LlmObserver> =
        std::sync::Arc::new(TauriLlmObserver { app: app.clone() });
    let mut config = state.config.clone();
    if let Some(base_url) = options.ollama_base_url.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        config.ollama_base_url = base_url.to_string();
    }
    if let Some(model) = options.ollama_model.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        config.ollama_model = model.to_string();
    }
    // Hold an Arc clone of the store so the background task can lock it.
    let store_arc = std::sync::Arc::new(std::sync::Mutex::new(unsafe_clone_store(&state)));

    assistant::reset_agent_cancel();
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    tokio::spawn(async move {
        let _ = app_clone.emit("auto-mix:start", serde_json::json!({ "stages": options.stages }));
        for (i, stage) in stages.iter().enumerate() {
            if assistant::agent_cancelled() {
                break;
            }
            let _ = app_clone.emit(
                "auto-mix:stage-start",
                serde_json::json!({ "index": i, "stageId": stage.id(), "displayName": stage.display_name() }),
            );
            let report =
                crate::auto_mix::run_stage(&config, store_arc.clone(), &session_id_clone, *stage, observer.clone())
                    .await;
            match report {
                Ok(r) => {
                    let _ = app_clone.emit("auto-mix:stage-done", serde_json::json!(r));
                    let _ = sync_audio_from_app(&app_clone, &session_id_clone);
                    if r.status == "error" || r.status == "cancelled" { break; }
                }
                Err(e) => {
                    let _ = app_clone.emit("auto-mix:stage-done", serde_json::json!({
                        "stageId": stage.id(),
                        "displayName": stage.display_name(),
                        "status": "error",
                        "actionCount": 0,
                        "warnings": [],
                        "error": e,
                        "tokens": 0,
                        "elapsedMs": 0,
                    }));
                    break;
                }
            }
        }
        // Reload the project once everything's done so the UI sees the final state.
        if let Ok(p) = state_get_project(&app_clone, &session_id_clone) {
            let _ = sync_audio_from_app(&app_clone, &session_id_clone);
            let _ = app_clone.emit("auto-mix:complete", serde_json::json!({ "project": p }));
        } else {
            let _ = app_clone.emit("auto-mix:complete", serde_json::json!({}));
        }
    });
    Ok(())
}

/// Clone the underlying SessionStore (it's a thin handle around a data_dir).
fn unsafe_clone_store(state: &State<'_, AppState>) -> SessionStore {
    use crate::config::Config;
    let cfg: &Config = &state.config;
    SessionStore::new(cfg.data_dir.clone())
}

fn state_get_project(app: &tauri::AppHandle, session_id: &str) -> Result<MixProject, String> {
    use tauri::Manager;
    let state = app.state::<AppState>();
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.get_project(session_id)
}

fn sync_audio_from_app(app: &tauri::AppHandle, session_id: &str) -> Result<(), String> {
    use tauri::Manager;
    let state = app.state::<AppState>();
    let project = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.get_project(session_id)?
    };
    let mut audio = state.audio.lock().map_err(|e| e.to_string())?;
    sync_session_to_engine(&mut audio, &project.session);
    audio.publish_automation(&project.session);
    Ok(())
}

#[tauri::command]
pub async fn analyze_master_structure(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<MixProject, String> {
    let emit = |stage: &str, message: &str, elapsed: f32| {
        let _ = app.emit(
            "audio:progress",
            serde_json::json!({
                "stage": stage,
                "message": message,
                "elapsedSeconds": elapsed,
            }),
        );
    };

    emit("starting", "Preparing analysis…", 0.0);

    let (project, audio_service) = {
        let store = state.store.lock().map_err(|error| error.to_string())?;
        let project = store.get_project(&session_id)?;
        let audio_service = state.audio_service.clone();
        (project, audio_service)
    };

    if project.session.tracks.is_empty() {
        return Err("Add at least one track before analyzing structure.".into());
    }

    let renders_dir = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .renders_dir();
    std::fs::create_dir_all(&renders_dir).map_err(|e| e.to_string())?;
    let render_path = renders_dir.join(format!("{session_id}.structure.wav"));

    emit("rendering", "Rendering master mix to WAV…", 0.0);
    audio::render_mix(&project.session, &render_path)?;

    emit("connecting", "Waiting for audio sidecar…", 0.0);
    if !audio_service.wait_ready(std::time::Duration::from_secs(60)).await {
        return Err(format!(
            "Audio analysis sidecar at {} did not respond. Run `cd audio-service && uv sync` and check that `uv` is on PATH.",
            audio_service.base_url()
        ));
    }

    // Spawn a poller that mirrors the sidecar's /status endpoint to the
    // frontend while the (blocking) analyze call runs.
    let poll_app = app.clone();
    let poll_svc = audio_service.clone();
    let poll_task = tokio::spawn(async move {
        let mut last_stage = String::new();
        let mut last_message = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Some(status) = poll_svc.status().await {
                if status.stage == "idle" {
                    continue;
                }
                if status.stage != last_stage || status.message != last_message {
                    last_stage = status.stage.clone();
                    last_message = status.message.clone();
                    let _ = poll_app.emit(
                        "audio:progress",
                        serde_json::json!({
                            "stage": status.stage,
                            "message": status.message,
                            "elapsedSeconds": status.elapsed_seconds,
                        }),
                    );
                }
                if status.stage == "done" {
                    break;
                }
            }
        }
    });

    let analysis_result = audio_service.analyze_structure(&render_path).await;
    poll_task.abort();

    let analysis = match analysis_result {
        Ok(a) => a,
        Err(e) => {
            emit("error", &e, 0.0);
            return Err(e);
        }
    };

    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    project.session.bpm = if analysis.bpm > 0.0 {
        Some(analysis.bpm)
    } else {
        project.session.bpm
    };
    // Render once more to memory so we can slice per section and analyze each.
    let rendered = crate::engine::render::render_session_to_buffer(&project.session).ok();
    project.session.sections = analysis
        .sections
        .into_iter()
        .map(|s| MixSection {
            analysis: rendered.as_ref().and_then(|r| analyze_section_window(r, s.start, s.end)),
            start: s.start,
            end: s.end,
            label: s.label,
        })
        .collect();
    store.save(&project)?;
    emit(
        "done",
        &format!(
            "Detected {} sections at {:.0} bpm",
            project.session.sections.len(),
            project.session.bpm.unwrap_or(0.0)
        ),
        0.0,
    );
    Ok(project)
}

#[tauri::command]
pub fn render_mix(state: State<'_, AppState>, session_id: String, output_path: String) -> Result<RenderResponse, String> {
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    let path = normalize_wav_path(PathBuf::from(output_path));
    audio::render_mix(&project.session, &path)?;
    Ok(RenderResponse { path: path.to_string_lossy().to_string() })
}

#[tauri::command]
pub fn save_video_recording(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    file_name: String,
    mime_type: String,
    start_sample: u64,
    duration_ms: u64,
    data_base64: String,
    create_audio_track: bool,
    source_offset_ms: u64,
) -> Result<MixProject, String> {
    let raw = data_base64
        .split_once(',')
        .map(|(_, payload)| payload)
        .unwrap_or(data_base64.as_str());
    let bytes = BASE64_STANDARD
        .decode(raw.as_bytes())
        .map_err(|error| format!("Could not decode video recording: {error}"))?;
    let extension = video_extension(&file_name, &mime_type);
    let temp_path = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .videos_dir()
        .join(format!("incoming-{}.{}", uuid::Uuid::new_v4(), extension));
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&temp_path, bytes).map_err(|error| format!("Could not write video recording: {error}"))?;
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let source_offset_ms = source_offset_ms.min(duration_ms.saturating_sub(1));
    let mut project = store.add_video_recording_clip(&session_id, &track_id, &temp_path, file_name.clone(), mime_type, start_sample, duration_ms, source_offset_ms)?;
    if create_audio_track {
        let audio_path = store
            .videos_dir()
            .join(format!("{}-audio-{}.wav", Path::new(&file_name).file_stem().and_then(|item| item.to_str()).unwrap_or("camera"), uuid::Uuid::new_v4()));
        if extract_video_audio(&temp_path, &audio_path, project.session.sample_rate, source_offset_ms).is_ok() {
            if let Ok(updated) = store.add_source_file_at(&session_id, &audio_path, start_sample) {
                project = updated;
            }
            let _ = fs::remove_file(&audio_path);
        }
    }
    let _ = fs::remove_file(&temp_path);
    Ok(project)
}

#[tauri::command]
pub fn render_video_mix(
    state: State<'_, AppState>,
    session_id: String,
    output_path: String,
    start_sample: Option<u64>,
    end_sample: Option<u64>,
    track_ids: Option<Vec<String>>,
    aspect_ratio: Option<String>,
    // "high" = `-preset slow -crf 17 -b:a 320k`; otherwise the previous fast defaults.
    quality: Option<String>,
) -> Result<RenderResponse, String> {
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    let path = normalize_mp4_path(PathBuf::from(output_path));
    let full_end = session_duration_samples(&project.session);
    let range_start = start_sample.unwrap_or(0).min(full_end);
    let range_end = end_sample.unwrap_or(full_end).min(full_end).max(range_start + 1);
    let selected_track_ids = track_ids
        .unwrap_or_default()
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<HashSet<_>>();
    if selected_track_ids.is_empty() {
        return Err("Select one or more video tracks in the canvas before exporting MP4.".into());
    }
    let video_inputs = collect_video_inputs(&project.session, range_start, range_end, &selected_track_ids);
    if video_inputs.is_empty() {
        return Err("The selected video tracks have no recorded clips in the export range.".into());
    }

    let renders_dir = state.store.lock().map_err(|error| error.to_string())?.renders_dir();
    fs::create_dir_all(&renders_dir).map_err(|error| error.to_string())?;
    let audio_path = renders_dir.join(format!("{session_id}.video-export.wav"));
    audio::render_mix(&project.session, &audio_path)?;

    let mut command = Command::new("ffmpeg");
    command.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error");

    for clip in &video_inputs {
        command.arg("-i").arg(&clip.path);
    }
    if range_start > 0 {
        command.arg("-ss").arg(format!("{:.3}", range_start as f64 / project.session.sample_rate as f64));
    }
    command
        .arg("-t")
        .arg(format!("{:.3}", range_end.saturating_sub(range_start) as f64 / project.session.sample_rate as f64))
        .arg("-i")
        .arg(&audio_path);
    let filter = build_video_filter(&video_inputs, &project.session, range_start, range_end, aspect_ratio.as_deref());
    command
        .arg("-filter_complex")
        .arg(filter)
        .arg("-map")
        .arg("[v]")
        .arg("-map")
        .arg(format!("{}:a:0", video_inputs.len()));

    command
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart");
    if quality.as_deref() == Some("high") {
        command
            .arg("-preset").arg("slow")
            .arg("-crf").arg("17")
            .arg("-c:a").arg("aac")
            .arg("-b:a").arg("320k");
    } else {
        command
            .arg("-preset").arg("veryfast")
            .arg("-c:a").arg("aac");
    }
    let output = command
        .arg("-shortest")
        .arg(&path)
        .output()
        .map_err(|error| format!("Could not run ffmpeg. Install ffmpeg to export video: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg video export failed: {}", stderr.trim()));
    }
    Ok(RenderResponse { path: path.to_string_lossy().to_string() })
}

#[tauri::command]
pub fn export_rendered_video(source_path: String, output_path: String, aspect_ratio: Option<String>, quality: Option<String>) -> Result<RenderResponse, String> {
    let source = PathBuf::from(source_path);
    if !source.exists() {
        return Err("The Main video render is missing. Run Agent Edit again before exporting.".into());
    }
    let path = normalize_mp4_path(PathBuf::from(output_path));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    // Letterbox/pillarbox the source into the requested aspect with black padding.
    // No aspect (or "original") just falls through to a fast copy.
    if let Some(aspect) = aspect_ratio.as_deref() {
        if aspect == "square" || aspect == "portrait916" {
            let (src_w, src_h) = probe_video_dimensions(&source)?;
            let (t_w, t_h) = aspect_target_box(src_w as i32, src_h as i32, Some(aspect))
                .ok_or("Unknown aspect ratio")?;
            let filter = format!(
                "scale={t_w}:{t_h}:force_original_aspect_ratio=decrease,pad={t_w}:{t_h}:(ow-iw)/2:(oh-ih)/2:color=black,format=yuv420p"
            );
            let mut cmd = Command::new("ffmpeg");
            cmd.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error")
                .arg("-i").arg(&source)
                .arg("-vf").arg(&filter)
                .arg("-c:v").arg("libx264")
                .arg("-pix_fmt").arg("yuv420p")
                .arg("-movflags").arg("+faststart");
            if quality.as_deref() == Some("high") {
                // Re-encode the padded video with the final-export quality knobs and
                // re-encode audio at 320 kbps. Copying audio when re-encoding video at
                // a different rate would risk container hiccups in some players.
                cmd.arg("-preset").arg("slow").arg("-crf").arg("17")
                    .arg("-c:a").arg("aac").arg("-b:a").arg("320k");
            } else {
                // Fast aspect transcode — keep audio bit-for-bit so we don't lose more.
                cmd.arg("-preset").arg("veryfast")
                    .arg("-c:a").arg("copy");
            }
            let output = cmd.arg(&path).output()
                .map_err(|error| format!("Could not run ffmpeg: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "ffmpeg aspect export failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            return Ok(RenderResponse { path: path.to_string_lossy().to_string() });
        }
    }
    if source == path {
        return Ok(RenderResponse { path: path.to_string_lossy().to_string() });
    }
    fs::copy(&source, &path).map_err(|error| format!("Could not export Main video: {error}"))?;
    Ok(RenderResponse { path: path.to_string_lossy().to_string() })
}

/// Compute the output box (even dims) for a target aspect ratio + max long-edge size.
/// `aspect` is "W:H" (e.g. "16:9", "9:16", "1:1", "4:5") or "original" to keep the
/// source ratio. `max_dim` is the longer side in pixels (None = keep the source's).
fn export_target_box(src_w: i32, src_h: i32, aspect: &str, max_dim: Option<u32>) -> (i32, i32) {
    let (aw, ah): (f64, f64) = if aspect == "original" || aspect.is_empty() {
        (src_w.max(1) as f64, src_h.max(1) as f64)
    } else {
        let mut parts = aspect.split(&[':', 'x', '/'][..]);
        let a = parts.next().and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(16.0);
        let b = parts.next().and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(9.0);
        (a.max(0.01), b.max(0.01))
    };
    let ratio = aw / ah; // width / height
    // Default long edge: the source's longer side, so "Source" resolution = no upscale.
    let long = max_dim.map(|m| m as f64).unwrap_or_else(|| src_w.max(src_h) as f64);
    let (w, h) = if ratio >= 1.0 {
        (long, long / ratio) // landscape or square
    } else {
        (long * ratio, long) // portrait
    };
    (even_dimension(w.round() as i32).max(2), even_dimension(h.round() as i32).max(2))
}

/// Export a rendered video at an arbitrary aspect ratio + resolution.
/// - `aspect`: "original" | "16:9" | "9:16" | "1:1" | "4:5" | "4:3" | "21:9" ...
/// - `max_dimension`: longer side in px (None = source size; no upscale on "original").
/// - `mode`: "fit" letterbox/pad (show everything) or "fill" cover/crop (fill the frame).
/// Always re-encodes at high quality (slow/CRF 17, AAC 320k).
#[tauri::command]
pub async fn export_video(
    app: AppHandle,
    source_path: String,
    output_path: String,
    aspect: String,
    max_dimension: Option<u32>,
    mode: Option<String>,
) -> Result<RenderResponse, String> {
    let source = PathBuf::from(&source_path);
    if !source.exists() {
        return Err("The video to export is missing. Render an Agent Edit first.".into());
    }
    let path = normalize_mp4_path(PathBuf::from(output_path));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    // Run the (blocking) encode off the main thread so the UI stays responsive,
    // and stream ffmpeg's progress out as `video-export:progress` events.
    tokio::task::spawn_blocking(move || run_export_blocking(app, source, path, aspect, max_dimension, mode))
        .await
        .map_err(|error| format!("export task failed: {error}"))?
}

/// Total duration in seconds via ffprobe (None if unavailable → indeterminate progress).
fn probe_video_duration(path: &Path) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok()
}

fn run_export_blocking(
    app: AppHandle,
    source: PathBuf,
    path: PathBuf,
    aspect: String,
    max_dimension: Option<u32>,
    mode: Option<String>,
) -> Result<RenderResponse, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;

    let (src_w, src_h) = probe_video_dimensions(&source)?;
    let (t_w, t_h) = export_target_box(src_w as i32, src_h as i32, &aspect, max_dimension);
    let total_secs = probe_video_duration(&source).unwrap_or(0.0);

    // "fill" scales to cover the box then center-crops; "fit" scales to fit then pads.
    let fill = mode.as_deref() == Some("fill");
    let filter = if fill {
        format!("scale={t_w}:{t_h}:force_original_aspect_ratio=increase,crop={t_w}:{t_h},setsar=1,format=yuv420p")
    } else {
        format!("scale={t_w}:{t_h}:force_original_aspect_ratio=decrease,pad={t_w}:{t_h}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=yuv420p")
    };

    let emit = |percent: f64, stage: &str| {
        let _ = app.emit("video-export:progress", serde_json::json!({ "percent": percent, "stage": stage }));
    };
    emit(0.0, "start");

    let mut child = Command::new("ffmpeg")
        .arg("-y").arg("-hide_banner").arg("-loglevel").arg("error")
        .arg("-i").arg(&source)
        .arg("-vf").arg(&filter)
        .arg("-c:v").arg("libx264")
        .arg("-preset").arg("slow")
        .arg("-crf").arg("17")
        .arg("-pix_fmt").arg("yuv420p")
        .arg("-movflags").arg("+faststart")
        .arg("-c:a").arg("aac").arg("-b:a").arg("320k")
        .arg("-progress").arg("pipe:1").arg("-nostats")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not run ffmpeg. Install ffmpeg to export video: {error}"))?;

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            // ffmpeg progress emits out_time_us / out_time_ms (both microseconds).
            let micros = line
                .strip_prefix("out_time_us=")
                .or_else(|| line.strip_prefix("out_time_ms="));
            if let Some(v) = micros {
                if let Ok(us) = v.trim().parse::<i64>() {
                    if total_secs > 0.0 {
                        let pct = (us as f64 / 1_000_000.0 / total_secs * 100.0).clamp(0.0, 99.0);
                        emit(pct, "encoding");
                    }
                }
            } else if line == "progress=end" {
                emit(99.0, "finishing");
            }
        }
    }

    let status = child.wait().map_err(|error| format!("ffmpeg wait failed: {error}"))?;
    if !status.success() {
        let mut err = String::new();
        if let Some(mut se) = child.stderr.take() {
            let _ = se.read_to_string(&mut err);
        }
        emit(0.0, "error");
        return Err(format!("ffmpeg export failed: {}", err.trim()));
    }
    emit(100.0, "done");
    Ok(RenderResponse { path: path.to_string_lossy().to_string() })
}

#[tauri::command]
pub fn render_auto_video_edit(
    state: State<'_, AppState>,
    session_id: String,
    output_path: String,
    start_sample: Option<u64>,
    end_sample: Option<u64>,
    track_ids: Vec<String>,
    sample_interval_seconds: Option<f64>,
) -> Result<RenderResponse, String> {
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    let path = normalize_mp4_path(PathBuf::from(output_path));
    let full_end = session_duration_samples(&project.session);
    let range_start = start_sample.unwrap_or(0).min(full_end);
    let range_end = end_sample.unwrap_or(full_end).min(full_end).max(range_start + 1);
    let selected_track_ids = track_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<HashSet<_>>();
    if selected_track_ids.is_empty() {
        return Err("Select one or more video tracks before running Auto Video Edit.".into());
    }
    let video_inputs = collect_video_inputs(&project.session, range_start, range_end, &selected_track_ids);
    if video_inputs.is_empty() {
        return Err("The selected video tracks have no recorded clips in the edit range.".into());
    }
    let interval_samples = ((sample_interval_seconds.unwrap_or(1.0).clamp(0.25, 16.0) * project.session.sample_rate as f64).round() as u64).max(1);
    let segments = build_auto_edit_segments(&video_inputs, range_start, range_end, interval_samples, project.session.sample_rate);
    if segments.is_empty() {
        return Err("Auto Video Edit could not find visible selected clips in the edit range.".into());
    }

    let renders_dir = state.store.lock().map_err(|error| error.to_string())?.renders_dir();
    fs::create_dir_all(&renders_dir).map_err(|error| error.to_string())?;
    let audio_path = renders_dir.join(format!("{session_id}.auto-video-edit.wav"));
    audio::render_mix(&project.session, &audio_path)?;

    let mut command = Command::new("ffmpeg");
    command.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error");
    for clip in &video_inputs {
        command.arg("-i").arg(&clip.path);
    }
    if range_start > 0 {
        command.arg("-ss").arg(format!("{:.3}", range_start as f64 / project.session.sample_rate as f64));
    }
    command
        .arg("-t")
        .arg(format!("{:.3}", range_end.saturating_sub(range_start) as f64 / project.session.sample_rate as f64))
        .arg("-i")
        .arg(&audio_path);

    let filter = build_auto_edit_filter(&video_inputs, &segments, &project.session, range_start, range_end, None);
    command
        .arg("-filter_complex")
        .arg(filter)
        .arg("-map")
        .arg("[v]")
        .arg("-map")
        .arg(format!("{}:a:0", video_inputs.len()));

    let output = command
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("aac")
        .arg("-shortest")
        .arg(&path)
        .output()
        .map_err(|error| format!("Could not run ffmpeg. Install ffmpeg to export video: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg auto video edit failed: {}", stderr.trim()));
    }
    Ok(RenderResponse { path: path.to_string_lossy().to_string() })
}

/// Direct-edit path for a single video clip. Takes the user's chat text, extracts a
/// sample frame, runs ONE vision call to refine look/grade/effects, falls back to
/// deterministic keyword detection per-field where the LLM left things blank, then
/// ffmpeg-renders only that clip's source range with the resolved filter chain and
/// swaps the result in via `replace_track_video`. No multicam, no cuts, no per-window
/// loop — instant compared to the agent edit. Returns what was applied for the chat.
#[tauri::command]
pub async fn apply_clip_effects(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    clip_id: String,
    instructions: String,
    ollama_base_url: Option<String>,
    vision_model: Option<String>,
) -> Result<ApplyClipEffectsResponse, String> {
    assistant::reset_agent_cancel();
    let project = state.store.lock().map_err(|e| e.to_string())?.get_project(&session_id)?;
    let track = project.session.tracks.iter().find(|t| t.id == track_id)
        .ok_or("Track not found")?;
    if !matches!(track.kind, crate::model::TrackKind::Video) {
        return Err("Selected track is not a video track.".into());
    }
    let clip = track.video_clips.iter().find(|c| c.id == clip_id)
        .ok_or("Clip not found on the selected track")?
        .clone();
    let source = project.session.video_source_files.iter()
        .find(|s| s.id == clip.video_source_file_id)
        .ok_or("Clip's video source file is missing")?
        .clone();
    let source_path = PathBuf::from(&source.path);
    if !source_path.exists() {
        return Err("The clip's source video file is no longer on disk.".into());
    }
    let source_offset_s = clip.source_offset_ms as f64 / 1000.0;
    let duration_samples = clip.end_sample.saturating_sub(clip.start_sample);
    let duration_s = duration_samples as f64 / project.session.sample_rate as f64;
    if duration_s <= 0.0 {
        return Err("Clip has zero duration; nothing to edit.".into());
    }

    // 1. Keyword baseline from the user's text — runs in microseconds and seeds the
    //    LLM in case it returns nothing for some field.
    let (kw_look, kw_grade) = infer_look_from_instructions(&instructions);
    let kw_effects = infer_effects_from_instructions(&instructions);

    // 2. LLM refinement: extract one frame at the clip's midpoint, ask the vision
    //    model to refine. Best-effort — failures are silent and the keyword baseline
    //    still ships, so the user always sees a change.
    let temp_dir = state.config.data_dir.join("clip-edit").join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).map_err(|e| format!("Could not prepare temp dir: {e}"))?;
    let frame_path = temp_dir.join("frame.jpg");
    let frame_time_in_source = source_offset_s + (duration_s / 2.0);
    let _ = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .arg("-ss").arg(format!("{frame_time_in_source:.3}"))
        .arg("-i").arg(&source_path)
        .args(["-frames:v", "1", "-q:v", "8", "-vf", "scale=512:512:force_original_aspect_ratio=decrease"])
        .arg(&frame_path)
        .status();
    let llm_choice: Option<ClipEffectsChoice> = if frame_path.exists() {
        let bytes = fs::read(&frame_path).ok();
        if let Some(bytes) = bytes {
            let base_url = ollama_base_url
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| state.config.ollama_base_url.clone())
                .trim_end_matches('/')
                .to_string();
            let model = vision_model
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "qwen2.5vl:latest".to_string());
            let frame_b64 = BASE64_STANDARD.encode(bytes);
            analyze_clip_effects(&base_url, &model, frame_b64, &instructions).await.ok()
        } else { None }
    } else { None };
    let _ = fs::remove_dir_all(&temp_dir);

    // 3. Merge: LLM wins where present; keyword fallback per-field.
    let llm_look = llm_choice.as_ref()
        .and_then(|c| c.look_preset.as_deref())
        .and_then(parse_video_filter_preset);
    let llm_grade = llm_choice.as_ref().and_then(|c| c.color_grade.clone())
        .filter(|g| build_color_grade_filter(g).is_some());
    let llm_effects = llm_choice.as_ref().and_then(|c| c.video_effects.clone())
        .filter(|e| e.fade_in_seconds.is_some() || e.fade_out_seconds.is_some() || e.speed_factor.is_some());
    let look_source = if llm_look.is_some() { "llm" } else if kw_look.is_some() { "keywords" } else { "none" };
    let grade_source = if llm_grade.is_some() { "llm" } else if kw_grade.is_some() { "keywords" } else { "none" };
    let effects_source = if llm_effects.is_some() { "llm" } else if kw_effects.is_some() { "keywords" } else { "none" };
    let chosen_look = llm_look.or(kw_look);
    let chosen_grade = llm_grade.or(kw_grade);
    let chosen_effects = llm_effects.or(kw_effects);

    if chosen_look.is_none() && chosen_grade.is_none() && chosen_effects.is_none() {
        return Err("Could not detect any visual change in your message. Try words like \"cinematic\", \"warm\", \"fade in 2 seconds\", or \"slow down\".".into());
    }

    // 4. Build filter chains and render only the clip's source range to a new mp4.
    let grade_filter = chosen_grade.as_ref().and_then(build_color_grade_filter);
    let (veff_chain, aeff_chain, _speed) = chosen_effects.as_ref()
        .map(|e| build_effects_filters(e, duration_s))
        .unwrap_or((None, None, 1.0));
    let mut vparts: Vec<String> = Vec::new();
    if let Some(g) = grade_filter { vparts.push(g); }
    if let Some(v) = veff_chain { vparts.push(v); }
    let vf = if vparts.is_empty() { None } else { Some(vparts.join(",")) };

    let renders_dir = state.store.lock().map_err(|e| e.to_string())?.renders_dir();
    fs::create_dir_all(&renders_dir).map_err(|e| e.to_string())?;
    let output_path = renders_dir.join(format!("clip-edit-{}.mp4", uuid::Uuid::new_v4()));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"])
        .arg("-ss").arg(format!("{source_offset_s:.3}"))
        .arg("-t").arg(format!("{duration_s:.3}"))
        .arg("-i").arg(&source_path);
    if let Some(filter) = vf { cmd.arg("-vf").arg(filter); }
    if let Some(af) = aeff_chain { cmd.arg("-af").arg(af); }
    cmd.args([
        "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p",
        "-movflags", "+faststart", "-r", "30", "-g", "30", "-keyint_min", "30", "-sc_threshold", "0",
        "-c:a", "aac", "-shortest",
    ]).arg(&output_path);
    let out = cmd.output().map_err(|e| format!("Could not run ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffmpeg clip edit failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }

    // 5. Swap the rendered file into the clip in place.
    let duration_ms = (duration_s * 1000.0).round() as u64;
    let updated = state.store.lock().map_err(|e| e.to_string())?
        .replace_track_video(&session_id, &track_id, &clip_id, &output_path, duration_ms.max(1))?;
    let _ = fs::remove_file(&output_path);
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&updated.session)?;
        sync_session_to_engine(&mut audio, &updated.session);
        audio.publish_automation(&updated.session);
    }

    Ok(ApplyClipEffectsResponse {
        project: updated,
        look_preset: chosen_look,
        color_grade: chosen_grade,
        video_effects: chosen_effects,
        source_summary: format!("look:{look_source} grade:{grade_source} effects:{effects_source}"),
    })
}

/// Revert a video clip to the pristine recording (the source it had before any
/// effects/grade render). Cheap — no ffmpeg, no LLM; just swaps the clip's
/// source-id/offset/duration back to the saved snapshot.
#[tauri::command]
pub fn revert_clip_video(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    clip_id: String,
) -> Result<MixProject, String> {
    let updated = state.store.lock().map_err(|e| e.to_string())?
        .revert_clip_to_pristine(&session_id, &track_id, &clip_id)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&updated.session)?;
        sync_session_to_engine(&mut audio, &updated.session);
        audio.publish_automation(&updated.session);
    }
    Ok(updated)
}

#[tauri::command]
pub async fn render_agent_video_edit(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    output_path: Option<String>,
    start_sample: Option<u64>,
    end_sample: Option<u64>,
    track_ids: Vec<String>,
    sample_interval_seconds: Option<f64>,
    ollama_base_url: Option<String>,
    ollama_model: Option<String>,
    vision_model: Option<String>,
    edit_model: Option<String>,
    instructions: Option<String>,
    // When true, run the agent's decision phase only and return the script — no ffmpeg
    // render. The frontend uses this to show a plan that the user can review/edit before
    // clicking Process to actually render.
    plan_only: Option<bool>,
) -> Result<AgentVideoEditResponse, String> {
    assistant::reset_agent_cancel();
    let started = std::time::Instant::now();
    emit_agent_progress(&app, &started, "starting", "Preparing Agent Video Edit...", 0, 1);
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    // No save dialog by default — render straight to the renders folder. The frontend
    // adds the result as a new video track; the Download button lets the user export later.
    let path = match output_path.filter(|value| !value.trim().is_empty()) {
        Some(value) => normalize_mp4_path(PathBuf::from(value)),
        None => {
            let renders_dir = state.store.lock().map_err(|error| error.to_string())?.renders_dir();
            std::fs::create_dir_all(&renders_dir).map_err(|error| error.to_string())?;
            renders_dir.join(format!("agent-edit-{}.mp4", uuid::Uuid::new_v4()))
        }
    };
    let full_end = session_duration_samples(&project.session);
    let range_start = start_sample.unwrap_or(0).min(full_end);
    let range_end = end_sample.unwrap_or(full_end).min(full_end).max(range_start + 1);
    let selected_track_ids = track_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<HashSet<_>>();
    if selected_track_ids.is_empty() {
        emit_agent_progress(&app, &started, "error", "No selected video tracks.", 0, 1);
        return Err("Select one or more video tracks before running Agent Video Edit.".into());
    }
    let video_inputs = collect_video_inputs(&project.session, range_start, range_end, &selected_track_ids);
    if video_inputs.is_empty() {
        emit_agent_progress(&app, &started, "error", "Selected tracks have no clips in range.", 0, 1);
        return Err("The selected video tracks have no recorded clips in the edit range.".into());
    }
    let interval_samples = ((sample_interval_seconds.unwrap_or(1.0).clamp(0.25, 16.0) * project.session.sample_rate as f64).round() as u64).max(1);
    let base_url = ollama_base_url
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| state.config.ollama_base_url.clone())
        .trim_end_matches('/')
        .to_string();
    let fallback_model = ollama_model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "qwen2.5vl:latest".to_string());
    let vision_model = vision_model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_model.clone());
    let edit_model = edit_model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_model.clone());
    if base_url.is_empty() || vision_model.is_empty() || edit_model.is_empty() {
        emit_agent_progress(&app, &started, "error", "No Ollama vision model configured.", 0, 1);
        return Err("Configure Ollama models before running Agent Video Edit.".into());
    }
    let instructions = instructions
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let total_windows = range_end.saturating_sub(range_start).div_ceil(interval_samples).max(1) as u32;
    emit_agent_progress(&app, &started, "audio", "Rendering mix audio for agent analysis...", 0, total_windows);
    let renders_dir = state.store.lock().map_err(|error| error.to_string())?.renders_dir();
    fs::create_dir_all(&renders_dir).map_err(|error| error.to_string())?;
    let audio_path = renders_dir.join(format!("{session_id}.agent-video-edit.wav"));
    // Render the audio mix only across the selected range, in memory — way faster than
    // rendering the whole session and round-tripping through a WAV file. The full-render
    // path below for the final ffmpeg step still writes the audio to disk.
    let rendered_range = crate::engine::render::render_session_range_to_buffer(
        &project.session,
        range_start,
        range_end,
    )
    .map_err(|error| {
        emit_agent_progress(&app, &started, "error", "Could not render mix audio for the video agent.", 0, total_windows);
        error
    })?;
    let audio_analysis = RenderedAudioAnalysis {
        samples: rendered_range.samples,
        channels: rendered_range.channels as usize,
        sample_rate: rendered_range.sample_rate,
        timeline_start_sample: range_start,
    };
    // The final render still needs the mixed audio on disk for the ffmpeg-step (only
    // when the user clicks Process / when this isn't plan_only). Defer that until we
    // know we're not in plan_only mode.
    if !plan_only.unwrap_or(false) {
        audio::render_mix(&project.session, &audio_path)?;
    }
    emit_agent_progress(
        &app,
        &started,
        "sampling",
        &format!("Sampling frames from {} selected video tracks...", selected_track_ids.len()),
        0,
        total_windows,
    );
    let temp_dir = state.config.data_dir.join("agent-video-edit").join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir).map_err(|error| format!("Could not prepare agent frame cache: {error}"))?;
    let (segments, script, agent_look_preset, agent_color_grade, agent_effects) = build_agent_edit_segments(
        &app,
        &started,
        &video_inputs,
        &project.session,
        range_start,
        range_end,
        interval_samples,
        &base_url,
        &vision_model,
        &edit_model,
        instructions.as_deref(),
        Some(&audio_analysis),
        &temp_dir,
    )
    .await
    .unwrap_or_else(|error| {
        emit_agent_progress(&app, &started, "fallback", &format!("Vision agent failed; using automatic cuts. {error}"), 0, total_windows);
        let segments = build_auto_edit_segments(&video_inputs, range_start, range_end, interval_samples, project.session.sample_rate);
        let script = build_fallback_agent_script(&video_inputs, &segments, range_start, range_end, total_windows, project.session.sample_rate, &error);
        // Even on agent failure, honor explicit keyword effects from the instructions.
        let kw_effects = instructions.as_deref().and_then(infer_effects_from_instructions);
        let (kw_look, kw_grade) = instructions.as_deref().map(infer_look_from_instructions).unwrap_or((None, None));
        (segments, script, kw_look, kw_grade, kw_effects)
    });
    let _ = fs::remove_dir_all(&temp_dir);
    if segments.is_empty() {
        emit_agent_progress(&app, &started, "error", "No visible clips found in selected range.", 0, total_windows);
        return Err("Agent Video Edit could not find visible selected clips in the edit range.".into());
    }

    if plan_only.unwrap_or(false) {
        emit_agent_progress(&app, &started, "done", "Plan ready for review.", total_windows, total_windows);
        return Ok(AgentVideoEditResponse { path: String::new(), script, look_preset: agent_look_preset, color_grade: agent_color_grade, video_effects: agent_effects });
    }

    emit_agent_progress(&app, &started, "audio", "Using analyzed mix audio for video export...", total_windows, total_windows);
    // Prefer the free-form color grade when present (richer than a named preset).
    // Fall back to the named preset if the model only voted that.
    let grade_filter = agent_color_grade.as_ref().and_then(build_color_grade_filter);
    let look_label = if grade_filter.is_some() {
        agent_color_grade.as_ref().and_then(|g| g.name.clone()).unwrap_or_else(|| "custom grade".to_string())
    } else {
        agent_look_preset.as_ref().map(|preset| format!("{:?}", preset)).unwrap_or_else(|| "no grade".to_string())
    };
    emit_agent_progress(&app, &started, "rendering", &format!("Rendering {} selected cuts to MP4 ({look_label})...", segments.len()), total_windows, total_windows);
    render_segments_ffmpeg(&project.session, &video_inputs, &segments, &audio_path, range_start, range_end, &path, agent_look_preset.clone(), grade_filter, agent_effects.clone(), false)
        .map_err(|error| {
            emit_agent_progress(&app, &started, "error", "ffmpeg failed while rendering the agent edit.", total_windows, total_windows);
            error
        })?;
    emit_agent_progress(&app, &started, "done", "Agent Video Edit complete.", total_windows, total_windows);
    Ok(AgentVideoEditResponse { path: path.to_string_lossy().to_string(), script, look_preset: agent_look_preset, color_grade: agent_color_grade, video_effects: agent_effects })
}

/// Render a sequence of edit segments (each picking one source clip for a time span)
/// to an MP4 over the supplied mix audio. Shared by the agent edit and the no-agent
/// re-render from an edited script.
fn render_segments_ffmpeg(
    session: &MixSession,
    video_inputs: &[VideoRenderClip],
    segments: &[AutoEditSegment],
    audio_path: &Path,
    range_start: u64,
    range_end: u64,
    output_path: &Path,
    look_override: Option<crate::model::VideoFilterPreset>,
    // Free-form ffmpeg filter chain (already built by build_color_grade_filter, so
    // every segment is clamped & safe). Wraps the final [v] output. None = no grade.
    color_grade_filter: Option<String>,
    // Whole-edit effects (fade in/out, speed) applied AFTER the grade.
    effects: Option<AgentVideoEffects>,
    // Encoder quality. False = `-preset veryfast` + default CRF + AAC default
    // (fast preview); true = `-preset slow -crf 17 -b:a 320k` (visually lossless,
    // ~5-10x slower). Used by the final-export path; preview renders pass false.
    high_quality: bool,
) -> Result<(), String> {
    let mut command = Command::new("ffmpeg");
    command.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error");
    for clip in video_inputs {
        command.arg("-i").arg(&clip.path);
    }
    if range_start > 0 {
        command.arg("-ss").arg(format!("{:.3}", range_start as f64 / session.sample_rate as f64));
    }
    let range_duration_s = range_end.saturating_sub(range_start) as f64 / session.sample_rate as f64;
    command
        .arg("-t")
        .arg(format!("{range_duration_s:.3}"))
        .arg("-i")
        .arg(audio_path);

    let base_filter = build_auto_edit_filter(video_inputs, segments, session, range_start, range_end, look_override);
    let (video_post_chain, audio_chain, _speed) = effects
        .as_ref()
        .map(|e| build_effects_filters(e, range_duration_s))
        .unwrap_or((None, None, 1.0));
    // Build the post-[v] chain: grade then video effects, joined by commas. Both are
    // already comma-separated filter strings, so we can flatten them.
    let mut post_parts: Vec<String> = Vec::new();
    if let Some(g) = color_grade_filter.as_ref().filter(|s| !s.is_empty()) {
        post_parts.push(g.clone());
    }
    if let Some(v) = video_post_chain.as_ref().filter(|s| !s.is_empty()) {
        post_parts.push(v.clone());
    }
    let combined_post = if post_parts.is_empty() { None } else { Some(post_parts.join(",")) };
    let audio_index = video_inputs.len();
    // Wrap the base filter with post-video chain when present.
    let filter_with_video = match combined_post {
        Some(chain) => {
            let mut s = base_filter.replace("[v]", "[vraw]");
            s.push(';');
            s.push_str(&format!("[vraw]{chain},format=yuv420p[v]"));
            s
        }
        None => base_filter,
    };
    // Add audio effects to filter_complex too when present. Otherwise map the audio
    // input directly (unchanged from the pre-effects behavior).
    let (final_filter, audio_map) = match audio_chain {
        Some(chain) if !chain.is_empty() => {
            let mut s = filter_with_video;
            s.push(';');
            s.push_str(&format!("[{audio_index}:a:0]{chain}[a]"));
            (s, "[a]".to_string())
        }
        _ => (filter_with_video, format!("{audio_index}:a:0")),
    };
    command
        .arg("-filter_complex")
        .arg(final_filter)
        .arg("-map")
        .arg("[v]")
        .arg("-map")
        .arg(audio_map);

    command
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-r")
        .arg("30");
    if high_quality {
        // Visually-lossless final export: slow preset, CRF 17, AAC 320 kbps.
        // Longer GOP (60) for better compression — final output is for delivery,
        // not for the in-app scrubber.
        command
            .arg("-preset").arg("slow")
            .arg("-crf").arg("17")
            .arg("-g").arg("60")
            .arg("-c:a").arg("aac")
            .arg("-b:a").arg("320k");
    } else {
        // Fast preview render: short GOP so the in-app `<video>.currentTime = ...`
        // scrubber doesn't decode long GOPs to display one frame.
        command
            .arg("-preset").arg("veryfast")
            .arg("-g").arg("30")
            .arg("-keyint_min").arg("30")
            .arg("-sc_threshold").arg("0")
            .arg("-c:a").arg("aac");
    }
    let output = command
        .arg("-shortest")
        .arg(output_path)
        .output()
        .map_err(|error| format!("Could not run ffmpeg. Install ffmpeg to export video: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg video edit failed: {}", stderr.trim()));
    }
    Ok(())
}

/// Build render segments from an (edited) agent script, reusing each window's chosen
/// track instead of re-running the vision/edit models.
fn build_segments_from_script(
    clips: &[VideoRenderClip],
    script: &[AgentVideoScriptEntry],
    range_start: u64,
    range_end: u64,
    sample_rate: u32,
) -> Vec<AutoEditSegment> {
    let mut segments = Vec::new();
    for entry in script {
        let Some(track_index) = entry.chosen_track_index else { continue };
        let window_start = ((entry.start_seconds * sample_rate as f64).round() as u64).max(range_start);
        let window_end = ((entry.end_seconds * sample_rate as f64).round() as u64).min(range_end);
        if window_end <= window_start {
            continue;
        }
        let Some((input_index, clip)) = clips
            .iter()
            .enumerate()
            .find(|(_, clip)| clip.track_index == track_index && clip.start_sample < window_end && clip.end_sample > window_start)
        else {
            continue;
        };
        let segment_start = window_start.max(clip.start_sample);
        let segment_end = window_end.min(clip.end_sample);
        if segment_end <= segment_start {
            continue;
        }
        let source_offset_ms = clip.source_offset_ms.saturating_add(
            (((segment_start.saturating_sub(clip.start_sample)) as f64 / sample_rate as f64) * 1000.0).round() as u64,
        );
        segments.push(AutoEditSegment {
            input_index,
            timeline_start: segment_start,
            timeline_end: segment_end,
            source_offset_ms,
        });
    }
    segments
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderFromScriptResponse {
    path: String,
    duration_ms: u64,
}

/// Render a video edit from a script — no vision/edit LLMs. Returns the rendered mp4
/// path and duration; the frontend then attaches it to a session as a new track or
/// replaces an existing track's clip.
#[tauri::command]
pub fn render_video_from_script(
    state: State<'_, AppState>,
    session_id: String,
    source_track_ids: Vec<String>,
    start_sample: Option<u64>,
    end_sample: Option<u64>,
    script: Vec<AgentVideoScriptEntry>,
    look_preset: Option<crate::model::VideoFilterPreset>,
    color_grade: Option<AgentColorGrade>,
    video_effects: Option<AgentVideoEffects>,
    // "high" = final-export encoder (slow + CRF 17 + AAC 320k); anything else
    // (None / "fast" / "preview") = fast preview encoder.
    quality: Option<String>,
) -> Result<RenderFromScriptResponse, String> {
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    let full_end = session_duration_samples(&project.session);
    let range_start = start_sample.unwrap_or(0).min(full_end);
    let range_end = end_sample.unwrap_or(full_end).min(full_end).max(range_start + 1);
    let selected_track_ids = source_track_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<HashSet<_>>();
    if selected_track_ids.is_empty() {
        return Err("No source video tracks to render from.".into());
    }
    let video_inputs = collect_video_inputs(&project.session, range_start, range_end, &selected_track_ids);
    if video_inputs.is_empty() {
        return Err("The source video tracks have no clips in the edit range.".into());
    }
    let mut segments = build_segments_from_script(&video_inputs, &script, range_start, range_end, project.session.sample_rate);
    if segments.is_empty() {
        let interval_samples = (project.session.sample_rate as u64).max(1);
        segments = build_auto_edit_segments(&video_inputs, range_start, range_end, interval_samples, project.session.sample_rate);
    }
    if segments.is_empty() {
        return Err("No visible source clips in the edit range.".into());
    }

    let renders_dir = state.store.lock().map_err(|error| error.to_string())?.renders_dir();
    fs::create_dir_all(&renders_dir).map_err(|error| error.to_string())?;
    let audio_path = renders_dir.join(format!("{session_id}.video-edit.wav"));
    audio::render_mix(&project.session, &audio_path)?;
    let output_path = renders_dir.join(format!("agent-edit-{}.mp4", uuid::Uuid::new_v4()));
    let grade_filter = color_grade.as_ref().and_then(build_color_grade_filter);
    let high_quality = quality.as_deref() == Some("high");
    render_segments_ffmpeg(&project.session, &video_inputs, &segments, &audio_path, range_start, range_end, &output_path, look_preset, grade_filter, video_effects, high_quality)?;

    let duration_ms = (((range_end.saturating_sub(range_start)) as f64 / project.session.sample_rate as f64) * 1000.0).round() as u64;
    Ok(RenderFromScriptResponse {
        path: output_path.to_string_lossy().to_string(),
        duration_ms: duration_ms.max(1),
    })
}

/// Re-render the agent edit from an (optionally edited) script without invoking the
/// vision/edit models, then replace the clip on the existing agent-edit track in place.
/// `source_track_ids` are the original camera tracks the script refers to.
#[tauri::command]
pub fn rerender_agent_edit(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    clip_id: String,
    source_track_ids: Vec<String>,
    start_sample: Option<u64>,
    end_sample: Option<u64>,
    script: Vec<AgentVideoScriptEntry>,
    look_preset: Option<crate::model::VideoFilterPreset>,
    color_grade: Option<AgentColorGrade>,
    video_effects: Option<AgentVideoEffects>,
) -> Result<MixProject, String> {
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    let full_end = session_duration_samples(&project.session);
    let range_start = start_sample.unwrap_or(0).min(full_end);
    let range_end = end_sample.unwrap_or(full_end).min(full_end).max(range_start + 1);
    let selected_track_ids = source_track_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty() && id != &track_id)
        .collect::<HashSet<_>>();
    if selected_track_ids.is_empty() {
        return Err("No source video tracks to re-render from.".into());
    }
    let video_inputs = collect_video_inputs(&project.session, range_start, range_end, &selected_track_ids);
    if video_inputs.is_empty() {
        return Err("The source video tracks have no clips in the edit range.".into());
    }
    let mut segments = build_segments_from_script(&video_inputs, &script, range_start, range_end, project.session.sample_rate);
    if segments.is_empty() {
        // No usable per-window choices in the script; fall back to automatic 1s cuts.
        let interval_samples = (project.session.sample_rate as u64).max(1);
        segments = build_auto_edit_segments(&video_inputs, range_start, range_end, interval_samples, project.session.sample_rate);
    }
    if segments.is_empty() {
        return Err("No visible source clips in the edit range.".into());
    }

    let renders_dir = state.store.lock().map_err(|error| error.to_string())?.renders_dir();
    fs::create_dir_all(&renders_dir).map_err(|error| error.to_string())?;
    let audio_path = renders_dir.join(format!("{session_id}.video-edit.wav"));
    audio::render_mix(&project.session, &audio_path)?;
    let output_path = renders_dir.join(format!("rerender-{}.mp4", uuid::Uuid::new_v4()));
    let grade_filter = color_grade.as_ref().and_then(build_color_grade_filter);
    // Re-render path stays at preview quality — it's invoked from Look chip clicks
    // and similar quick iteration. Final export uses render_video_from_script(quality=high).
    render_segments_ffmpeg(&project.session, &video_inputs, &segments, &audio_path, range_start, range_end, &output_path, look_preset, grade_filter, video_effects, false)?;

    let duration_ms = (((range_end.saturating_sub(range_start)) as f64 / project.session.sample_rate as f64) * 1000.0).round() as u64;
    let updated = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .replace_track_video(&session_id, &track_id, &clip_id, &output_path, duration_ms.max(1))?;
    let _ = fs::remove_file(&output_path);
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&updated.session)?;
        sync_session_to_engine(&mut audio, &updated.session);
        audio.publish_automation(&updated.session);
    }
    Ok(updated)
}

/// Read a video file's pixel dimensions via ffprobe.
fn probe_video_dimensions(path: &Path) -> Result<(u32, u32), String> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height",
            "-of", "csv=p=0:s=x",
        ])
        .arg(path)
        .output()
        .map_err(|error| format!("Could not run ffprobe. Install ffmpeg/ffprobe: {error}"))?;
    if !output.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    let mut parts = line.split('x');
    let width: u32 = parts.next().and_then(|value| value.trim().parse().ok()).ok_or("Could not read video width")?;
    let height: u32 = parts.next().and_then(|value| value.trim().parse().ok()).ok_or("Could not read video height")?;
    if width == 0 || height == 0 {
        return Err("Video reported zero dimensions".into());
    }
    Ok((width, height))
}

/// Set the session's video canvas to match the source footage resolution, so cut-style
/// edits render close to 1:1 instead of upscaling a small camera into a huge frame.
/// Uses the smallest-area source (the camera footage, not a previously rendered edit).
#[tauri::command]
pub fn fit_canvas_to_footage(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    let by_id: std::collections::HashMap<&str, &crate::model::VideoSourceFile> =
        project.session.video_source_files.iter().map(|source| (source.id.as_str(), source)).collect();
    let mut best: Option<(u32, u32)> = None;
    for track in &project.session.tracks {
        if track.kind != crate::model::TrackKind::Video {
            continue;
        }
        for clip in &track.video_clips {
            if let Some(source) = by_id.get(clip.video_source_file_id.as_str()) {
                if let Ok((width, height)) = probe_video_dimensions(Path::new(&source.path)) {
                    if best.map_or(true, |(bw, bh)| (width as u64 * height as u64) < (bw as u64 * bh as u64)) {
                        best = Some((width, height));
                    }
                }
            }
        }
    }
    let (width, height) = best.ok_or("No readable video footage found to size the canvas.")?;
    project.session.video_canvas.width = even_dimension((width as i32).clamp(240, 3840)) as u32;
    project.session.video_canvas.height = even_dimension((height as i32).clamp(240, 3840)) as u32;
    store.save(&project)?;
    Ok(project)
}

#[tauri::command]
pub async fn judge_mix_ab(
    state: State<'_, AppState>,
    session_id: String,
    options: AbJudgeOptions,
) -> Result<AbJudgeResponse, String> {
    let project = {
        let store = state.store.lock().map_err(|error| error.to_string())?;
        store.get_project(&session_id)?
    };
    let temp_dir = state.config.data_dir.join("ab-judge");
    crate::ab_judge::judge_session(&project.session, &temp_dir, options).await
}

fn analyze_section_window(
    render: &crate::engine::render::RenderedMix,
    start_seconds: f32,
    end_seconds: f32,
) -> Option<SectionAnalysis> {
    if end_seconds <= start_seconds {
        return None;
    }
    let ch = render.channels.max(1) as usize;
    let sr = render.sample_rate as f32;
    let start_idx = ((start_seconds * sr).max(0.0) as usize).saturating_mul(ch);
    let end_idx = ((end_seconds * sr).max(0.0) as usize).saturating_mul(ch);
    let end_idx = end_idx.min(render.samples.len());
    if end_idx <= start_idx + ch * 2 {
        return None;
    }
    let slice = &render.samples[start_idx..end_idx];
    let a = crate::engine::source::analysis::analyze(slice, render.channels, render.sample_rate);
    Some(SectionAnalysis {
        peak_db: round1(a.peak_db),
        rms_db: round1(a.rms_db),
        lufs: round1(a.lufs),
        spectral_centroid_hz: a.spectral_centroid_hz.round(),
        low_energy: round2(a.low_energy),
        mid_energy: round2(a.mid_energy),
        high_energy: round2(a.high_energy),
        dynamic_range_db: round1(a.dynamic_range_db),
    })
}

fn round1(x: f32) -> f32 { if x.is_finite() { (x * 10.0).round() / 10.0 } else { 0.0 } }
fn round2(x: f32) -> f32 { if x.is_finite() { (x * 100.0).round() / 100.0 } else { 0.0 } }

fn emit_agent_progress(
    app: &AppHandle,
    started: &std::time::Instant,
    stage: &str,
    message: &str,
    current: u32,
    total: u32,
) {
    let _ = app.emit(
        "agent-video:progress",
        AgentVideoProgress {
            stage: stage.to_string(),
            message: message.to_string(),
            current,
            total: total.max(1),
            elapsed_seconds: started.elapsed().as_secs_f32(),
        },
    );
}

fn normalize_wav_path(path: PathBuf) -> PathBuf {
    if path.extension().and_then(|item| item.to_str()).is_some() {
        path
    } else {
        path.with_extension("wav")
    }
}

fn normalize_mp4_path(path: PathBuf) -> PathBuf {
    if path.extension().and_then(|item| item.to_str()).is_some() {
        path
    } else {
        path.with_extension("mp4")
    }
}

fn video_extension(file_name: &str, mime_type: &str) -> &'static str {
    if file_name.to_ascii_lowercase().ends_with(".mp4") || mime_type.contains("mp4") {
        "mp4"
    } else {
        "webm"
    }
}

fn extract_video_audio(video_path: &Path, audio_path: &Path, sample_rate: u32, source_offset_ms: u64) -> Result<(), String> {
    let mut command = Command::new("ffmpeg");
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error");
    if source_offset_ms > 0 {
        command.arg("-ss").arg(format!("{:.3}", source_offset_ms as f64 / 1000.0));
    }
    let output = command
        .arg("-i")
        .arg(video_path)
        .arg("-vn")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-ar")
        .arg(sample_rate.to_string())
        .arg("-ac")
        .arg("2")
        .arg(audio_path)
        .output()
        .map_err(|error| format!("Could not run ffmpeg to extract camera audio: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg camera audio extraction failed: {}", stderr.trim()));
    }
    Ok(())
}

fn collect_video_inputs(session: &MixSession, range_start: u64, range_end: u64, selected_track_ids: &HashSet<String>) -> Vec<VideoRenderClip> {
    let by_id: std::collections::HashMap<&str, &crate::model::VideoSourceFile> =
        session.video_source_files.iter().map(|source| (source.id.as_str(), source)).collect();
    let mut clips = Vec::new();
    for (track_index, track) in session.tracks.iter().enumerate() {
        if track.kind != crate::model::TrackKind::Video || track.muted || !selected_track_ids.contains(&track.id) {
            continue;
        }
        for clip in &track.video_clips {
            if clip.end_sample <= range_start || clip.start_sample >= range_end {
                continue;
            }
            if let Some(source) = by_id.get(clip.video_source_file_id.as_str()) {
                let trimmed_start = clip.start_sample.max(range_start);
                let trimmed_end = clip.end_sample.min(range_end);
                let offset_ms = clip.source_offset_ms.saturating_add(
                    (((trimmed_start.saturating_sub(clip.start_sample)) as f64 / session.sample_rate as f64) * 1000.0).round() as u64
                );
                clips.push(VideoRenderClip {
                    track_id: track.id.clone(),
                    track_index,
                    track_name: track.name.clone(),
                    path: PathBuf::from(&source.path),
                    start_sample: trimmed_start,
                    end_sample: trimmed_end,
                    source_offset_ms: offset_ms,
                    layout: clip.layout.clone().unwrap_or_else(|| default_video_layout(track_index)),
                });
            }
        }
    }
    clips.sort_by(|a, b| {
        a.layout
            .z_index
            .cmp(&b.layout.z_index)
            .then_with(|| a.start_sample.cmp(&b.start_sample))
    });
    clips
}

fn default_video_layout(index: usize) -> VideoLayout {
    if index == 0 {
        return VideoLayout::default();
    }
    let slot = (index - 1) % 4;
    let mut layout = default_video_layout(0);
    layout.x = if slot == 0 || slot == 2 { 64.0 } else { 4.0 };
    layout.y = if slot < 2 { 5.0 } else { 55.0 };
    layout.width = 32.0;
    layout.height = 40.0;
    layout.z_index = index as i32;
    layout
}

/// Pick a default look + color grade by scanning the user's instructions for known
/// style keywords. Runs BEFORE the per-window vision calls and seeds the pipeline so
/// the user gets a real grade even when the model is too conservative to emit one.
/// The vision model can still override per-window. Match is case-insensitive and on
/// whole-word boundaries via simple substring on a lowercase copy — good enough for
/// chat-style prompts. Returns (preset, grade) where either may be None.
pub fn infer_look_from_instructions(text: &str) -> (Option<crate::model::VideoFilterPreset>, Option<AgentColorGrade>) {
    use crate::model::VideoFilterPreset;
    let t = text.to_lowercase();
    let has = |needle: &str| t.contains(needle);
    let mut preset: Option<VideoFilterPreset> = None;
    let mut name_parts: Vec<&'static str> = Vec::new();
    // Build a grade by accumulating per-keyword nudges. Multiple keywords compound.
    let mut brightness: Option<f32> = None;
    let mut contrast: Option<f32> = None;
    let mut saturation: Option<f32> = None;
    let mut gamma: Option<f32> = None;
    let mut rr: Option<f32> = None;
    let mut gg: Option<f32> = None;
    let mut bb: Option<f32> = None;
    let mut vignette: Option<f32> = None;
    let mut sharpen: Option<f32> = None;
    let mut blur: Option<f32> = None;
    let mut grain: Option<f32> = None;
    let mut hue_shift: Option<f32> = None;

    // Macro looks (preset + grade defaults).
    if has("cinema") || has("cinematic") || has("epic") || has("film") || has("movie") || has("blockbuster") {
        preset = Some(VideoFilterPreset::Cinema);
        name_parts.push("epic cinema");
        contrast = Some(1.12);
        saturation = Some(1.04);
        rr = Some(1.08); gg = Some(0.96); bb = Some(0.88); // mild teal-orange
        vignette = Some(0.22);
        sharpen = Some(0.45);
        grain = Some(1.5);
    }
    if has("teal") || has("teal-and-orange") || has("teal and orange") {
        preset = Some(VideoFilterPreset::Cinema);
        rr = Some(rr.unwrap_or(1.0).max(1.10));
        bb = Some(bb.unwrap_or(1.0).min(0.85));
        name_parts.push("teal-and-orange");
    }
    if has("warm") || has("sunset") || has("golden hour") {
        preset = preset.or(Some(VideoFilterPreset::Warm));
        rr = Some(rr.unwrap_or(1.0).max(1.06));
        bb = Some(bb.unwrap_or(1.0).min(0.94));
        name_parts.push("warm");
    }
    if has("golden") {
        preset = Some(VideoFilterPreset::Golden);
        rr = Some(1.10); gg = Some(1.02); bb = Some(0.82);
        saturation = Some(saturation.unwrap_or(1.0).max(1.10));
        name_parts.push("golden");
    }
    if has("cool") || has("blue") || has("icy") {
        preset = preset.or(Some(VideoFilterPreset::Cool));
        rr = Some(rr.unwrap_or(1.0).min(0.94));
        bb = Some(bb.unwrap_or(1.0).max(1.08));
        name_parts.push("cool");
    }
    if has("cold") {
        preset = Some(VideoFilterPreset::Cold);
        rr = Some(0.84); gg = Some(0.95); bb = Some(1.16);
        name_parts.push("cold");
    }
    if has("mono") || has("black and white") || has("b&w") || has("grayscale") || has("greyscale") {
        preset = Some(VideoFilterPreset::Mono);
        saturation = Some(0.0);
        name_parts.push("monochrome");
    }
    if has("noir") {
        preset = Some(VideoFilterPreset::Noir);
        saturation = Some(0.0);
        contrast = Some(contrast.unwrap_or(1.0).max(1.28));
        name_parts.push("noir");
    }
    if has("punch") || has("vibrant") || has("punchy") {
        preset = preset.or(Some(VideoFilterPreset::Punch));
        contrast = Some(contrast.unwrap_or(1.0).max(1.14));
        saturation = Some(saturation.unwrap_or(1.0).max(1.14));
        name_parts.push("punchy");
    }
    if has("dream") || has("soft") {
        preset = preset.or(Some(VideoFilterPreset::Dream));
        blur = Some(1.0);
        brightness = Some(brightness.unwrap_or(0.0) + 0.04);
        saturation = Some(saturation.unwrap_or(1.0).min(0.92));
        name_parts.push("dreamy");
    }
    if has("moody") || has("dark") || has("serious") {
        preset = preset.or(Some(VideoFilterPreset::Moody));
        brightness = Some(brightness.unwrap_or(0.0) - 0.05);
        contrast = Some(contrast.unwrap_or(1.0).max(1.16));
        saturation = Some(saturation.unwrap_or(1.0).min(0.88));
        name_parts.push("moody");
    }
    if has("vintage") || has("retro") {
        preset = preset.or(Some(VideoFilterPreset::Vintage));
        contrast = Some(0.94);
        saturation = Some(0.70);
        rr = Some(rr.unwrap_or(1.0).max(1.06));
        gg = Some(gg.unwrap_or(1.0).min(0.98));
        bb = Some(bb.unwrap_or(1.0).min(0.86));
        grain = Some(grain.unwrap_or(0.0).max(3.0));
        name_parts.push("vintage");
    }

    // Modifier knobs — words that nudge the grade without choosing a preset.
    if has("bright") || has("brighter") || has("more white") || has("whiter") || has("lifted") {
        brightness = Some(brightness.unwrap_or(0.0) + 0.05);
        gamma = Some(gamma.unwrap_or(1.0).max(1.05));
        name_parts.push("brighter");
    }
    if has("dim") || has("darker") || has("dimmer") {
        brightness = Some(brightness.unwrap_or(0.0) - 0.05);
        name_parts.push("darker");
    }
    if has("clear") || has("clearer") || has("sharp") || has("sharper") || has("crisp") {
        sharpen = Some(sharpen.unwrap_or(0.0).max(0.6));
        name_parts.push("crisp");
    }
    if has("desaturat") || has("muted") || has("faded") {
        saturation = Some(saturation.unwrap_or(1.0).min(0.75));
        name_parts.push("desaturated");
    }
    if has("vivid") || has("saturated") || has("punchy") || has("pop") {
        // A strong, clearly-visible boost — this is what people mean by "vivid".
        saturation = Some(saturation.unwrap_or(1.0).max(1.35));
        contrast = Some(contrast.unwrap_or(1.0).max(1.06));
        name_parts.push("vivid");
    } else if has("vibrant") || has("colorful") || has("colourful") {
        saturation = Some(saturation.unwrap_or(1.0).max(1.18));
        name_parts.push("vibrant");
    }
    if has("vignette") {
        vignette = Some(vignette.unwrap_or(0.0).max(0.30));
    }
    if has("grain") || has("film grain") {
        grain = Some(grain.unwrap_or(0.0).max(3.0));
    }
    if has("hue shift") {
        hue_shift = Some(20.0);
    }

    let nothing_set =
        brightness.is_none() && contrast.is_none() && saturation.is_none() && gamma.is_none()
        && rr.is_none() && gg.is_none() && bb.is_none() && vignette.is_none()
        && sharpen.is_none() && blur.is_none() && grain.is_none() && hue_shift.is_none();
    if nothing_set && preset.is_none() {
        return (None, None);
    }
    let name = if name_parts.is_empty() { None } else { Some(name_parts.join(" + ")) };
    let reason = if name_parts.is_empty() {
        None
    } else {
        Some(format!(
            "Inferred from instruction keywords ({}) — vision model may refine these per window.",
            name_parts.join(", ")
        ))
    };
    let rgb_mix = if rr.is_some() || gg.is_some() || bb.is_some() {
        Some(RgbMix { rr, gg, bb })
    } else { None };
    let grade = AgentColorGrade {
        name,
        reason,
        brightness, contrast, saturation, gamma,
        rgb_mix,
        hue_shift, vignette, blur, sharpen, grain,
    };
    let grade = if build_color_grade_filter(&grade).is_some() { Some(grade) } else { None };
    (preset, grade)
}

/// Scan the user's instructions for video-effect keywords ("fade in", "fade out",
/// "speed up", "slow down") and produce a default AgentVideoEffects. The LLM can
/// override; if it doesn't, this keeps the user's natural request honored.
/// Numeric capture: matches "<number>(s|sec|seconds)" near the phrase so the user
/// can say "fade out 2s" or "half-second fade in" (parsed as 0.5 via the word match).
pub fn infer_effects_from_instructions(text: &str) -> Option<AgentVideoEffects> {
    let t = text.to_lowercase();
    let mut effects = AgentVideoEffects::default();
    let mut hits: Vec<String> = Vec::new();

    // Tiny helper: find the first number (int or decimal) in a substring window
    // around the keyword. Returns seconds. Falls back to a default when missing.
    fn nearby_seconds(text: &str, anchor: &str, default_s: f32) -> f32 {
        let Some(at) = text.find(anchor) else { return default_s; };
        // 30 chars on each side is plenty for "fade out at the end 1.5s".
        let lo = at.saturating_sub(30);
        let hi = (at + anchor.len() + 30).min(text.len());
        let window = &text[lo..hi];
        // Greedy first match for `\d+(?:\.\d+)?` without a regex dep.
        let mut acc = String::new();
        let mut started = false;
        for ch in window.chars() {
            if ch.is_ascii_digit() || (ch == '.' && started && !acc.contains('.')) {
                acc.push(ch);
                started = true;
            } else if started {
                break;
            }
        }
        if acc.is_empty() {
            // Word-number fallback for "half" / "one" / "two" / "three" near the anchor.
            if window.contains("half") { return 0.5; }
            if window.contains("one ") || window.ends_with("one") { return 1.0; }
            if window.contains("two") { return 2.0; }
            if window.contains("three") { return 3.0; }
            return default_s;
        }
        acc.parse::<f32>().unwrap_or(default_s)
    }

    if t.contains("fade in") || t.contains("fade-in") || t.contains("fadein") {
        let s = nearby_seconds(&t, "fade in", 1.5).clamp(0.0, 10.0);
        effects.fade_in_seconds = Some(s);
        hits.push(format!("fade in {s:.1}s"));
    }
    if t.contains("fade out") || t.contains("fade-out") || t.contains("fadeout") {
        let s = nearby_seconds(&t, "fade out", 1.5).clamp(0.0, 10.0);
        effects.fade_out_seconds = Some(s);
        hits.push(format!("fade out {s:.1}s"));
    }
    // Generic "fade" without direction = apply to both ends.
    if (t.contains("fade") || t.contains("dissolve")) && effects.fade_in_seconds.is_none() && effects.fade_out_seconds.is_none() {
        effects.fade_in_seconds = Some(1.0);
        effects.fade_out_seconds = Some(1.5);
        hits.push("fade in/out".to_string());
    }
    if t.contains("speed up") || t.contains("faster") || t.contains("speed it up") {
        // Optional explicit factor "2x"/"1.5x" near the phrase.
        let factor = {
            let f = if let Some(at) = t.find('x') {
                let lo = at.saturating_sub(6);
                let window = &t[lo..at];
                let mut acc = String::new();
                let mut started = false;
                for ch in window.chars().rev() {
                    if ch.is_ascii_digit() || (ch == '.' && started && !acc.contains('.')) {
                        acc.insert(0, ch);
                        started = true;
                    } else if started {
                        break;
                    }
                }
                acc.parse::<f32>().ok()
            } else { None };
            f.unwrap_or(1.5)
        };
        effects.speed_factor = Some(factor.clamp(1.0, 4.0));
        hits.push(format!("speed {:.2}x", effects.speed_factor.unwrap()));
    }
    if t.contains("slow down") || t.contains("slower") || t.contains("slow-mo") {
        effects.speed_factor = Some(0.5);
        hits.push("slow down 0.50x".to_string());
    }

    if hits.is_empty() { return None; }
    effects.reason = Some(format!(
        "Inferred from instruction keywords ({}) — applied to the final video.",
        hits.join(", ")
    ));
    Some(effects)
}

/// Translate an `AgentVideoEffects` into ready-to-use ffmpeg filter chains for both
/// the video and audio streams. Returns `(video_chain, audio_chain, applied_speed)`.
/// `total_duration_s` is the unscaled length of the source range; the returned chain
/// places the fade-out at the post-speed final duration so it lands at the very end.
pub fn build_effects_filters(effects: &AgentVideoEffects, total_duration_s: f64) -> (Option<String>, Option<String>, f32) {
    fn clamp_finite(v: Option<f32>, lo: f32, hi: f32) -> Option<f32> {
        let x = v?;
        if !x.is_finite() { return None; }
        Some(x.clamp(lo, hi))
    }
    let speed = clamp_finite(effects.speed_factor, 0.25, 4.0).unwrap_or(1.0);
    let fade_in = clamp_finite(effects.fade_in_seconds, 0.0, 10.0).filter(|s| *s > 0.001);
    let fade_out = clamp_finite(effects.fade_out_seconds, 0.0, 10.0).filter(|s| *s > 0.001);
    let final_duration = (total_duration_s / speed as f64).max(0.01);

    let mut vparts: Vec<String> = Vec::new();
    if (speed - 1.0).abs() > 0.001 {
        // setpts multiplies the PTS — to play faster (speed > 1) we shrink PTS by 1/speed.
        vparts.push(format!("setpts={:.4}*PTS", 1.0 / speed));
    }
    if let Some(s) = fade_in {
        // Fade from black at the start.
        vparts.push(format!("fade=t=in:st=0:d={s:.3}"));
    }
    if let Some(s) = fade_out {
        let st = ((final_duration as f32) - s).max(0.0);
        vparts.push(format!("fade=t=out:st={st:.3}:d={s:.3}"));
    }
    let video_chain = if vparts.is_empty() { None } else { Some(vparts.join(",")) };

    let mut aparts: Vec<String> = Vec::new();
    if (speed - 1.0).abs() > 0.001 {
        // atempo is limited to [0.5, 100.0] per filter, so we chain stages when the
        // requested factor falls outside that range. With our clamp 0.25..4 we need at
        // most one extra stage on the slow side and one on the fast side.
        let mut remaining = speed;
        while remaining > 2.0 {
            aparts.push("atempo=2.0".to_string());
            remaining /= 2.0;
        }
        while remaining < 0.5 {
            aparts.push("atempo=0.5".to_string());
            remaining /= 0.5;
        }
        if (remaining - 1.0).abs() > 0.001 {
            aparts.push(format!("atempo={remaining:.3}"));
        }
    }
    if let Some(s) = fade_in {
        aparts.push(format!("afade=t=in:st=0:d={s:.3}"));
    }
    if let Some(s) = fade_out {
        let st = ((final_duration as f32) - s).max(0.0);
        aparts.push(format!("afade=t=out:st={st:.3}:d={s:.3}"));
    }
    let audio_chain = if aparts.is_empty() { None } else { Some(aparts.join(",")) };
    (video_chain, audio_chain, speed)
}

/// Build a safe ffmpeg filter chain from the agent's free-form color-grade
/// parameters. Every field is clamped to a vetted range and any NaN/inf is
/// dropped. Returns None when the grade has nothing meaningful set (so the
/// caller can skip the `-vf`/post-process step entirely).
pub fn build_color_grade_filter(grade: &AgentColorGrade) -> Option<String> {
    fn clamp_finite(value: Option<f32>, lo: f32, hi: f32) -> Option<f32> {
        let v = value?;
        if !v.is_finite() { return None; }
        Some(v.clamp(lo, hi))
    }
    let brightness = clamp_finite(grade.brightness, -0.5, 0.5);
    let contrast = clamp_finite(grade.contrast, 0.4, 2.0);
    let saturation = clamp_finite(grade.saturation, 0.0, 2.5);
    let gamma = clamp_finite(grade.gamma, 0.5, 1.8);
    let hue_shift = clamp_finite(grade.hue_shift, -180.0, 180.0);
    let vignette = clamp_finite(grade.vignette, 0.0, 1.0);
    let blur = clamp_finite(grade.blur, 0.0, 8.0);
    let sharpen = clamp_finite(grade.sharpen, 0.0, 2.0);
    let grain = clamp_finite(grade.grain, 0.0, 30.0);
    let rgb = grade.rgb_mix.as_ref().map(|m| (
        clamp_finite(m.rr, 0.4, 1.6),
        clamp_finite(m.gg, 0.4, 1.6),
        clamp_finite(m.bb, 0.4, 1.6),
    ));
    let mut parts: Vec<String> = Vec::new();
    // eq: only emit fields that actually move the picture.
    let has_eq = brightness.is_some() || contrast.is_some() || saturation.is_some() || gamma.is_some();
    if has_eq {
        let b = brightness.unwrap_or(0.0);
        let c = contrast.unwrap_or(1.0);
        let s = saturation.unwrap_or(1.0);
        let g = gamma.unwrap_or(1.0);
        parts.push(format!("eq=brightness={b:.3}:contrast={c:.3}:saturation={s:.3}:gamma={g:.3}"));
    }
    if let Some((rr, gg, bb)) = rgb {
        if rr.is_some() || gg.is_some() || bb.is_some() {
            parts.push(format!(
                "colorchannelmixer=rr={:.3}:gg={:.3}:bb={:.3}",
                rr.unwrap_or(1.0), gg.unwrap_or(1.0), bb.unwrap_or(1.0)
            ));
        }
    }
    if let Some(h) = hue_shift { if h.abs() >= 0.5 { parts.push(format!("hue=h={h:.2}")); } }
    if let Some(v) = vignette { if v >= 0.05 {
        // Map 0..1 to ffmpeg's angle param 0..PI/3 — gentle range.
        let angle = (v as f64) * std::f64::consts::FRAC_PI_3;
        parts.push(format!("vignette=angle={angle:.4}"));
    }}
    if let Some(b) = blur { if b >= 0.5 { parts.push(format!("gblur=sigma={b:.2}")); } }
    if let Some(s) = sharpen { if s >= 0.05 {
        // unsharp: positive amount sharpens. 5x5 luma matrix, mild chroma.
        parts.push(format!("unsharp=5:5:{:.2}:5:5:0.0", s));
    }}
    if let Some(g) = grain { if g >= 0.5 { parts.push(format!("noise=alls={g:.1}:allf=t")); } }
    if parts.is_empty() { None } else { Some(parts.join(",")) }
}

/// Map a preset name string (as returned by the agent vision model) to a
/// VideoFilterPreset variant. Returns None for "none" or anything unrecognized.
fn parse_video_filter_preset(name: &str) -> Option<crate::model::VideoFilterPreset> {
    use crate::model::VideoFilterPreset;
    match name.trim().to_lowercase().as_str() {
        "warm" => Some(VideoFilterPreset::Warm),
        "cool" => Some(VideoFilterPreset::Cool),
        "mono" => Some(VideoFilterPreset::Mono),
        "punch" => Some(VideoFilterPreset::Punch),
        "dream" => Some(VideoFilterPreset::Dream),
        "cinema" => Some(VideoFilterPreset::Cinema),
        "noir" => Some(VideoFilterPreset::Noir),
        "moody" => Some(VideoFilterPreset::Moody),
        "vintage" => Some(VideoFilterPreset::Vintage),
        "golden" => Some(VideoFilterPreset::Golden),
        "cold" => Some(VideoFilterPreset::Cold),
        _ => None,
    }
}

/// Compute the target (width, height) for a letterbox/pillarbox fit so the W×H
/// source canvas fits inside a box with the requested aspect, without cropping.
/// Returns None for "original" or unknown values.
fn aspect_target_box(out_w: i32, out_h: i32, aspect: Option<&str>) -> Option<(i32, i32)> {
    let aspect = aspect?;
    let (t_w, t_h) = match aspect {
        "square" => {
            let side = out_w.max(out_h);
            (side, side)
        }
        // 9:16 portrait: enlarge whichever dimension is needed so the box ratio is exactly 9/16.
        "portrait916" => {
            let cand_h_from_w = (out_w as f64 * 16.0 / 9.0).ceil() as i32;
            if cand_h_from_w >= out_h {
                (out_w, cand_h_from_w)
            } else {
                let cand_w_from_h = (out_h as f64 * 9.0 / 16.0).ceil() as i32;
                (cand_w_from_h, out_h)
            }
        }
        _ => return None,
    };
    Some((even_dimension(t_w), even_dimension(t_h)))
}

fn build_video_filter(clips: &[VideoRenderClip], session: &MixSession, range_start: u64, range_end: u64, aspect: Option<&str>) -> String {
    let sample_rate = session.sample_rate;
    let canvas = &session.video_canvas;
    let reference_w = canvas.width.clamp(240, 3840) as i32;
    let reference_h = canvas.height.clamp(240, 3840) as i32;
    let layouts: Vec<VideoLayout> = clips.iter().map(|clip| normalized_video_layout(&clip.layout)).collect();
    let (min_x, min_y, max_x, max_y) = content_bounds(&layouts);
    let output_w = even_dimension(pct_to_px((max_x - min_x).max(1.0), reference_w).clamp(240, 7680));
    let output_h = even_dimension(pct_to_px((max_y - min_y).max(1.0), reference_h).clamp(240, 7680));
    let background = ffmpeg_color(&canvas.background);
    let duration = ((range_end.saturating_sub(range_start) as f64 / sample_rate as f64).max(0.1)) + 0.1;
    let mut filter = format!("color=c={background}:s={output_w}x{output_h}:d={duration:.3}[base0]");
    for (index, clip) in clips.iter().enumerate() {
        let layout = layouts[index].clone();
        let start = clip.start_sample.saturating_sub(range_start) as f64 / sample_rate as f64;
        let clip_duration = clip.end_sample.saturating_sub(clip.start_sample) as f64 / sample_rate as f64;
        let source_offset = clip.source_offset_ms as f64 / 1000.0;
        let out_w = even_dimension(pct_to_px(layout.width, reference_w).max(2));
        let out_h = even_dimension(pct_to_px(layout.height, reference_h).max(2));
        let crop_w = (1.0 - ((layout.crop_left + layout.crop_right).min(90.0) / 100.0)).max(0.05);
        let crop_h = (1.0 - ((layout.crop_top + layout.crop_bottom).min(90.0) / 100.0)).max(0.05);
        let crop_x = (layout.crop_left / 100.0).clamp(0.0, 0.9);
        let crop_y = (layout.crop_top / 100.0).clamp(0.0, 0.9);
        let mut chain = format!(
            "[{index}:v]trim=start={source_offset:.3}:duration={clip_duration:.3},setpts=PTS-STARTPTS+{start:.3}/TB,crop=iw*{crop_w:.5}:ih*{crop_h:.5}:iw*{crop_x:.5}:ih*{crop_y:.5},scale={out_w}:{out_h}:force_original_aspect_ratio=increase,crop={out_w}:{out_h},setsar=1"
        );
        let eq_brightness = (layout.brightness - 1.0).clamp(-0.8, 0.8);
        chain.push_str(&format!(",eq=brightness={eq_brightness:.3}:contrast={:.3}:saturation={:.3}", layout.contrast.clamp(0.2, 2.0), layout.saturation.clamp(0.0, 2.0)));
        match layout.preset {
            VideoFilterPreset::Warm => chain.push_str(",colorchannelmixer=rr=1.06:gg=1.01:bb=0.94"),
            VideoFilterPreset::Cool => chain.push_str(",colorchannelmixer=rr=0.94:gg=1.01:bb=1.08"),
            VideoFilterPreset::Mono => chain.push_str(",hue=s=0"),
            VideoFilterPreset::Punch => chain.push_str(",eq=contrast=1.12:saturation=1.14"),
            VideoFilterPreset::Dream => chain.push_str(",boxblur=1:1,eq=brightness=0.04:saturation=0.82"),
            VideoFilterPreset::Cinema => chain.push_str(",eq=contrast=1.10:saturation=1.05,colorchannelmixer=rr=1.08:gg=0.98:bb=0.90"),
            VideoFilterPreset::Noir => chain.push_str(",eq=contrast=1.28,hue=s=0"),
            VideoFilterPreset::Moody => chain.push_str(",eq=brightness=-0.06:contrast=1.18:saturation=0.85,colorchannelmixer=rr=0.93:gg=0.97:bb=1.08"),
            VideoFilterPreset::Vintage => chain.push_str(",eq=contrast=0.94:saturation=0.70,colorchannelmixer=rr=1.06:gg=0.98:bb=0.86"),
            VideoFilterPreset::Golden => chain.push_str(",eq=brightness=0.04:saturation=1.12,colorchannelmixer=rr=1.10:gg=1.02:bb=0.82"),
            VideoFilterPreset::Cold => chain.push_str(",eq=contrast=1.05:saturation=0.92,colorchannelmixer=rr=0.84:gg=0.95:bb=1.16"),
            VideoFilterPreset::None => {}
        }
        if layout.blur >= 0.5 {
            chain.push_str(&format!(",boxblur={}:1", layout.blur.round().clamp(1.0, 10.0)));
        }
        if layout.rotation.abs() >= 0.5 {
            let radians = layout.rotation as f64 * std::f64::consts::PI / 180.0;
            chain.push_str(&format!(",rotate={radians:.6}:c=none:ow=rotw(iw):oh=roth(ih),scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0"));
        }
        if layout.opacity < 0.999 {
            chain.push_str(&format!(",format=rgba,colorchannelmixer=aa={:.3}", layout.opacity.clamp(0.0, 1.0)));
        } else {
            chain.push_str(",format=rgba");
        }
        chain.push_str(&format!("[clip{index}]"));
        filter.push(';');
        filter.push_str(&chain);
        let x = pct_to_px(layout.x - min_x, reference_w);
        let y = pct_to_px(layout.y - min_y, reference_h);
        filter.push(';');
        filter.push_str(&format!(
            "[base{index}][clip{index}]overlay={x}:{y}:enable='between(t,{start:.3},{:.3})':eof_action=pass[base{}]",
            start + clip_duration,
            index + 1
        ));
    }
    filter.push(';');
    if let Some((t_w, t_h)) = aspect_target_box(output_w, output_h, aspect) {
        filter.push_str(&format!(
            "[base{}]scale={t_w}:{t_h}:force_original_aspect_ratio=decrease,pad={t_w}:{t_h}:(ow-iw)/2:(oh-ih)/2:color=black,format=yuv420p[v]",
            clips.len()
        ));
    } else {
        filter.push_str(&format!("[base{}]format=yuv420p[v]", clips.len()));
    }
    filter
}

fn layout_processing_suffix(layout: &VideoLayout, reference_w: i32, reference_h: i32) -> String {
    let layout = normalized_video_layout(layout);
    let (out_w, out_h) = layout_output_size(&layout, reference_w, reference_h);
    let crop_w = (1.0 - ((layout.crop_left + layout.crop_right).min(90.0) / 100.0)).max(0.05);
    let crop_h = (1.0 - ((layout.crop_top + layout.crop_bottom).min(90.0) / 100.0)).max(0.05);
    let crop_x = (layout.crop_left / 100.0).clamp(0.0, 0.9);
    let crop_y = (layout.crop_top / 100.0).clamp(0.0, 0.9);
    let mut suffix = format!(
        ",crop=iw*{crop_w:.5}:ih*{crop_h:.5}:iw*{crop_x:.5}:ih*{crop_y:.5},scale={out_w}:{out_h}:force_original_aspect_ratio=increase,crop={out_w}:{out_h},setsar=1"
    );
    let eq_brightness = (layout.brightness - 1.0).clamp(-0.8, 0.8);
    suffix.push_str(&format!(
        ",eq=brightness={eq_brightness:.3}:contrast={:.3}:saturation={:.3}:gamma={:.3}",
        layout.contrast.clamp(0.2, 2.0),
        layout.saturation.clamp(0.0, 2.0),
        layout.gamma.clamp(0.5, 1.8),
    ));
    // Photos-style adjustments — mirror client/src/glColor.ts so preview == export.
    let exposure = layout.exposure.clamp(-1.0, 1.0);
    if exposure.abs() >= 0.005 {
        let gain = 2f32.powf(exposure);
        suffix.push_str(&format!(",colorchannelmixer=rr={gain:.4}:gg={gain:.4}:bb={gain:.4}"));
    }
    let temp = layout.temperature.clamp(-1.0, 1.0);
    let tint = layout.tint.clamp(-1.0, 1.0);
    if temp.abs() >= 0.005 || tint.abs() >= 0.005 {
        suffix.push_str(&format!(
            ",colorbalance=rm={:.4}:gm={:.4}:bm={:.4}",
            temp * 0.3,
            -tint * 0.3,
            -temp * 0.3,
        ));
    }
    let hl = layout.highlights.clamp(-1.0, 1.0);
    let sh = layout.shadows.clamp(-1.0, 1.0);
    if hl.abs() >= 0.005 || sh.abs() >= 0.005 {
        let sy = (0.25 + sh * 0.15).clamp(0.0, 1.0);
        let hy = (0.75 + hl * 0.15).clamp(0.0, 1.0);
        suffix.push_str(&format!(",curves=m=0/0 0.25/{sy:.3} 0.75/{hy:.3} 1/1"));
    }
    match layout.preset {
        VideoFilterPreset::Warm => suffix.push_str(",colorchannelmixer=rr=1.06:gg=1.01:bb=0.94"),
        VideoFilterPreset::Cool => suffix.push_str(",colorchannelmixer=rr=0.94:gg=1.01:bb=1.08"),
        VideoFilterPreset::Mono => suffix.push_str(",hue=s=0"),
        VideoFilterPreset::Punch => suffix.push_str(",eq=contrast=1.12:saturation=1.14"),
        VideoFilterPreset::Dream => suffix.push_str(",boxblur=1:1,eq=brightness=0.04:saturation=0.82"),
        VideoFilterPreset::Cinema => suffix.push_str(",eq=contrast=1.10:saturation=1.05,colorchannelmixer=rr=1.08:gg=0.98:bb=0.90"),
        VideoFilterPreset::Noir => suffix.push_str(",eq=contrast=1.28,hue=s=0"),
        VideoFilterPreset::Moody => suffix.push_str(",eq=brightness=-0.06:contrast=1.18:saturation=0.85,colorchannelmixer=rr=0.93:gg=0.97:bb=1.08"),
        VideoFilterPreset::Vintage => suffix.push_str(",eq=contrast=0.94:saturation=0.70,colorchannelmixer=rr=1.06:gg=0.98:bb=0.86"),
        VideoFilterPreset::Golden => suffix.push_str(",eq=brightness=0.04:saturation=1.12,colorchannelmixer=rr=1.10:gg=1.02:bb=0.82"),
        VideoFilterPreset::Cold => suffix.push_str(",eq=contrast=1.05:saturation=0.92,colorchannelmixer=rr=0.84:gg=0.95:bb=1.16"),
        VideoFilterPreset::None => {}
    }
    if layout.blur >= 0.5 {
        suffix.push_str(&format!(",boxblur={}:1", layout.blur.round().clamp(1.0, 10.0)));
    }
    if layout.sharpen.clamp(0.0, 2.0) >= 0.02 {
        suffix.push_str(&format!(",unsharp=5:5:{:.3}:5:5:0.0", layout.sharpen.clamp(0.0, 2.0)));
    }
    if layout.grain.clamp(0.0, 1.0) >= 0.02 {
        suffix.push_str(&format!(",noise=alls={}:allf=t", (layout.grain.clamp(0.0, 1.0) * 30.0).round() as i32));
    }
    if layout.vignette.clamp(0.0, 1.0) >= 0.02 {
        let ang = (layout.vignette.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_3) as f64;
        suffix.push_str(&format!(",vignette=a={ang:.4}"));
    }
    if layout.rotation.abs() >= 0.5 {
        let radians = layout.rotation as f64 * std::f64::consts::PI / 180.0;
        suffix.push_str(&format!(",rotate={radians:.6}:c=none:ow=rotw(iw):oh=roth(ih),scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0"));
    }
    if layout.opacity < 0.999 {
        suffix.push_str(&format!(",format=rgba,colorchannelmixer=aa={:.3}", layout.opacity.clamp(0.0, 1.0)));
    } else {
        suffix.push_str(",format=rgba");
    }
    suffix
}

fn layout_output_size(layout: &VideoLayout, reference_w: i32, reference_h: i32) -> (i32, i32) {
    let layout = normalized_video_layout(layout);
    (
        even_dimension(pct_to_px(layout.width, reference_w).max(2)),
        even_dimension(pct_to_px(layout.height, reference_h).max(2)),
    )
}

fn centered_layout_position(layout: &VideoLayout, reference_w: i32, reference_h: i32) -> (i32, i32) {
    let (out_w, out_h) = layout_output_size(layout, reference_w, reference_h);
    ((reference_w - out_w) / 2, (reference_h - out_h) / 2)
}

fn layout_summary(layout: &VideoLayout) -> String {
    let layout = normalized_video_layout(layout);
    format!(
        "manual x {:.1}%, y {:.1}%, w {:.1}%, h {:.1}%, crop T/R/B/L {:.1}/{:.1}/{:.1}/{:.1}%, rotation {:.1} deg, brightness {:.2}, contrast {:.2}, saturation {:.2}, blur {:.1}, preset {:?}; agent cut placement: centered",
        layout.x,
        layout.y,
        layout.width,
        layout.height,
        layout.crop_top,
        layout.crop_right,
        layout.crop_bottom,
        layout.crop_left,
        layout.rotation,
        layout.brightness,
        layout.contrast,
        layout.saturation,
        layout.blur,
        layout.preset
    )
}

fn build_auto_edit_segments(
    clips: &[VideoRenderClip],
    range_start: u64,
    range_end: u64,
    interval_samples: u64,
    sample_rate: u32,
) -> Vec<AutoEditSegment> {
    let mut segments: Vec<AutoEditSegment> = Vec::new();
    let mut cursor = range_start;
    let mut previous_track: Option<&str> = None;
    while cursor < range_end {
        let next = (cursor + interval_samples).min(range_end);
        let mut active = clips
            .iter()
            .enumerate()
            .filter(|(_, clip)| clip.start_sample < next && clip.end_sample > cursor)
            .collect::<Vec<_>>();
        if active.is_empty() {
            cursor = next;
            continue;
        }
        active.sort_by(|(_, a), (_, b)| {
            let a_same = previous_track == Some(a.track_id.as_str());
            let b_same = previous_track == Some(b.track_id.as_str());
            a_same
                .cmp(&b_same)
                .then_with(|| a.track_index.cmp(&b.track_index))
                .then_with(|| a.start_sample.cmp(&b.start_sample))
        });
        let (input_index, clip) = active[0];
        let segment_start = cursor.max(clip.start_sample);
        let segment_end = next.min(clip.end_sample);
        if segment_end > segment_start {
            let source_offset_ms = clip.source_offset_ms.saturating_add(
                (((segment_start.saturating_sub(clip.start_sample)) as f64 / sample_rate as f64) * 1000.0).round() as u64
            );
            segments.push(AutoEditSegment {
                input_index,
                timeline_start: segment_start,
                timeline_end: segment_end,
                source_offset_ms,
            });
            previous_track = Some(clip.track_id.as_str());
        }
        cursor = next;
    }
    segments
}

fn build_fallback_agent_script(
    clips: &[VideoRenderClip],
    segments: &[AutoEditSegment],
    range_start: u64,
    range_end: u64,
    total_windows: u32,
    sample_rate: u32,
    error: &str,
) -> Vec<AgentVideoScriptEntry> {
    let mut script = Vec::new();
    let mut timeline_cursor = range_start;
    let mut window_index = 0_u32;
    let mut sorted_segments = segments.iter().collect::<Vec<_>>();
    sorted_segments.sort_by_key(|segment| segment.timeline_start);
    for segment in sorted_segments {
        if segment.timeline_start > timeline_cursor {
            window_index += 1;
            script.push(AgentVideoScriptEntry {
                window_index,
                total_windows,
                start_seconds: timeline_cursor as f64 / sample_rate as f64,
                end_seconds: segment.timeline_start as f64 / sample_rate as f64,
                decision: "black".into(),
                candidates: Vec::new(),
                chosen_track_index: None,
                chosen_track_name: None,
                reason: "No selected video clip is active in this gap, so it remains black to preserve sync.".into(),
                data_provided: vec![
                    format!("Window time: {:.2}s-{:.2}s", timeline_cursor as f64 / sample_rate as f64, segment.timeline_start as f64 / sample_rate as f64),
                    "No active selected video candidates.".into(),
                ],
                model_choice: None,
                variety_override: false,
                source_offset_seconds: None,
            });
        }
        let clip = &clips[segment.input_index];
        window_index += 1;
        script.push(AgentVideoScriptEntry {
            window_index,
            total_windows,
            start_seconds: segment.timeline_start as f64 / sample_rate as f64,
            end_seconds: segment.timeline_end as f64 / sample_rate as f64,
            decision: "fallback-cut".into(),
            candidates: vec![AgentVideoScriptCandidate {
                image_number: 1,
                track_index: clip.track_index,
                track_name: clip.track_name.clone(),
                timeline_seconds: segment.timeline_start as f64 / sample_rate as f64,
                angle_label: Some(clip.track_name.clone()),
                note: Some("Automatic fallback candidate.".into()),
            }],
            chosen_track_index: Some(clip.track_index),
            chosen_track_name: Some(clip.track_name.clone()),
            reason: format!("Vision model failed, so the automatic cutter selected this active angle. Error: {error}"),
            data_provided: vec![
                format!("Window time: {:.2}s-{:.2}s", segment.timeline_start as f64 / sample_rate as f64, segment.timeline_end as f64 / sample_rate as f64),
                format!("Fallback candidate: {} on track {}", clip.track_name, clip.track_index + 1),
            ],
            model_choice: None,
            variety_override: false,
            source_offset_seconds: Some(segment.source_offset_ms as f64 / 1000.0),
        });
        timeline_cursor = segment.timeline_end;
    }
    if range_end > timeline_cursor {
        window_index += 1;
        script.push(AgentVideoScriptEntry {
            window_index,
            total_windows,
            start_seconds: timeline_cursor as f64 / sample_rate as f64,
            end_seconds: range_end as f64 / sample_rate as f64,
            decision: "black".into(),
            candidates: Vec::new(),
            chosen_track_index: None,
            chosen_track_name: None,
            reason: "No selected video clip is active in this final gap, so it remains black to preserve sync.".into(),
            data_provided: vec![
                format!("Window time: {:.2}s-{:.2}s", timeline_cursor as f64 / sample_rate as f64, range_end as f64 / sample_rate as f64),
                "No active selected video candidates.".into(),
            ],
            model_choice: None,
            variety_override: false,
            source_offset_seconds: None,
        });
    }
    script
}

#[allow(dead_code)] // Kept as a fallback path that reads the rendered WAV from disk.
fn load_rendered_audio_analysis(path: &Path) -> Result<RenderedAudioAnalysis, String> {
    let mut reader = hound::WavReader::open(path).map_err(|error| format!("Could not open rendered mix audio: {error}"))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map(|value| value.clamp(-1.0, 1.0)).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int if spec.bits_per_sample <= 16 => {
            let scale = ((1_i32 << spec.bits_per_sample.saturating_sub(1)) - 1).max(1) as f32;
            reader
                .samples::<i16>()
                .map(|sample| sample.map(|value| (value as f32 / scale).clamp(-1.0, 1.0)).map_err(|error| error.to_string()))
                .collect::<Result<Vec<_>, _>>()?
        }
        hound::SampleFormat::Int => {
            let scale = ((1_i64 << spec.bits_per_sample.saturating_sub(1)) - 1).max(1) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| (value as f32 / scale).clamp(-1.0, 1.0)).map_err(|error| error.to_string()))
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(RenderedAudioAnalysis {
        samples,
        channels,
        sample_rate: spec.sample_rate,
        timeline_start_sample: 0,
    })
}

fn audio_features_for_window(
    analysis: Option<&RenderedAudioAnalysis>,
    window_start: u64,
    window_end: u64,
    session_sample_rate: u32,
) -> AgentAudioWindowFeatures {
    let Some(analysis) = analysis else {
        return AgentAudioWindowFeatures {
            peak_db: -90.0,
            rms_db: -90.0,
            lufs_estimate: -90.0,
            loudness: "unknown".into(),
            transient_density: 0.0,
            transient_activity: "unknown".into(),
        };
    };
    let frame_count = analysis.samples.len() / analysis.channels.max(1);
    // Subtract the analysis's timeline offset so a range-render (which starts at
    // sample range_start, not 0) still lines up with timeline-space window samples.
    let offset = analysis.timeline_start_sample;
    let local_start = window_start.saturating_sub(offset);
    let local_end = window_end.saturating_sub(offset);
    let start_frame = ((local_start as f64 / session_sample_rate as f64) * analysis.sample_rate as f64)
        .round()
        .clamp(0.0, frame_count as f64) as usize;
    let end_frame = ((local_end as f64 / session_sample_rate as f64) * analysis.sample_rate as f64)
        .round()
        .clamp(start_frame as f64, frame_count as f64) as usize;
    if end_frame <= start_frame {
        return AgentAudioWindowFeatures {
            peak_db: -90.0,
            rms_db: -90.0,
            lufs_estimate: -90.0,
            loudness: "silence".into(),
            transient_density: 0.0,
            transient_activity: "low".into(),
        };
    }

    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f64;
    let mut previous = 0.0_f32;
    let mut transient_hits = 0_u32;
    let mut count = 0_u32;
    for frame in start_frame..end_frame {
        let offset = frame * analysis.channels;
        let mut mono = 0.0_f32;
        for channel in 0..analysis.channels {
            mono += analysis.samples.get(offset + channel).copied().unwrap_or(0.0);
        }
        mono /= analysis.channels as f32;
        let abs = mono.abs();
        peak = peak.max(abs);
        sum_squares += (mono as f64) * (mono as f64);
        if count > 0 && (mono - previous).abs() > 0.10 {
            transient_hits += 1;
        }
        previous = mono;
        count += 1;
    }
    let rms = (sum_squares / count.max(1) as f64).sqrt() as f32;
    let peak_db = amplitude_to_db(peak);
    let rms_db = amplitude_to_db(rms);
    let lufs_estimate = (rms_db - 1.5).max(-90.0);
    let transient_density = transient_hits as f32 / count.max(1) as f32;
    let loudness = if rms_db < -45.0 {
        "silence"
    } else if rms_db < -28.0 {
        "quiet"
    } else if rms_db < -18.0 {
        "medium"
    } else {
        "loud"
    };
    let transient_activity = if transient_density > 0.08 {
        "high"
    } else if transient_density > 0.025 {
        "medium"
    } else {
        "low"
    };
    AgentAudioWindowFeatures {
        peak_db: round1(peak_db),
        rms_db: round1(rms_db),
        lufs_estimate: round1(lufs_estimate),
        loudness: loudness.into(),
        transient_density: round2(transient_density),
        transient_activity: transient_activity.into(),
    }
}

fn amplitude_to_db(value: f32) -> f32 {
    20.0 * value.max(0.000_001).log10()
}

fn audio_features_text(features: &AgentAudioWindowFeatures) -> String {
    format!(
        "peak {:.1} dBFS, RMS {:.1} dB, LUFS estimate {:.1}, loudness {}, transient activity {} ({:.2})",
        features.peak_db,
        features.rms_db,
        features.lufs_estimate,
        features.loudness,
        features.transient_activity,
        features.transient_density
    )
}

async fn build_agent_edit_segments(
    app: &AppHandle,
    started: &std::time::Instant,
    clips: &[VideoRenderClip],
    session: &MixSession,
    range_start: u64,
    range_end: u64,
    interval_samples: u64,
    base_url: &str,
    vision_model: &str,
    edit_model: &str,
    instructions: Option<&str>,
    audio_analysis: Option<&RenderedAudioAnalysis>,
    temp_dir: &Path,
) -> Result<(Vec<AutoEditSegment>, Vec<AgentVideoScriptEntry>, Option<crate::model::VideoFilterPreset>, Option<AgentColorGrade>, Option<AgentVideoEffects>), String> {
    let sample_rate = session.sample_rate;
    let mut segments: Vec<AutoEditSegment> = Vec::new();
    let mut script = Vec::new();
    // Each window's vision call also returns a `look_preset` derived from the user
    // instructions. We tally votes and pick the most common non-"none" preset for the
    // whole render. Windows with no instruction-driven preference vote "none".
    let mut look_votes: HashMap<String, u32> = HashMap::new();
    // First non-empty custom color grade from any window. All windows see the same
    // user instructions, so the first response is just as informed as the last.
    let mut first_color_grade: Option<AgentColorGrade> = None;
    // Deterministic keyword pre-detection — picks a default look + grade from the
    // user's text BEFORE we hit the vision model. The model can override per window
    // (its votes go into look_votes and first_color_grade above) but if it doesn't,
    // we fall back to this so the user always sees a real grade when they asked for one.
    let (keyword_look, keyword_grade) = match instructions {
        Some(text) if !text.is_empty() => infer_look_from_instructions(text),
        _ => (None, None),
    };
    if let Some(preset) = keyword_look.as_ref() {
        *look_votes.entry(format!("{:?}", preset).to_lowercase()).or_insert(0) += 1;
    }
    // Same idea for whole-edit effects (fade in/out, speed). LLM can override per
    // window via the `video_effects` field; otherwise the keyword detector wins.
    let keyword_effects = match instructions {
        Some(text) if !text.is_empty() => infer_effects_from_instructions(text),
        _ => None,
    };
    let mut first_video_effects: Option<AgentVideoEffects> = None;
    let mut cursor = range_start;
    let total_windows = range_end.saturating_sub(range_start).div_ceil(interval_samples).max(1) as u32;
    let mut window_index = 0_u32;
    let mut previous_input_index: Option<usize> = None;
    let mut consecutive_same = 0_u32;
    let mut usage_counts: HashMap<usize, u32> = HashMap::new();
    // Once the video model fails to analyze a frame (e.g. a text-only endpoint that
    // can't see images), stop calling it for the rest of the edit and fall back to
    // deterministic multicam cuts — otherwise every window would eat the full timeout.
    let mut vision_off = false;
    // Rolling editorial history — a compact log of the shots already taken and what was
    // in them — fed into each window's decision so the agent cuts based on how the
    // sequence is EVOLVING (coverage, development, rhythm), not on an isolated frame.
    let mut edit_history: Vec<String> = Vec::new();
    while cursor < range_end {
        if assistant::agent_cancelled() {
            return Err(assistant::CANCELLED_MESSAGE.into());
        }
        window_index += 1;
        let next = (cursor + interval_samples).min(range_end);
        let audio_features = audio_features_for_window(audio_analysis, cursor, next, sample_rate);
        let active = clips
            .iter()
            .enumerate()
            .filter(|(_, clip)| clip.start_sample < next && clip.end_sample > cursor)
            .collect::<Vec<_>>();
        if active.is_empty() {
            emit_agent_progress(app, started, "sampling", "No active selected clip in this window; skipping.", window_index, total_windows);
            script.push(AgentVideoScriptEntry {
                window_index,
                total_windows,
                start_seconds: cursor as f64 / sample_rate as f64,
                end_seconds: next as f64 / sample_rate as f64,
                decision: "black".into(),
                candidates: Vec::new(),
                chosen_track_index: None,
                chosen_track_name: None,
                reason: "No selected video clip is active in this window, so the export keeps this section black to preserve sync.".into(),
                data_provided: vec![
                    format!("Window time: {:.2}s-{:.2}s", cursor as f64 / sample_rate as f64, next as f64 / sample_rate as f64),
                    "Active selected video candidates: 0".into(),
                    format!("Audio features: {}", audio_features_text(&audio_features)),
                ],
                model_choice: None,
                variety_override: false,
                source_offset_seconds: None,
            });
            cursor = next;
            continue;
        }
        emit_agent_progress(
            app,
            started,
            "sampling",
            &format!("Extracting frames for window {window_index}/{total_windows}..."),
            window_index,
            total_windows,
        );
        let sample = cursor + (next - cursor) / 2;
        let mut labels = Vec::new();
        let mut images = Vec::new();
        let mut candidates = Vec::new();
        // Extract every camera's frame for this window CONCURRENTLY. The model server
        // runs one request at a time (`--parallel 1`), but ffmpeg extractions are
        // independent CPU work, so doing them in parallel removes the per-camera stall
        // (2+ cameras went from ~1s sequential to ~0.5s).
        let frame_paths: Vec<std::path::PathBuf> = active
            .iter()
            .enumerate()
            .map(|(slot, _)| temp_dir.join(format!("shot-{}-{slot}.jpg", segments.len())))
            .collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = active
                .iter()
                .enumerate()
                .map(|(slot, (_, clip))| {
                    let frame_path = frame_paths[slot].clone();
                    let sample = sample.clamp(clip.start_sample, clip.end_sample.saturating_sub(1));
                    scope.spawn(move || extract_video_frame(clip, sample, session, &frame_path))
                })
                .collect();
            for handle in handles {
                let _ = handle.join();
            }
        });
        for (slot, (input_index, clip)) in active.iter().enumerate() {
            let sample = sample.clamp(clip.start_sample, clip.end_sample.saturating_sub(1));
            let frame_path = frame_paths[slot].clone();
            if frame_path.exists() {
                if let Ok(bytes) = fs::read(&frame_path) {
                    images.push(BASE64_STANDARD.encode(bytes));
                    labels.push((
                        *input_index,
                        format!(
                            "{} (track {}, timeline {:.2}s, {})",
                            clip.track_name,
                            clip.track_index + 1,
                            sample as f64 / sample_rate as f64,
                            layout_summary(&clip.layout)
                        ),
                    ));
                    candidates.push(AgentVideoScriptCandidate {
                        image_number: labels.len(),
                        track_index: clip.track_index,
                        track_name: clip.track_name.clone(),
                        timeline_seconds: sample as f64 / sample_rate as f64,
                        angle_label: None,
                        note: None,
                    });
                }
            }
        }
        if labels.is_empty() {
            emit_agent_progress(
                app,
                started,
                "sampling",
                &format!("Window {window_index}/{total_windows}: no readable frames; skipping."),
                window_index,
                total_windows,
            );
            script.push(AgentVideoScriptEntry {
                window_index,
                total_windows,
                start_seconds: cursor as f64 / sample_rate as f64,
                end_seconds: next as f64 / sample_rate as f64,
                decision: "black".into(),
                candidates: Vec::new(),
                chosen_track_index: None,
                chosen_track_name: None,
                reason: "The selected tracks were active here, but no readable frame could be extracted, so the export leaves this window black.".into(),
                data_provided: vec![
                    format!("Window time: {:.2}s-{:.2}s", cursor as f64 / sample_rate as f64, next as f64 / sample_rate as f64),
                    format!("Active selected video candidates: {}", active.len()),
                    "Readable extracted frames: 0".into(),
                    format!("Audio features: {}", audio_features_text(&audio_features)),
                ],
                model_choice: None,
                variety_override: false,
                source_offset_seconds: None,
            });
            cursor = next;
            continue;
        }
        let image_count = images.len();
        emit_agent_progress(
            app,
            started,
            "vision",
            &format!("Analyzing frames and deciding edit for window {window_index}/{total_windows}..."),
            window_index,
            total_windows,
        );
        let previous_label = previous_input_index
            .and_then(|previous| labels.iter().position(|(input_index, _)| *input_index == previous))
            .map(|index| index + 1);
        // Merged describe+decide call: one HTTP roundtrip per window instead of two.
        // We still skip the LLM entirely when there's only one readable angle.
        let history_text = if edit_history.is_empty() {
            "Nothing yet — this is the opening shot of the edit.".to_string()
        } else {
            edit_history.join("\n")
        };
        let merged = if image_count >= 2 && !vision_off {
            match analyze_and_decide_window(
                base_url,
                vision_model,
                &labels,
                images,
                &audio_features,
                previous_label,
                consecutive_same,
                instructions,
                &history_text,
                first_color_grade.is_none() && look_votes.is_empty(),
            )
            .await
            {
                Ok(m) => Some(m),
                Err(_) => {
                    // First failure: the model can't see frames. Disable vision and
                    // let deterministic variety-cuts carry the rest of the edit.
                    vision_off = true;
                    emit_agent_progress(
                        &app,
                        &started,
                        "vision",
                        "Video model can't analyze frames — switching to automatic time/beat-based cuts.",
                        window_index,
                        total_windows,
                    );
                    None
                }
            }
        } else if image_count >= 2 {
            // Vision already disabled — rely on the deterministic cut logic below.
            None
        } else {
            Some(AgentMergedChoice {
                candidate_labels: None,
                candidate_notes: None,
                window_summary: Some("Single available angle.".into()),
                choice: 1,
                decision: Some("cut".into()),
                reason: Some("Only one readable camera angle was available for this window.".into()),
                edit_intent: Some("single available angle".into()),
                continuity_plan: Some("Use the only available readable shot.".into()),
                look_preset: None,
                color_grade: None,
                video_effects: None,
            })
        };
        // `edit_model` is unused now that vision + decide are merged into one
        // vision-model call. Keep the parameter so existing callers don't break.
        let _ = edit_model;
        if let Some(angle_labels) = merged.as_ref().and_then(|m| m.candidate_labels.as_ref()) {
            for (candidate, angle_label) in candidates.iter_mut().zip(angle_labels.iter()) {
                let angle_label = angle_label.trim();
                if !angle_label.is_empty() {
                    candidate.angle_label = Some(angle_label.to_string());
                }
            }
        }
        if let Some(notes) = merged.as_ref().and_then(|m| m.candidate_notes.as_ref()) {
            for (candidate, note) in candidates.iter_mut().zip(notes.iter()) {
                let note = note.trim();
                if !note.is_empty() {
                    candidate.note = Some(note.to_string());
                }
            }
        }
        let frame_summary = merged
            .as_ref()
            .and_then(|m| m.window_summary.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("No visual summary returned.");
        let model_choice = merged.as_ref().map(|m| AgentShotChoice {
            choice: m.choice,
            decision: m.decision.clone(),
            reason: m.reason.clone(),
            edit_intent: m.edit_intent.clone(),
            continuity_plan: m.continuity_plan.clone(),
        });
        if let Some(preset_name) = merged.as_ref().and_then(|m| m.look_preset.as_deref()) {
            let normalized = preset_name.trim().to_lowercase();
            if !normalized.is_empty() {
                *look_votes.entry(normalized).or_insert(0) += 1;
            }
        }
        if first_color_grade.is_none() {
            if let Some(grade) = merged.as_ref().and_then(|m| m.color_grade.clone()) {
                // Only adopt the grade if it would actually produce a filter — skip
                // empty/neutral objects the model emits when no look is requested.
                if build_color_grade_filter(&grade).is_some() {
                    first_color_grade = Some(grade);
                }
            }
        }
        if first_video_effects.is_none() {
            if let Some(effects) = merged.as_ref().and_then(|m| m.video_effects.clone()) {
                // Only adopt if any field is actually populated.
                if effects.fade_in_seconds.is_some()
                    || effects.fade_out_seconds.is_some()
                    || effects.speed_factor.is_some()
                {
                    first_video_effects = Some(effects);
                }
            }
        }
        let model_requested_hold = model_choice
            .as_ref()
            .and_then(|choice| choice.decision.as_deref())
            .map(|decision| decision.eq_ignore_ascii_case("hold"))
            .unwrap_or(false);
        let mut chosen_label_index = model_choice
            .as_ref()
            .and_then(|choice| choice.choice.checked_sub(1))
            .filter(|index| *index < labels.len())
            .unwrap_or(0);
        if model_requested_hold {
            if let Some(previous_label) = previous_label {
                if previous_label > 0 && previous_label <= labels.len() {
                    chosen_label_index = previous_label - 1;
                }
            }
        }
        let mut variety_override = false;
        let mut coverage_override = false;
        // Full freedom for the agent: when the vision model made a decision, honor it —
        // no forced variety/coverage cuts. The deterministic rules below only kick in as
        // a FALLBACK when there's no model decision (vision unavailable), otherwise a
        // single camera would play for the whole edit.
        if model_choice.is_none() && labels.len() > 1 {
            if let Some(previous) = previous_input_index {
                let chosen_input_index = labels[chosen_label_index].0;
                if chosen_input_index == previous && consecutive_same >= MAX_DYNAMIC_HOLD_WINDOWS {
                    let alternate = labels
                        .iter()
                        .enumerate()
                        .filter(|(_, (input_index, _))| *input_index != previous)
                        .filter(|(label_index, _)| {
                            candidates
                                .get(*label_index)
                                .and_then(|candidate| candidate.note.as_deref())
                                .map(|note| !candidate_note_rejects_dynamic_cut(note))
                                .unwrap_or(true)
                        })
                        .min_by_key(|(_, (input_index, _))| usage_counts.get(input_index).copied().unwrap_or(0))
                        .or_else(|| {
                            labels
                                .iter()
                                .enumerate()
                                .filter(|(_, (input_index, _))| *input_index != previous)
                                .min_by_key(|(_, (input_index, _))| usage_counts.get(input_index).copied().unwrap_or(0))
                        });
                    if let Some((alternate_index, _)) = alternate {
                        chosen_label_index = alternate_index;
                        variety_override = true;
                    }
                }
            }
        }
        if model_choice.is_none() && !variety_override && labels.len() > 1 && window_index >= MIN_WINDOWS_BEFORE_COVERAGE_CUT {
            let chosen_input_index = labels[chosen_label_index].0;
            let chosen_usage = usage_counts.get(&chosen_input_index).copied().unwrap_or(0);
            let alternate = labels
                .iter()
                .enumerate()
                .filter(|(_, (input_index, _))| *input_index != chosen_input_index)
                .filter(|(label_index, _)| {
                    candidates
                        .get(*label_index)
                        .and_then(|candidate| candidate.note.as_deref())
                        .map(|note| !candidate_note_rejects_dynamic_cut(note))
                        .unwrap_or(true)
                })
                .min_by_key(|(_, (input_index, _))| usage_counts.get(input_index).copied().unwrap_or(0));
            if let Some((alternate_index, (alternate_input_index, _))) = alternate {
                let alternate_usage = usage_counts.get(alternate_input_index).copied().unwrap_or(0);
                let usage_gap = chosen_usage.saturating_sub(alternate_usage);
                if alternate_usage == 0 || usage_gap >= MIN_USAGE_GAP_FOR_COVERAGE_CUT {
                    chosen_label_index = alternate_index;
                    coverage_override = true;
                    variety_override = true;
                }
            }
        }
        let input_index = labels[chosen_label_index].0;
        let clip = &clips[input_index];
        let segment_start = cursor.max(clip.start_sample);
        let segment_end = next.min(clip.end_sample);
        if segment_end > segment_start {
            let source_offset_ms = clip.source_offset_ms.saturating_add(
                (((segment_start.saturating_sub(clip.start_sample)) as f64 / sample_rate as f64) * 1000.0).round() as u64
            );
            if let Some(previous_segment) = segments
                .last_mut()
                .filter(|segment| segment.input_index == input_index && segment.timeline_end == segment_start)
            {
                previous_segment.timeline_end = segment_end;
            } else {
                segments.push(AutoEditSegment {
                    input_index,
                    timeline_start: segment_start,
                    timeline_end: segment_end,
                    source_offset_ms,
                });
            }
            *usage_counts.entry(input_index).or_insert(0) += 1;
            let entering_previous_input_index = previous_input_index;
            let held_count_before_update = if previous_input_index == Some(input_index) {
                consecutive_same
            } else {
                0
            };
            if previous_input_index == Some(input_index) {
                consecutive_same += 1;
            } else {
                previous_input_index = Some(input_index);
                consecutive_same = 1;
            }
            let model_reason = model_choice
                .as_ref()
                .and_then(|choice| choice.reason.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("The model selected this as the strongest readable shot for this edit window.");
            let edit_intent = model_choice
                .as_ref()
                .and_then(|choice| choice.edit_intent.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let chosen_image_number = chosen_label_index + 1;
            let model_decision = if coverage_override {
                "coverage-cut"
            } else if variety_override {
                "dynamic-cut"
            } else if model_requested_hold && held_count_before_update > 0 {
                "hold"
            } else if held_count_before_update > 0 {
                "continue"
            } else {
                "cut"
            };
            let selected_note = candidates
                .iter()
                .find(|candidate| candidate.image_number == chosen_image_number)
                .and_then(|candidate| candidate.note.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            // Append to the rolling editorial history (kept short) so later windows see
            // what's already on screen and how the sequence is developing.
            let history_what = selected_note
                .or_else(|| candidates.iter().find(|c| c.image_number == chosen_image_number).and_then(|c| c.angle_label.as_deref()))
                .unwrap_or("shot");
            edit_history.push(format!(
                "t={:.0}s {} cam{}: {}",
                segment_start as f64 / sample_rate as f64,
                model_decision,
                input_index + 1,
                history_what,
            ));
            if edit_history.len() > 6 {
                edit_history.remove(0);
            }
            let continuity_plan = model_choice
                .as_ref()
                .and_then(|choice| choice.continuity_plan.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let reason = if coverage_override {
                let model_pick = model_choice.as_ref().map(|choice| choice.choice).unwrap_or(chosen_image_number);
                let selected_note = selected_note.unwrap_or("the underused alternate angle was readable and active in this window");
                format!(
                    "Decision coverage-cut. Image {chosen_image_number} ({}) was selected because this camera had been underused while other active angles were already used. The model's raw pick was image {model_pick}; alternate evidence: {selected_note}",
                    clip.track_name
                )
            } else if variety_override {
                let model_pick = model_choice.as_ref().map(|choice| choice.choice).unwrap_or(chosen_image_number);
                let selected_note = selected_note.unwrap_or("the alternate angle was readable and active in this window");
                format!(
                    "Decision dynamic-cut. Image {chosen_image_number} ({}) was selected to add needed camera movement after holding the previous angle for {held_count_before_update} window(s). The model's raw pick was image {model_pick}; alternate evidence: {selected_note}",
                    clip.track_name
                )
            } else if let Some(edit_intent) = edit_intent {
                format!("Decision {model_decision}. Image {chosen_image_number} ({}) for {edit_intent}: {model_reason}", clip.track_name)
            } else {
                format!("Decision {model_decision}. Image {chosen_image_number} ({}): {model_reason}", clip.track_name)
            };
            let reason = if let Some(plan) = continuity_plan {
                format!("{reason} Continuity plan: {plan}")
            } else if let Some(selected_note) = selected_note {
                format!("{reason} Selected-frame evidence: {selected_note}")
            } else {
                reason
            };
            let source_offset_seconds = source_offset_ms as f64 / 1000.0;
            let mut data_provided = vec![
                format!("Window time: {:.2}s-{:.2}s", cursor as f64 / sample_rate as f64, next as f64 / sample_rate as f64),
                format!("Sample frame time: {:.2}s", sample as f64 / sample_rate as f64),
                format!("Interval size: {:.2}s", interval_samples as f64 / sample_rate as f64),
                match entering_previous_input_index {
                    Some(previous) => {
                        let previous_clip = &clips[previous];
                        format!(
                            "Previous shot entering decision: {} on track {}, held for {} window(s)",
                            previous_clip.track_name,
                            previous_clip.track_index + 1,
                            held_count_before_update
                        )
                    }
                    None => "Previous shot entering decision: none".into(),
                },
                format!("Active selected video candidates: {}", labels.len()),
                format!("Dynamic hold limit: {MAX_DYNAMIC_HOLD_WINDOWS} window(s) before using a readable alternate"),
                format!("Coverage rule: after {MIN_WINDOWS_BEFORE_COVERAGE_CUT} windows, use readable underused cameras when their usage trails by {MIN_USAGE_GAP_FOR_COVERAGE_CUT}+"),
                format!("User instructions: {}", instructions.unwrap_or("none")),
                format!("Vision model: {vision_model}"),
                format!("Edit decision model: {edit_model}"),
                format!("Audio features: {}", audio_features_text(&audio_features)),
                format!("Frame-analysis summary: {frame_summary}"),
            ];
            data_provided.extend(candidates.iter().map(|candidate| {
                let layout = active
                    .iter()
                    .find(|(_, clip)| clip.track_index == candidate.track_index && clip.track_name == candidate.track_name)
                    .map(|(_, clip)| layout_summary(&clip.layout))
                    .unwrap_or_else(|| "unavailable".into());
                format!(
                    "Image {}: {} on track {} at {:.2}s, canvas layout: {}{}{}",
                    candidate.image_number,
                    candidate.track_name,
                    candidate.track_index + 1,
                    candidate.timeline_seconds,
                    layout,
                    candidate
                        .angle_label
                        .as_deref()
                        .map(|label| format!(", agent label: {label}"))
                        .unwrap_or_default(),
                    candidate
                        .note
                        .as_deref()
                        .map(|note| format!(", note: {note}"))
                        .unwrap_or_default()
                )
            }));
            script.push(AgentVideoScriptEntry {
                window_index,
                total_windows,
                start_seconds: segment_start as f64 / sample_rate as f64,
                end_seconds: segment_end as f64 / sample_rate as f64,
                decision: model_decision.into(),
                candidates,
                chosen_track_index: Some(clip.track_index),
                chosen_track_name: Some(clip.track_name.clone()),
                reason,
                data_provided,
                model_choice: model_choice.as_ref().map(|choice| choice.choice),
                variety_override,
                source_offset_seconds: Some(source_offset_seconds),
            });
            let message = if variety_override {
                format!(
                    "Window {window_index}/{total_windows}: switched to selected video track {} for angle variation.",
                    clip.track_index + 1
                )
            } else {
                format!(
                    "Window {window_index}/{total_windows}: chose selected video track {}.",
                    clip.track_index + 1
                )
            };
            emit_agent_progress(
                app,
                started,
                "decision",
                &message,
                window_index,
                total_windows,
            );
        }
        cursor = next;
    }
    // Pick the most-voted preset. "none" is a vote against any grade; we only return
    // a real preset when a non-"none" choice wins (or ties for first by ordering).
    // Falls back to the keyword pre-detection when the model votes "none" everywhere.
    let chosen_look = look_votes
        .iter()
        .filter(|(name, _)| name.as_str() != "none")
        .max_by_key(|(_, count)| *count)
        .and_then(|(name, _)| parse_video_filter_preset(name))
        .or(keyword_look);
    // Same fallback for the free-form grade: prefer what the model actually emitted,
    // otherwise use the deterministic grade derived from instruction keywords.
    let chosen_grade = first_color_grade.or(keyword_grade);
    let chosen_effects = first_video_effects.or(keyword_effects);
    Ok((segments, script, chosen_look, chosen_grade, chosen_effects))
}

fn extract_video_frame(clip: &VideoRenderClip, sample: u64, session: &MixSession, output_path: &Path) -> Result<(), String> {
    let sample_rate = session.sample_rate;
    let source_offset = clip.source_offset_ms as f64 / 1000.0
        + sample.saturating_sub(clip.start_sample) as f64 / sample_rate as f64;
    // Build a slim thumbnail for the vision model: apply the user's crop (so the agent
    // sees the same framing it'll render to), scale to max 512 px on the long edge,
    // and write a lower-quality JPEG. The full render uses the full pipeline; vision
    // analysis does not need the canvas-sized image.
    let layout = normalized_video_layout(&clip.layout);
    let crop_w = (1.0 - ((layout.crop_left + layout.crop_right).min(90.0) / 100.0)).max(0.05);
    let crop_h = (1.0 - ((layout.crop_top + layout.crop_bottom).min(90.0) / 100.0)).max(0.05);
    let crop_x = (layout.crop_left / 100.0).clamp(0.0, 0.9);
    let crop_y = (layout.crop_top / 100.0).clamp(0.0, 0.9);
    // 384px (down from 512) is plenty for the model to judge framing/action and cuts
    // the per-frame vision-encoding cost noticeably.
    let filter = format!(
        "crop=iw*{crop_w:.5}:ih*{crop_h:.5}:iw*{crop_x:.5}:ih*{crop_y:.5},scale=384:384:force_original_aspect_ratio=decrease"
    );
    let output = Command::new("ffmpeg")
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-ss")
        .arg(format!("{source_offset:.3}"))
        .arg("-i")
        .arg(&clip.path)
        .arg("-vf")
        .arg(filter)
        .arg("-frames:v")
        .arg("1")
        .arg("-q:v")
        .arg("8") // 1..31, higher = lower quality. 8 is small + clearly readable.
        .arg(output_path)
        .output()
        .map_err(|error| format!("Could not extract video frame: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg frame extraction failed: {}", stderr.trim()));
    }
    Ok(())
}

fn candidate_note_rejects_dynamic_cut(note: &str) -> bool {
    let note = note.to_ascii_lowercase();
    [
        "black",
        "blocked",
        "empty",
        "unreadable",
        "unusable",
        "no useful",
        "too dark",
        "very dark",
        "blurry",
        "out of focus",
        "occluded",
    ]
    .iter()
    .any(|term| note.contains(term))
}

#[allow(dead_code)] // superseded by analyze_and_decide_window (single-call agent path)
async fn analyze_agent_window_frames(
    base_url: &str,
    model: &str,
    labels: &[(usize, String)],
    images: Vec<String>,
    instructions: Option<&str>,
) -> Result<AgentWindowFrameAnalysis, String> {
    let label_text = labels
        .iter()
        .enumerate()
        .map(|(index, (_, label))| format!("{} = {label}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let instruction_note = instructions
        .map(|value| format!("User edit instructions for context:\n{value}\n"))
        .unwrap_or_else(|| "User edit instructions: none.\n".to_string());
    let prompt = format!(
        "Stage 1 of a two-stage multicam edit. Analyze the visible content of each simultaneous camera frame.\n\
         {instruction_note}\
         Do not decide cuts yet. Only describe what is visible and useful for editing.\n\
         Assign each image a short angle label, such as \"wide room\", \"overhead/top-down\", \"face/profile\", \"guitar hands\", \"fretboard close-up\", \"keyboard hands\", \"drums\", \"dark/weak angle\", or \"blocked/unclear\".\n\
         For each candidate note, use concrete visible evidence: framing, face/eyes, hands, instrument, gesture, motion, focus, exposure, occlusion, and shot uniqueness. Never write the literal phrase \"concrete note\".\n\
         Images are in this order:\n{label_text}\n\
         Reply only as JSON with this shape:\n\
         {{\"window_summary\":\"one sentence summarizing the available visual choices\", \"candidate_labels\":[\"overhead/top-down\", \"face/profile\"], \"candidate_notes\":[\"Image 1: visible framing/action/quality in 8-14 words\", \"Image 2: visible framing/action/quality in 8-14 words\"]}}"
    );
    let parsed = call_ollama_chat(base_url, model, prompt, Some(images)).await?;
    let extracted = crate::assistant::extract_json_object(&parsed.message.content).unwrap_or(parsed.message.content);
    serde_json::from_str::<AgentWindowFrameAnalysis>(&extracted)
        .map_err(|error| format!("Could not parse frame analysis response: {error}"))
}

#[allow(dead_code)] // superseded by analyze_and_decide_window (single-call agent path)
async fn decide_agent_shot(
    base_url: &str,
    model: &str,
    labels: &[(usize, String)],
    candidates: &[AgentVideoScriptCandidate],
    audio_features: &AgentAudioWindowFeatures,
    frame_summary: &str,
    previous_label: Option<usize>,
    consecutive_same: u32,
    instructions: Option<&str>,
) -> Result<AgentShotChoice, String> {
    let label_text = labels
        .iter()
        .enumerate()
        .map(|(index, (_, label))| format!("{} = {label}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let continuity_note = previous_label
        .map(|label| {
            format!(
                "Previous chosen image number was {label}, held for {consecutive_same} consecutive edit window(s). You may HOLD it if the current alternatives do not justify a cohesive cut, but if it has held for 3+ windows you should actively look for a readable alternate to create dynamics."
            )
        })
        .unwrap_or_else(|| "No previous shot has been chosen yet.".to_string());
    let instruction_note = instructions
        .map(|value| format!("User edit instructions. Treat these as creative guidelines unless they would force a black/unusable shot:\n{value}\n"))
        .unwrap_or_else(|| "User edit instructions: none.\n".to_string());
    let candidate_text = candidates
        .iter()
        .map(|candidate| {
            format!(
                "Image {} = {} (track {}, timeline {:.2}s), label: {}, visual note: {}",
                candidate.image_number,
                candidate.track_name,
                candidate.track_index + 1,
                candidate.timeline_seconds,
                candidate.angle_label.as_deref().unwrap_or("unlabeled"),
                candidate.note.as_deref().unwrap_or("no visual note")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let audio_text = audio_features_text(audio_features);
    let prompt = format!(
        "Stage 2 of a two-stage multicam edit. You are deciding whether to HOLD or CUT for a music performance at this exact moment.\n\
         {instruction_note}\
         Use the Stage 1 visual analysis plus audio features. Do not invent visual or musical facts beyond the data below.\n\
         Window visual summary: {frame_summary}\n\
         Audio features: {audio_text}\n\
         Candidate frame analysis:\n{candidate_text}\n\
         CUT only when the new shot creates a coherent edit: a meaningfully different angle, better performance detail, face/reaction, clearer action, or a sensible pacing change. HOLD when changing would feel arbitrary or sudden, but do not park the whole edit on one camera when another usable angle exists.\n\
         Use audio to support pacing: high transient activity or loud sections can justify a cut; quiet/low-transient sections should favor holds unless the new visual is clearly better.\n\
         With two or more readable cameras, aim for visual dynamics over the section: after several held windows, prefer a clean alternate angle even if the current angle is still good.\n\
         {continuity_note}\n\
         Candidate index mapping:\n{label_text}\n\
         Reply only as JSON with this shape:\n\
         {{\"decision\": \"hold|cut\", \"choice\": 1, \"edit_intent\": \"hold continuity | wide context | hands detail | face/reaction | motion accent | pacing variation\", \"reason\": \"one specific sentence explaining why this is a hold or cut using visual + audio data\", \"continuity_plan\": \"how this decision supports the surrounding edit\", \"confidence\": \"low|medium|high\"}}"
    );
    let parsed = call_ollama_chat(base_url, model, prompt, None).await?;
    let extracted = crate::assistant::extract_json_object(&parsed.message.content).unwrap_or(parsed.message.content);
    let choice = serde_json::from_str::<AgentShotChoice>(&extracted)
        .map_err(|error| format!("Could not parse agent edit choice: {error}"))?;
    Ok(choice)
}

/// Single-call replacement for `analyze_agent_window_frames` + `decide_agent_shot`.
/// Asks the vision model to describe the frames *and* pick the best image in one
/// prompt. Cuts wall-clock time roughly in half because we skip one HTTP roundtrip
/// and one prompt-prefill per window. The decision rules are the same as the
/// Stage-2 prompt, just folded into the Stage-1 prompt.
async fn analyze_and_decide_window(
    base_url: &str,
    model: &str,
    labels: &[(usize, String)],
    images: Vec<String>,
    audio_features: &AgentAudioWindowFeatures,
    previous_label: Option<usize>,
    consecutive_same: u32,
    instructions: Option<&str>,
    edit_history: &str,
    // Only the FIRST window needs the full color-grade/effects design (we adopt the
    // grade once for the whole edit). Later windows return a minimal shot decision —
    // ~80 output tokens instead of ~400 — which is the dominant per-window cost.
    request_grade: bool,
) -> Result<AgentMergedChoice, String> {
    let label_text = labels
        .iter()
        .enumerate()
        .map(|(index, (_, label))| format!("{} = {label}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let continuity_note = previous_label
        .map(|label| format!(
            "Previous chosen image number was {label}, held for {consecutive_same} consecutive edit window(s). You have full creative freedom: HOLD it as long as it serves the edit, and CUT only when another angle genuinely improves the sequence. There is no required cutting cadence and no coverage quota — follow the user's direction (e.g. \"keep camera X as the main shot, only complement with the others\") over any urge to add variety."
        ))
        .unwrap_or_else(|| "No previous shot has been chosen yet.".to_string());
    let instruction_note = instructions
        .map(|value| format!("User edit instructions (treat as creative guidelines unless they force a black/unusable shot):\n{value}\n"))
        .unwrap_or_else(|| "User edit instructions: none.\n".to_string());
    let audio_text = audio_features_text(audio_features);
    let history_note = format!(
        "EDIT SO FAR (oldest to newest — the shots already on screen and what was in them):\n{edit_history}\n\
         Cut like an editor shaping a SEQUENCE, not someone rating one frame in isolation. Base this shot on how the performance/action is EVOLVING across the edit: develop what is becoming interesting, follow the energy, introduce a fresh angle when the moment shifts, keep balanced coverage over time (don't keep returning to the same angle or repeat the recent rhythm), and let cuts breathe with the music. The goal is a coherent arc, not a string of locally-best frames.\n"
    );
    // The full color-grade/effects spec is large; only ask for it on the first window
    // (we adopt the grade once for the whole edit). Every other window returns a small
    // shot decision, which is far faster to generate.
    let grade_block = if request_grade {
        "Also design a color grade for the WHOLE edit that fits the user's instructions. Use a named `look_preset` and/or a custom `color_grade` (numeric, clamped server-side): brightness(-0.5..0.5), contrast(0.4..2), saturation(0..2.5), gamma(0.5..1.8), rgbMix{rr,gg,bb each 0.4..1.6}, hueShift(-180..180), vignette(0..1), blur(0..8), sharpen(0..2), grain(0..30). Presets: none,warm,cool,mono,punch,dream,cinema,noir,moody,vintage,golden,cold (cinematic/epic->cinema; teal-and-orange->cinema rr>1 bb<1; warm->warm/golden; cool->cool; b&w->mono; punchy->punch; dreamy->dream). Also pick `video_effects` (fadeInSeconds 0..10, fadeOutSeconds 0..10, speedFactor 0.25..4) only if the user asked. If no look direction, use look_preset \"none\".\n"
    } else {
        ""
    };
    let json_shape = if request_grade {
        "{{\"window_summary\": \"one sentence\", \"candidate_labels\": [\"label1\", \"label2\"], \"candidate_notes\": [\"note1\", \"note2\"], \"decision\": \"hold|cut\", \"choice\": 1, \"edit_intent\": \"...\", \"reason\": \"one sentence\", \"continuity_plan\": \"...\", \"look_preset\": \"cinema\", \"color_grade\": {{\"name\": \"epic cinema\", \"reason\": \"...\", \"contrast\": 1.12, \"saturation\": 1.05, \"rgbMix\": {{\"rr\": 1.10, \"bb\": 0.85}}, \"vignette\": 0.25, \"sharpen\": 0.4}}, \"video_effects\": {{\"fadeInSeconds\": 1.0}}}}"
    } else {
        // Minimal per-window response — no grade/effects (already set for the edit).
        "{{\"candidate_labels\": [\"label1\", \"label2\"], \"candidate_notes\": [\"note1\", \"note2\"], \"decision\": \"hold|cut\", \"choice\": 1, \"reason\": \"one short sentence\"}}"
    };
    let prompt = format!(
        "You are a multicam editor cutting a continuous sequence. Look at each simultaneous camera frame and decide the strongest shot for THIS window given everything that has come before.\n\
         {instruction_note}\
         Images are in this order:\n{label_text}\n\
         For each image, derive a short angle label (e.g. \"wide room\", \"guitar hands\", \"fretboard close-up\", \"face/profile\", \"dark/weak\") and a concise 6-12 word visual note (framing, hands, instrument, motion, focus, exposure).\n\
         Then pick the best image. CUT only when the new shot creates a coherent edit (different angle, better detail, face/reaction, clearer action, pacing change). HOLD when changing would feel arbitrary. Use audio for pacing: loud sections justify cuts; quiet sections favor holds.\n\
         {grade_block}\
         --- CONTEXT FOR THIS WINDOW ---\n\
         Audio features: {audio_text}\n\
         {continuity_note}\n\
         {history_note}\n\
         Reply ONLY as compact JSON (no prose) with this exact shape:\n{json_shape}"
    );
    let parsed = call_ollama_chat(base_url, model, prompt, Some(images)).await?;
    let extracted = crate::assistant::extract_json_object(&parsed.message.content).unwrap_or(parsed.message.content);
    serde_json::from_str::<AgentMergedChoice>(&extracted)
        .map_err(|error| format!("Could not parse merged agent decision: {error}"))
}

/// Single vision call for the clip-direct-edit path. Asks the model to pick a
/// look_preset + color_grade + video_effects based on the user's instructions and
/// ONE sample frame from the clip. No cuts, no per-window iteration. Returns
/// best-effort — missing fields fall back to keyword detection.
async fn analyze_clip_effects(
    base_url: &str,
    model: &str,
    frame_b64: String,
    instructions: &str,
) -> Result<ClipEffectsChoice, String> {
    let prompt = format!(
        "You are designing a color grade + effects for a SINGLE recorded video clip. The user wants visual improvements only — no cuts, no multicam.\n\
         User instructions:\n{instructions}\n\n\
         Look at the sample frame and pick effects that match the user's words.\n\
         look_preset: one of none, warm, cool, mono, punch, dream, cinema, noir, moody, vintage, golden, cold.\n\
         color_grade (optional): {{name, reason, brightness(-0.5..0.5), contrast(0.4..2), saturation(0..2.5), gamma(0.5..1.8), rgbMix{{rr,gg,bb (0.4..1.6 each)}}, hueShift(-180..180), vignette(0..1), blur(0..8), sharpen(0..2), grain(0..30)}}.\n\
         video_effects (optional): {{reason, fadeInSeconds(0..10), fadeOutSeconds(0..10), speedFactor(0.25..4)}}.\n\
         For each field, write a short rationale (color_grade.reason, video_effects.reason) that maps which user word drove which knob.\n\
         Reply only as JSON with this shape:\n\
         {{\"look_preset\": \"cinema\", \"color_grade\": {{\"name\": \"epic cinema\", \"reason\": \"...\", \"contrast\": 1.12, \"rgbMix\": {{\"rr\": 1.10, \"bb\": 0.85}}, \"vignette\": 0.25, \"sharpen\": 0.4, \"grain\": 1.5}}, \"video_effects\": {{\"reason\": \"...\", \"fadeInSeconds\": 1, \"fadeOutSeconds\": 2}}}}"
    );
    let parsed = call_ollama_chat(base_url, model, prompt, Some(vec![frame_b64])).await?;
    let extracted = crate::assistant::extract_json_object(&parsed.message.content).unwrap_or(parsed.message.content);
    serde_json::from_str::<ClipEffectsChoice>(&extracted)
        .map_err(|error| format!("Could not parse clip-effects response: {error}"))
}

/// One-shot (non-streaming) chat call, optionally with base64 JPEG frames attached.
/// Dispatches to the native Ollama API or the OpenAI-compatible API (vLLM,
/// llama.cpp) depending on what the server at `base_url` speaks.
async fn call_ollama_chat(
    base_url: &str,
    model: &str,
    prompt: String,
    images: Option<Vec<String>>,
) -> Result<OllamaChatResponse, String> {
    if assistant::agent_cancelled() {
        return Err(assistant::CANCELLED_MESSAGE.into());
    }
    // These are all vision/decision calls that want a fast JSON answer, NOT chain-of-
    // thought. Reasoning models (qwen3.x) take ~100s/window with thinking on vs <2s off.
    // The `/no_think` prefix is honored by the qwen chat template regardless of which
    // HTTP path (native Ollama vs OpenAI-compat) or server processes it — more robust
    // than the `chat_template_kwargs` body flag, which some endpoints silently drop.
    let prompt = format!("/no_think\n{prompt}");
    match assistant::detect_provider(base_url).await? {
        assistant::LlmProvider::Ollama => call_ollama_chat_native(base_url, model, prompt, images).await,
        assistant::LlmProvider::OpenAiCompat => call_openai_chat(base_url, model, prompt, images).await,
    }
}

async fn call_ollama_chat_native(
    base_url: &str,
    model: &str,
    prompt: String,
    images: Option<Vec<String>>,
) -> Result<OllamaChatResponse, String> {
    // Bounded timeout so a model that can't process images (e.g. a text-only endpoint
    // handed video frames) fails fast instead of hanging the whole edit.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(format!("{base_url}/api/chat"))
        .json(&OllamaChatRequest {
            model: model.to_string(),
            stream: false,
            messages: vec![OllamaChatMessage {
                role: "user".into(),
                content: prompt,
                images,
            }],
        })
        .send()
        .await
        .map_err(|error| format!("Could not call Ollama model: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Ollama model returned {}", response.status()));
    }
    response
        .json::<OllamaChatResponse>()
        .await
        .map_err(|error| format!("Could not parse Ollama response: {error}"))
}

/// OpenAI-compatible chat (vLLM, llama.cpp). Images go in as multimodal
/// `image_url` content parts with data URLs; the response is adapted into the
/// same shape the Ollama path returns so callers don't care which server ran.
async fn call_openai_chat(
    base_url: &str,
    model: &str,
    prompt: String,
    images: Option<Vec<String>>,
) -> Result<OllamaChatResponse, String> {
    let mut content = vec![serde_json::json!({ "type": "text", "text": prompt })];
    for image in images.into_iter().flatten() {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": format!("data:image/jpeg;base64,{image}") },
        }));
    }
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{ "role": "user", "content": content }],
        // Disable the model's chain-of-thought for these vision calls. We need a fast
        // JSON decision, not reasoning — with thinking ON, a single window decision on
        // qwen3.6 takes ~105s/4500 tokens; with it OFF, ~0.3s. The field is honored by
        // llama.cpp / vLLM qwen chat templates and ignored by servers that don't use it.
        "chat_template_kwargs": { "enable_thinking": false },
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(format!("{base_url}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("Could not call the model server: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("The model server returned {}", response.status()));
    }
    #[derive(Deserialize)]
    struct OpenAiChatResponse {
        #[serde(default)]
        choices: Vec<OpenAiChatChoice>,
    }
    #[derive(Deserialize)]
    struct OpenAiChatChoice {
        message: OpenAiChatMessage,
    }
    #[derive(Deserialize)]
    struct OpenAiChatMessage {
        #[serde(default)]
        content: Option<String>,
    }
    let parsed = response
        .json::<OpenAiChatResponse>()
        .await
        .map_err(|error| format!("Could not parse the model server response: {error}"))?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .ok_or("The model server returned no content")?;
    Ok(OllamaChatResponse { message: OllamaChatResponseMessage { content } })
}

fn build_auto_edit_filter(
    clips: &[VideoRenderClip],
    segments: &[AutoEditSegment],
    session: &MixSession,
    range_start: u64,
    range_end: u64,
    look_override: Option<crate::model::VideoFilterPreset>,
) -> String {
    // We build ONE canvas the length of the entire range and overlay each segment at
    // its timeline PTS, gated by `enable='between(t,...)'`. This is the same pattern
    // build_video_filter uses for multicam and crucially avoids the audio/video drift
    // caused by concatenating per-segment streams whose durations don't snap to the
    // frame grid (with fps=30 and segment durations in arbitrary seconds, every cut
    // adds a fractional-frame rounding error that compounds against the audio).
    let canvas = &session.video_canvas;
    let output_w = even_dimension(canvas.width.clamp(240, 3840) as i32);
    let output_h = even_dimension(canvas.height.clamp(240, 3840) as i32);
    let background = ffmpeg_color(&canvas.background);
    let sample_rate = session.sample_rate as f64;
    let total_duration = ((range_end.saturating_sub(range_start)) as f64 / sample_rate).max(0.001);
    let mut sorted_segments = segments.iter().collect::<Vec<_>>();
    sorted_segments.sort_by_key(|segment| segment.timeline_start);

    let mut filter = format!(
        "color=c={background}:s={output_w}x{output_h}:d={total_duration:.3},setsar=1,fps=30[base0]"
    );
    let mut overlay_count = 0_usize;
    for segment in sorted_segments {
        let segment_start = segment.timeline_start.max(range_start).min(range_end);
        let segment_end = segment.timeline_end.max(range_start).min(range_end);
        if segment_end <= segment_start {
            continue;
        }
        let clip = &clips[segment.input_index];
        let timeline_offset = (segment_start.saturating_sub(range_start) as f64 / sample_rate).max(0.0);
        let duration = (segment_end.saturating_sub(segment_start) as f64 / sample_rate).max(1.0 / 30.0);
        let source_offset = segment.source_offset_ms as f64 / 1000.0
            + (segment_start.saturating_sub(segment.timeline_start) as f64 / sample_rate);
        // A cut-style edit shows one camera at a time, so each shot fills the whole canvas.
        // Force the box to full-frame (drop the picture-in-picture position/size used for the
        // multi-cam composition) but KEEP the per-track crop (the user's framing), rotation
        // (to un-flip cameras) and color grading. The kept (cropped) region is then scaled
        // to cover the whole canvas.
        let mut fill_layout = clip.layout.clone();
        fill_layout.x = 0.0;
        fill_layout.y = 0.0;
        fill_layout.width = 100.0;
        fill_layout.height = 100.0;
        fill_layout.opacity = 1.0;
        // A user-picked "look" overrides the per-clip preset so a single look
        // applies uniformly across every cut without modifying the source layouts.
        if let Some(preset) = look_override.clone() {
            fill_layout.preset = preset;
        }
        let (x, y) = centered_layout_position(&fill_layout, output_w, output_h);
        let suffix = layout_processing_suffix(&fill_layout, output_w, output_h);
        let clip_label = format!("clip{overlay_count}");
        // Normalise the source's frame rate first — webcam captures often report bogus
        // r_frame_rate values (e.g. 600/1) which propagate through trim and confuse timing.
        // setpts shifts the clip's PTS to its absolute timeline position so the overlay's
        // `enable='between(t,...)'` window picks it up at exactly the right moment.
        filter.push(';');
        filter.push_str(&format!(
            "[{input}:v]fps=30,trim=start={source_offset:.3}:duration={duration:.3},setpts=PTS-STARTPTS+{timeline_offset:.3}/TB{suffix}[{clip_label}]",
            input = segment.input_index,
        ));
        let next_base = overlay_count + 1;
        let segment_end_seconds = timeline_offset + duration;
        filter.push(';');
        filter.push_str(&format!(
            "[base{overlay_count}][{clip_label}]overlay={x}:{y}:enable='between(t,{timeline_offset:.3},{segment_end_seconds:.3})':eof_action=pass[base{next_base}]"
        ));
        overlay_count += 1;
    }
    filter.push(';');
    filter.push_str(&format!("[base{overlay_count}]format=yuv420p[v]"));
    filter
}

pub fn normalized_video_layout(layout: &VideoLayout) -> VideoLayout {
    let mut next = layout.clone();
    next.width = next.width.clamp(1.0, 300.0);
    next.height = next.height.clamp(1.0, 300.0);
    next.x = next.x.clamp(-300.0, 300.0);
    next.y = next.y.clamp(-300.0, 300.0);
    next.crop_top = next.crop_top.clamp(0.0, 45.0);
    next.crop_right = next.crop_right.clamp(0.0, 45.0);
    next.crop_bottom = next.crop_bottom.clamp(0.0, 45.0);
    next.crop_left = next.crop_left.clamp(0.0, 45.0);
    next.opacity = next.opacity.clamp(0.0, 1.0);
    next.brightness = next.brightness.clamp(0.2, 2.0);
    next.contrast = next.contrast.clamp(0.2, 2.0);
    next.saturation = next.saturation.clamp(0.0, 2.0);
    next.blur = next.blur.clamp(0.0, 10.0);
    next.exposure = next.exposure.clamp(-1.0, 1.0);
    next.highlights = next.highlights.clamp(-1.0, 1.0);
    next.shadows = next.shadows.clamp(-1.0, 1.0);
    next.temperature = next.temperature.clamp(-1.0, 1.0);
    next.tint = next.tint.clamp(-1.0, 1.0);
    next.gamma = next.gamma.clamp(0.5, 1.8);
    next.vignette = next.vignette.clamp(0.0, 1.0);
    next.sharpen = next.sharpen.clamp(0.0, 2.0);
    next.grain = next.grain.clamp(0.0, 1.0);
    next
}

fn pct_to_px(value: f32, total: i32) -> i32 {
    ((value / 100.0) * total as f32).round() as i32
}

fn content_bounds(layouts: &[VideoLayout]) -> (f32, f32, f32, f32) {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for layout in layouts {
        min_x = min_x.min(layout.x);
        min_y = min_y.min(layout.y);
        max_x = max_x.max(layout.x + layout.width);
        max_y = max_y.max(layout.y + layout.height);
    }
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        (0.0, 0.0, 100.0, 100.0)
    } else {
        (min_x, min_y, max_x, max_y)
    }
}

fn even_dimension(value: i32) -> i32 {
    let value = value.max(2);
    if value % 2 == 0 { value } else { value + 1 }
}

fn ffmpeg_color(value: &str) -> String {
    let trimmed = value.trim();
    let is_hex = trimmed.len() == 7
        && trimmed.starts_with('#')
        && trimmed.chars().skip(1).all(|c| c.is_ascii_hexdigit());
    if !is_hex {
        return "black".into();
    }
    // Prefer named colors when possible (some ffmpeg builds misparse "0x000000" as
    // 0 and fall back to a default that isn't black — which is where the green
    // background in rendered edits was coming from).
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "#000000" => "black".into(),
        "#ffffff" => "white".into(),
        _ => lower, // ffmpeg accepts "#RRGGBB" directly in the color filter
    }
}

fn track_slot(session: &MixSession, track_id: &str) -> Option<u32> {
    session.tracks.iter().position(|t| t.id == track_id).map(|i| i as u32)
}

fn session_duration_samples(session: &MixSession) -> u64 {
    let by_id: std::collections::HashMap<&str, &crate::model::SourceFile> =
        session.source_files.iter().map(|source| (source.id.as_str(), source)).collect();
    session.tracks.iter().map(|track| {
        if track.clips.is_empty() {
            by_id
                .get(track.source_file_id.as_str())
                .map(|source| track.start_sample + source.duration_samples)
                .unwrap_or(0)
        } else {
            track.clips.iter().map(|clip| clip.end_sample).max().unwrap_or(0)
        }
    }).max().unwrap_or(0)
}

fn push_engine_commands(state: &AppState, session: &MixSession, actions: &[MixAction]) {
    let Ok(mut audio) = state.audio.lock() else {
        return;
    };
    for action in actions {
        match action {
            MixAction::SetTrackGain { track_id, gain_db } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackGainDb { slot, db: *gain_db });
                }
            }
            MixAction::AdjustTrackGain { track_id, .. } => {
                if let (Some(slot), Some(track)) = (
                    track_slot(session, track_id),
                    session.tracks.iter().find(|t| &t.id == track_id),
                ) {
                    audio.send(EngineCommand::SetTrackGainDb { slot, db: track.gain_db });
                }
            }
            MixAction::SetTrackPan { track_id, pan } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackPan { slot, pan: *pan });
                }
            }
            MixAction::MuteTrack { track_id, muted } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackMuted { slot, muted: *muted });
                }
            }
            MixAction::SoloTrack { track_id, solo } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackSolo { slot, solo: *solo });
                }
            }
            MixAction::SetHighPass { track_id, frequency_hz, slope_db_oct } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackHighPass {
                        slot,
                        enabled: true,
                        frequency_hz: *frequency_hz,
                        slope_db_oct: *slope_db_oct,
                    });
                }
            }
            MixAction::SetLowPass { track_id, frequency_hz, slope_db_oct } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackLowPass {
                        slot,
                        enabled: true,
                        frequency_hz: *frequency_hz,
                        slope_db_oct: *slope_db_oct,
                    });
                }
            }
            MixAction::SetEqBand { track_id, band, frequency_hz, gain_db, q } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackEqBand {
                        slot,
                        band: *band as u8,
                        frequency_hz: *frequency_hz,
                        gain_db: *gain_db,
                        q: *q,
                    });
                }
            }
            MixAction::SetCompressor {
                track_id,
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                knee_db,
                makeup_db,
            } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackCompressor {
                        slot,
                        enabled: true,
                        threshold_db: *threshold_db,
                        ratio: *ratio,
                        attack_ms: *attack_ms,
                        release_ms: *release_ms,
                        knee_db: *knee_db,
                        makeup_db: *makeup_db,
                    });
                }
            }
            MixAction::SetReverbSend { track_id, level_db } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackReverbSendDb { slot, db: *level_db });
                }
            }
            MixAction::SetDelaySend { track_id, level_db } => {
                if let Some(slot) = track_slot(session, track_id) {
                    audio.send(EngineCommand::SetTrackDelaySendDb { slot, db: *level_db });
                }
            }
            MixAction::SetMasterGain { .. } | MixAction::AdjustMasterGain { .. } => {
                audio.send(EngineCommand::SetMasterGainDb(session.master.gain_db));
            }
            _ => {}
        }
    }
}

/// Push the entire current session state to the engine. Called when starting
/// playback or after history navigation, so the audio thread reflects the
/// session without needing to replay every individual action.
pub fn sync_session_to_engine(audio: &mut crate::engine::AudioEngine, session: &MixSession) {
    audio.send(EngineCommand::SetMasterGainDb(session.master.gain_db));
    audio.send(EngineCommand::SetMasterCeilingDb(session.master.limiter.ceiling_db));
    for (index, track) in session.tracks.iter().enumerate() {
        let slot = index as u32;
        audio.send(EngineCommand::SetTrackActive { slot, active: true });
        audio.send(EngineCommand::SetTrackGainDb { slot, db: track.gain_db });
        audio.send(EngineCommand::SetTrackPan { slot, pan: track.pan });
        audio.send(EngineCommand::SetTrackMuted { slot, muted: track.muted });
        audio.send(EngineCommand::SetTrackSolo { slot, solo: track.solo });
        audio.send(EngineCommand::SetTrackReverbSendDb { slot, db: track.sends.reverb_db });
        audio.send(EngineCommand::SetTrackDelaySendDb { slot, db: track.sends.delay_db });
        audio.send(EngineCommand::SetTrackHighPass {
            slot,
            enabled: track.chain.high_pass.enabled,
            frequency_hz: track.chain.high_pass.frequency_hz,
            slope_db_oct: track.chain.high_pass.slope_db_oct,
        });
        audio.send(EngineCommand::SetTrackLowPass {
            slot,
            enabled: track.chain.low_pass.enabled,
            frequency_hz: track.chain.low_pass.frequency_hz,
            slope_db_oct: track.chain.low_pass.slope_db_oct,
        });
        for (band_idx, band) in track.chain.eq.iter().enumerate().take(4) {
            audio.send(EngineCommand::SetTrackEqBand {
                slot,
                band: band_idx as u8,
                frequency_hz: band.frequency_hz,
                gain_db: band.gain_db,
                q: band.q,
            });
        }
        let comp = &track.chain.compressor;
        audio.send(EngineCommand::SetTrackCompressor {
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
    for slot in (session.tracks.len() as u32)..(crate::engine::mixer::MAX_TRACKS as u32) {
        audio.send(EngineCommand::SetTrackActive { slot, active: false });
    }
}
