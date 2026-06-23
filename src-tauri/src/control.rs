//! In-process HTTP control surface.
//!
//! Lets an external agent (the Hermes sidecar and its `automixer-mcp` MCP server)
//! drive the **live** session: tool calls land here, run through the same
//! `apply_and_sync` path the UI uses, and the UI refreshes via a
//! `session:externally-updated` Tauri event. The server is bound to loopback on an
//! OS-assigned ephemeral port and guarded by a random per-launch bearer token; both
//! the port and token are handed to the sidecar through env vars when it is spawned.
//!
//! This is deliberately a plain HTTP surface, not an MCP server: Hermes discovers
//! MCP servers as stdio child processes it spawns itself, so the MCP protocol lives
//! in a thin Python shim that translates tool calls into HTTP calls against here.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::{
    extract::{Path as AxumPath, State as AxumState},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{apply_and_sync, redo_and_sync, undo_and_sync};
use crate::model::{HistorySource, MixAction, MixProject};
use crate::AppState;

/// Port + token of the running control server, populated once at startup so that
/// sidecar-spawning code can pass them to the Hermes child process.
static CONTROL: OnceLock<ControlInfo> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct ControlInfo {
    pub port: u16,
    pub token: String,
}

impl ControlInfo {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Returns the control server's port + token once it has been spawned.
pub fn info() -> Option<&'static ControlInfo> {
    CONTROL.get()
}

/// The user's current track selection per session, pushed from the UI. The
/// video-edit skill defaults to the selected video tracks so the agent only
/// touches what the user picked.
static SELECTION: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
fn selection_map() -> &'static Mutex<HashMap<String, Vec<String>>> {
    SELECTION.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn set_selection(session_id: &str, track_ids: Vec<String>) {
    if let Ok(mut map) = selection_map().lock() {
        map.insert(session_id.to_string(), track_ids);
    }
}
pub fn get_selection(session_id: &str) -> Vec<String> {
    selection_map()
        .lock()
        .ok()
        .and_then(|map| map.get(session_id).cloned())
        .unwrap_or_default()
}

#[derive(Clone)]
struct ControlState {
    app: AppHandle,
    token: String,
}

#[derive(Deserialize)]
struct ActionsBody {
    actions: Vec<MixAction>,
    #[serde(default)]
    explanation: Option<String>,
}

type CtlResult<T> = Result<Json<T>, (StatusCode, String)>;

fn check_auth(state: &ControlState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let ok = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|token| token == state.token)
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "missing or invalid bearer token".into()))
    }
}

/// Bind the control server (synchronously, so the port is known) and spawn it on the
/// Tauri async runtime. Stores the resulting `ControlInfo` in the process-wide cell.
pub fn spawn(app: AppHandle) -> Result<&'static ControlInfo, String> {
    let token = uuid::Uuid::new_v4().to_string();
    let state = ControlState { app, token: token.clone() };

    // Bind on the std listener first so we can read back the assigned port before
    // handing control to the async server.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener.set_nonblocking(true).map_err(|error| error.to_string())?;
    let port = listener.local_addr().map_err(|error| error.to_string())?.port();

    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/control/session/{session_id}", get(get_session))
        .route("/control/session/{session_id}/selection", get(get_session_selection).post(post_session_selection))
        .route("/control/session/{session_id}/actions", post(post_actions))
        .route("/control/session/{session_id}/undo", post(post_undo))
        .route("/control/session/{session_id}/redo", post(post_redo))
        .route("/control/session/{session_id}/video-edit", post(post_video_edit))
        .route("/control/session/{session_id}/auto-mix", post(post_auto_mix))
        .route("/control/session/{session_id}/clip-layout", post(post_clip_layout))
        .route("/control/session/{session_id}/auto-crop", post(post_auto_crop))
        .with_state(state);

    tauri::async_runtime::spawn(async move {
        match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => {
                if let Err(error) = axum::serve(listener, router).await {
                    eprintln!("[control] server error: {error}");
                }
            }
            Err(error) => eprintln!("[control] failed to adopt listener: {error}"),
        }
    });

    let _ = CONTROL.set(ControlInfo { port, token: token.clone() });
    // Publish port+token to ~/.automixer/control.json so the Hermes MCP shim (and
    // local tooling) can discover how to reach the control surface. Rewritten every
    // launch because the port and token rotate.
    if let Some(home) = std::env::var_os("HOME") {
        let dir = std::path::Path::new(&home).join(".automixer");
        let _ = std::fs::create_dir_all(&dir);
        let body = serde_json::json!({ "port": port, "token": token, "baseUrl": format!("http://127.0.0.1:{port}") });
        if let Err(error) = std::fs::write(dir.join("control.json"), body.to_string()) {
            eprintln!("[control] could not write control.json: {error}");
        }
    }
    info().ok_or_else(|| "control info not set".to_string())
}

async fn get_session(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app_state = state.app.state::<AppState>();
    let store = app_state
        .store
        .lock()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let project = store
        .get_project(&session_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(project))
}

/// Return the track ids the user currently has selected in the UI, so the agent
/// can scope edits to "the selected video" rather than touching every track.
async fn get_session_selection(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> CtlResult<serde_json::Value> {
    check_auth(&state, &headers)?;
    let track_ids = get_selection(&session_id);
    Ok(Json(serde_json::json!({ "trackIds": track_ids })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectionBody {
    track_ids: Vec<String>,
}

/// Let the agent set the track selection itself (there is no UI "select" action it can
/// otherwise reach). Stored server-side so edit_video scopes to it; also emitted so the
/// UI highlights the same tracks.
async fn post_session_selection(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<SelectionBody>,
) -> CtlResult<serde_json::Value> {
    check_auth(&state, &headers)?;
    set_selection(&session_id, body.track_ids.clone());
    let _ = state.app.emit(
        "selection:set",
        serde_json::json!({ "sessionId": session_id, "trackIds": body.track_ids }),
    );
    Ok(Json(serde_json::json!({ "ok": true, "trackIds": body.track_ids })))
}

async fn post_actions(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<ActionsBody>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app_state = state.app.state::<AppState>();
    let project = apply_and_sync(
        app_state.inner(),
        &session_id,
        &body.actions,
        HistorySource::Assistant,
        body.explanation,
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    emit_updated(&state.app, &session_id, &project);
    Ok(Json(project))
}

async fn post_undo(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app_state = state.app.state::<AppState>();
    let project = undo_and_sync(app_state.inner(), &session_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    emit_updated(&state.app, &session_id, &project);
    Ok(Json(project))
}

async fn post_redo(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app_state = state.app.state::<AppState>();
    let project = redo_and_sync(app_state.inner(), &session_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    emit_updated(&state.app, &session_id, &project);
    Ok(Json(project))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoEditBody {
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    track_ids: Option<Vec<String>>,
    #[serde(default)]
    start_sample: Option<u64>,
    #[serde(default)]
    end_sample: Option<u64>,
    #[serde(default)]
    interval_seconds: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoEditResult {
    path: String,
    cuts: usize,
    look_preset: Option<String>,
}

/// The video-edit skill: run AutoMixer's agent video pipeline (frame analysis via
/// the configured video VLM + ffmpeg multicam cut), then add the rendered result to
/// the session. This is how the text-only Hermes agent "processes video" — it calls
/// this tool; the vision happens here against the separately-configured endpoint.
async fn post_video_edit(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<VideoEditBody>,
) -> CtlResult<VideoEditResult> {
    check_auth(&state, &headers)?;
    let app = state.app.clone();
    let config = crate::config::Config::load();

    // Decide which video tracks to edit: explicit body > the user's current
    // selection (video tracks only) > all video tracks. Then compute the footage
    // region from *those* tracks (rendering the whole timeline produces black).
    let (track_ids, region_start, region_end) = {
        let app_state = app.state::<AppState>();
        let project = app_state
            .store
            .lock()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .get_project(&session_id)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

        // Source cameras only — never the agent's own rendered output, or edit_video
        // would re-cut its previous result instead of the real footage.
        let is_agent_output = |name: &str| name == "Agent video edit" || name.starts_with("Agent Edit");
        let video_ids: Vec<String> = project
            .session
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, crate::model::TrackKind::Video) && !is_agent_output(&t.name))
            .map(|t| t.id.clone())
            .collect();

        let chosen: Vec<String> = match &body.track_ids {
            Some(ids) if !ids.is_empty() => ids.clone(),
            _ => {
                let selected_video: Vec<String> = get_selection(&session_id)
                    .into_iter()
                    .filter(|id| video_ids.contains(id))
                    .collect();
                if selected_video.is_empty() { video_ids.clone() } else { selected_video }
            }
        };

        let mut min_start = u64::MAX;
        let mut max_end = 0u64;
        for t in &project.session.tracks {
            if chosen.contains(&t.id) {
                for clip in &t.video_clips {
                    min_start = min_start.min(clip.start_sample);
                    max_end = max_end.max(clip.end_sample);
                }
            }
        }
        let region = if max_end > min_start { (Some(min_start), Some(max_end)) } else { (None, None) };
        (chosen, region.0, region.1)
    };

    if track_ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "This session has no video tracks to edit.".into()));
    }
    let start_sample = body.start_sample.or(region_start);
    let end_sample = body.end_sample.or(region_end);

    let resp = crate::commands::render_agent_video_edit(
        app.clone(),
        app.state::<AppState>(),
        session_id.clone(),
        None,
        start_sample,
        end_sample,
        track_ids,
        body.interval_seconds,
        Some(config.video_base_url),
        Some(config.video_model.clone()),
        Some(config.video_model.clone()),
        Some(config.video_model),
        body.instructions,
        Some(false),
    )
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Upsert the single canonical agent-edit track at the footage's start position —
    // replace it in place rather than stacking a new identical copy every run.
    let duration_ms = probe_duration_ms(&resp.path);
    let project = crate::commands::upsert_agent_video_track(
        &app.state::<AppState>(),
        &session_id,
        std::path::Path::new(&resp.path),
        start_sample.unwrap_or(0),
        duration_ms,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    emit_updated(&app, &session_id, &project);

    // Tell the UI a render is ready so it can post a chat chip + open the monitor.
    let look = resp.look_preset.as_ref().map(|p| format!("{p:?}"));
    let _ = app.emit(
        "video:rendered",
        serde_json::json!({
            "sessionId": session_id,
            "path": resp.path,
            "cuts": resp.script.len(),
            "lookPreset": look,
        }),
    );

    Ok(Json(VideoEditResult {
        path: resp.path,
        cuts: resp.script.len(),
        look_preset: look,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoMixBody {
    /// Optional subset of stage ids; empty/omitted = the full 10-stage pipeline.
    #[serde(default)]
    stages: Option<Vec<String>>,
}

/// The auto-mix skill: run the full (or partial) auto-mix pipeline to completion and
/// report a summary. The agent calls this; the existing `auto-mix:*` progress events
/// drive the UI; the live session refreshes when it finishes.
async fn post_auto_mix(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<AutoMixBody>,
) -> CtlResult<crate::commands::AutoMixSummary> {
    check_auth(&state, &headers)?;
    let app = state.app.clone();
    let summary = crate::commands::run_auto_mix_blocking(&app, &session_id, body.stages.unwrap_or_default())
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Refresh the live UI/engine with the mixed result.
    let app_state = app.state::<AppState>();
    if let Ok(project) = app_state
        .store
        .lock()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        .and_then(|store| store.get_project(&session_id).map_err(|e| (StatusCode::BAD_REQUEST, e)))
    {
        emit_updated(&app, &session_id, &project);
        let _ = app.emit("auto-mix:complete", serde_json::json!({ "project": project }));
    }
    Ok(Json(summary))
}

/// Probe a rendered file's duration (ms) via ffprobe for the new track's length.
fn probe_duration_ms(path: &str) -> u64 {
    std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|s| (s * 1000.0) as u64)
        .unwrap_or(0)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClipLayoutBody {
    track_id: String,
    clip_id: String,
    #[serde(default)] crop_top: Option<f32>,
    #[serde(default)] crop_right: Option<f32>,
    #[serde(default)] crop_bottom: Option<f32>,
    #[serde(default)] crop_left: Option<f32>,
    #[serde(default)] x: Option<f32>,
    #[serde(default)] y: Option<f32>,
    #[serde(default)] width: Option<f32>,
    #[serde(default)] height: Option<f32>,
    #[serde(default)] rotation: Option<f32>,
    #[serde(default)] opacity: Option<f32>,
    #[serde(default)] brightness: Option<f32>,
    #[serde(default)] contrast: Option<f32>,
    #[serde(default)] saturation: Option<f32>,
    #[serde(default)] blur: Option<f32>,
}

/// Parametric crop/reframe: merge the supplied fields into a clip's layout. Takes
/// effect on the next render; the monitor previews it live.
async fn post_clip_layout(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<ClipLayoutBody>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app = state.app.clone();
    let app_state = app.state::<AppState>();
    let store = app_state
        .store
        .lock()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut project = store.get_project(&session_id).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Locate the clip and snapshot its current layout so the change is reversible.
    let ti = project
        .session
        .tracks
        .iter()
        .position(|t| t.id == body.track_id)
        .ok_or((StatusCode::BAD_REQUEST, "track not found".to_string()))?;
    let ci = project.session.tracks[ti]
        .video_clips
        .iter()
        .position(|c| c.id == body.clip_id)
        .ok_or((StatusCode::BAD_REQUEST, "clip not found".to_string()))?;
    let old_layout = project.session.tracks[ti].video_clips[ci].layout.clone().unwrap_or_default();

    let mut layout = old_layout.clone();
    if let Some(v) = body.crop_top { layout.crop_top = v; }
    if let Some(v) = body.crop_right { layout.crop_right = v; }
    if let Some(v) = body.crop_bottom { layout.crop_bottom = v; }
    if let Some(v) = body.crop_left { layout.crop_left = v; }
    if let Some(v) = body.x { layout.x = v; }
    if let Some(v) = body.y { layout.y = v; }
    if let Some(v) = body.width { layout.width = v; }
    if let Some(v) = body.height { layout.height = v; }
    if let Some(v) = body.rotation { layout.rotation = v; }
    if let Some(v) = body.opacity { layout.opacity = v; }
    if let Some(v) = body.brightness { layout.brightness = v; }
    if let Some(v) = body.contrast { layout.contrast = v; }
    if let Some(v) = body.saturation { layout.saturation = v; }
    if let Some(v) = body.blur { layout.blur = v; }
    let new_layout = crate::commands::normalized_video_layout(&layout);

    // Record as a reversible history entry (so ⌘Z / Undo restores the prior layout)
    // instead of silently overwriting the clip — every agent video change is tracked.
    let path = format!("/tracks/{ti}/videoClips/{ci}/layout");
    let to_value = |l: &crate::model::VideoLayout| {
        serde_json::to_value(l).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    };
    let forward = vec![crate::model::JsonPatchOp { op: "replace".into(), path: path.clone(), value: Some(to_value(&new_layout)?) }];
    let inverse = vec![crate::model::JsonPatchOp { op: "replace".into(), path, value: Some(to_value(&old_layout)?) }];
    crate::actions::record_patch(&mut project, forward, inverse, HistorySource::Assistant, Some("Video layout change".to_string()))
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    store.save(&project).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    drop(store);
    emit_updated(&app, &session_id, &project);
    Ok(Json(project))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoCropBody {
    track_id: String,
    clip_id: String,
    #[serde(default)]
    instructions: Option<String>,
}

/// Vision auto-crop: the configured video model looks at a frame of the clip and
/// returns a crop, which we apply to the clip's layout.
async fn post_auto_crop(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<AutoCropBody>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app = state.app.clone();
    let project = crate::commands::auto_crop_clip(
        &app,
        &session_id,
        &body.track_id,
        &body.clip_id,
        body.instructions.as_deref().unwrap_or("Crop to a tight, well-composed frame around the main subject."),
    )
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    emit_updated(&app, &session_id, &project);
    Ok(Json(project))
}

/// Tell the UI that a session changed out from under it (an external agent edit) so
/// it re-renders. The frontend listener mirrors this onto `setProject`.
fn emit_updated(app: &AppHandle, session_id: &str, project: &MixProject) {
    let _ = app.emit(
        "session:externally-updated",
        serde_json::json!({ "sessionId": session_id, "project": project }),
    );
}
