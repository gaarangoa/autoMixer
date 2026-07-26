//! Native multicam camera service — single owner of every camera.
//!
//! Capture runs through a tiny Swift helper (`helpers/camera-helper.swift`)
//! using AVFoundation directly, because ffmpeg's avfoundation input can only
//! request UNCOMPRESSED formats: USB 2.0 webcams (Logitech C920 & co) then top
//! out at ~5fps @1080p. AVFoundation negotiates the camera's compressed 30fps
//! formats exactly like browsers do.
//!
//! One helper process per camera provides BOTH:
//!   • an MJPEG preview stream on stdout (served by the control server), and
//!   • an H.264 recording at the camera's full native mode (hardware encoder),
//! so previews keep running while recording, and no two subsystems ever fight
//! over a device. Camera audio (when requested) is a separate ffmpeg audio-only
//! capture — audio has none of the USB-video constraints.
//!
//! stderr protocol from the helper: `size=WxH fps=F`, `ready`, `t=<ms>` (media
//! clock while recording), `error: …`.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSpec {
    pub track_id: String,
    pub device_label: String,
    pub include_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopSpec {
    pub track_id: String,
    pub start_sample: u64,
    pub create_audio_track: bool,
}

/// A finished, finalized take ready to be registered as a clip.
pub struct FinishedCapture {
    pub track_id: String,
    pub path: PathBuf,
    pub duration_ms: u64,
    pub offset_ms: u64,
    /// Camera audio captured alongside (wav path + its own head-trim ms).
    pub audio_wav: Option<(PathBuf, u64)>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One camera-helper process (preview always; recording when `record` is set).
struct CameraProc {
    child: Child,
    device_label: String,
    tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    media_ms: Arc<AtomicU64>,
    ready: Arc<AtomicBool>,
    err_tail: Arc<Mutex<Vec<String>>>,
    record: Option<RecordInfo>,
}

struct RecordInfo {
    track_id: String,
    path: PathBuf,
    offset_ms: u64,
}

/// Camera-audio side capture (ffmpeg, audio only).
struct AudioJob {
    child: Child,
    media_ms: Arc<AtomicU64>,
    offset_ms: u64,
    wav: PathBuf,
}

fn procs() -> &'static Mutex<HashMap<String, CameraProc>> {
    static PROCS: OnceLock<Mutex<HashMap<String, CameraProc>>> = OnceLock::new();
    PROCS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn audio_jobs() -> &'static Mutex<HashMap<String, AudioJob>> {
    static AUDIO: OnceLock<Mutex<HashMap<String, AudioJob>>> = OnceLock::new();
    AUDIO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Devices whose recording helper is CURRENTLY being started. While a device is
/// in here, subscribe_preview must NOT spawn a preview for it — the monitor
/// tiles' auto-reconnect would otherwise race the recorder and steal the camera
/// back the instant its old preview dies (the recorder then never gets frames).
fn record_pending() -> &'static Mutex<std::collections::HashSet<String>> {
    static PENDING: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Kill helper/ffmpeg processes orphaned by a previous hard-killed app instance.
pub fn cleanup_orphans() {
    let _ = Command::new("pkill")
        .args(["-f", "camera-helper --device"])
        .status();
    let _ = Command::new("pkill")
        .args(["-f", "avfoundation.*capture-audio-"])
        .status();
}

/// Background GC: previews nobody is watching (no HTTP subscribers) get reaped so
/// switching a track's camera releases the old device within seconds (LED off).
/// Recording processes are never touched. Spawned once at app startup.
pub fn spawn_preview_gc() {
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(15));
        let Ok(mut guard) = procs().lock() else {
            continue;
        };
        let idle: Vec<String> = guard
            .iter()
            .filter(|(_, p)| p.record.is_none() && p.tx.receiver_count() == 0)
            .map(|(k, _)| k.clone())
            .collect();
        drop(guard);
        for key in idle {
            let proc = procs().lock().ok().and_then(|mut g| {
                // Re-check under the lock: a subscriber may have just connected.
                match g.get(&key) {
                    Some(p) if p.record.is_none() && p.tx.receiver_count() == 0 => g.remove(&key),
                    _ => None,
                }
            });
            if let Some(proc) = proc {
                eprintln!("[camera:{}] preview idle — released", proc.device_label);
                stop_proc(proc);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Swift helper binary (compiled on demand)
// ---------------------------------------------------------------------------

fn helper_source() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/helpers/camera-helper.swift"
    ))
}

fn helper_bin() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".automixer/bin/camera-helper")
}

/// Compile the helper if missing or older than its source. Called at app startup
/// (background) and defensively before spawns.
pub fn ensure_helper() -> Result<PathBuf, String> {
    let bin = helper_bin();
    let src = helper_source();
    let stale = match (bin.metadata(), src.metadata()) {
        (Ok(b), Ok(s)) => s.modified().ok() > b.modified().ok(),
        (Err(_), _) => true,
        _ => false,
    };
    if stale {
        if let Some(dir) = bin.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        eprintln!("[camera] compiling capture helper…");
        let out = Command::new("swiftc")
            .arg("-O")
            .arg("-o")
            .arg(&bin)
            .arg(&src)
            .output()
            .map_err(|e| format!("swiftc not available: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "camera helper failed to compile: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        eprintln!("[camera] capture helper compiled");
    }
    Ok(bin)
}

// ---------------------------------------------------------------------------
// Device listing (still ffmpeg — cheap and reliable for names)
// ---------------------------------------------------------------------------

/// Parse `ffmpeg -f avfoundation -list_devices` output into (video, audio) name lists.
fn list_avfoundation_devices() -> Result<(Vec<String>, Vec<String>), String> {
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .output()
        .map_err(|e| format!("Could not run ffmpeg: {e}"))?;
    let text = String::from_utf8_lossy(&output.stderr);
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut section = 0;
    for line in text.lines() {
        if line.contains("AVFoundation video devices") {
            section = 1;
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            section = 2;
            continue;
        }
        if let Some(idx_start) = line.find("] [") {
            let rest = &line[idx_start + 3..];
            if let Some(close) = rest.find(']') {
                let name = rest[close + 1..].trim().to_string();
                if name.is_empty() {
                    continue;
                }
                match section {
                    1 => video.push(name),
                    2 => audio.push(name),
                    _ => {}
                }
            }
        }
    }
    Ok((video, audio))
}

fn find_device_index(devices: &[String], label: &str) -> Option<usize> {
    let l = label.trim().to_lowercase();
    if l.is_empty() {
        return None;
    }
    devices
        .iter()
        .position(|d| d.trim().to_lowercase() == l)
        .or_else(|| {
            devices.iter().position(|d| {
                let dl = d.trim().to_lowercase();
                dl.contains(&l) || l.contains(&dl)
            })
        })
}

/// The audio device that belongs to this camera (e.g. "Gustavo's iPhone Camera" →
/// "Gustavo's iPhone Microphone", or the exact-name match for USB webcams).
fn find_camera_audio_name(audio_devices: &[String], camera_label: &str) -> Option<String> {
    if let Some(i) = find_device_index(audio_devices, camera_label) {
        return Some(audio_devices[i].clone());
    }
    let base = camera_label
        .trim()
        .trim_end_matches("Desk View Camera")
        .trim_end_matches("Camera")
        .trim();
    if base.is_empty() {
        return None;
    }
    let bl = base.to_lowercase();
    audio_devices
        .iter()
        .find(|d| d.to_lowercase().starts_with(&bl))
        .cloned()
}

// ---------------------------------------------------------------------------
// Helper process management
// ---------------------------------------------------------------------------

/// Spawn a camera-helper for `device_label`. Preview always streams; when
/// `record` is set the same process also writes the take.
fn spawn_helper(
    device_label: &str,
    record: Option<&Path>,
    max_width: u32,
) -> Result<CameraProc, String> {
    let bin = ensure_helper()?;
    let mut cmd = Command::new(bin);
    cmd.arg("--device").arg(device_label).arg("--preview");
    if let Some(path) = record {
        cmd.arg("--record").arg(path);
    }
    if max_width > 0 {
        cmd.arg("--max-width").arg(max_width.to_string());
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start camera helper: {e}"))?;

    let (tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(256);
    if let Some(stdout) = child.stdout.take() {
        let tx_reader = tx.clone();
        std::thread::spawn(move || {
            let mut reader = stdout;
            let mut buf = [0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let _ = tx_reader.send(buf[..n].to_vec());
                    }
                }
            }
        });
    }

    let media_ms = Arc::new(AtomicU64::new(0));
    let ready = Arc::new(AtomicBool::new(false));
    let err_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    if let Some(stderr) = child.stderr.take() {
        let media = media_ms.clone();
        let ready_flag = ready.clone();
        let tail = err_tail.clone();
        let label = device_label.to_string();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(v) = line.strip_prefix("t=") {
                    if let Ok(ms) = v.trim().parse::<u64>() {
                        media.store(ms, Ordering::Relaxed);
                    }
                    continue;
                }
                if line.trim() == "ready" {
                    ready_flag.store(true, Ordering::Relaxed);
                }
                eprintln!("[camera:{label}] {line}");
                if let Ok(mut t) = tail.lock() {
                    t.push(line);
                    let excess = t.len().saturating_sub(8);
                    if excess > 0 {
                        t.drain(..excess);
                    }
                }
            }
        });
    }

    Ok(CameraProc {
        child,
        device_label: device_label.to_string(),
        tx,
        media_ms,
        ready,
        err_tail,
        record: None,
    })
}

fn stop_proc(mut proc: CameraProc) -> CameraProc {
    // SIGINT lets the helper finalize the mp4 (moov atom) before exiting.
    let pid = proc.child.id().to_string();
    let _ = Command::new("kill").args(["-INT", &pid]).status();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match proc.child.try_wait() {
            Ok(Some(_)) => break,
            _ => {
                if Instant::now() >= deadline {
                    let _ = proc.child.kill();
                    let _ = proc.child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    proc
}

// ---------------------------------------------------------------------------
// Preview API (used by the control server)
// ---------------------------------------------------------------------------

/// Ensure a helper runs for this camera and subscribe to its MJPEG stream.
/// Works during recording too — the recording helper streams preview as well.
pub fn subscribe_preview(
    device_label: &str,
) -> Result<tokio::sync::broadcast::Receiver<Vec<u8>>, String> {
    // A recorder is being started on this camera right now: do NOT spawn a
    // preview (it would steal the device mid-handoff). The tile keeps retrying
    // and connects to the recorder's own preview stream once it's registered.
    if record_pending()
        .lock()
        .map(|p| p.contains(device_label))
        .unwrap_or(false)
    {
        return Err("starting".into());
    }
    let mut guard = procs().lock().map_err(|e| e.to_string())?;
    if let Some(proc) = guard.get_mut(device_label) {
        match proc.child.try_wait() {
            Ok(None) => return Ok(proc.tx.subscribe()),
            _ => {
                // Died — respawn below (unless it was recording; that's fatal to the
                // take and stop_captures will report it).
                guard.remove(device_label);
            }
        }
    }
    // Previews are capped at 720p-class capture: full-native previews reserve so
    // much isochronous USB bandwidth that a SECOND camera can fail to start.
    let proc = spawn_helper(device_label, None, 1280)?;
    let rx = proc.tx.subscribe();
    guard.insert(device_label.to_string(), proc);
    Ok(rx)
}

/// Kill previews (empty list = all). Recording processes are left alone.
pub fn stop_previews(device_labels: &[String]) {
    let Ok(mut guard) = procs().lock() else {
        return;
    };
    let keys: Vec<String> = guard
        .iter()
        .filter(|(k, p)| {
            p.record.is_none()
                && (device_labels.is_empty() || device_labels.iter().any(|l| &l == k))
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in keys {
        if let Some(proc) = guard.remove(&key) {
            stop_proc(proc);
        }
    }
}

// ---------------------------------------------------------------------------
// Recording API
// ---------------------------------------------------------------------------

/// Start recording on every spec'd camera. The camera's existing preview process
/// is replaced by a record+preview process (same owner — deterministic, no
/// contention). All-or-nothing; errors name the culprit camera.
pub fn start_captures(videos_dir: PathBuf, specs: Vec<CaptureSpec>) -> Result<(), String> {
    if specs.is_empty() {
        return Ok(());
    }
    // HARD idempotency FIRST — before any cleanup. A double-trigger (Space
    // key-repeat, double-tap) used to run this twice concurrently: the second
    // call's stop_captures_discard killed the first call's recorders, and the
    // duplicate helpers deadlocked each other on the exclusive USB devices.
    static START_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
    if START_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return Err("A recording start is already in progress.".into());
    }
    struct StartGuard;
    impl Drop for StartGuard {
        fn drop(&mut self) {
            START_IN_PROGRESS.store(false, Ordering::SeqCst);
        }
    }
    let _start_guard = StartGuard;

    let _ = stop_captures_discard();
    std::fs::create_dir_all(&videos_dir).map_err(|e| e.to_string())?;
    ensure_helper()?;

    // Block preview spawns on these devices for the whole start window (see
    // record_pending). RAII so every exit path — including errors — unblocks.
    struct PendingGuard(Vec<String>);
    impl Drop for PendingGuard {
        fn drop(&mut self) {
            if let Ok(mut p) = record_pending().lock() {
                for label in &self.0 {
                    p.remove(label);
                }
            }
        }
    }
    let labels: Vec<String> = specs.iter().map(|s| s.device_label.clone()).collect();
    if let Ok(mut p) = record_pending().lock() {
        for label in &labels {
            p.insert(label.clone());
        }
    }
    let _pending_guard = PendingGuard(labels);

    let (video_devices, audio_devices) = list_avfoundation_devices()?;
    // Validate all devices up front.
    for spec in &specs {
        if find_device_index(&video_devices, &spec.device_label).is_none() {
            return Err(format!(
                "Camera \"{}\" not found (available: {}).",
                spec.device_label,
                video_devices.join(", ")
            ));
        }
    }

    // Swap preview→record helper per camera, all in parallel threads.
    let handles: Vec<_> = specs
        .iter()
        .cloned()
        .map(|spec| {
            let videos_dir = videos_dir.clone();
            let audio_devices = audio_devices.clone();
            std::thread::spawn(move || -> Result<(CameraProc, Option<(String, AudioJob)>), String> {
                // Take over the device from its preview helper (we own it).
                if let Ok(mut guard) = procs().lock() {
                    if let Some(existing) = guard.remove(&spec.device_label) {
                        stop_proc(existing);
                    }
                }
                // macOS can take a beat to release a just-closed camera; the helper
                // exits with a distinct error when it hits that window ("Cannot
                // Use…" / "no frames…"), and we simply respawn until the overall
                // deadline. Both layers retry, so the handoff is race-free.
                let path = videos_dir.join(format!("capture-{}.mp4", uuid::Uuid::new_v4()));
                let overall_deadline = Instant::now() + Duration::from_secs(30);
                // Bandwidth tier ladder: try the camera's full native mode first; if
                // it starts but delivers no frames (USB isochronous reservation did
                // not fit — several cameras sharing bus budget), step the request
                // down and retry. Every camera records at the best size that FITS.
                let tiers: [u32; 4] = [0, 1920, 1280, 960];
                let mut tier_idx = 0usize;
                let mut attempts_at_tier = 0u32;
                let mut proc;
                'open: loop {
                    std::thread::sleep(Duration::from_millis(250));
                    let mut candidate = spawn_helper(&spec.device_label, Some(&path), tiers[tier_idx])?;
                    loop {
                        if candidate.ready.load(Ordering::Relaxed) {
                            if tier_idx > 0 {
                                eprintln!("[camera:{}] recording at reduced size (max width {}) — USB bandwidth", spec.device_label, tiers[tier_idx]);
                            }
                            proc = candidate;
                            break 'open;
                        }
                        if let Ok(Some(status)) = candidate.child.try_wait() {
                            let tail = candidate.err_tail.lock().map(|t| t.join(" | ")).unwrap_or_default();
                            let no_frames = tail.contains("no frames from");
                            let locked = tail.contains("Cannot Use");
                            let _ = std::fs::remove_file(&path);
                            if (no_frames || locked) && Instant::now() < overall_deadline {
                                attempts_at_tier += 1;
                                // One repeat per tier for release races; then step down —
                                // repeated no-frames means the format doesn't FIT.
                                if no_frames && attempts_at_tier >= 2 && tier_idx + 1 < tiers.len() {
                                    tier_idx += 1;
                                    attempts_at_tier = 0;
                                    eprintln!("[camera:{}] no frames at this size — stepping down to max width {}", spec.device_label, tiers[tier_idx]);
                                } else {
                                    eprintln!("[camera:{}] retrying (attempt {})", spec.device_label, attempts_at_tier + 1);
                                }
                                continue 'open;
                            }
                            return Err(format!("\"{}\" failed to start ({status}): {tail}", spec.device_label));
                        }
                        if Instant::now() >= overall_deadline {
                            let _ = candidate.child.kill();
                            let _ = candidate.child.wait();
                            let _ = std::fs::remove_file(&path);
                            return Err(format!(
                                "\"{}\" delivers no frames even at reduced sizes — USB bandwidth or a wedged camera. Try a different port/hub, or unplug and replug it.",
                                spec.device_label
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                proc.record = Some(RecordInfo {
                    track_id: spec.track_id.clone(),
                    path: path.clone(),
                    offset_ms: 0,
                });
                // Camera audio: separate ffmpeg audio-only capture (wav).
                let audio = if spec.include_audio {
                    find_camera_audio_name(&audio_devices, &spec.device_label).and_then(|audio_name| {
                        let wav = videos_dir.join(format!("capture-audio-{}.wav", uuid::Uuid::new_v4()));
                        match spawn_audio_capture(&audio_name, &wav) {
                            Ok(job) => Some((spec.track_id.clone(), job)),
                            Err(e) => {
                                eprintln!("[camera:{}] audio capture failed ({e}) — video-only take", spec.device_label);
                                None
                            }
                        }
                    })
                } else {
                    None
                };
                Ok((proc, audio))
            })
        })
        .collect();

    let mut started: Vec<CameraProc> = Vec::new();
    let mut audio_started: Vec<(String, AudioJob)> = Vec::new();
    let mut fail: Option<String> = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok((proc, audio))) => {
                started.push(proc);
                if let Some(a) = audio {
                    audio_started.push(a);
                }
            }
            Ok(Err(message)) => {
                fail = Some(fail.map_or(message.clone(), |f: String| format!("{f} {message}")))
            }
            Err(_) => fail = Some("camera worker thread panicked".into()),
        }
    }

    if let Some(message) = fail {
        for proc in started {
            let record_path = proc.record.as_ref().map(|r| r.path.clone());
            stop_proc(proc);
            if let Some(p) = record_path {
                let _ = std::fs::remove_file(p);
            }
        }
        for (_, mut job) in audio_started {
            let _ = job.child.kill();
            let _ = job.child.wait();
            let _ = std::fs::remove_file(&job.wav);
        }
        return Err(message);
    }

    let mut guard = procs().lock().map_err(|e| e.to_string())?;
    for proc in started {
        guard.insert(proc.device_label.clone(), proc);
    }
    let mut aguard = audio_jobs().lock().map_err(|e| e.to_string())?;
    for (track_id, job) in audio_started {
        aguard.insert(track_id, job);
    }
    Ok(())
}

/// Audio-only camera-mic capture via ffmpeg (none of the USB video constraints
/// apply to audio). Progress on stdout gives us the media clock for alignment.
fn spawn_audio_capture(audio_name: &str, wav: &Path) -> Result<AudioJob, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(["-progress", "pipe:1", "-stats_period", "0.2"])
        .args(["-f", "avfoundation", "-i", &format!("none:{audio_name}")])
        .args(["-c:a", "pcm_s16le"])
        .arg("-y")
        .arg(wav);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let media_ms = Arc::new(AtomicU64::new(0));
    if let Some(stdout) = child.stdout.take() {
        let media = media_ms.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(v) = line
                    .strip_prefix("out_time_us=")
                    .or_else(|| line.strip_prefix("out_time_ms="))
                {
                    if let Ok(us) = v.trim().parse::<i64>() {
                        media.store(us.max(0) as u64 / 1000, Ordering::Relaxed);
                    }
                }
            }
        });
    }
    Ok(AudioJob {
        child,
        media_ms,
        offset_ms: 0,
        wav: wav.to_path_buf(),
    })
}

/// Snapshot every capture's media clock the instant the transport starts —
/// that exact time is trimmed from each take's head so it aligns with the timeline.
pub fn mark_transport_start() {
    if let Ok(mut guard) = procs().lock() {
        for proc in guard.values_mut() {
            let ms = proc.media_ms.load(Ordering::Relaxed);
            if let Some(record) = proc.record.as_mut() {
                record.offset_ms = ms;
            }
        }
    }
    if let Ok(mut aguard) = audio_jobs().lock() {
        for job in aguard.values_mut() {
            job.offset_ms = job.media_ms.load(Ordering::Relaxed);
        }
    }
}

/// Kill all active recordings and delete their files (failed/cancelled starts).
/// Preview-only helpers stay untouched.
pub fn stop_captures_discard() -> Result<(), String> {
    let recording_keys: Vec<String> = {
        let guard = procs().lock().map_err(|e| e.to_string())?;
        guard
            .iter()
            .filter(|(_, p)| p.record.is_some())
            .map(|(k, _)| k.clone())
            .collect()
    };
    for key in recording_keys {
        let proc = procs().lock().ok().and_then(|mut g| g.remove(&key));
        if let Some(proc) = proc {
            let record_path = proc.record.as_ref().map(|r| r.path.clone());
            stop_proc(proc);
            if let Some(p) = record_path {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    if let Ok(mut aguard) = audio_jobs().lock() {
        for (_, mut job) in aguard.drain() {
            let _ = job.child.kill();
            let _ = job.child.wait();
            let _ = std::fs::remove_file(&job.wav);
        }
    }
    Ok(())
}

fn probe_duration_ms(path: &PathBuf) -> Option<u64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    let secs: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    Some((secs * 1000.0).round().max(1.0) as u64)
}

/// Stop the requested captures and return their finalized files. Captures not in
/// `track_ids` are discarded. Tiles reconnect automatically (their helper exits;
/// the <img> retry respawns a preview-only helper).
pub fn stop_captures(track_ids: &[String]) -> Result<Vec<FinishedCapture>, String> {
    let recording_keys: Vec<String> = {
        let guard = procs().lock().map_err(|e| e.to_string())?;
        guard
            .iter()
            .filter(|(_, p)| p.record.is_some())
            .map(|(k, _)| k.clone())
            .collect()
    };
    let mut audio_map: HashMap<String, AudioJob> = {
        let mut aguard = audio_jobs().lock().map_err(|e| e.to_string())?;
        aguard.drain().collect()
    };
    let mut finished = Vec::new();
    for key in recording_keys {
        let Some(proc) = procs().lock().ok().and_then(|mut g| g.remove(&key)) else {
            continue;
        };
        let device_label = proc.device_label.clone();
        let record = proc
            .record
            .as_ref()
            .map(|r| (r.track_id.clone(), r.path.clone(), r.offset_ms));
        stop_proc(proc);
        let Some((track_id, path, offset_ms)) = record else {
            continue;
        };
        // Stop this track's audio capture (if any) regardless of keep/discard.
        let audio = audio_map.remove(&track_id).map(|mut job| {
            let pid = job.child.id().to_string();
            let _ = Command::new("kill").args(["-INT", &pid]).status();
            let _ = job.child.wait();
            (job.wav, job.offset_ms)
        });
        if !track_ids.contains(&track_id) {
            let _ = std::fs::remove_file(&path);
            if let Some((wav, _)) = audio {
                let _ = std::fs::remove_file(wav);
            }
            continue;
        }
        let Some(duration_ms) = probe_duration_ms(&path) else {
            let _ = std::fs::remove_file(&path);
            eprintln!("[camera:{device_label}] unreadable take — discarded");
            continue;
        };
        finished.push(FinishedCapture {
            track_id,
            path,
            duration_ms,
            offset_ms: offset_ms.min(duration_ms.saturating_sub(1)),
            audio_wav: audio,
        });
    }
    // Any audio jobs left without a matching video take: kill + discard.
    for (_, mut job) in audio_map {
        let _ = job.child.kill();
        let _ = job.child.wait();
        let _ = std::fs::remove_file(&job.wav);
    }
    Ok(finished)
}
