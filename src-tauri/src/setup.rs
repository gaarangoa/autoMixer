//! First-run managed runtime installer.
//!
//! The application bundle contains small, pinned executables (uv,
//! FFmpeg/FFprobe and llama.cpp), while Python environments and optional model
//! weights live under ~/.automixer where they can be updated without mutating
//! the application bundle. Model downloads are resumable and SHA-256 verified.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::AppState;

const SETUP_VERSION: u32 = 1;
const HERMES_VERSION: &str = "0.17.0";
const LOCAL_BASE_URL: &str = "http://127.0.0.1:2261";
const LOCAL_MODEL_ALIAS: &str = "qwen3.6-35b-a3b";
const MODEL_FILE: &str = "Qwen3.6-35B-A3B-UD-Q5_K_M.gguf";
const MODEL_SIZE: u64 = 26_456_194_016;
const MODEL_SHA256: &str = "c13ce26253ea334df472bd8fbd2d6da66d8a41195c17f6fcbf44c4d20ece0932";
const MODEL_URL: &str = "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/resolve/main/Qwen3.6-35B-A3B-UD-Q5_K_M.gguf";
const MMPROJ_FILE: &str = "mmproj-F16.gguf";
const MMPROJ_SIZE: u64 = 899_283_680;
const MMPROJ_SHA256: &str = "8971ee4f331ff0a4c609374f32984b3d4e6dc086c0aa35f1d637fad1829e887f";
const MMPROJ_URL: &str =
    "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/resolve/main/mmproj-F16.gguf";

static SETUP_CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    pub complete: bool,
    pub setup_version: u32,
    pub platform: String,
    pub managed_root: String,
    pub tools_ready: bool,
    pub hermes_ready: bool,
    pub audio_service_ready: bool,
    pub agent_service_ready: bool,
    pub model_server_ready: bool,
    pub local_model_installed: bool,
    pub launch_agent_installed: bool,
    pub configured_mode: Option<String>,
    pub configured_base_url: String,
    pub configured_model: String,
    pub model_download_bytes: u64,
    pub model_download_total: u64,
    pub memory_bytes: Option<u64>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupRequest {
    pub mode: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupProgress {
    stage: String,
    message: String,
    current_bytes: u64,
    total_bytes: u64,
    progress: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupReceipt {
    version: u32,
    mode: String,
    base_url: String,
    model: String,
    completed_at_unix: u64,
}

fn home_dir() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "Could not resolve the current user's home directory".into())
}

fn managed_root() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".automixer"))
}

fn setup_receipt_path() -> Result<PathBuf, String> {
    Ok(managed_root()?.join("setup.json"))
}

fn model_dir() -> Result<PathBuf, String> {
    Ok(managed_root()?.join("models"))
}

fn managed_hermes_bin() -> Result<PathBuf, String> {
    Ok(managed_root()?
        .join("hermes-agent")
        .join("venv")
        .join("bin")
        .join("hermes"))
}

fn model_endpoint_healthy(base_url: &str) -> bool {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return false;
    }
    let root = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let models_url = if trimmed.ends_with("/v1") {
        format!("{trimmed}/models")
    } else {
        format!("{trimmed}/v1/models")
    };
    let api_key = read_api_key();
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    [format!("{root}/health"), models_url]
        .into_iter()
        .any(|url| {
            let mut request = client.get(url);
            if let Some(key) = api_key.as_deref() {
                request = request.bearer_auth(key);
            }
            request
                .send()
                .is_ok_and(|response| response.status().is_success())
        })
}

fn service_healthy(port: u16) -> bool {
    model_endpoint_healthy(&format!("http://127.0.0.1:{port}"))
}

fn read_receipt() -> Option<SetupReceipt> {
    let path = setup_receipt_path().ok()?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn physical_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn setup_status_sync() -> SetupStatus {
    let root = managed_root().unwrap_or_else(|_| PathBuf::from(".automixer"));
    let tool_status = crate::media_tools::check_external_dependencies();
    let tools_ready = tool_status.iter().all(|tool| tool.available);
    let mut errors: Vec<String> = tool_status
        .iter()
        .filter(|tool| !tool.available)
        .filter_map(|tool| {
            tool.error
                .clone()
                .map(|error| format!("{}: {error}", tool.name))
        })
        .collect();

    let managed_hermes = managed_hermes_bin().ok();
    let fallback_hermes = crate::hermes_service::hermes_bin_path();
    let hermes_path = managed_hermes
        .as_ref()
        .filter(|path| path.is_file())
        .cloned()
        .unwrap_or(fallback_hermes);
    let hermes_ready = hermes_path.is_file()
        && Command::new(&hermes_path)
            .args(["acp", "--check"])
            .output()
            .is_ok_and(|output| output.status.success());
    if !hermes_ready {
        errors.push("Hermes ACP runtime is not installed".into());
    }

    let model_path = root.join("models").join(MODEL_FILE);
    let mmproj_path = root.join("models").join(MMPROJ_FILE);
    let local_model_installed =
        file_has_size(&model_path, MODEL_SIZE) && file_has_size(&mmproj_path, MMPROJ_SIZE);
    let launch_agent_path = home_dir()
        .ok()
        .map(|home| home.join("Library/LaunchAgents/com.automixer.model-server.plist"));
    let receipt = read_receipt();
    let receipt_is_current = receipt
        .as_ref()
        .is_some_and(|entry| entry.version >= SETUP_VERSION);
    let configured = crate::config::Config::load();
    let existing_install = tools_ready && hermes_ready;

    SetupStatus {
        // A receipt records the selected mode, but never masks a damaged core
        // runtime. Conversely, a healthy pre-installer development setup migrates
        // without forcing the user through onboarding.
        complete: existing_install && (receipt.is_none() || receipt_is_current),
        setup_version: SETUP_VERSION,
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        managed_root: root.display().to_string(),
        tools_ready,
        hermes_ready,
        audio_service_ready: service_healthy(7321),
        agent_service_ready: service_healthy(7322),
        model_server_ready: model_endpoint_healthy(&configured.ollama_base_url),
        local_model_installed,
        launch_agent_installed: launch_agent_path.is_some_and(|path| path.is_file()),
        configured_mode: receipt.map(|entry| entry.mode),
        configured_base_url: configured.ollama_base_url,
        configured_model: configured.ollama_model,
        model_download_bytes: partial_download_bytes(&model_path),
        model_download_total: MODEL_SIZE + MMPROJ_SIZE,
        memory_bytes: physical_memory_bytes(),
        errors,
    }
}

fn file_has_size(path: &Path, expected: u64) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.len() == expected)
}

fn partial_download_bytes(model_path: &Path) -> u64 {
    let model_part = model_path.with_extension("gguf.part");
    let Some(parent) = model_path.parent() else {
        return 0;
    };
    let mmproj = parent.join(MMPROJ_FILE);
    let mmproj_part = parent.join(format!("{MMPROJ_FILE}.part"));
    completed_or_partial_bytes(model_path, &model_part, MODEL_SIZE)
        + completed_or_partial_bytes(&mmproj, &mmproj_part, MMPROJ_SIZE)
}

fn completed_or_partial_bytes(complete: &Path, partial: &Path, expected: u64) -> u64 {
    if file_has_size(complete, expected) {
        expected
    } else {
        fs::metadata(partial)
            .map(|metadata| metadata.len().min(expected))
            .unwrap_or(0)
    }
}

#[tauri::command]
pub async fn get_setup_status() -> Result<SetupStatus, String> {
    tokio::task::spawn_blocking(setup_status_sync)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn cancel_setup() {
    SETUP_CANCELLED.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub async fn run_setup(
    app: AppHandle,
    state: State<'_, AppState>,
    request: SetupRequest,
) -> Result<SetupStatus, String> {
    SETUP_CANCELLED.store(false, Ordering::Relaxed);
    let mode = request.mode.trim().to_lowercase();
    if mode != "remote" && mode != "local" {
        return Err("Setup mode must be remote or local".into());
    }
    if std::env::consts::OS != "macos" || std::env::consts::ARCH != "aarch64" {
        return Err("This managed installer currently supports Apple Silicon Macs only".into());
    }

    emit_progress(&app, "runtime", "Preparing the managed runtime…", 0, 1);
    install_managed_hermes(&app).await?;
    ensure_not_cancelled()?;
    prepare_sidecar_environments(&app).await?;
    ensure_not_cancelled()?;

    let (base_url, model) = if mode == "local" {
        install_local_model(&app).await?;
        (LOCAL_BASE_URL.to_string(), LOCAL_MODEL_ALIAS.to_string())
    } else {
        let base_url = request.base_url.unwrap_or_default().trim().to_string();
        let model = request.model.unwrap_or_default().trim().to_string();
        if base_url.is_empty() || model.is_empty() {
            return Err("Remote endpoint URL and model name are required".into());
        }
        (base_url, model)
    };

    ensure_not_cancelled()?;
    write_api_key(request.api_key.as_deref())?;
    emit_progress(
        &app,
        "configure",
        "Configuring chat, auto-mix, and vision…",
        0,
        1,
    );
    crate::commands::set_config(base_url.clone(), model.clone())?;
    crate::commands::set_video_model(base_url.clone(), model.clone())?;
    emit_progress(&app, "health", "Checking the audio-analysis service…", 1, 3);
    if !state
        .audio_service
        .wait_ready(Duration::from_secs(45))
        .await
    {
        return Err(
            "The audio-analysis service did not become ready. Check ~/.automixer/audio-service.log"
                .into(),
        );
    }
    crate::commands::set_hermes_model(state, base_url.clone(), model.clone()).await?;

    emit_progress(&app, "health", "Running final health checks…", 2, 3);
    let health = get_setup_status().await?;
    let core_ready = health.tools_ready
        && health.hermes_ready
        && health.audio_service_ready
        && health.agent_service_ready
        && health.model_server_ready;
    if !core_ready {
        return Err(format!(
            "Final health check failed. tools={}, Hermes={}, audio service={}, agent service={}, model endpoint={}",
            health.tools_ready,
            health.hermes_ready,
            health.audio_service_ready,
            health.agent_service_ready,
            health.model_server_ready,
        ));
    }
    write_receipt(&mode, &base_url, &model)?;
    emit_progress(&app, "health", "All health checks passed.", 3, 3);
    emit_progress(
        &app,
        "complete",
        "Setup complete. AutoMixer is ready.",
        1,
        1,
    );
    get_setup_status().await
}

fn emit_progress(app: &AppHandle, stage: &str, message: &str, current: u64, total: u64) {
    let progress = if total > 0 {
        (current as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let _ = app.emit(
        "setup:progress",
        SetupProgress {
            stage: stage.into(),
            message: message.into(),
            current_bytes: current,
            total_bytes: total,
            progress,
        },
    );
}

fn ensure_not_cancelled() -> Result<(), String> {
    if SETUP_CANCELLED.load(Ordering::Relaxed) {
        Err("Setup cancelled. Any partial model download was kept for the next run.".into())
    } else {
        Ok(())
    }
}

async fn install_managed_hermes(app: &AppHandle) -> Result<(), String> {
    let hermes = managed_hermes_bin()?;
    if hermes.is_file()
        && Command::new(&hermes)
            .args(["acp", "--check"])
            .output()
            .is_ok_and(|output| output.status.success())
    {
        emit_progress(app, "hermes", "Hermes ACP runtime is ready.", 1, 1);
        return Ok(());
    }

    let root = managed_root()?;
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let venv = root.join("hermes-agent").join("venv");
    let uv = crate::media_tools::uv_path().to_path_buf();
    let uv_cache = root.join("cache").join("uv");
    let python_install = root.join("python");
    emit_progress(
        app,
        "hermes",
        "Installing Python 3.11 and Hermes ACP…",
        0,
        1,
    );

    run_blocking(move || {
        fs::create_dir_all(&uv_cache).map_err(|error| error.to_string())?;
        fs::create_dir_all(&python_install).map_err(|error| error.to_string())?;
        run_checked(
            Command::new(&uv)
                .env("UV_CACHE_DIR", &uv_cache)
                .env("UV_PYTHON_INSTALL_DIR", &python_install)
                .args(["venv", "--python", "3.11"])
                .arg(&venv),
            "create the Hermes Python environment",
        )?;
        let python = venv.join("bin").join("python");
        run_checked(
            Command::new(&uv)
                .env("UV_CACHE_DIR", &uv_cache)
                .env("UV_PYTHON_INSTALL_DIR", &python_install)
                .args(["pip", "install", "--python"])
                .arg(&python)
                .arg(format!("hermes-agent[acp]=={HERMES_VERSION}")),
            "install Hermes ACP",
        )?;
        let hermes = venv.join("bin").join("hermes");
        run_checked(
            Command::new(&hermes).args(["acp", "--check"]),
            "verify Hermes ACP",
        )
    })
    .await?;
    emit_progress(app, "hermes", "Hermes ACP runtime installed.", 1, 1);
    Ok(())
}

async fn prepare_sidecar_environments(app: &AppHandle) -> Result<(), String> {
    let root = managed_root()?;
    let uv = crate::media_tools::uv_path().to_path_buf();
    let uv_cache = root.join("cache").join("uv");
    let python_install = root.join("python");
    let hermes_dir = crate::hermes_service::runnable_dir("hermes-service")
        .ok_or_else(|| "Could not stage the agent service".to_string())?;
    let audio_dir = crate::hermes_service::runnable_dir("audio-service")
        .ok_or_else(|| "Could not stage the audio-analysis service".to_string())?;
    emit_progress(app, "services", "Preparing AutoMixer services…", 0, 3);

    run_uv_sync(&uv, &uv_cache, &python_install, &hermes_dir).await?;
    emit_progress(app, "services", "Agent bridge prepared.", 1, 3);
    run_uv_sync(
        &uv,
        &uv_cache,
        &python_install,
        &hermes_dir.join("automixer-mcp"),
    )
    .await?;
    emit_progress(app, "services", "Editing tools prepared.", 2, 3);
    run_uv_sync(&uv, &uv_cache, &python_install, &audio_dir).await?;
    emit_progress(app, "services", "Audio analysis prepared.", 3, 3);
    Ok(())
}

async fn run_uv_sync(
    uv: &Path,
    cache: &Path,
    python_install: &Path,
    dir: &Path,
) -> Result<(), String> {
    let uv = uv.to_path_buf();
    let cache = cache.to_path_buf();
    let python_install = python_install.to_path_buf();
    let dir = dir.to_path_buf();
    run_blocking(move || {
        run_checked(
            Command::new(uv)
                .env("UV_CACHE_DIR", cache)
                .env("UV_PYTHON_INSTALL_DIR", python_install)
                .args(["sync", "--quiet", "--python", "3.11", "--directory"])
                .arg(dir),
            "prepare an AutoMixer service",
        )
    })
    .await
}

async fn install_local_model(app: &AppHandle) -> Result<(), String> {
    if SETUP_CANCELLED.load(Ordering::Relaxed) {
        return Err("Setup cancelled".into());
    }
    emit_progress(app, "local-runtime", "Installing llama.cpp…", 0, 1);
    install_local_runtime()?;
    emit_progress(app, "local-runtime", "llama.cpp installed.", 1, 1);

    let models = model_dir()?;
    fs::create_dir_all(&models).map_err(|error| error.to_string())?;
    let model_path = models.join(MODEL_FILE);
    let mmproj_path = models.join(MMPROJ_FILE);
    adopt_legacy_model(&model_path, MODEL_FILE, MODEL_SIZE, MODEL_SHA256, app).await?;
    adopt_legacy_model(&mmproj_path, MMPROJ_FILE, MMPROJ_SIZE, MMPROJ_SHA256, app).await?;

    download_verified(
        app,
        MODEL_URL,
        &model_path,
        MODEL_SIZE,
        MODEL_SHA256,
        "model",
        "Downloading local language model",
    )
    .await?;
    download_verified(
        app,
        MMPROJ_URL,
        &mmproj_path,
        MMPROJ_SIZE,
        MMPROJ_SHA256,
        "vision",
        "Downloading vision projector",
    )
    .await?;
    install_launch_agent(app).await?;
    Ok(())
}

fn install_local_runtime() -> Result<(), String> {
    let runtime_source = bundled_dir("runtime")?.join("llama.cpp");
    let service_source = bundled_dir("model-service")?;
    if !runtime_source.join("llama-server").is_file() {
        return Err("The application bundle is missing llama-server".into());
    }
    let root = managed_root()?;
    let runtime_dest = root.join("model-runtime").join("llama.cpp");
    let service_dest = root.join("model-service");
    copy_dir_merge(&runtime_source, &runtime_dest)?;
    copy_dir_merge(&service_source, &service_dest)?;
    let models = model_dir()?;
    fs::create_dir_all(&models).map_err(|error| error.to_string())?;

    let config = format!(
        "AUTOMIXER_MODEL_RUNTIME=\"llama_cpp\"\n\
         AUTOMIXER_MODEL_ROOT=\"{}\"\n\
         AUTOMIXER_MODEL_HOST=\"127.0.0.1\"\n\
         AUTOMIXER_MODEL_PORT=\"2261\"\n\
         AUTOMIXER_MODEL_ALIASES=\"qwen3.6-35b-a3b,qwythos-9b\"\n\
         AUTOMIXER_MODEL_CONTEXT_SIZE=\"122880\"\n\
         AUTOMIXER_MODEL_LLAMA_SERVER_BIN=\"{}\"\n\
         AUTOMIXER_MODEL_LLAMA_FILE=\"{}\"\n\
         AUTOMIXER_MODEL_MMPROJ_FILE=\"{}\"\n\
         AUTOMIXER_MODEL_GPU_LAYERS=\"99\"\n\
         AUTOMIXER_MODEL_PARALLEL=\"1\"\n\
         AUTOMIXER_MODEL_CACHE_TYPE_K=\"q8_0\"\n\
         AUTOMIXER_MODEL_CACHE_TYPE_V=\"q8_0\"\n\
         AUTOMIXER_MODEL_FLASH_ATTN=\"1\"\n",
        root.display(),
        runtime_dest.join("llama-server").display(),
        models.join(MODEL_FILE).display(),
        models.join(MMPROJ_FILE).display(),
    );
    fs::write(service_dest.join("config.env"), config).map_err(|error| error.to_string())?;
    Ok(())
}

async fn adopt_legacy_model(
    destination: &Path,
    filename: &str,
    expected_size: u64,
    expected_sha: &str,
    app: &AppHandle,
) -> Result<(), String> {
    if destination.is_file() || destination.with_extension("gguf.part").is_file() {
        return Ok(());
    }
    let legacy = home_dir()?.join("vLLM").join("models").join(filename);
    if !file_has_size(&legacy, expected_size) {
        return Ok(());
    }
    emit_progress(
        app,
        "verify",
        &format!("Verifying existing {filename}…"),
        0,
        expected_size,
    );
    let legacy_for_hash = legacy.clone();
    let digest = run_blocking(move || sha256_path(&legacy_for_hash)).await?;
    if digest != expected_sha {
        return Ok(());
    }
    if fs::hard_link(&legacy, destination).is_err() {
        fs::copy(&legacy, destination).map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn download_verified(
    app: &AppHandle,
    url: &str,
    destination: &Path,
    expected_size: u64,
    expected_sha: &str,
    stage: &str,
    label: &str,
) -> Result<(), String> {
    if file_has_size(destination, expected_size) {
        emit_progress(
            app,
            "verify",
            &format!(
                "Verifying {}…",
                destination
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            0,
            expected_size,
        );
        let path = destination.to_path_buf();
        let digest = run_blocking(move || sha256_path(&path)).await?;
        if digest == expected_sha {
            emit_progress(
                app,
                stage,
                &format!("{label} ready."),
                expected_size,
                expected_size,
            );
            return Ok(());
        }
        preserve_invalid_file(destination)?;
    }

    let part = PathBuf::from(format!("{}.part", destination.display()));
    if fs::metadata(&part).is_ok_and(|metadata| metadata.len() > expected_size) {
        preserve_invalid_file(&part)?;
    }
    let mut offset = fs::metadata(&part)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .user_agent("AutoMixer/0.1 managed installer")
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(header::RANGE, format!("bytes={offset}-"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("{label} failed: {error}"))?;
    let append = offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
    if !response.status().is_success() {
        return Err(format!("{label} failed with HTTP {}", response.status()));
    }
    if !append {
        offset = 0;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .await
        .map_err(|error| error.to_string())?;
    let mut received = offset;
    let mut stream = response.bytes_stream();
    let mut last_emitted = received;
    emit_progress(app, stage, label, received, expected_size);
    while let Some(chunk) = stream.next().await {
        if SETUP_CANCELLED.load(Ordering::Relaxed) {
            file.flush().await.map_err(|error| error.to_string())?;
            return Err(
                "Setup cancelled. The partial download was kept and will resume next time.".into(),
            );
        }
        let chunk = chunk.map_err(|error| format!("{label} failed: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| error.to_string())?;
        received += chunk.len() as u64;
        if received.saturating_sub(last_emitted) >= 2 * 1024 * 1024 || received >= expected_size {
            emit_progress(app, stage, label, received, expected_size);
            last_emitted = received;
        }
    }
    file.flush().await.map_err(|error| error.to_string())?;
    drop(file);
    if received != expected_size {
        return Err(format!(
            "{label} was incomplete: received {received} of {expected_size} bytes"
        ));
    }

    emit_progress(
        app,
        "verify",
        &format!(
            "Verifying {}…",
            destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ),
        0,
        expected_size,
    );
    let verify_path = part.clone();
    let digest = run_blocking(move || sha256_path(&verify_path)).await?;
    if digest != expected_sha {
        preserve_invalid_file(&part)?;
        return Err(format!(
            "Checksum verification failed for {}",
            destination.display()
        ));
    }
    fs::rename(&part, destination).map_err(|error| error.to_string())?;
    emit_progress(
        app,
        stage,
        &format!("{label} ready."),
        expected_size,
        expected_size,
    );
    Ok(())
}

async fn install_launch_agent(app: &AppHandle) -> Result<(), String> {
    let script = managed_root()?.join("model-service").join("launchd.sh");
    emit_progress(
        app,
        "model-service",
        "Installing the local model service…",
        0,
        1,
    );
    run_blocking(move || {
        run_checked(
            Command::new("/bin/bash").arg(script).arg("install"),
            "install the llama.cpp LaunchAgent",
        )
    })
    .await?;

    for second in 0..240u64 {
        if SETUP_CANCELLED.load(Ordering::Relaxed) {
            return Err("Setup cancelled while the model was loading".into());
        }
        if model_endpoint_healthy(LOCAL_BASE_URL) {
            emit_progress(app, "model-service", "Local model server is ready.", 1, 1);
            return Ok(());
        }
        emit_progress(
            app,
            "model-service",
            &format!("Loading the local model… {}s", second + 1),
            second + 1,
            240,
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err("The local model server did not become healthy within four minutes. Check ~/.automixer/model-server/model-server.error.log".into())
}

fn write_api_key(api_key: Option<&str>) -> Result<(), String> {
    let home = crate::hermes_service::automixer_hermes_home();
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    let path = home.join(".env");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| !line.trim_start().starts_with("OPENAI_API_KEY="))
        .map(str::to_string)
        .collect();
    if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
        lines.push(format!("OPENAI_API_KEY={}", key.replace(['\n', '\r'], "")));
    }
    fs::write(path, format!("{}\n", lines.join("\n"))).map_err(|error| error.to_string())
}

fn read_api_key() -> Option<String> {
    let path = crate::hermes_service::automixer_hermes_home().join(".env");
    fs::read_to_string(path).ok()?.lines().find_map(|line| {
        line.trim()
            .strip_prefix("OPENAI_API_KEY=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn write_receipt(mode: &str, base_url: &str, model: &str) -> Result<(), String> {
    let receipt = SetupReceipt {
        version: SETUP_VERSION,
        mode: mode.into(),
        base_url: base_url.into(),
        model: model.into(),
        completed_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let path = setup_receipt_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(
        path,
        serde_json::to_string_pretty(&receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn bundled_dir(name: &str) -> Result<PathBuf, String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let macos = executable
        .parent()
        .ok_or_else(|| "Application executable has no parent".to_string())?;
    let running_from_app_bundle = executable.ancestors().any(|ancestor| {
        ancestor
            .extension()
            .is_some_and(|extension| extension == "app")
    });
    let candidates = [
        macos
            .parent()
            .map(|contents| contents.join("Resources").join(name)),
        Some(macos.join(name)),
    ];
    if running_from_app_bundle {
        return candidates
            .into_iter()
            .flatten()
            .find(|path| path.exists())
            .ok_or_else(|| format!("The application bundle is missing {name}"));
    }
    if let Some(dev) = manifest
        .parent()
        .map(|parent| parent.join("src-tauri/resources").join(name))
    {
        if dev.exists() {
            return Ok(dev);
        }
    }
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.exists())
        .ok_or_else(|| format!("The application bundle is missing {name}"))
}

fn copy_dir_merge(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            copy_dir_merge(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn sha256_path(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 4 * 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn preserve_invalid_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup = PathBuf::from(format!("{}.invalid-{timestamp}", path.display()));
    fs::rename(path, backup).map_err(|error| error.to_string())
}

fn run_checked(command: &mut Command, action: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("Could not {action}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(format!("Could not {action}: {detail}"))
}

async fn run_blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| error.to_string())?
}
