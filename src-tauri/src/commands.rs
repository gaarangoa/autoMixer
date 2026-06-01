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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentVideoEditResponse {
    path: String,
    script: Vec<AgentVideoScriptEntry>,
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
struct AgentWindowFrameAnalysis {
    candidate_labels: Option<Vec<String>>,
    candidate_notes: Option<Vec<String>>,
    window_summary: Option<String>,
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
    Ok(ModelsResponse { models: assistant::list_ollama_models(base_url).await? })
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
pub fn list_sessions(state: State<'_, AppState>) -> Result<Vec<MixSession>, String> {
    state.store.lock().map_err(|error| error.to_string())?.list_sessions()
}

#[tauri::command]
pub fn create_session(state: State<'_, AppState>, name: String) -> Result<MixProject, String> {
    state.store.lock().map_err(|error| error.to_string())?.create_session(name)
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

/// Hard-restart the whole app. Used as a "stop" for a mistaken or stuck agent run:
/// relaunching the process kills any in-flight LLM call or video render. The last
/// saved session is reloaded on startup. This call never returns.
#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

#[tauri::command]
pub fn apply_mix_actions(
    state: State<'_, AppState>,
    session_id: String,
    actions: Vec<MixAction>,
    explanation: Option<String>,
) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    apply_actions(&mut project, &actions, HistorySource::User, explanation)?;
    store.save(&project)?;
    push_engine_commands(&state, &project.session, &actions);
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

#[tauri::command]
pub fn undo_mix_action(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    undo(&mut project)?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
}

#[tauri::command]
pub fn redo_mix_action(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    redo(&mut project)?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(project)
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

#[tauri::command]
pub async fn assistant_request(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: AssistantRequest,
) -> Result<AssistantResponse, String> {
    let project = {
        let store = state.store.lock().map_err(|error| error.to_string())?;
        store.get_project(&request.session_id)?
    };
    let _ = app.emit("llm:turn-start", serde_json::json!({ "userText": request.user_text }));
    let observer: std::sync::Arc<dyn assistant::LlmObserver> =
        std::sync::Arc::new(TauriLlmObserver { app: app.clone() });
    let (response, project) =
        assistant::handle_assistant(state.config.clone(), project, request, observer).await?;
    {
        let store = state.store.lock().map_err(|error| error.to_string())?;
        store.save(&project)?;
    }
    if let Ok(mut audio) = state.audio.lock() {
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    let _ = app.emit("llm:turn-end", serde_json::json!({}));
    Ok(response)
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

    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    tokio::spawn(async move {
        let _ = app_clone.emit("auto-mix:start", serde_json::json!({ "stages": options.stages }));
        for (i, stage) in stages.iter().enumerate() {
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
                    if r.status == "error" { break; }
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
    let filter = build_video_filter(&video_inputs, &project.session, range_start, range_end);
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
        return Err(format!("ffmpeg video export failed: {}", stderr.trim()));
    }
    Ok(RenderResponse { path: path.to_string_lossy().to_string() })
}

#[tauri::command]
pub fn export_rendered_video(source_path: String, output_path: String) -> Result<RenderResponse, String> {
    let source = PathBuf::from(source_path);
    if !source.exists() {
        return Err("The Main video render is missing. Run Agent Edit again before exporting.".into());
    }
    let path = normalize_mp4_path(PathBuf::from(output_path));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if source == path {
        return Ok(RenderResponse { path: path.to_string_lossy().to_string() });
    }
    fs::copy(&source, &path).map_err(|error| format!("Could not export Main video: {error}"))?;
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
    audio::render_mix(&project.session, &audio_path)?;
    let audio_analysis = load_rendered_audio_analysis(&audio_path)
        .map_err(|error| {
            emit_agent_progress(&app, &started, "error", "Could not analyze rendered audio for the video agent.", 0, total_windows);
            error
        })?;
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
    let (segments, script) = build_agent_edit_segments(
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
        (segments, script)
    });
    let _ = fs::remove_dir_all(&temp_dir);
    if segments.is_empty() {
        emit_agent_progress(&app, &started, "error", "No visible clips found in selected range.", 0, total_windows);
        return Err("Agent Video Edit could not find visible selected clips in the edit range.".into());
    }

    if plan_only.unwrap_or(false) {
        emit_agent_progress(&app, &started, "done", "Plan ready for review.", total_windows, total_windows);
        return Ok(AgentVideoEditResponse { path: String::new(), script });
    }

    emit_agent_progress(&app, &started, "audio", "Using analyzed mix audio for video export...", total_windows, total_windows);
    emit_agent_progress(&app, &started, "rendering", &format!("Rendering {} selected cuts to MP4...", segments.len()), total_windows, total_windows);
    render_segments_ffmpeg(&project.session, &video_inputs, &segments, &audio_path, range_start, range_end, &path, None)
        .map_err(|error| {
            emit_agent_progress(&app, &started, "error", "ffmpeg failed while rendering the agent edit.", total_windows, total_windows);
            error
        })?;
    emit_agent_progress(&app, &started, "done", "Agent Video Edit complete.", total_windows, total_windows);
    Ok(AgentVideoEditResponse { path: path.to_string_lossy().to_string(), script })
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
) -> Result<(), String> {
    let mut command = Command::new("ffmpeg");
    command.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error");
    for clip in video_inputs {
        command.arg("-i").arg(&clip.path);
    }
    if range_start > 0 {
        command.arg("-ss").arg(format!("{:.3}", range_start as f64 / session.sample_rate as f64));
    }
    command
        .arg("-t")
        .arg(format!("{:.3}", range_end.saturating_sub(range_start) as f64 / session.sample_rate as f64))
        .arg("-i")
        .arg(audio_path);

    let filter = build_auto_edit_filter(video_inputs, segments, session, range_start, range_end, look_override);
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
        .arg("-preset")
        .arg("veryfast")
        .arg("-r")
        .arg("30")
        // Force a keyframe every 30 frames (1s) so the player can seek anywhere quickly
        // and the playhead-driven `<video>.currentTime = ...` calls don't have to decode
        // a long GOP just to display a single frame.
        .arg("-g")
        .arg("30")
        .arg("-keyint_min")
        .arg("30")
        .arg("-sc_threshold")
        .arg("0")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-c:a")
        .arg("aac")
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
    render_segments_ffmpeg(&project.session, &video_inputs, &segments, &audio_path, range_start, range_end, &output_path, look_preset)?;

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
    render_segments_ffmpeg(&project.session, &video_inputs, &segments, &audio_path, range_start, range_end, &output_path, look_preset)?;

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
        return VideoLayout {
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
        };
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

fn build_video_filter(clips: &[VideoRenderClip], session: &MixSession, range_start: u64, range_end: u64) -> String {
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
    filter.push_str(&format!("[base{}]format=yuv420p[v]", clips.len()));
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
        ",eq=brightness={eq_brightness:.3}:contrast={:.3}:saturation={:.3}",
        layout.contrast.clamp(0.2, 2.0),
        layout.saturation.clamp(0.0, 2.0)
    ));
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
    let start_frame = ((window_start as f64 / session_sample_rate as f64) * analysis.sample_rate as f64)
        .round()
        .clamp(0.0, frame_count as f64) as usize;
    let end_frame = ((window_end as f64 / session_sample_rate as f64) * analysis.sample_rate as f64)
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
) -> Result<(Vec<AutoEditSegment>, Vec<AgentVideoScriptEntry>), String> {
    let sample_rate = session.sample_rate;
    let mut segments: Vec<AutoEditSegment> = Vec::new();
    let mut script = Vec::new();
    let mut cursor = range_start;
    let total_windows = range_end.saturating_sub(range_start).div_ceil(interval_samples).max(1) as u32;
    let mut window_index = 0_u32;
    let mut previous_input_index: Option<usize> = None;
    let mut consecutive_same = 0_u32;
    let mut usage_counts: HashMap<usize, u32> = HashMap::new();
    while cursor < range_end {
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
        for (slot, (input_index, clip)) in active.iter().enumerate() {
            let sample = sample.clamp(clip.start_sample, clip.end_sample.saturating_sub(1));
            let frame_path = temp_dir.join(format!("shot-{}-{slot}.jpg", segments.len()));
            if extract_video_frame(clip, sample, session, &frame_path).is_ok() {
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
            &format!("Stage 1/2: analyzing frames and audio for window {window_index}/{total_windows}..."),
            window_index,
            total_windows,
        );
        let frame_analysis = analyze_agent_window_frames(base_url, vision_model, &labels, images, instructions)
            .await
            .ok();
        if let Some(angle_labels) = frame_analysis.as_ref().and_then(|analysis| analysis.candidate_labels.as_ref()) {
            for (candidate, angle_label) in candidates.iter_mut().zip(angle_labels.iter()) {
                let angle_label = angle_label.trim();
                if !angle_label.is_empty() {
                    candidate.angle_label = Some(angle_label.to_string());
                }
            }
        }
        if let Some(notes) = frame_analysis.as_ref().and_then(|analysis| analysis.candidate_notes.as_ref()) {
            for (candidate, note) in candidates.iter_mut().zip(notes.iter()) {
                let note = note.trim();
                if !note.is_empty() {
                    candidate.note = Some(note.to_string());
                }
            }
        }
        let frame_summary = frame_analysis
            .as_ref()
            .and_then(|analysis| analysis.window_summary.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("No visual summary returned.");
        emit_agent_progress(
            app,
            started,
            "decision",
            &format!("Stage 2/2: deciding edit point for window {window_index}/{total_windows}..."),
            window_index,
            total_windows,
        );
        let previous_label = previous_input_index
            .and_then(|previous| labels.iter().position(|(input_index, _)| *input_index == previous))
            .map(|index| index + 1);
        let model_choice = if image_count >= 2 {
            decide_agent_shot(
                base_url,
                edit_model,
                &labels,
                &candidates,
                &audio_features,
                frame_summary,
                previous_label,
                consecutive_same,
                instructions,
            )
                .await
                .ok()
        } else {
            Some(AgentShotChoice {
                choice: 1,
                decision: Some("cut".into()),
                reason: Some("Only one readable camera angle was available for this window.".into()),
                edit_intent: Some("single available angle".into()),
                continuity_plan: Some("Use the only available readable shot.".into()),
            })
        };
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
        if labels.len() > 1 {
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
        if !variety_override && labels.len() > 1 && window_index >= MIN_WINDOWS_BEFORE_COVERAGE_CUT {
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
    Ok((segments, script))
}

fn extract_video_frame(clip: &VideoRenderClip, sample: u64, session: &MixSession, output_path: &Path) -> Result<(), String> {
    let sample_rate = session.sample_rate;
    let source_offset = clip.source_offset_ms as f64 / 1000.0
        + sample.saturating_sub(clip.start_sample) as f64 / sample_rate as f64;
    let canvas = &session.video_canvas;
    let output_w = even_dimension(canvas.width.clamp(240, 3840) as i32);
    let output_h = even_dimension(canvas.height.clamp(240, 3840) as i32);
    let background = ffmpeg_color(&canvas.background);
    let (x, y) = centered_layout_position(&clip.layout, output_w, output_h);
    let suffix = layout_processing_suffix(&clip.layout, output_w, output_h);
    let filter = format!(
        "[0:v]setpts=PTS-STARTPTS{suffix}[clip];[1:v][clip]overlay={x}:{y}:eof_action=pass,format=yuv420p[v]"
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
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg(format!("color=c={background}:s={output_w}x{output_h}:d=1"))
        .arg("-filter_complex")
        .arg(filter)
        .arg("-map")
        .arg("[v]")
        .arg("-frames:v")
        .arg("1")
        .arg("-q:v")
        .arg("3")
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

async fn call_ollama_chat(
    base_url: &str,
    model: &str,
    prompt: String,
    images: Option<Vec<String>>,
) -> Result<OllamaChatResponse, String> {
    let client = reqwest::Client::new();
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

fn build_auto_edit_filter(
    clips: &[VideoRenderClip],
    segments: &[AutoEditSegment],
    session: &MixSession,
    range_start: u64,
    range_end: u64,
    look_override: Option<crate::model::VideoFilterPreset>,
) -> String {
    let canvas = &session.video_canvas;
    let output_w = even_dimension(canvas.width.clamp(240, 3840) as i32);
    let output_h = even_dimension(canvas.height.clamp(240, 3840) as i32);
    let background = ffmpeg_color(&canvas.background);
    let mut filter = String::new();
    let mut concat_labels = Vec::new();
    let mut timeline_cursor = range_start;
    let mut part_index = 0_usize;
    let mut sorted_segments = segments.iter().collect::<Vec<_>>();
    sorted_segments.sort_by_key(|segment| segment.timeline_start);

    for segment in sorted_segments {
        let segment_start = segment.timeline_start.max(range_start).min(range_end);
        let segment_end = segment.timeline_end.max(range_start).min(range_end);
        if segment_end <= segment_start {
            continue;
        }
        if segment_start > timeline_cursor {
            let duration = segment_start.saturating_sub(timeline_cursor) as f64 / session.sample_rate as f64;
            if !filter.is_empty() {
                filter.push(';');
            }
            filter.push_str(&format!(
                "color=c={background}:s={output_w}x{output_h}:d={duration:.3},setsar=1,fps=30,format=yuv420p[v{part_index}]"
            ));
            concat_labels.push(format!("[v{part_index}]"));
            part_index += 1;
            timeline_cursor = segment_start;
        }
        let effective_start = segment_start.max(timeline_cursor);
        if segment_end <= effective_start {
            continue;
        }
        let clip = &clips[segment.input_index];
        let duration = segment_end.saturating_sub(effective_start) as f64 / session.sample_rate as f64;
        let source_offset = segment.source_offset_ms as f64 / 1000.0
            + effective_start.saturating_sub(segment.timeline_start) as f64 / session.sample_rate as f64;
        // A cut-style edit shows one camera at a time, so each shot fills the whole canvas.
        // Force the box to full-frame (drop the picture-in-picture position/size used for the
        // multi-cam composition) but KEEP the per-track crop (the user's framing, e.g. cropping
        // the face out), rotation (to un-flip cameras) and color grading. The kept (cropped)
        // region is then scaled to cover the whole canvas.
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
        if !filter.is_empty() {
            filter.push(';');
        }
        filter.push_str(&format!(
            // Normalise the source's frame rate first — webcam captures often report bogus
            // r_frame_rate values (e.g. 600/1) which propagate through trim and confuse the
            // concat timing, producing stuttery/slow playback.
            "[{}:v]fps=30,trim=start={source_offset:.3}:duration={duration:.3},setpts=PTS-STARTPTS{suffix}[clip{part_index}];color=c={background}:s={output_w}x{output_h}:d={duration:.3},setsar=1[base{part_index}];[base{part_index}][clip{part_index}]overlay={x}:{y}:eof_action=pass,fps=30,format=yuv420p[v{part_index}]",
            segment.input_index
        ));
        concat_labels.push(format!("[v{part_index}]"));
        part_index += 1;
        timeline_cursor = segment_end;
    }
    if range_end > timeline_cursor {
        let duration = range_end.saturating_sub(timeline_cursor) as f64 / session.sample_rate as f64;
        if !filter.is_empty() {
            filter.push(';');
        }
        filter.push_str(&format!(
            "color=c={background}:s={output_w}x{output_h}:d={duration:.3},setsar=1,fps=30,format=yuv420p[v{part_index}]"
        ));
        concat_labels.push(format!("[v{part_index}]"));
        part_index += 1;
    }
    if filter.is_empty() {
        let duration = range_end.saturating_sub(range_start) as f64 / session.sample_rate as f64;
        filter.push_str(&format!(
            "color=c={background}:s={output_w}x{output_h}:d={duration:.3},setsar=1,fps=30,format=yuv420p[v0]"
        ));
        concat_labels.push("[v0]".to_string());
        part_index = 1;
    }
    filter.push(';');
    filter.push_str(&concat_labels.join(""));
    filter.push_str(&format!("concat=n={part_index}:v=1:a=0[v]"));
    filter
}

fn normalized_video_layout(layout: &VideoLayout) -> VideoLayout {
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

fn push_engine_commands(state: &State<'_, AppState>, session: &MixSession, actions: &[MixAction]) {
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
