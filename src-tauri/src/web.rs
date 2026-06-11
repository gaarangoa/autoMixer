use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use crate::{
    actions::{apply_actions, redo, undo},
    assistant,
    audio,
    capabilities,
    config::Config,
    model::{AssistantRequest, AssistantResponse, HistorySource, MixAction, MixProject, MixSession, SkillCatalog},
    store::SessionStore,
};

#[derive(Clone)]
struct WebState {
    config: Config,
    store: Arc<Mutex<SessionStore>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UiConfig {
    ollama_base_url: String,
    ollama_model: String,
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<String>,
    provider: String,
}

#[derive(Serialize)]
struct RenderResponse {
    path: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Deserialize)]
struct CreateSessionBody {
    name: Option<String>,
}

#[derive(Deserialize)]
struct ActionsBody {
    actions: Vec<MixAction>,
    explanation: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportPathsBody {
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderBody {
    output_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsQuery {
    base_url: Option<String>,
}

type WebResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;

pub async fn run_remote_server(config: Config) -> Result<(), String> {
    let host = std::env::var("AUTOMIXER_WEB_HOST").unwrap_or_else(|_| config.web.host.clone());
    let port = std::env::var("AUTOMIXER_WEB_PORT")
        .ok()
        .and_then(|item| item.parse::<u16>().ok())
        .unwrap_or(config.web.port);
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|error| format!("Invalid web bind address {host}:{port}: {error}"))?;
    let state = WebState {
        config: config.clone(),
        store: Arc::new(Mutex::new(SessionStore::new(config.data_dir.clone()))),
    };

    let app = Router::new()
        .route("/api/config", get(get_config))
        .route("/api/ollama/models", get(list_ollama_models))
        .route("/api/skills", get(get_skill_catalog))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/{session_id}", get(get_project))
        .route("/api/sessions/{session_id}/actions", post(apply_mix_actions))
        .route("/api/sessions/{session_id}/undo", post(undo_mix_action))
        .route("/api/sessions/{session_id}/redo", post(redo_mix_action))
        .route("/api/sessions/{session_id}/import-paths", post(import_audio_paths))
        .route("/api/sessions/{session_id}/render", post(render_mix))
        .route("/api/assistant", post(assistant_request))
        .layer(CorsLayer::permissive())
        .with_state(state);

    println!("AutoMixer Rust web bridge listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|error| error.to_string())?;
    axum::serve(listener, app).await.map_err(|error| error.to_string())
}

async fn get_config(State(state): State<WebState>) -> Json<UiConfig> {
    Json(UiConfig {
        ollama_base_url: state.config.ollama_base_url,
        ollama_model: state.config.ollama_model,
    })
}

async fn list_ollama_models(State(state): State<WebState>, Query(query): Query<ModelsQuery>) -> WebResult<ModelsResponse> {
    let base_url = query.base_url.unwrap_or(state.config.ollama_base_url);
    assistant::list_models(base_url)
        .await
        .map(|(provider, models)| Json(ModelsResponse { models, provider: provider.label().to_string() }))
        .map_err(error)
}

async fn get_skill_catalog() -> Json<SkillCatalog> {
    Json(capabilities::skill_catalog())
}

async fn list_sessions(State(state): State<WebState>) -> WebResult<Vec<MixSession>> {
    let store = state.store.lock().map_err(|item| error(item.to_string()))?;
    store.list_sessions().map(Json).map_err(error)
}

async fn create_session(State(state): State<WebState>, Json(body): Json<CreateSessionBody>) -> WebResult<MixProject> {
    let store = state.store.lock().map_err(|item| error(item.to_string()))?;
    store.create_session(body.name.unwrap_or_else(|| "Untitled mix".to_string())).map(Json).map_err(error)
}

async fn get_project(State(state): State<WebState>, AxumPath(session_id): AxumPath<String>) -> WebResult<MixProject> {
    let store = state.store.lock().map_err(|item| error(item.to_string()))?;
    store.get_project(&session_id).map(Json).map_err(error)
}

async fn apply_mix_actions(
    State(state): State<WebState>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<ActionsBody>,
) -> WebResult<MixProject> {
    let store = state.store.lock().map_err(|item| error(item.to_string()))?;
    let mut project = store.get_project(&session_id).map_err(error)?;
    apply_actions(&mut project, &body.actions, HistorySource::User, body.explanation).map_err(error)?;
    store.save(&project).map_err(error)?;
    Ok(Json(project))
}

async fn undo_mix_action(State(state): State<WebState>, AxumPath(session_id): AxumPath<String>) -> WebResult<MixProject> {
    let store = state.store.lock().map_err(|item| error(item.to_string()))?;
    let mut project = store.get_project(&session_id).map_err(error)?;
    undo(&mut project).map_err(error)?;
    store.save(&project).map_err(error)?;
    Ok(Json(project))
}

async fn redo_mix_action(State(state): State<WebState>, AxumPath(session_id): AxumPath<String>) -> WebResult<MixProject> {
    let store = state.store.lock().map_err(|item| error(item.to_string()))?;
    let mut project = store.get_project(&session_id).map_err(error)?;
    redo(&mut project).map_err(error)?;
    store.save(&project).map_err(error)?;
    Ok(Json(project))
}

async fn import_audio_paths(
    State(state): State<WebState>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<ImportPathsBody>,
) -> WebResult<MixProject> {
    let store = state.store.lock().map_err(|item| error(item.to_string()))?;
    let mut latest = None;
    for path in body.paths {
        latest = Some(store.add_source_file(&session_id, Path::new(&path)).map_err(error)?);
    }
    latest.map(Json).map_or_else(|| store.get_project(&session_id).map(Json).map_err(error), Ok)
}

async fn render_mix(
    State(state): State<WebState>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<RenderBody>,
) -> WebResult<RenderResponse> {
    let project = {
        let store = state.store.lock().map_err(|item| error(item.to_string()))?;
        store.get_project(&session_id).map_err(error)?
    };
    let path = normalize_wav_path(PathBuf::from(body.output_path));
    audio::render_mix(&project.session, &path).map_err(error)?;
    Ok(Json(RenderResponse { path: path.to_string_lossy().to_string() }))
}

async fn assistant_request(State(state): State<WebState>, Json(request): Json<AssistantRequest>) -> WebResult<AssistantResponse> {
    let project = {
        let store = state.store.lock().map_err(|item| error(item.to_string()))?;
        store.get_project(&request.session_id).map_err(error)?
    };
    let observer: std::sync::Arc<dyn assistant::LlmObserver> = std::sync::Arc::new(assistant::NoopObserver);
    let (response, project) = assistant::handle_assistant(state.config.clone(), project, request, observer).await.map_err(error)?;
    {
        let store = state.store.lock().map_err(|item| error(item.to_string()))?;
        store.save(&project).map_err(error)?;
    }
    Ok(Json(response))
}

fn normalize_wav_path(path: PathBuf) -> PathBuf {
    if path.extension().and_then(|item| item.to_str()).is_some() {
        path
    } else {
        path.with_extension("wav")
    }
}

fn error(message: String) -> (StatusCode, Json<ErrorResponse>) {
    (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: message }))
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, Json(self)).into_response()
    }
}
