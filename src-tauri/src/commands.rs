use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::{
    actions::{apply_actions, record_patch, redo, undo},
    assistant,
    audio,
    engine::commands::EngineCommand,
    model::{AssistantRequest, AssistantResponse, HistorySource, JsonPatchOp, MixAction, MixProject, MixSession, SkillCatalog},
    AppState,
};

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

#[tauri::command]
pub async fn assistant_request(state: State<'_, AppState>, request: AssistantRequest) -> Result<AssistantResponse, String> {
    let project = {
        let store = state.store.lock().map_err(|error| error.to_string())?;
        store.get_project(&request.session_id)?
    };
    let (response, project) = assistant::handle_assistant(state.config.clone(), project, request).await?;
    {
        let store = state.store.lock().map_err(|error| error.to_string())?;
        store.save(&project)?;
    }
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
pub fn render_mix(state: State<'_, AppState>, session_id: String, output_path: String) -> Result<RenderResponse, String> {
    let project = state.store.lock().map_err(|error| error.to_string())?.get_project(&session_id)?;
    let path = normalize_wav_path(PathBuf::from(output_path));
    audio::render_mix(&project.session, &path)?;
    Ok(RenderResponse { path: path.to_string_lossy().to_string() })
}

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
