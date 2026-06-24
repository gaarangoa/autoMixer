pub mod actions;
pub mod ab_judge;
pub mod assistant;
pub mod audio;
pub mod audio_service;
pub mod auto_mix;
pub mod capabilities;
pub mod commands;
pub mod config;
pub mod control;
pub mod defaults;
pub mod engine;
pub mod hermes_service;
pub mod model;
pub mod recorder;
pub mod store;
pub mod web;

use std::sync::{Arc, Mutex};

use audio_service::AudioService;
use hermes_service::HermesService;
use config::Config;
use engine::AudioEngine;
use store::SessionStore;
use tauri::{
    menu::{Menu, MenuItemBuilder, MenuItemKind, PredefinedMenuItem},
    Emitter,
};

pub struct AppState {
    pub config: Config,
    pub store: Mutex<SessionStore>,
    pub audio: Mutex<AudioEngine>,
    pub audio_service: Arc<AudioService>,
    pub hermes_service: Arc<HermesService>,
    pub recorder: Mutex<Option<recorder::RecordingHandle>>,
    pub input_monitor: Mutex<Option<recorder::InputMonitorHandle>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config = Config::load();
    let store = SessionStore::new(config.data_dir.clone());
    let block_size = config.audio.block_size as u32;

    let engine = AudioEngine::new(block_size);
    let shared = engine.shared();
    let audio_service = Arc::new(AudioService::spawn());
    let hermes_service = Arc::new(HermesService::spawn());

    // Captured before `config`/`hermes_service` are moved into manage(), for the
    // background startup warm-up.
    let warm_video_base = config.video_base_url.clone();
    let warm_video_model = config.video_model.clone();
    let warm_hermes = hermes_service.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Remember window size/position across launches.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(AppState {
            config,
            store: Mutex::new(store),
            audio: Mutex::new(engine),
            audio_service,
            hermes_service,
            recorder: Mutex::new(None),
            input_monitor: Mutex::new(None),
        })
        .setup(move |app| {
            let menu = Menu::default(app.handle())?;
            let detect_structure = MenuItemBuilder::with_id("edit_detect_structure", "Detect Song Structure")
                .accelerator("CmdOrCtrl+Shift+D")
                .build(app)?;
            let level_sections = MenuItemBuilder::with_id("edit_level_sections", "Level Song Sections")
                .accelerator("CmdOrCtrl+Shift+L")
                .build(app)?;
            if let Some(MenuItemKind::Submenu(edit)) = menu.get("Edit") {
                edit.append(&PredefinedMenuItem::separator(app)?)?;
                edit.append(&detect_structure)?;
                edit.append(&level_sections)?;
            }
            app.set_menu(menu)?;
            app.on_menu_event(|app, event| match event.id().as_ref() {
                "edit_detect_structure" => {
                    let _ = app.emit("menu:detect-structure", serde_json::json!({}));
                }
                "edit_level_sections" => {
                    let _ = app.emit("menu:level-sections", serde_json::json!({}));
                }
                _ => {}
            });
            engine::telemetry::spawn_telemetry(app.handle().clone(), shared.clone());
            // In-process HTTP control surface that the Hermes agent sidecar drives
            // (loopback, ephemeral port, per-launch bearer token). Failing to bind it
            // is non-fatal — the rest of the app still runs.
            match control::spawn(app.handle().clone()) {
                Ok(info) => println!(
                    "[control] live session control surface on {} (token in env for sidecars)",
                    info.base_url()
                ),
                Err(error) => eprintln!("[control] failed to start: {error}"),
            }
            // Warm up the agent tool server + the video model's vision encoder in the
            // background so the first chat turn / video edit isn't slow. Best-effort.
            tauri::async_runtime::spawn(commands::warm_up_models(
                warm_video_base.clone(),
                warm_video_model.clone(),
                warm_hermes.clone(),
            ));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::get_skill_catalog,
            commands::list_ollama_models,
            commands::list_input_devices,
            commands::list_input_device_channels,
            commands::list_sessions,
            commands::create_session,
            commands::get_project,
            commands::import_audio_files,
            commands::create_recording_track,
            commands::create_video_track,
            commands::add_rendered_video_track,
            commands::replace_rendered_video_track,
            commands::render_video_from_script,
            commands::rerender_agent_edit,
            commands::fit_canvas_to_footage,
            commands::restart_app,
            commands::cancel_agent,
            commands::get_hermes_model,
            commands::set_hermes_model,
            commands::clear_chat,
            commands::get_video_model,
            commands::set_video_model,
            commands::set_video_selection,
            commands::get_video_selection,
            commands::apply_mix_actions,
            commands::undo_mix_action,
            commands::redo_mix_action,
            commands::apply_recorded_patch,
            commands::reset_session,
            commands::assistant_request,
            commands::transport_play,
            commands::transport_pause,
            commands::transport_stop,
            commands::transport_seek,
            commands::start_recording,
            commands::poll_recording_meters,
            commands::stop_recording,
            commands::start_input_monitor,
            commands::poll_input_monitor_meters,
            commands::stop_input_monitor,
            commands::delete_clip,
            commands::delete_clip_range,
            commands::set_master_bypass,
            commands::set_master_gain,
            commands::list_mixer_profiles,
            commands::set_mixer_profile,
            commands::save_chat_messages,
            commands::rename_session,
            commands::delete_session,
            commands::export_project_bundle,
            commands::import_project_bundle,
            commands::start_auto_mix,
            commands::analyze_master_structure,
            commands::judge_mix_ab,
            commands::render_mix,
            commands::save_video_recording,
            commands::render_video_mix,
            commands::export_rendered_video,
            commands::render_auto_video_edit,
            commands::render_agent_video_edit,
            commands::apply_clip_effects,
            commands::revert_clip_video,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AutoMixer");
}
