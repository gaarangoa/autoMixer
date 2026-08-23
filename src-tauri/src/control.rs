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
    body::Body,
    extract::{Path as AxumPath, Query, State as AxumState},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{apply_and_sync, redo_and_sync, reset_all_changes_and_sync, undo_and_sync};
use crate::model::{HistorySource, MixAction, MixProject, Track, TrackKind};
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

/// True while a background video edit is being generated. Guards against launching a
/// concurrent analysis/render that would fight over the single model slot and progress events.
static VIDEO_EDIT_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// RAII guard that releases VIDEO_EDIT_RUNNING on drop — including if the planning task
/// PANICS or is dropped mid-flight. Without this, a panic would leave the flag stuck
/// true forever and every future edit would 409 ("already planning") with nothing
/// actually running — which looks like the agent is permanently stuck.
struct EditRunningGuard;
impl Drop for EditRunningGuard {
    fn drop(&mut self) {
        VIDEO_EDIT_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct ControlState {
    app: AppHandle,
    token: String,
}

#[derive(Deserialize)]
struct ActionsBody {
    // Raw values so we can parse each action individually and report a precise error
    // (which action, what's wrong) instead of a bare 422 for the whole batch.
    actions: Vec<serde_json::Value>,
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
        Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token".into(),
        ))
    }
}

/// Bind the control server (synchronously, so the port is known) and spawn it on the
/// Tauri async runtime. Stores the resulting `ControlInfo` in the process-wide cell.
pub fn spawn(app: AppHandle) -> Result<&'static ControlInfo, String> {
    let token = uuid::Uuid::new_v4().to_string();
    let state = ControlState {
        app,
        token: token.clone(),
    };

    // Bind on the std listener first so we can read back the assigned port before
    // handing control to the async server.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();

    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/control/session/{session_id}", get(get_session))
        .route(
            "/control/session/{session_id}/selection",
            get(get_session_selection).post(post_session_selection),
        )
        .route("/control/session/{session_id}/actions", post(post_actions))
        .route(
            "/control/session/{session_id}/mix-track",
            post(post_create_mix_track),
        )
        .route(
            "/control/session/{session_id}/podcast-cleanup",
            post(post_podcast_cleanup),
        )
        .route("/control/session/{session_id}/undo", post(post_undo))
        .route("/control/session/{session_id}/redo", post(post_redo))
        .route(
            "/control/session/{session_id}/reset-all-changes",
            post(post_reset_all_changes),
        )
        .route(
            "/control/session/{session_id}/video-edit",
            post(post_video_edit),
        )
        .route(
            "/control/session/{session_id}/auto-mix",
            post(post_auto_mix),
        )
        .route(
            "/control/session/{session_id}/clip-layout",
            post(post_clip_layout),
        )
        .route(
            "/control/session/{session_id}/clip-effects",
            post(post_clip_effects),
        )
        .route(
            "/control/session/{session_id}/auto-crop",
            post(post_auto_crop),
        )
        // Live MJPEG camera previews (single-owner camera service). Token comes as a
        // query param because <img> tags can't set Authorization headers.
        .route("/camera/preview/{device_label}", get(get_camera_preview))
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

    let _ = CONTROL.set(ControlInfo {
        port,
        token: token.clone(),
    });
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
    Ok(Json(
        serde_json::json!({ "ok": true, "trackIds": body.track_ids }),
    ))
}

async fn post_actions(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<ActionsBody>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app_state = state.app.state::<AppState>();
    // Parse each action on its own so a single malformed one yields an actionable error
    // ("action 9 (set_compressor): missing field `releaseMs`") rather than a bare 422.
    let mut actions: Vec<MixAction> = Vec::with_capacity(body.actions.len());
    for (i, raw) in body.actions.iter().enumerate() {
        let tool = raw
            .get("tool")
            .and_then(|t| t.as_str())
            .unwrap_or("(no tool)");
        match serde_json::from_value::<MixAction>(raw.clone()) {
            Ok(action) => actions.push(action),
            Err(error) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("action {} ({}): {}", i + 1, tool, error),
                ));
            }
        }
    }
    let project = apply_and_sync(
        app_state.inner(),
        &session_id,
        &actions,
        HistorySource::Assistant,
        body.explanation,
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    emit_updated(&state.app, &session_id, &project);
    Ok(Json(project))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateMixTrackBody {
    #[serde(default)]
    track_ids: Vec<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mono: bool,
    #[serde(default)]
    include_master: bool,
    #[serde(default)]
    mute_sources: bool,
}

async fn post_create_mix_track(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<CreateMixTrackBody>,
) -> CtlResult<crate::mix_track::CreateMixTrackResult> {
    check_auth(&state, &headers)?;
    let track_ids = if body.track_ids.is_empty() {
        get_selection(&session_id)
    } else {
        body.track_ids
    };
    let app_state = state.app.state::<AppState>();
    let result = crate::mix_track::create_mix_track_and_sync(
        app_state.inner(),
        &session_id,
        &track_ids,
        crate::mix_track::CreateMixTrackOptions {
            name: body.name,
            mono: body.mono,
            include_master: body.include_master,
            mute_sources: body.mute_sources,
        },
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    emit_updated(&state.app, &session_id, &result.project);
    set_selection(&session_id, vec![result.mix_track_id.clone()]);
    let _ = state.app.emit(
        "selection:set",
        serde_json::json!({ "sessionId": session_id, "trackIds": [result.mix_track_id.clone()] }),
    );
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodcastCleanupBody {
    #[serde(default)]
    track_ids: Vec<String>,
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanedPodcastTrack {
    original_track_id: String,
    original_track_name: String,
    cleaned_track_id: String,
    cleaned_track_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PodcastCleanupResult {
    status: String,
    message: String,
    cleaned_tracks: Vec<CleanedPodcastTrack>,
}

fn podcast_cleanup_track_is_eligible(track: &Track, request_scoped: bool) -> bool {
    if track.kind != TrackKind::Audio {
        return false;
    }
    // Clean Voice stems must not recursively clean themselves. A rendered mix track is
    // also generated, but is valid input when the user or agent selected it explicitly.
    if track.ai_generated && (!request_scoped || track.role.as_deref() != Some("mix")) {
        return false;
    }
    request_scoped || !track.muted
}

fn podcast_cleanup_prompt(track: &Track, requested_prompt: &str) -> String {
    let requested_prompt = requested_prompt.trim();
    if !requested_prompt.is_empty() {
        return requested_prompt.to_lowercase();
    }
    if track.role.as_deref() == Some("mix") {
        crate::sam_audio::PODCAST_CONVERSATION_PROMPT.to_string()
    } else {
        crate::sam_audio::PODCAST_VOICE_PROMPT.to_string()
    }
}

/// Isolate spoken conversation on each requested microphone or selected mix track with
/// SAM-Audio. Sources remain in the project but are muted; the audible replacements are
/// new clean voice tracks that inherit the source track's downstream processing.
async fn post_podcast_cleanup(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<PodcastCleanupBody>,
) -> CtlResult<PodcastCleanupResult> {
    check_auth(&state, &headers)?;
    let app_state = state.app.state::<AppState>();
    let (requested_ids, request_scoped) = if !body.track_ids.is_empty() {
        (body.track_ids, true)
    } else {
        let selected = get_selection(&session_id);
        if selected.is_empty() {
            let store = app_state
                .store
                .lock()
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            let project = store
                .get_project(&session_id)
                .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
            (
                project
                    .session
                    .tracks
                    .iter()
                    .filter(|track| {
                        track.kind == TrackKind::Audio && !track.ai_generated && !track.muted
                    })
                    .map(|track| track.id.clone())
                    .collect(),
                false,
            )
        } else {
            (selected, true)
        }
    };

    let project = {
        let store = app_state
            .store
            .lock()
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
        store
            .get_project(&session_id)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?
    };
    let mut targets = Vec::new();
    for requested_id in requested_ids {
        let Some(track) = project
            .session
            .tracks
            .iter()
            .find(|track| track.id == requested_id)
        else {
            continue;
        };
        if !podcast_cleanup_track_is_eligible(track, request_scoped) {
            continue;
        }
        if !targets
            .iter()
            .any(|(track_id, _, _): &(String, String, String)| track_id == &track.id)
        {
            targets.push((
                track.id.clone(),
                track.name.clone(),
                podcast_cleanup_prompt(track, body.prompt.as_deref().unwrap_or_default()),
            ));
        }
    }
    if targets.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No eligible microphone or selected mix tracks are available for podcast cleanup. Existing Clean Voice tracks cannot be cleaned again.".into(),
        ));
    }

    let starting_message = format!(
        "SAM-Audio voice cleanup is starting for {} audio track{}. Audio will be sent to the configured SAM-Audio endpoint; each source will be preserved and muted only after its clean voice stem succeeds.",
        targets.len(),
        if targets.len() == 1 { "" } else { "s" }
    );
    let _ = state.app.emit(
        "podcast-cleanup:status",
        serde_json::json!({
            "sessionId": session_id,
            "phase": "starting",
            "message": starting_message,
            "current": 0,
            "total": targets.len(),
        }),
    );

    if let Err(error) = crate::sam_audio::test_sam_audio_connection().await {
        let message = format!("SAM-Audio cleanup could not start: {error}");
        let _ = state.app.emit(
            "podcast-cleanup:status",
            serde_json::json!({
                "sessionId": session_id,
                "phase": "error",
                "message": message,
                "current": 0,
                "total": targets.len(),
            }),
        );
        return Err((StatusCode::BAD_GATEWAY, message));
    }

    let mut cleaned_tracks = Vec::new();
    for (index, (track_id, track_name, prompt)) in targets.iter().enumerate() {
        let current_project = {
            let store = app_state
                .store
                .lock()
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            store
                .get_project(&session_id)
                .map_err(|error| (StatusCode::BAD_REQUEST, error))?
        };
        let (start_sample, end_sample) =
            crate::sam_audio::track_audio_bounds(&current_project.session, track_id)
                .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        let track_message = format!(
            "SAM-Audio is isolating spoken voice from {} ({} of {}).",
            track_name,
            index + 1,
            targets.len()
        );
        let _ = state.app.emit(
            "podcast-cleanup:status",
            serde_json::json!({
                "sessionId": session_id,
                "phase": "track-start",
                "message": track_message,
                "trackId": track_id,
                "trackName": track_name,
                "current": index + 1,
                "total": targets.len(),
            }),
        );
        let preview = crate::sam_audio::prepare_track_split_internal(
            app_state.inner(),
            session_id.clone(),
            track_id.clone(),
            start_sample,
            end_sample,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        let preview_id = preview.preview_id().to_string();
        if let Err(error) =
            crate::sam_audio::run_track_split(state.app.clone(), preview_id.clone(), prompt.clone())
                .await
        {
            let _ = crate::sam_audio::discard_track_split(preview_id);
            let message = format!("SAM-Audio cleanup failed on {track_name}: {error}");
            let _ = state.app.emit(
                "podcast-cleanup:status",
                serde_json::json!({
                    "sessionId": session_id,
                    "phase": "error",
                    "message": message,
                    "current": index + 1,
                    "total": targets.len(),
                }),
            );
            return Err((StatusCode::BAD_GATEWAY, message));
        }
        let validation_message = format!(
            "Checking that SAM-Audio kept {}'s speech in the clean voice result.",
            track_name
        );
        let _ = state.app.emit(
            "podcast-cleanup:status",
            serde_json::json!({
                "sessionId": session_id,
                "phase": "validating",
                "message": validation_message,
                "trackId": track_id,
                "trackName": track_name,
                "current": index + 1,
                "total": targets.len(),
            }),
        );
        if let Err(error) = crate::sam_audio::validate_podcast_voice_preview(&preview_id) {
            let _ = crate::sam_audio::discard_track_split(preview_id);
            let message = format!("SAM-Audio cleanup was rejected for {track_name}: {error}");
            let _ = state.app.emit(
                "podcast-cleanup:status",
                serde_json::json!({
                    "sessionId": session_id,
                    "phase": "error",
                    "message": message,
                    "current": index + 1,
                    "total": targets.len(),
                }),
            );
            return Err((StatusCode::BAD_GATEWAY, message));
        }
        let applied = crate::sam_audio::apply_track_split_internal(
            app_state.inner(),
            preview_id,
            crate::sam_audio::SplitApplyMode::PodcastVoiceCleanup,
            HistorySource::Assistant,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        let cleaned_name = applied
            .project
            .session
            .tracks
            .iter()
            .find(|track| track.id == applied.extracted_track_id)
            .map(|track| track.name.clone())
            .unwrap_or_else(|| format!("{track_name} · Clean Voice"));
        emit_updated(&state.app, &session_id, &applied.project);
        cleaned_tracks.push(CleanedPodcastTrack {
            original_track_id: track_id.clone(),
            original_track_name: track_name.clone(),
            cleaned_track_id: applied.extracted_track_id,
            cleaned_track_name: cleaned_name,
        });
    }

    let message = format!(
        "SAM-Audio cleanup completed: {} clean voice track{} added; the source tracks remain preserved and muted.",
        cleaned_tracks.len(),
        if cleaned_tracks.len() == 1 { " was" } else { "s were" }
    );
    let cleaned_track_ids: Vec<String> = cleaned_tracks
        .iter()
        .map(|track| track.cleaned_track_id.clone())
        .collect();
    set_selection(&session_id, cleaned_track_ids.clone());
    let _ = state.app.emit(
        "selection:set",
        serde_json::json!({ "sessionId": session_id, "trackIds": cleaned_track_ids }),
    );
    let _ = state.app.emit(
        "podcast-cleanup:status",
        serde_json::json!({
            "sessionId": session_id,
            "phase": "complete",
            "message": message,
            "current": cleaned_tracks.len(),
            "total": cleaned_tracks.len(),
        }),
    );
    Ok(Json(PodcastCleanupResult {
        status: "completed".into(),
        message,
        cleaned_tracks,
    }))
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResetAllChangesResult {
    status: &'static str,
    reverted_entries: usize,
    message: String,
    project: MixProject,
}

async fn post_reset_all_changes(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> CtlResult<ResetAllChangesResult> {
    check_auth(&state, &headers)?;
    let app_state = state.app.state::<AppState>();
    let (project, reverted_entries) = reset_all_changes_and_sync(app_state.inner(), &session_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    emit_updated(&state.app, &session_id, &project);
    let message = if reverted_entries == 0 {
        "The project is already at its original state.".into()
    } else {
        format!(
            "Restored the original project state by reverting {reverted_entries} edit history {}. Original media and tracks were preserved.",
            if reverted_entries == 1 { "entry" } else { "entries" }
        )
    };
    Ok(Json(ResetAllChangesResult {
        status: "completed",
        reverted_entries,
        message,
        project,
    }))
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
    /// Optional review workflow. The normal chat command renders and attaches the final
    /// video automatically; this mode deliberately stops at the editable cut plan.
    #[serde(default)]
    review_only: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoEditResult {
    status: String,
    message: String,
}

/// Run AutoMixer's directed edit pipeline (frame analysis via the configured video VLM),
/// render the result, and attach it to the timeline. An explicit review-only request can
/// stop at the editable plan instead.
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
        let is_agent_output =
            |name: &str| name == "Agent video edit" || name.starts_with("Agent Edit");
        let video_ids: Vec<String> = project
            .session
            .tracks
            .iter()
            .filter(|t| {
                matches!(t.kind, crate::model::TrackKind::Video) && !is_agent_output(&t.name)
            })
            .map(|t| t.id.clone())
            .collect();

        let chosen: Vec<String> = match &body.track_ids {
            Some(ids) if !ids.is_empty() => ids.clone(),
            _ => {
                let selected_video: Vec<String> = get_selection(&session_id)
                    .into_iter()
                    .filter(|id| video_ids.contains(id))
                    .collect();
                if selected_video.is_empty() {
                    video_ids.clone()
                } else {
                    selected_video
                }
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
        let region = if max_end > min_start {
            (Some(min_start), Some(max_end))
        } else {
            (None, None)
        };
        (chosen, region.0, region.1)
    };

    if track_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "This session has no video tracks to edit.".into(),
        ));
    }
    let start_sample = body.start_sample.or(region_start);
    let end_sample = body.end_sample.or(region_end);

    // Only one background edit at a time (they'd contend on the single model slot).
    if VIDEO_EDIT_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err((
            StatusCode::CONFLICT,
            "A video edit is already running. Wait for it to finish before starting another."
                .into(),
        ));
    }

    // Run in a DETACHED background task so analysis and rendering survive the agent's
    // chat turn ending. Direct chat requests create the output track automatically.
    let instructions = body.instructions.clone();
    let interval_seconds = body.interval_seconds;
    let review_only = body.review_only;
    let task_session = session_id.clone();
    tauri::async_runtime::spawn(async move {
        // Releases the running-flag on ANY exit (return, error, panic, drop).
        let _guard = EditRunningGuard;
        let result = render_video_edit_job(
            app.clone(),
            task_session.clone(),
            start_sample,
            end_sample,
            track_ids,
            interval_seconds,
            config,
            instructions,
            review_only,
        )
        .await;
        if let Err(error) = result {
            // Surface the failure to the UI as a terminal progress event so the
            // planning overlay clears instead of hanging.
            let _ = app.emit(
                "video:edit-failed",
                serde_json::json!({ "sessionId": task_session, "error": error }),
            );
        }
    });

    Ok(Json(VideoEditResult {
        status: "started".into(),
        message: if review_only {
            "The directed edit plan is being generated in the background for optional review. YOUR TURN IS COMPLETE: reply with ONE short sentence saying the review plan is being generated, then STOP. Do NOT call any more tools or re-run edit_video.".into()
        } else {
            "The directed edit is being analyzed and rendered in the background. The finished video will be added automatically as the Agent video edit track. YOUR TURN IS COMPLETE: reply with ONE short sentence saying rendering started, then STOP. Do NOT call any more tools or re-run edit_video.".into()
        },
    }))
}

/// The directed video job, run in a detached background task (see post_video_edit).
/// Normally it renders and attaches the result; explicit review-only mode emits the
/// structured contract and editable shot plan instead.
#[allow(clippy::too_many_arguments)]
async fn render_video_edit_job(
    app: AppHandle,
    session_id: String,
    start_sample: Option<u64>,
    end_sample: Option<u64>,
    track_ids: Vec<String>,
    interval_seconds: Option<f64>,
    config: crate::config::Config,
    instructions: Option<String>,
    review_only: bool,
) -> Result<(), String> {
    let source_track_ids = track_ids.clone();
    let resp = crate::commands::render_agent_video_edit(
        app.clone(),
        app.state::<AppState>(),
        session_id.clone(),
        None,
        start_sample,
        end_sample,
        track_ids,
        interval_seconds,
        Some(config.video_base_url),
        Some(config.video_model.clone()),
        Some(config.video_model.clone()),
        Some(config.video_model),
        instructions,
        None,
        Some(review_only),
    )
    .await?;
    if review_only {
        let _ = app.emit(
            "video:plan-ready",
            serde_json::json!({
                "sessionId": session_id,
                "sourceTrackIds": source_track_ids,
                "startSample": start_sample,
                "endSample": end_sample,
                "intervalSeconds": interval_seconds.unwrap_or(0.5),
                "script": resp.script,
                "editBrief": resp.edit_brief,
                "validation": resp.validation,
                "lookPreset": resp.look_preset,
                "colorGrade": resp.color_grade,
                "videoEffects": resp.video_effects,
            }),
        );
        return Ok(());
    }

    let render_path = std::path::Path::new(&resp.path);
    let app_state = app.state::<AppState>();
    let sample_rate = {
        let store = app_state.store.lock().map_err(|error| error.to_string())?;
        store.get_project(&session_id)?.session.sample_rate
    };
    let fallback_duration_ms = end_sample
        .unwrap_or(start_sample.unwrap_or(0).saturating_add(1))
        .saturating_sub(start_sample.unwrap_or(0))
        .saturating_mul(1_000)
        .checked_div(sample_rate.max(1) as u64)
        .unwrap_or(1)
        .max(1);
    let duration_ms = crate::commands::probe_video_duration(render_path)
        .map(|seconds| (seconds * 1_000.0).round() as u64)
        .unwrap_or(fallback_duration_ms)
        .max(1);
    let project = crate::commands::upsert_agent_video_track(
        app_state.inner(),
        &session_id,
        render_path,
        start_sample.unwrap_or(0),
        duration_ms,
    )?;
    emit_updated(&app, &session_id, &project);
    let _ = app.emit(
        "video:rendered",
        serde_json::json!({
            "sessionId": session_id,
            "path": resp.path,
            "cuts": resp.script.len(),
            "lookPreset": resp.look_preset,
        }),
    );
    Ok(())
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
    let summary =
        crate::commands::run_auto_mix_blocking(&app, &session_id, body.stages.unwrap_or_default())
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Refresh the live UI/engine with the mixed result.
    let app_state = app.state::<AppState>();
    if let Ok(project) = app_state
        .store
        .lock()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        .and_then(|store| {
            store
                .get_project(&session_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, e))
        })
    {
        emit_updated(&app, &session_id, &project);
        let _ = app.emit(
            "auto-mix:complete",
            serde_json::json!({ "sessionId": session_id, "project": project }),
        );
    }
    Ok(Json(summary))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClipLayoutBody {
    track_id: String,
    clip_id: String,
    #[serde(default)]
    crop_top: Option<f32>,
    #[serde(default)]
    crop_right: Option<f32>,
    #[serde(default)]
    crop_bottom: Option<f32>,
    #[serde(default)]
    crop_left: Option<f32>,
    #[serde(default)]
    x: Option<f32>,
    #[serde(default)]
    y: Option<f32>,
    #[serde(default)]
    width: Option<f32>,
    #[serde(default)]
    height: Option<f32>,
    #[serde(default)]
    rotation: Option<f32>,
    #[serde(default)]
    opacity: Option<f32>,
    #[serde(default)]
    brightness: Option<f32>,
    #[serde(default)]
    contrast: Option<f32>,
    #[serde(default)]
    saturation: Option<f32>,
    #[serde(default)]
    blur: Option<f32>,
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
    let mut project = store
        .get_project(&session_id)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

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
    let old_layout = project.session.tracks[ti].video_clips[ci]
        .layout
        .clone()
        .unwrap_or_default();

    let mut layout = old_layout.clone();
    if let Some(v) = body.crop_top {
        layout.crop_top = v;
    }
    if let Some(v) = body.crop_right {
        layout.crop_right = v;
    }
    if let Some(v) = body.crop_bottom {
        layout.crop_bottom = v;
    }
    if let Some(v) = body.crop_left {
        layout.crop_left = v;
    }
    if let Some(v) = body.x {
        layout.x = v;
    }
    if let Some(v) = body.y {
        layout.y = v;
    }
    if let Some(v) = body.width {
        layout.width = v;
    }
    if let Some(v) = body.height {
        layout.height = v;
    }
    if let Some(v) = body.rotation {
        layout.rotation = v;
    }
    if let Some(v) = body.opacity {
        layout.opacity = v;
    }
    if let Some(v) = body.brightness {
        layout.brightness = v;
    }
    if let Some(v) = body.contrast {
        layout.contrast = v;
    }
    if let Some(v) = body.saturation {
        layout.saturation = v;
    }
    if let Some(v) = body.blur {
        layout.blur = v;
    }
    let new_layout = crate::commands::normalized_video_layout(&layout);

    // Record as a reversible history entry (so ⌘Z / Undo restores the prior layout)
    // instead of silently overwriting the clip — every agent video change is tracked.
    let path = format!("/tracks/{ti}/videoClips/{ci}/layout");
    let to_value = |l: &crate::model::VideoLayout| {
        serde_json::to_value(l).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    };
    let forward = vec![crate::model::JsonPatchOp {
        op: "replace".into(),
        path: path.clone(),
        value: Some(to_value(&new_layout)?),
    }];
    let inverse = vec![crate::model::JsonPatchOp {
        op: "replace".into(),
        path,
        value: Some(to_value(&old_layout)?),
    }];
    crate::actions::record_patch(
        &mut project,
        forward,
        inverse,
        HistorySource::Assistant,
        Some("Video layout change".to_string()),
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    store
        .save(&project)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    drop(store);
    emit_updated(&app, &session_id, &project);
    Ok(Json(project))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClipEffectsBody {
    track_id: String,
    clip_id: String,
    #[serde(default)]
    fade_in_seconds: Option<f32>,
    #[serde(default)]
    fade_out_seconds: Option<f32>,
    #[serde(default)]
    speed_factor: Option<f32>,
}

/// Apply fade-in / fade-out / speed to a video clip (re-encodes it in place). Lets the
/// agent do a simple "fade in 2s, fade out 10s" without a full multicam re-edit.
async fn post_clip_effects(
    AxumState(state): AxumState<ControlState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<ClipEffectsBody>,
) -> CtlResult<MixProject> {
    check_auth(&state, &headers)?;
    let app = state.app.clone();
    let project = crate::commands::apply_video_effects(
        &app,
        &session_id,
        &body.track_id,
        &body.clip_id,
        body.fade_in_seconds,
        body.fade_out_seconds,
        body.speed_factor,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
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
        body.instructions
            .as_deref()
            .unwrap_or("Crop to a tight, well-composed frame around the main subject."),
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

/// Stream a camera's live MJPEG preview (multipart/x-mixed-replace). The preview
/// process is owned by camera_capture — starting one here can never fight a
/// recording (subscribe_preview refuses while that device records).
async fn get_camera_preview(
    AxumState(state): AxumState<ControlState>,
    AxumPath(device_label): AxumPath<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if query.get("token").map(|t| t == &state.token) != Some(true) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("missing or invalid token"))
            .unwrap();
    }
    let rx = match crate::camera_capture::subscribe_preview(&device_label) {
        Ok(rx) => rx,
        Err(message) => {
            let status = if message == "recording" {
                StatusCode::CONFLICT
            } else {
                StatusCode::NOT_FOUND
            };
            return Response::builder()
                .status(status)
                .body(Body::from(message))
                .unwrap();
        }
    };
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    return Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(chunk)), rx))
                }
                // Lagged: this client fell behind the broadcast — skip ahead.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "multipart/x-mixed-replace;boundary=ffmpeg")
        .header("cache-control", "no-store")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicitly_selected_mix_track_is_valid_podcast_cleanup_input() {
        let mut mix =
            crate::defaults::make_track("source".into(), "Selected Tracks · Mix".into(), 0);
        mix.ai_generated = true;
        mix.role = Some("mix".into());
        mix.muted = true;

        assert!(podcast_cleanup_track_is_eligible(&mix, true));
        assert!(!podcast_cleanup_track_is_eligible(&mix, false));
    }

    #[test]
    fn clean_voice_stem_cannot_be_cleaned_recursively() {
        let mut clean =
            crate::defaults::make_track("source".into(), "Dialogue · Clean Voice".into(), 0);
        clean.ai_generated = true;
        clean.role = Some("lead_vocal".into());

        assert!(!podcast_cleanup_track_is_eligible(&clean, true));
    }

    #[test]
    fn cleanup_prompt_matches_single_mic_or_conversation_mix() {
        let mic = crate::defaults::make_track("source-a".into(), "David".into(), 0);
        let mut mix =
            crate::defaults::make_track("source-b".into(), "Selected Tracks · Mix".into(), 1);
        mix.role = Some("mix".into());

        assert_eq!(podcast_cleanup_prompt(&mic, ""), "person speaking");
        assert_eq!(podcast_cleanup_prompt(&mix, ""), "people speaking");
        assert_eq!(
            podcast_cleanup_prompt(&mix, "  Group Conversation  "),
            "group conversation"
        );
    }
}
