//! Hermes agent bridge sidecar (Python, managed by `uv`).
//!
//! Spawned at app startup alongside the audio sidecar. It holds a persistent
//! `hermes acp` connection and exposes a tiny HTTP/SSE surface:
//!   GET  /health
//!   POST /chat {sessionId, userText}  -> SSE stream of chat events
//!
//! The agent's tool calls flow through the `automixer-mcp` server into the
//! in-process control surface (see `control.rs`), mutating the live session.
//! This struct mirrors `AudioService`'s lifecycle (spawn / wait_ready / kill on
//! Drop) and adds a streaming `chat` helper.

use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::Duration,
};

use serde::Deserialize;

const DEFAULT_PORT: u16 = 7322;

/// One Server-Sent event emitted by the sidecar during a chat turn.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ChatEvent {
    /// A visible assistant-message token.
    Chunk { text: String },
    /// A reasoning/thinking token (shown in the agent log, not the bubble).
    Thought { text: String },
    /// A tool-call lifecycle update.
    Tool {
        #[serde(default)]
        name: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        kind: String,
    },
    /// Estimated token usage + context state for the current agent session.
    Usage {
        #[serde(rename = "outputTokens", default)]
        output_tokens: u64,
        #[serde(rename = "thoughtTokens", default)]
        thought_tokens: u64,
        #[serde(rename = "turnsSinceCompaction", default)]
        turns_since_compaction: u64,
        #[serde(rename = "compactAfter", default)]
        compact_after: u64,
    },
    /// The turn finished.
    Done {
        #[serde(rename = "stopReason", default)]
        stop_reason: Option<String>,
    },
    /// The sidecar reported an error.
    Error { message: String },
}

pub struct HermesService {
    child: Mutex<Option<Child>>,
    port: u16,
    base_url: String,
}

impl HermesService {
    pub fn spawn() -> Self {
        let port = std::env::var("AUTOMIXER_HERMES_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        let base_url = format!("http://127.0.0.1:{port}");
        let child = Self::spawn_child(port);
        Self {
            child: Mutex::new(child),
            port,
            base_url,
        }
    }

    /// Kill any process still bound to `port` — a stale sidecar left over from a
    /// previous launch that didn't clean up on a hard exit (e.g. a `tauri dev`
    /// rebuild SIGKILL). Without this, a new app would talk to the dead sidecar
    /// and every chat would abort mid-stream ("error decoding response body").
    fn free_stale_port(port: u16) {
        let Ok(out) = Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output()
        else {
            return;
        };
        let me = std::process::id();
        let mut killed = false;
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            if let Ok(n) = pid.parse::<u32>() {
                if n != me {
                    let _ = Command::new("kill").arg("-9").arg(n.to_string()).status();
                    killed = true;
                }
            }
        }
        if killed {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }

    /// Build and launch the uvicorn sidecar process for the given port.
    fn spawn_child(port: u16) -> Option<Child> {
        Self::free_stale_port(port);
        let log_path = log_path();
        let log_handle = log_path.as_ref().and_then(|p| std::fs::File::create(p).ok());
        let stdout_target = log_handle
            .as_ref()
            .and_then(|h| h.try_clone().ok())
            .map(Stdio::from)
            .unwrap_or_else(Stdio::null);
        let stderr_target = log_handle.map(Stdio::from).unwrap_or_else(Stdio::null);

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
                // The sidecar uses uv to spawn the automixer-mcp server.
                .env("AUTOMIXER_UV", uv_path())
                // Point the embedded Hermes agent at AutoMixer's dedicated home
                // (isolated config + no shared skills). The sidecar forwards this to
                // the `hermes acp` process it spawns.
                .env("HERMES_HOME", bootstrap_hermes_home())
                .stdout(stdout_target)
                .stderr(stderr_target)
                .spawn()
                .ok(),
            Some(_) | None => {
                eprintln!("[hermes-service] could not locate hermes-service/ directory");
                None
            }
        };
        if child.is_some() {
            eprintln!("[hermes-service] spawned uv sidecar at http://127.0.0.1:{port}");
        }
        child
    }

    /// Kill and relaunch the sidecar — used after the orchestration model config
    /// changes, since `hermes acp` reads its provider/model config at startup.
    pub fn restart(&self) {
        let fresh = Self::spawn_child(self.port);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(old) = guard.as_mut() {
                let _ = old.kill();
            }
            *guard = fresh;
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Pre-spawn the agent's tool server at startup so the first real turn is fast.
    pub async fn warmup(&self) -> Result<(), String> {
        let client = reqwest::Client::new();
        client
            .post(format!("{}/warmup", self.base_url))
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| format!("hermes-service unreachable at {}: {e}", self.base_url))?;
        Ok(())
    }

    /// Forget the agent's conversation for a session (Clear chat). The next chat turn
    /// starts a fresh ACP session, so stale context can't leak in.
    pub async fn reset_session(&self, session_id: &str) -> Result<(), String> {
        let client = reqwest::Client::new();
        client
            .post(format!("{}/reset", self.base_url))
            .json(&serde_json::json!({ "sessionId": session_id }))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("hermes-service unreachable at {}: {e}", self.base_url))?;
        Ok(())
    }

    /// Block-poll /health until ready. The sidecar only reports healthy after its
    /// persistent `hermes acp` connection has initialized, so this also gates on
    /// the agent being reachable.
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

    /// Run one chat turn against the agent, invoking `on_event` for each streamed
    /// event (tokens, thoughts, tool calls, done/error). Returns when the stream
    /// ends, or early if `is_cancelled` flips true — in which case the response
    /// stream is dropped, which disconnects the sidecar and makes it cancel the
    /// agent's ACP turn.
    pub async fn chat<F, C>(
        &self,
        session_id: &str,
        user_text: &str,
        mut on_event: F,
        is_cancelled: C,
    ) -> Result<(), String>
    where
        F: FnMut(ChatEvent),
        C: Fn() -> bool,
    {
        use futures_util::StreamExt;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(format!("{}/chat", self.base_url))
            .json(&serde_json::json!({ "sessionId": session_id, "userText": user_text }))
            .send()
            .await
            .map_err(|e| format!("hermes-service unreachable at {}: {e}", self.base_url))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("hermes-service {status}: {body}"));
        }

        // Parse the SSE byte stream: events are separated by a blank line, each
        // carrying one `data: {json}` line.
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            if is_cancelled() {
                // Drop the stream (and thus the HTTP connection) so the sidecar
                // sees the disconnect and cancels the in-flight ACP turn.
                break;
            }
            let bytes = chunk.map_err(|e| format!("hermes-service stream error: {e}"))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(idx) = buf.find("\n\n") {
                let block: String = buf.drain(..idx + 2).collect();
                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        match serde_json::from_str::<ChatEvent>(data) {
                            Ok(event) => on_event(event),
                            Err(error) => eprintln!("[hermes-service] bad event {data}: {error}"),
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Drop for HermesService {
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

/// AutoMixer's DEDICATED Hermes home — its own config + memory + EMPTY skill dirs,
/// kept under `~/.automixer/` so the agent (a) doesn't inherit the user's global
/// `~/.hermes` skills catalog (which bloats the prompt and slows every turn) and
/// (b) never disturbs the standalone Hermes desktop app that shares `~/.hermes`.
pub fn automixer_hermes_home() -> PathBuf {
    dirs_home()
        .map(|h| h.join(".automixer/hermes-home"))
        .unwrap_or_else(|| PathBuf::from(".automixer/hermes-home"))
}

/// Ensure the dedicated home exists with a usable config + empty skill dirs. On first
/// run we seed `config.yaml` from the user's `~/.hermes` (so the working model + the
/// automixer MCP registration carry over); afterwards it's fully independent — model
/// changes from Settings write here, not to the shared home.
pub fn bootstrap_hermes_home() -> PathBuf {
    let home = automixer_hermes_home();
    let _ = std::fs::create_dir_all(home.join("skills"));
    let _ = std::fs::create_dir_all(home.join("optional-skills"));
    let cfg = home.join("config.yaml");
    if !cfg.exists() {
        if let Some(shared) = dirs_home().map(|h| h.join(".hermes/config.yaml")) {
            if shared.exists() {
                let _ = std::fs::copy(&shared, &cfg);
            }
        }
        // Carry over secrets (.env) if the model endpoint needs an API key; harmless
        // for local no-auth servers.
        if let (Some(src), Some(dst)) = (
            dirs_home().map(|h| h.join(".hermes/.env")),
            Some(home.join(".env")),
        ) {
            if src.exists() && !dst.exists() {
                let _ = std::fs::copy(&src, &dst);
            }
        }
    }
    home
}

fn log_path() -> Option<PathBuf> {
    let home = dirs_home()?;
    let dir = home.join(".automixer");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("hermes-service.log"))
}

fn service_dir() -> Option<PathBuf> {
    runnable_dir("hermes-service")
}

/// Return a *runnable* sidecar directory (one `uv` can build its `.venv` in).
///
/// - **Dev checkout:** if the compile-time source tree still exists on this
///   machine, use the in-repo sidecar dir directly — it already has its env.
/// - **Packaged `.app`:** the bundled source lives in the read-only, code-signed
///   Resources dir, where `uv` cannot create a `.venv`. So on first run we copy
///   it into a writable app-data dir (`~/.automixer/sidecars/<name>`) and run
///   there. Subsequent launches reuse it (and its built env).
pub fn runnable_dir(name: &str) -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.exists() {
        if let Some(dev) = manifest.parent().map(|p| p.join(name)) {
            if dev.join("pyproject.toml").exists() {
                return Some(dev);
            }
        }
    }

    let bundled = resolve_bundled_source(name)?;
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dest = home.join(".automixer").join("sidecars").join(name);
    if !dest.join("pyproject.toml").exists() {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&dest);
        let copied = std::process::Command::new("cp")
            .arg("-R")
            .arg(&bundled)
            .arg(&dest)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !copied {
            eprintln!("[sidecar] failed to stage {name} from {bundled:?}");
            return None;
        }
    }
    Some(dest)
}

/// Find the sidecar source bundled into a packaged app, relative to the running
/// executable. On macOS the binary sits at `AutoMixer.app/Contents/MacOS/automixer`
/// and bundled resources at `AutoMixer.app/Contents/Resources/<name>`.
fn resolve_bundled_source(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent().map(PathBuf::from);
    let candidates = [
        exe_dir.as_ref().and_then(|d| d.parent()).map(|c| c.join("Resources").join(name)),
        exe_dir.as_ref().map(|d| d.join(name)),
        exe_dir.as_ref().and_then(|d| d.parent()).map(|c| c.join(name)),
    ];
    candidates.into_iter().flatten().find(|c| c.join("pyproject.toml").exists())
}
