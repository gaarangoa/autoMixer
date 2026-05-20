use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::Serialize;
use tauri::{Emitter, State};

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
}

#[derive(Serialize)]
pub struct RenderResponse {
    path: String,
}

#[derive(Clone)]
struct VideoRenderClip {
    path: PathBuf,
    start_sample: u64,
    end_sample: u64,
    source_offset_ms: u64,
    layout: VideoLayout,
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
pub fn create_recording_track(state: State<'_, AppState>, session_id: String) -> Result<MixProject, String> {
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .create_recording_track(&session_id)?;
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
    if let Ok(audio) = state.audio.lock() {
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
    let handle = crate::recorder::start_recording(path, safe_start, target_track_id, input_device)?;
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
    let peaks = recorder
        .as_ref()
        .map(|handle| handle.drain_meters().into_iter().map(|meter| meter.peak).collect())
        .unwrap_or_default();
    Ok(RecordingMetersResponse { peaks })
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
    let peaks = monitor
        .as_ref()
        .map(|handle| handle.drain_meters().into_iter().map(|meter| meter.peak).collect())
        .unwrap_or_default();
    Ok(RecordingMetersResponse { peaks })
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
    if value.len() == 7 && value.starts_with('#') && value.chars().skip(1).all(|c| c.is_ascii_hexdigit()) {
        format!("0x{}", &value[1..])
    } else {
        "black".into()
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
