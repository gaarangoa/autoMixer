use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{Emitter, State};

use crate::{
    actions::{apply_actions, record_patch, redo, undo},
    assistant,
    audio,
    engine::commands::EngineCommand,
    model::{AssistantRequest, AssistantResponse, HistorySource, JsonPatchOp, MixAction, MixProject, MixSection, MixSession, MixerProfile, SectionAnalysis, SkillCatalog},
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
pub struct RenderResponse {
    path: String,
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

fn track_slot(session: &MixSession, track_id: &str) -> Option<u32> {
    session.tracks.iter().position(|t| t.id == track_id).map(|i| i as u32)
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
