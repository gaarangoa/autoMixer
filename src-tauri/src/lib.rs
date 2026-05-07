pub mod actions;
pub mod assistant;
pub mod audio;
pub mod capabilities;
pub mod commands;
pub mod config;
pub mod defaults;
pub mod engine;
pub mod model;
pub mod store;
pub mod web;

use std::sync::Mutex;

use config::Config;
use engine::AudioEngine;
use store::SessionStore;

pub struct AppState {
    pub config: Config,
    pub store: Mutex<SessionStore>,
    pub audio: Mutex<AudioEngine>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config = Config::load();
    let store = SessionStore::new(config.data_dir.clone());
    let block_size = config.audio.block_size as u32;

    let engine = AudioEngine::new(block_size);
    let shared = engine.shared();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            config,
            store: Mutex::new(store),
            audio: Mutex::new(engine),
        })
        .setup(move |app| {
            engine::telemetry::spawn_telemetry(app.handle().clone(), shared.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::get_skill_catalog,
            commands::list_ollama_models,
            commands::list_sessions,
            commands::create_session,
            commands::get_project,
            commands::import_audio_files,
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
            commands::render_mix,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AutoMixer");
}
