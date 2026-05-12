//! Local audio-analysis sidecar (Python, managed by `uv`).
//!
//! Spawned at app startup. Exposes a small HTTP API for music structure
//! detection (beats / downbeats / sections via the `all-in-one` library).
//! All ML work is done out-of-process so the Rust audio engine stays light.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};

const DEFAULT_PORT: u16 = 7321;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub start: f32,
    pub end: f32,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructureAnalysis {
    pub bpm: f32,
    #[serde(default)]
    pub beats: Vec<f32>,
    #[serde(default)]
    pub downbeats: Vec<f32>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProgressStatus {
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub elapsed_seconds: f32,
}

pub struct AudioService {
    child: Mutex<Option<Child>>,
    base_url: String,
}

impl AudioService {
    /// Spawn the sidecar via `uv run uvicorn ...`. Returns even if spawn
    /// fails — endpoints will surface the error per-request rather than
    /// crashing the app.
    pub fn spawn() -> Self {
        let port = std::env::var("AUTOMIXER_AUDIO_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        let base_url = format!("http://127.0.0.1:{port}");

        let log_path = log_path();
        let log_handle = log_path
            .as_ref()
            .and_then(|p| std::fs::File::create(p).ok());
        let stdout_target = log_handle
            .as_ref()
            .and_then(|h| h.try_clone().ok())
            .map(Stdio::from)
            .unwrap_or_else(Stdio::null);
        let stderr_target = log_handle
            .map(Stdio::from)
            .unwrap_or_else(Stdio::null);

        let child = match service_dir() {
            Some(dir) if dir.exists() => Command::new(uv_path())
                .arg("run")
                .arg("--directory")
                .arg(&dir)
                .arg("--quiet")
                .arg("uvicorn")
                .arg("main:app")
                .arg("--host")
                .arg("127.0.0.1")
                .arg("--port")
                .arg(port.to_string())
                .stdout(stdout_target)
                .stderr(stderr_target)
                .spawn()
                .ok(),
            Some(_) | None => {
                eprintln!("[audio-service] could not locate audio-service/ directory");
                None
            }
        };
        if child.is_some() {
            eprintln!(
                "[audio-service] spawned uv sidecar at {base_url} (log: {})",
                log_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "<unavailable>".into())
            );
        }

        Self {
            child: Mutex::new(child),
            base_url,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Block-poll the sidecar's /health until it responds or the deadline elapses.
    pub async fn wait_ready(&self, total: Duration) -> bool {
        let deadline = std::time::Instant::now() + total;
        let client = reqwest::Client::new();
        loop {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            if let Ok(resp) = client
                .get(format!("{}/health", self.base_url))
                .timeout(Duration::from_millis(500))
                .send()
                .await
            {
                if resp.status().is_success() {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    pub async fn analyze_structure(&self, wav_path: &Path) -> Result<StructureAnalysis, String> {
        let path_str = wav_path.to_string_lossy().to_string();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1800))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(format!("{}/analyze/structure", self.base_url))
            .json(&serde_json::json!({ "wav_path": path_str }))
            .send()
            .await
            .map_err(|e| format!("audio-service unreachable at {}: {e}", self.base_url))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("audio-service {status}: {body}"));
        }
        resp.json::<StructureAnalysis>()
            .await
            .map_err(|e| format!("audio-service returned malformed JSON: {e}"))
    }

    pub async fn status(&self) -> Option<ProgressStatus> {
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/status", self.base_url))
            .timeout(Duration::from_millis(800))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<ProgressStatus>().await.ok()
    }
}

impl Drop for AudioService {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

fn uv_path() -> PathBuf {
    if let Ok(p) = std::env::var("AUTOMIXER_UV") {
        return PathBuf::from(p);
    }
    for candidate in [
        dirs_home().map(|h| h.join(".local/bin/uv")),
        Some(PathBuf::from("/opt/homebrew/bin/uv")),
        Some(PathBuf::from("/usr/local/bin/uv")),
    ]
    .into_iter()
    .flatten()
    {
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("uv")
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn log_path() -> Option<PathBuf> {
    let home = dirs_home()?;
    let dir = home.join(".automixer");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("audio-service.log"))
}

fn service_dir() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR points at .../autoMixer/src-tauri at compile time —
    // the sidecar lives next to it at .../autoMixer/audio-service.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest.parent().map(|p| p.join("audio-service"));
    if let Some(path) = candidate {
        if path.exists() {
            return Some(path);
        }
    }
    None
}
