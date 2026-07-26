pub mod ab_judge;
pub mod actions;
pub mod assistant;
pub mod audio;
pub mod audio_service;
pub mod auto_mix;
pub mod camera_capture;
pub mod capabilities;
pub mod clip_transfer;
pub mod commands;
pub mod config;
pub mod control;
pub mod defaults;
pub mod engine;
pub mod hermes_service;
pub mod model;
pub mod recorder;
pub mod sam_audio;
pub mod store;
pub mod web;

use std::sync::{Arc, Mutex};

use audio_service::AudioService;
use config::Config;
use engine::AudioEngine;
use hermes_service::HermesService;
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

/// Build the full app menu (default + Edit extras + the open album's songs) and
/// set it. Called at setup and whenever the frontend's document state changes.
/// macOS menu mutation
/// must happen on the main thread.
pub fn build_and_set_menu(
    app: &tauri::AppHandle,
    _albums: &[commands::MenuEntry],
    sessions: &[commands::MenuEntry],
    _current_album: &str,
    current_session: &str,
) -> tauri::Result<()> {
    use tauri::menu::{CheckMenuItemBuilder, SubmenuBuilder};

    let mut songs_b = SubmenuBuilder::new(app, "Songs");
    for s in sessions {
        let item = CheckMenuItemBuilder::with_id(format!("song::{}", s.id), s.name.as_str())
            .checked(s.id == current_session)
            .build(app)?;
        songs_b = songs_b.item(&item);
    }
    let songs_sub = songs_b.build()?;

    let file = SubmenuBuilder::new(app, "Project")
        .text("file_new_album", "New Album…")
        .text("file_open_album", "Open Album…")
        .text("file_close_album", "Close Album")
        .separator()
        .text("file_new_song", "New Song")
        .item(&songs_sub)
        .separator()
        .text("file_rename_album", "Rename Album")
        .text("file_rename_song", "Rename Song")
        .separator()
        .text("file_delete_song", "Delete Song")
        .separator()
        .text("file_save_bundle", "Save Project Bundle…")
        .text("file_open_bundle", "Open Project Bundle…")
        .build()?;

    let menu = Menu::default(app)?;
    // The default menu's submenus have auto-generated IDs, NOT their titles — so
    // menu.get("File")/menu.get("Edit") silently return None. Find submenus by
    // their visible TEXT instead.
    let find_submenu = |title: &str| -> Option<tauri::menu::Submenu<tauri::Wry>> {
        menu.items().ok()?.into_iter().find_map(|item| match item {
            MenuItemKind::Submenu(sub) if sub.text().map(|t| t == title).unwrap_or(false) => {
                Some(sub)
            }
            _ => None,
        })
    };
    // Drop the default "File" submenu so our "Project" doesn't coexist with it.
    if let Some(existing_file) = find_submenu("File") {
        let _ = menu.remove(&existing_file);
    }
    let detect = MenuItemBuilder::with_id("edit_detect_structure", "Detect Song Structure")
        .accelerator("CmdOrCtrl+Shift+D")
        .build(app)?;
    let level = MenuItemBuilder::with_id("edit_level_sections", "Level Song Sections")
        .accelerator("CmdOrCtrl+Shift+L")
        .build(app)?;
    let sync_tracks = MenuItemBuilder::with_id("edit_sync_tracks", "Sync Tracks to Reference…")
        .accelerator("CmdOrCtrl+Shift+Y")
        .build(app)?;
    let split_track = MenuItemBuilder::with_id("edit_split_track", "Split Track…").build(app)?;
    if let Some(edit) = find_submenu("Edit") {
        edit.append(&PredefinedMenuItem::separator(app)?)?;
        edit.append(&detect)?;
        edit.append(&level)?;
        edit.append(&sync_tracks)?;
        edit.append(&split_track)?;
    } else {
        // No "Edit" found (unexpected) — expose the tools in their own submenu so
        // they're never unreachable.
        eprintln!("[menu] default Edit submenu not found — adding a Tracks menu");
        let tracks_menu = SubmenuBuilder::new(app, "Tracks")
            .item(&detect)
            .item(&level)
            .item(&sync_tracks)
            .item(&split_track)
            .build()?;
        menu.append(&tracks_menu)?;
    }
    menu.insert(&file, 1)?;
    app.set_menu(menu)?;
    Ok(())
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
            let handle = app.handle().clone();
            build_and_set_menu(&handle, &[], &[], "", "")?;
            app.on_menu_event(|app, event| {
                let id = event.id().as_ref();
                if let Some(song_id) = id.strip_prefix("song::") {
                    let _ = app.emit("menu:open-song", serde_json::json!({ "id": song_id }));
                    return;
                }
                let event_name = match id {
                    "edit_detect_structure" => "menu:detect-structure",
                    "edit_level_sections" => "menu:level-sections",
                    "edit_sync_tracks" => "menu:sync-tracks",
                    "edit_split_track" => "menu:split-track",
                    "file_new_album" => "menu:new-album",
                    "file_open_album" => "menu:open-album",
                    "file_close_album" => "menu:close-album",
                    "file_new_song" => "menu:new-song",
                    "file_rename_album" => "menu:rename-album",
                    "file_rename_song" => "menu:rename-song",
                    "file_delete_song" => "menu:delete-song",
                    "file_save_bundle" => "menu:save-bundle",
                    "file_open_bundle" => "menu:open-bundle",
                    _ => return,
                };
                let _ = app.emit(event_name, serde_json::json!({}));
            });
            engine::telemetry::spawn_telemetry(app.handle().clone(), shared.clone());
            // Reap camera processes orphaned by a previous hard-killed instance, and
            // compile the Swift capture helper in the background so the first
            // preview/record doesn't pay the compile cost.
            camera_capture::cleanup_orphans();
            camera_capture::spawn_preview_gc();
            std::thread::spawn(|| {
                if let Err(error) = camera_capture::ensure_helper() {
                    eprintln!("[camera] helper compile failed: {error}");
                }
            });
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
            commands::list_albums,
            commands::create_album,
            commands::open_album,
            commands::get_album,
            commands::rename_album,
            commands::close_album,
            commands::set_file_menu,
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
            sam_audio::get_sam_audio_config,
            sam_audio::set_sam_audio_config,
            sam_audio::test_sam_audio_connection,
            sam_audio::prepare_track_split,
            sam_audio::run_track_split,
            sam_audio::cancel_track_split,
            sam_audio::discard_track_split,
            sam_audio::apply_track_split,
            commands::set_config,
            commands::set_video_selection,
            commands::get_video_selection,
            commands::apply_mix_actions,
            commands::undo_mix_action,
            commands::redo_mix_action,
            commands::apply_recorded_patch,
            commands::stretch_session_tempo,
            commands::sync_tracks_to_reference,
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
            clip_transfer::transfer_clip,
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
            commands::start_camera_captures,
            commands::mark_camera_transport_start,
            commands::stop_camera_captures,
            commands::get_camera_preview_info,
            commands::stop_camera_previews,
            commands::render_video_mix,
            commands::export_rendered_video,
            commands::export_video,
            commands::render_auto_video_edit,
            commands::render_agent_video_edit,
            commands::apply_clip_effects,
            commands::revert_clip_video,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AutoMixer");
}
