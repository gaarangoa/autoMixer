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
const AUTOMIXER_LONG_TIMEOUT_SECS: u64 = 2 * 60 * 60;

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
        let log_handle = log_path
            .as_ref()
            .and_then(|p| std::fs::File::create(p).ok());
        let stdout_target = log_handle
            .as_ref()
            .and_then(|h| h.try_clone().ok())
            .map(Stdio::from)
            .unwrap_or_else(Stdio::null);
        let stderr_target = log_handle.map(Stdio::from).unwrap_or_else(Stdio::null);

        let child = match service_dir() {
            Some(dir) if dir.exists() => {
                let installed_uvicorn = if cfg!(windows) {
                    dir.join(".venv").join("Scripts").join("uvicorn.exe")
                } else {
                    dir.join(".venv").join("bin").join("uvicorn")
                };
                let mut command = if installed_uvicorn.is_file() {
                    Command::new(installed_uvicorn)
                } else {
                    let mut command = Command::new(crate::media_tools::uv_path());
                    command
                        .arg("run")
                        .arg("--directory")
                        .arg(&dir)
                        .arg("--quiet")
                        .arg("uvicorn");
                    command
                };
                command
                    // Keep the sidecar independent of LaunchServices' inherited cwd,
                    // which may be stale immediately after an in-place app update.
                    .current_dir(&dir)
                    .arg("main:app")
                    .arg("--host")
                    .arg("127.0.0.1")
                    .arg("--port")
                    .arg(port.to_string())
                    // The sidecar uses uv to spawn the automixer-mcp server.
                    .env("AUTOMIXER_UV", crate::media_tools::uv_path())
                    // Point the embedded Hermes agent at AutoMixer's dedicated home
                    // (isolated config + no shared skills). The sidecar forwards this to
                    // the `hermes acp` process it spawns.
                    .env("HERMES_HOME", bootstrap_hermes_home())
                    .env("AUTOMIXER_HERMES_BIN", hermes_bin_path())
                    .stdout(stdout_target)
                    .stderr(stderr_target)
                    .spawn()
                    .ok()
            }
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

    /// Verify the sidecar can create an ACP session and run a tiny prompt with
    /// the currently configured model. `/health` only proves the process is up;
    /// a bad model/provider can still fail when Hermes creates the first session.
    pub async fn probe(&self) -> Result<(), String> {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/probe", self.base_url))
            .timeout(Duration::from_secs(180))
            .send()
            .await
            .map_err(|e| format!("hermes-service unreachable at {}: {e}", self.base_url))?;
        if resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("detail")
                    .and_then(|detail| detail.as_str())
                    .map(str::to_string)
            })
            .unwrap_or(body);
        Err(format!(
            "hermes-service model probe failed ({status}): {detail}"
        ))
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

        // No total timeout: a chat turn can legitimately run long when the agent
        // calls a slow tool (e.g. auto_mix runs many LLM stages, streaming nothing
        // on this SSE for minutes). A 600s total cap aborted those mid-run with
        // "error decoding response body". Use a very generous per-read timeout so
        // a full local llama.cpp auto-mix can exceed 30 minutes without the app
        // dropping the Hermes stream first.
        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_secs(AUTOMIXER_LONG_TIMEOUT_SECS))
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
        let mut stream_error: Option<String> = None;
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
                            Ok(event) => {
                                if let ChatEvent::Error { message } = &event {
                                    stream_error.get_or_insert_with(|| message.clone());
                                }
                                on_event(event);
                            }
                            Err(error) => eprintln!("[hermes-service] bad event {data}: {error}"),
                        }
                    }
                }
            }
        }
        if let Some(error) = stream_error {
            return Err(error);
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

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Hermes executable used by AutoMixer's embedded ACP bridge. Prefer an
/// AutoMixer-managed install when present; fall back to the user's global Hermes
/// install so existing development machines keep working until the managed
/// runtime has been bootstrapped.
pub fn hermes_bin_path() -> PathBuf {
    if let Ok(p) = std::env::var("AUTOMIXER_HERMES_BIN") {
        return PathBuf::from(p);
    }
    for candidate in [
        dirs_home().map(|h| h.join(".automixer/hermes-agent/venv/bin/hermes")),
        dirs_home().map(|h| h.join(".automixer/hermes-agent/.venv/bin/hermes")),
        dirs_home().map(|h| h.join(".hermes/hermes-agent/venv/bin/hermes")),
        Some(PathBuf::from("hermes")),
    ]
    .into_iter()
    .flatten()
    {
        if candidate == PathBuf::from("hermes") || candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("hermes")
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

/// Optional bearer token shared by the embedded agent and direct model calls
/// (video analysis / legacy auto-mix). Setup stores it only in AutoMixer's
/// dedicated Hermes home rather than the project or application bundle.
pub fn model_api_key() -> Option<String> {
    if let Ok(value) = std::env::var("OPENAI_API_KEY") {
        if !value.trim().is_empty() {
            return Some(value.trim().to_string());
        }
    }
    let text = std::fs::read_to_string(automixer_hermes_home().join(".env")).ok()?;
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("OPENAI_API_KEY=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
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
    repair_hermes_config(&cfg);
    home
}

fn repair_hermes_config(cfg: &PathBuf) {
    let Ok(text) = std::fs::read_to_string(cfg) else {
        return;
    };
    let automixer_mcp_dir = runnable_dir("hermes-service").map(|dir| dir.join("automixer-mcp"));
    let automixer_mcp_python = automixer_mcp_dir.as_ref().map(|dir| {
        if cfg!(windows) {
            dir.join(".venv").join("Scripts").join("python.exe")
        } else {
            dir.join(".venv").join("bin").join("python")
        }
    });
    let use_direct_mcp_python = automixer_mcp_python
        .as_ref()
        .is_some_and(|path| path.is_file());
    let automixer_mcp_server = automixer_mcp_dir.as_ref().map(|dir| dir.join("server.py"));
    let mut changed = false;
    let mut in_agent = false;
    let mut in_auxiliary = false;
    let mut in_title_generation = false;
    let mut title_generation_has_enabled = false;
    let mut in_mcp_servers = false;
    let mut in_automixer = false;
    let mut in_automixer_args = false;
    let mut out = Vec::with_capacity(text.lines().count());

    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len().saturating_sub(trimmed.len());

        if in_title_generation && indent <= 2 && !trimmed.is_empty() {
            if !title_generation_has_enabled {
                out.push("    enabled: false".to_string());
                changed = true;
            }
            in_title_generation = false;
            title_generation_has_enabled = false;
        }

        if indent == 0 {
            in_agent = trimmed == "agent:";
            in_auxiliary = trimmed == "auxiliary:";
            in_mcp_servers = trimmed == "mcp_servers:";
            in_title_generation = false;
            in_automixer = false;
            in_automixer_args = false;
        } else if in_auxiliary && indent == 2 && trimmed.ends_with(':') {
            in_title_generation = trimmed == "title_generation:";
            title_generation_has_enabled = false;
        } else if in_mcp_servers && indent == 2 && trimmed.ends_with(':') {
            in_automixer = trimmed == "automixer:";
            in_automixer_args = false;
        }

        if in_automixer_args && !(indent == 4 && trimmed.starts_with("- ")) {
            in_automixer_args = false;
        }
        if in_mcp_servers
            && in_automixer
            && use_direct_mcp_python
            && indent == 4
            && trimmed == "args:"
        {
            out.push(line.to_string());
            if let Some(server) = automixer_mcp_server.as_ref() {
                // Hermes starts globally configured MCP processes from its own
                // working directory, not from the ACP session cwd. Always pass an
                // absolute script path when invoking the MCP venv's Python.
                out.push(format!("    - {}", server.to_string_lossy()));
            }
            in_automixer_args = true;
            changed = true;
            continue;
        }
        if in_automixer_args && indent == 4 && trimmed.starts_with("- ") {
            changed = true;
            continue;
        }

        let replacement = if indent == 0 && trimmed.starts_with("mcp_discovery_timeout:") {
            // Do not let the first ACP session snapshot its tools before the
            // bundled AutoMixer MCP server has finished registering.
            Some("mcp_discovery_timeout: 15".to_string())
        } else if in_agent && indent == 2 && trimmed.starts_with("gateway_timeout:") {
            Some(format!("  gateway_timeout: {AUTOMIXER_LONG_TIMEOUT_SECS}"))
        } else if in_agent && indent == 2 && trimmed.starts_with("gateway_timeout_warning:") {
            Some(format!(
                "  gateway_timeout_warning: {}",
                AUTOMIXER_LONG_TIMEOUT_SECS / 2
            ))
        } else if in_title_generation && indent == 4 && trimmed.starts_with("enabled:") {
            title_generation_has_enabled = true;
            Some("    enabled: false".to_string())
        } else if in_title_generation && indent == 4 && trimmed.starts_with("provider:") {
            Some("    provider: disabled".to_string())
        } else if in_title_generation && indent == 4 && trimmed.starts_with("timeout:") {
            Some("    timeout: 1".to_string())
        } else if in_mcp_servers && in_automixer && indent == 4 && trimmed.starts_with("timeout:") {
            Some(format!("    timeout: {AUTOMIXER_LONG_TIMEOUT_SECS}"))
        } else if in_mcp_servers
            && in_automixer
            && indent == 4
            && trimmed.starts_with("command:")
            && use_direct_mcp_python
        {
            automixer_mcp_python
                .as_ref()
                .map(|path| format!("    command: {}", path.to_string_lossy()))
        } else if in_mcp_servers
            && in_automixer
            && indent == 4
            && trimmed.starts_with("- ")
            && trimmed.contains("automixer-mcp")
        {
            automixer_mcp_dir
                .as_ref()
                .map(|path| format!("    - {}", path.to_string_lossy()))
        } else {
            None
        };

        if in_title_generation && indent == 4 && trimmed.starts_with("enabled:") {
            title_generation_has_enabled = true;
        }

        if let Some(replacement) = replacement {
            if replacement != line {
                changed = true;
            }
            out.push(replacement);
        } else {
            out.push(line.to_string());
        }
    }
    if in_title_generation && !title_generation_has_enabled {
        out.push("    enabled: false".to_string());
        changed = true;
    }

    if changed {
        if let Err(error) = std::fs::write(cfg, format!("{}\n", out.join("\n"))) {
            eprintln!("[hermes-service] could not update Hermes config: {error}");
        }
    }
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
    let running_from_app_bundle = std::env::current_exe().ok().is_some_and(|executable| {
        executable.ancestors().any(|ancestor| {
            ancestor
                .extension()
                .is_some_and(|extension| extension == "app")
        })
    });
    if !running_from_app_bundle && manifest.exists() {
        if let Some(dev) = manifest.parent().map(|p| p.join(name)) {
            if dev.join("pyproject.toml").exists() {
                return Some(dev);
            }
        }
    }

    let bundled = resolve_bundled_source(name)?;
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dest = home.join(".automixer").join("sidecars").join(name);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::create_dir_all(&dest);
    // Refresh the signed bundle's source on every launch so app upgrades also
    // upgrade their Python services. A recursive merge intentionally preserves
    // the destination-only `.venv` created by the installer/first run.
    let copied = std::process::Command::new("cp")
        .arg("-R")
        .arg(bundled.join("."))
        .arg(&dest)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !copied && !dest.join("pyproject.toml").exists() {
        eprintln!("[sidecar] failed to stage {name} from {bundled:?}");
        return None;
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
        exe_dir
            .as_ref()
            .and_then(|d| d.parent())
            .map(|c| c.join("Resources").join(name)),
        exe_dir.as_ref().map(|d| d.join(name)),
        exe_dir
            .as_ref()
            .and_then(|d| d.parent())
            .map(|c| c.join(name)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|c| c.join("pyproject.toml").exists())
}
