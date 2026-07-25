//! Promptable track-region separation through the remote SAM-Audio service.
//!
//! Extraction is deliberately two-phase: prepare/run only create audition files;
//! apply performs one recorded project patch after the user accepts the result.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use hound::{SampleFormat, WavSpec, WavWriter};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::{
    actions::record_patch,
    config::Config,
    engine::source::{cache::read_cache_all, decode::decode_file},
    model::{ClipRegion, HistorySource, JsonPatchOp, MixProject, MixSession, Track, TrackKind},
    AppState,
};

const POLL_INTERVAL: Duration = Duration::from_millis(800);
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
struct PendingSplit {
    id: String,
    session_id: String,
    track_id: String,
    track_name: String,
    start_sample: u64,
    end_sample: u64,
    sample_rate: u32,
    arrangement_fingerprint: String,
    original_path: PathBuf,
    original_peaks: Vec<f32>,
    target_path: Option<PathBuf>,
    residual_path: Option<PathBuf>,
    target_peaks: Vec<f32>,
    residual_peaks: Vec<f32>,
    prompt: Option<String>,
    remote_job_id: Option<String>,
    cancelled: Arc<AtomicBool>,
}

fn previews() -> &'static Mutex<HashMap<String, PendingSplit>> {
    static PREVIEWS: OnceLock<Mutex<HashMap<String, PendingSplit>>> = OnceLock::new();
    PREVIEWS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitTrackPreview {
    preview_id: String,
    session_id: String,
    track_id: String,
    track_name: String,
    start_sample: u64,
    end_sample: u64,
    sample_rate: u32,
    original_path: String,
    original_peaks: Vec<f32>,
    target_path: Option<String>,
    residual_path: Option<String>,
    target_peaks: Vec<f32>,
    residual_peaks: Vec<f32>,
    prompt: Option<String>,
}

impl From<&PendingSplit> for SplitTrackPreview {
    fn from(value: &PendingSplit) -> Self {
        Self {
            preview_id: value.id.clone(),
            session_id: value.session_id.clone(),
            track_id: value.track_id.clone(),
            track_name: value.track_name.clone(),
            start_sample: value.start_sample,
            end_sample: value.end_sample,
            sample_rate: value.sample_rate,
            original_path: value.original_path.to_string_lossy().to_string(),
            original_peaks: value.original_peaks.clone(),
            target_path: value.target_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            residual_path: value.residual_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            target_peaks: value.target_peaks.clone(),
            residual_peaks: value.residual_peaks.clone(),
            prompt: value.prompt.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySplitResponse {
    project: MixProject,
    extracted_track_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SplitProgress {
    preview_id: String,
    session_id: String,
    phase: String,
    message: String,
    progress: f64,
    chunk: Option<u32>,
    chunks: Option<u32>,
    elapsed_seconds: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamAudioConfigResponse {
    base_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamAudioHealthResponse {
    base_url: String,
    status: String,
    loaded: bool,
    loading: bool,
    busy: bool,
    device: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteHealth {
    #[serde(default)]
    status: String,
    #[serde(default)]
    loaded: bool,
    #[serde(default)]
    loading: bool,
    #[serde(default)]
    busy: bool,
    device: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateJobResponse {
    job_id: String,
}

#[derive(Debug, Deserialize)]
struct RemoteJobStatus {
    status: String,
    #[serde(default)]
    phase: String,
    #[serde(default)]
    progress: f64,
    chunk: Option<u32>,
    chunks: Option<u32>,
    error: Option<String>,
}

#[tauri::command]
pub fn get_sam_audio_config() -> SamAudioConfigResponse {
    SamAudioConfigResponse {
        base_url: Config::load().sam_audio_base_url,
    }
}

#[tauri::command]
pub fn set_sam_audio_config(base_url: String) -> Result<(), String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("SAM-Audio endpoint URL is required.".into());
    }
    let mut config = Config::load();
    config.sam_audio_base_url = trimmed.to_string();
    config.save()
}

#[tauri::command]
pub async fn test_sam_audio_connection() -> Result<SamAudioHealthResponse, String> {
    let base_url = Config::load().sam_audio_base_url.trim_end_matches('/').to_string();
    let health = reqwest::Client::new()
        .get(format!("{base_url}/health"))
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|error| format!("SAM-Audio is unreachable at {base_url}: {error}"))?
        .error_for_status()
        .map_err(|error| format!("SAM-Audio health check failed: {error}"))?
        .json::<RemoteHealth>()
        .await
        .map_err(|error| format!("SAM-Audio returned malformed health data: {error}"))?;
    Ok(SamAudioHealthResponse {
        base_url,
        status: health.status,
        loaded: health.loaded,
        loading: health.loading,
        busy: health.busy,
        device: health.device,
    })
}

#[tauri::command]
pub fn prepare_track_split(
    state: State<'_, AppState>,
    session_id: String,
    track_id: String,
    start_sample: u64,
    end_sample: u64,
) -> Result<SplitTrackPreview, String> {
    if end_sample <= start_sample {
        return Err("Select a non-empty timeline region first.".into());
    }
    let project = state
        .store
        .lock()
        .map_err(|error| error.to_string())?
        .get_project(&session_id)?;
    let track = project
        .session
        .tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or_else(|| "The selected track no longer exists.".to_string())?;
    if track.kind != TrackKind::Audio {
        return Err("Split Track only supports one audio track.".into());
    }

    let id = Uuid::new_v4().to_string();
    let dir = state.config.data_dir.join("sam-previews");
    fs::create_dir_all(&dir).map_err(|error| format!("Could not create split preview directory: {error}"))?;
    let original_path = dir.join(format!("{id}-original.wav"));
    let original_peaks = render_raw_track_region(
        &project.session,
        track,
        start_sample,
        end_sample,
        &original_path,
    )?;
    let pending = PendingSplit {
        id: id.clone(),
        session_id: session_id.clone(),
        track_id: track_id.clone(),
        track_name: track.name.clone(),
        start_sample,
        end_sample,
        sample_rate: project.session.sample_rate,
        arrangement_fingerprint: arrangement_fingerprint(track)?,
        original_path,
        original_peaks,
        target_path: None,
        residual_path: None,
        target_peaks: Vec::new(),
        residual_peaks: Vec::new(),
        prompt: None,
        remote_job_id: None,
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let response = SplitTrackPreview::from(&pending);
    previews().lock().map_err(|error| error.to_string())?.insert(id, pending);
    Ok(response)
}

#[tauri::command]
pub async fn run_track_split(
    app: AppHandle,
    preview_id: String,
    prompt: String,
) -> Result<SplitTrackPreview, String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Describe the sound to extract.".into());
    }
    let pending = previews()
        .lock()
        .map_err(|error| error.to_string())?
        .get(&preview_id)
        .cloned()
        .ok_or_else(|| "This split preview expired. Open Split Track again.".to_string())?;
    pending.cancelled.store(false, Ordering::Relaxed);
    let started = Instant::now();
    emit_progress(&app, &pending, "uploading", "Uploading selected audio to SAM-Audio…", 0.02, None, None, started);

    let base_url = Config::load().sam_audio_base_url.trim_end_matches('/').to_string();
    let bytes = fs::read(&pending.original_path)
        .map_err(|error| format!("Could not read the selected audio preview: {error}"))?;
    let part = Part::bytes(bytes)
        .file_name("selected-region.wav")
        .mime_str("audio/wav")
        .map_err(|error| error.to_string())?;
    let form = Form::new()
        .part("file", part)
        .text("description", prompt.clone())
        .text("residual", "true")
        .text("reranking", "0");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(format!("{base_url}/isolate"))
        .multipart(form)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|error| format!("Could not submit audio to SAM-Audio at {base_url}: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("SAM-Audio rejected the split ({status}): {body}"));
    }
    let created = response
        .json::<CreateJobResponse>()
        .await
        .map_err(|error| format!("SAM-Audio returned an invalid job response: {error}"))?;
    if let Ok(mut guard) = previews().lock() {
        if let Some(current) = guard.get_mut(&preview_id) {
            current.remote_job_id = Some(created.job_id.clone());
        }
    }

    let status = poll_job(&app, &client, &base_url, &created.job_id, &pending, started).await?;
    if status.status != "done" {
        return Err(format!("SAM-Audio ended in unexpected state `{}`.", status.status));
    }
    if pending.cancelled.load(Ordering::Relaxed) {
        return Err("Track split cancelled.".into());
    }

    emit_progress(&app, &pending, "downloading", "Downloading separated audio…", 0.96, status.chunk, status.chunks, started);
    let dir = pending.original_path.parent().ok_or("Invalid preview directory")?;
    let target_path = dir.join(format!("{preview_id}-target.wav"));
    let residual_path = dir.join(format!("{preview_id}-residual.wav"));
    download_artifact(&client, &base_url, &created.job_id, "target.wav", &target_path).await?;
    download_artifact(&client, &base_url, &created.job_id, "residual.wav", &residual_path).await?;
    let _ = client.delete(format!("{base_url}/jobs/{}", created.job_id)).send().await;

    let target_peaks = peaks_from_audio_file(&target_path)?;
    let residual_peaks = peaks_from_audio_file(&residual_path)?;
    let response = {
        let mut guard = previews().lock().map_err(|error| error.to_string())?;
        let Some(current) = guard.get_mut(&preview_id) else {
            let _ = fs::remove_file(&target_path);
            let _ = fs::remove_file(&residual_path);
            return Err("Track split was discarded.".into());
        };
        current.target_path = Some(target_path);
        current.residual_path = Some(residual_path);
        current.target_peaks = target_peaks;
        current.residual_peaks = residual_peaks;
        current.prompt = Some(prompt);
        current.remote_job_id = None;
        SplitTrackPreview::from(&*current)
    };
    emit_progress(&app, &pending, "ready", "Separated audio is ready to review.", 1.0, status.chunk, status.chunks, started);
    Ok(response)
}

#[tauri::command]
pub fn cancel_track_split(preview_id: String) -> Result<(), String> {
    let guard = previews().lock().map_err(|error| error.to_string())?;
    let preview = guard
        .get(&preview_id)
        .ok_or_else(|| "Unknown split preview.".to_string())?;
    preview.cancelled.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn discard_track_split(preview_id: String) -> Result<(), String> {
    let pending = previews().lock().map_err(|error| error.to_string())?.remove(&preview_id);
    if let Some(pending) = pending {
        pending.cancelled.store(true, Ordering::Relaxed);
        cleanup_preview_files(&pending);
    }
    Ok(())
}

#[tauri::command]
pub fn apply_track_split(
    state: State<'_, AppState>,
    preview_id: String,
) -> Result<ApplySplitResponse, String> {
    let pending = previews()
        .lock()
        .map_err(|error| error.to_string())?
        .get(&preview_id)
        .cloned()
        .ok_or_else(|| "This split preview expired.".to_string())?;
    let target_path = pending.target_path.as_ref().ok_or("Extracted audio is not ready.")?;
    let residual_path = pending.residual_path.as_ref().ok_or("Background audio is not ready.")?;
    let prompt = pending.prompt.as_deref().ok_or("Extraction prompt is missing.")?;

    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&pending.session_id)?;
    let track_index = project
        .session
        .tracks
        .iter()
        .position(|track| track.id == pending.track_id)
        .ok_or_else(|| "The source track was deleted while SAM-Audio was running.".to_string())?;
    if arrangement_fingerprint(&project.session.tracks[track_index])? != pending.arrangement_fingerprint {
        return Err("The source track changed while SAM-Audio was running. Discard this preview and run Split Track again.".into());
    }

    let target_source = store.import_source_standalone(target_path, project.session.sample_rate)?;
    let residual_source = store.import_source_standalone(residual_path, project.session.sample_rate)?;
    let before_tracks = project.session.tracks.clone();
    let before_sources = project.session.source_files.clone();
    let mut after_tracks = before_tracks.clone();
    let source_track = after_tracks[track_index].clone();
    let residual_clip = ClipRegion {
        id: Uuid::new_v4().to_string(),
        source_file_id: Some(residual_source.id.clone()),
        name: Some("Background".into()),
        start_sample: pending.start_sample,
        end_sample: pending.end_sample,
        source_offset_sample: 0,
        gain_db: 0.0,
    };
    let clips = replace_range_with_clip(
        &project.session,
        &source_track,
        pending.start_sample,
        pending.end_sample,
        residual_clip,
    )?;
    after_tracks[track_index].clips = clips;
    after_tracks[track_index].clips_materialized = true;

    let extracted_track_id = Uuid::new_v4().to_string();
    let extracted_name = extracted_track_name(prompt);
    let mut extracted_track = source_track;
    extracted_track.id = extracted_track_id.clone();
    extracted_track.name = extracted_name.clone();
    extracted_track.role = Some("separated stem".into());
    extracted_track.source_file_id = target_source.id.clone();
    extracted_track.start_sample = pending.start_sample;
    extracted_track.ai_generated = true;
    extracted_track.color = split_track_color(after_tracks.len());
    extracted_track.clips = vec![ClipRegion {
        id: Uuid::new_v4().to_string(),
        source_file_id: Some(target_source.id.clone()),
        name: Some(extracted_name),
        start_sample: pending.start_sample,
        end_sample: pending.end_sample,
        source_offset_sample: 0,
        gain_db: 0.0,
    }];
    extracted_track.clips_materialized = true;
    extracted_track.video_clips.clear();
    extracted_track.camera_device_id = None;
    extracted_track.record_camera_audio = false;
    for lane in &mut extracted_track.automation {
        lane.id = Uuid::new_v4().to_string();
    }
    after_tracks.push(extracted_track);

    let mut after_sources = before_sources.clone();
    after_sources.push(residual_source);
    after_sources.push(target_source);
    record_patch(
        &mut project,
        vec![
            JsonPatchOp { op: "replace".into(), path: "/sourceFiles".into(), value: Some(serde_json::json!(after_sources)) },
            JsonPatchOp { op: "replace".into(), path: "/tracks".into(), value: Some(serde_json::json!(after_tracks)) },
        ],
        vec![
            JsonPatchOp { op: "replace".into(), path: "/sourceFiles".into(), value: Some(serde_json::json!(before_sources)) },
            JsonPatchOp { op: "replace".into(), path: "/tracks".into(), value: Some(serde_json::json!(before_tracks)) },
        ],
        HistorySource::User,
        Some(format!("Split track: {prompt}")),
    )?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        crate::commands::sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    drop(store);
    if let Ok(mut guard) = previews().lock() {
        guard.remove(&preview_id);
    }
    cleanup_preview_files(&pending);
    Ok(ApplySplitResponse { project, extracted_track_id })
}

async fn poll_job(
    app: &AppHandle,
    client: &reqwest::Client,
    base_url: &str,
    job_id: &str,
    pending: &PendingSplit,
    started: Instant,
) -> Result<RemoteJobStatus, String> {
    loop {
        if pending.cancelled.load(Ordering::Relaxed) {
            let _ = client.post(format!("{base_url}/jobs/{job_id}/cancel")).send().await;
            return Err("Track split cancelled.".into());
        }
        if started.elapsed() > JOB_TIMEOUT {
            let _ = client.post(format!("{base_url}/jobs/{job_id}/cancel")).send().await;
            return Err("SAM-Audio did not finish within 30 minutes.".into());
        }
        let response = client
            .get(format!("{base_url}/jobs/{job_id}"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|error| format!("Could not read SAM-Audio job status: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("SAM-Audio job status failed with {}.", response.status()));
        }
        let status = response
            .json::<RemoteJobStatus>()
            .await
            .map_err(|error| format!("SAM-Audio returned malformed job status: {error}"))?;
        let phase = if status.phase.is_empty() { status.status.as_str() } else { status.phase.as_str() };
        let message = match phase {
            "waiting" | "queued" => "Waiting for the SAM-Audio worker…".to_string(),
            "loading" => "Loading the SAM-Audio model…".to_string(),
            "separating" => match (status.chunk, status.chunks) {
                (Some(chunk), Some(chunks)) => format!("Separating chunk {chunk} of {chunks}…"),
                _ => "Separating the selected audio…".to_string(),
            },
            "writing" => "Writing separated audio…".to_string(),
            _ => "Processing selected audio…".to_string(),
        };
        emit_progress(app, pending, phase, &message, status.progress.clamp(0.0, 1.0), status.chunk, status.chunks, started);
        match status.status.as_str() {
            "done" => return Ok(status),
            "error" => return Err(status.error.unwrap_or_else(|| "SAM-Audio separation failed.".into())),
            "cancelled" => return Err("Track split cancelled.".into()),
            _ => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}

async fn download_artifact(
    client: &reqwest::Client,
    base_url: &str,
    job_id: &str,
    artifact: &str,
    output: &Path,
) -> Result<(), String> {
    let response = client
        .get(format!("{base_url}/jobs/{job_id}/{artifact}"))
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|error| format!("Could not download {artifact}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("SAM-Audio {artifact} download failed with {}.", response.status()));
    }
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    fs::write(output, bytes).map_err(|error| format!("Could not save {artifact}: {error}"))
}

fn emit_progress(
    app: &AppHandle,
    pending: &PendingSplit,
    phase: &str,
    message: &str,
    progress: f64,
    chunk: Option<u32>,
    chunks: Option<u32>,
    started: Instant,
) {
    let _ = app.emit(
        "sam-split:progress",
        SplitProgress {
            preview_id: pending.id.clone(),
            session_id: pending.session_id.clone(),
            phase: phase.to_string(),
            message: message.to_string(),
            progress,
            chunk,
            chunks,
            elapsed_seconds: started.elapsed().as_secs_f64(),
        },
    );
}

fn arrangement_fingerprint(track: &Track) -> Result<String, String> {
    serde_json::to_string(&(&track.source_file_id, track.start_sample, &track.clips))
        .map_err(|error| error.to_string())
}

fn materialized_clips(session: &MixSession, track: &Track) -> Result<Vec<ClipRegion>, String> {
    if !track.clips.is_empty() || track.clips_materialized {
        return Ok(track.clips.clone());
    }
    let source = session
        .source_files
        .iter()
        .find(|source| source.id == track.source_file_id)
        .ok_or_else(|| format!("Source audio for `{}` is missing.", track.name))?;
    Ok(vec![ClipRegion {
        id: Uuid::new_v4().to_string(),
        source_file_id: Some(source.id.clone()),
        name: Some(track.name.clone()),
        start_sample: track.start_sample,
        end_sample: track.start_sample.saturating_add(source.duration_samples),
        source_offset_sample: 0,
        gain_db: 0.0,
    }])
}

fn render_raw_track_region(
    session: &MixSession,
    track: &Track,
    start_sample: u64,
    end_sample: u64,
    output_path: &Path,
) -> Result<Vec<f32>, String> {
    let frames = end_sample.saturating_sub(start_sample) as usize;
    if frames == 0 {
        return Err("Select a non-empty timeline region first.".into());
    }
    let sources: HashMap<&str, _> = session
        .source_files
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect();
    let mut output = vec![0.0_f32; frames];
    let mut found_audio = false;
    for clip in materialized_clips(session, track)? {
        let overlap_start = start_sample.max(clip.start_sample);
        let overlap_end = end_sample.min(clip.end_sample);
        if overlap_end <= overlap_start {
            continue;
        }
        let source_id = clip.source_file_id.as_deref().unwrap_or(track.source_file_id.as_str());
        let source = sources
            .get(source_id)
            .ok_or_else(|| format!("Audio source {source_id} is missing."))?;
        let (header, samples) = read_cache_all(Path::new(&source.cache_path))?;
        if header.sample_rate != session.sample_rate {
            return Err(format!("Source `{}` has an unexpected sample rate.", source.original_name));
        }
        let source_start = clip
            .source_offset_sample
            .saturating_add(overlap_start.saturating_sub(clip.start_sample));
        let available = header.frames.saturating_sub(source_start);
        let count = overlap_end.saturating_sub(overlap_start).min(available) as usize;
        if count == 0 {
            continue;
        }
        found_audio = true;
        let gain = 10.0_f32.powf(clip.gain_db / 20.0);
        let channels = header.channels.max(1) as usize;
        let out_start = overlap_start.saturating_sub(start_sample) as usize;
        for frame in 0..count {
            let source_frame = source_start as usize + frame;
            let base = source_frame * channels;
            let mut mono = 0.0;
            for channel in 0..channels {
                mono += samples.get(base + channel).copied().unwrap_or(0.0);
            }
            output[out_start + frame] += (mono / channels as f32) * gain;
        }
    }
    if !found_audio {
        return Err("The selected track has no audio in that region.".into());
    }
    let spec = WavSpec {
        channels: 1,
        sample_rate: session.sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::create(output_path, spec).map_err(|error| error.to_string())?;
    for sample in &output {
        writer.write_sample(sample.clamp(-1.0, 1.0)).map_err(|error| error.to_string())?;
    }
    writer.finalize().map_err(|error| error.to_string())?;
    Ok(build_peaks(&output))
}

fn peaks_from_audio_file(path: &Path) -> Result<Vec<f32>, String> {
    let decoded = decode_file(path)?;
    let channels = decoded.channels.max(1) as usize;
    let mut mono = Vec::with_capacity(decoded.samples.len() / channels);
    for frame in decoded.samples.chunks(channels) {
        mono.push(frame.iter().map(|sample| sample.abs()).sum::<f32>() / frame.len() as f32);
    }
    Ok(build_peaks(&mono))
}

fn build_peaks(samples: &[f32]) -> Vec<f32> {
    const BINS: usize = 360;
    if samples.is_empty() {
        return Vec::new();
    }
    let width = samples.len().div_ceil(BINS).max(1);
    samples
        .chunks(width)
        .map(|chunk| chunk.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs())).min(1.0))
        .collect()
}

fn replace_range_with_clip(
    session: &MixSession,
    track: &Track,
    start_sample: u64,
    end_sample: u64,
    replacement: ClipRegion,
) -> Result<Vec<ClipRegion>, String> {
    let mut next = Vec::new();
    for clip in materialized_clips(session, track)? {
        if clip.end_sample <= start_sample || clip.start_sample >= end_sample {
            next.push(clip);
            continue;
        }
        if clip.start_sample < start_sample {
            let mut left = clip.clone();
            left.end_sample = start_sample;
            next.push(left);
        }
        if clip.end_sample > end_sample {
            let mut right = clip;
            right.id = Uuid::new_v4().to_string();
            right.source_offset_sample = right
                .source_offset_sample
                .saturating_add(end_sample.saturating_sub(right.start_sample));
            right.start_sample = end_sample;
            next.push(right);
        }
    }
    next.push(replacement);
    next.sort_by_key(|clip| (clip.start_sample, clip.end_sample));
    Ok(next)
}

fn extracted_track_name(prompt: &str) -> String {
    let trimmed = prompt.trim();
    let lower = trimmed.to_ascii_lowercase();
    let subject = ["extract ", "isolate ", "separate "]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix).map(|_| &trimmed[prefix.len()..]))
        .unwrap_or(trimmed)
        .trim();
    if subject.is_empty() { "Extracted audio".into() } else { format!("Extracted - {subject}") }
}

fn split_track_color(index: usize) -> String {
    const COLORS: &[&str] = &["#e6be63", "#62c98a", "#d47ac7", "#68b7d4", "#d9826b", "#8ea7e8"];
    COLORS[index % COLORS.len()].to_string()
}

fn cleanup_preview_files(pending: &PendingSplit) {
    let _ = fs::remove_file(&pending.original_path);
    if let Some(path) = &pending.target_path {
        let _ = fs::remove_file(path);
    }
    if let Some(path) = &pending.residual_path {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(cache_path: &Path, clips: serde_json::Value) -> MixSession {
        serde_json::from_value(serde_json::json!({
            "id": "session",
            "name": "Song",
            "sampleRate": 48000,
            "sourceFiles": [{
                "id": "source",
                "originalName": "source.wav",
                "cachePath": cache_path.to_string_lossy(),
                "peakPath": "peaks.json",
                "durationSamples": 100,
                "sampleRate": 48000,
                "channels": 1,
                "analysis": {
                    "peakDb": 0.0, "rmsDb": -6.0, "lufsEstimate": -8.0,
                    "spectralCentroidHz": 1000.0, "lowEnergy": 0.2,
                    "midEnergy": 0.6, "highEnergy": 0.2,
                    "silencePercent": 0.0, "dynamicRangeDb": 6.0
                },
                "peakPreview": []
            }],
            "tracks": [{
                "id": "track", "kind": "audio", "name": "Guitar",
                "sourceFileId": "source", "startSample": 0,
                "gainDb": 0.0, "pan": 0.0, "muted": false, "solo": false,
                "color": "#fff",
                "chain": {
                    "highPass": {"enabled": false, "frequencyHz": 20.0, "slopeDbOct": 12},
                    "lowPass": {"enabled": false, "frequencyHz": 20000.0, "slopeDbOct": 12},
                    "eq": [],
                    "compressor": {"enabled": false, "thresholdDb": -18.0, "ratio": 2.0, "attackMs": 20.0, "releaseMs": 160.0, "kneeDb": 6.0, "makeupDb": 0.0}
                },
                "sends": {"reverbDb": -60.0, "delayDb": -60.0},
                "automation": [], "clips": clips
            }],
            "buses": [], "regions": [], "markers": [],
            "master": {"gainDb": 0.0, "limiter": {"enabled": false, "ceilingDb": -1.0}}
        })).expect("test session")
    }

    #[test]
    fn extracted_name_removes_command_prefix() {
        assert_eq!(extracted_track_name("extract solo guitar"), "Extracted - solo guitar");
        assert_eq!(extracted_track_name("vocals"), "Extracted - vocals");
    }

    #[test]
    fn peak_builder_is_bounded() {
        let peaks = build_peaks(&[0.0, -0.5, 1.2, 0.25]);
        assert!(peaks.iter().all(|peak| (0.0..=1.0).contains(peak)));
        assert_eq!(peaks.iter().copied().fold(0.0_f32, f32::max), 1.0);
    }

    #[test]
    fn range_replacement_preserves_both_clip_sides() {
        let session = test_session(
            Path::new("unused"),
            serde_json::json!([{
                "id": "clip", "sourceFileId": "source", "startSample": 0,
                "endSample": 100, "sourceOffsetSample": 10, "gainDb": -2.0
            }]),
        );
        let replacement = ClipRegion {
            id: "replacement".into(), source_file_id: Some("residual".into()),
            name: None, start_sample: 30, end_sample: 70,
            source_offset_sample: 0, gain_db: 0.0,
        };
        let clips = replace_range_with_clip(&session, &session.tracks[0], 30, 70, replacement).unwrap();
        assert_eq!(clips.len(), 3);
        assert_eq!((clips[0].start_sample, clips[0].end_sample, clips[0].source_offset_sample), (0, 30, 10));
        assert_eq!((clips[1].start_sample, clips[1].end_sample), (30, 70));
        assert_eq!((clips[2].start_sample, clips[2].end_sample, clips[2].source_offset_sample), (70, 100, 80));
    }

    #[test]
    fn raw_region_render_respects_timeline_and_clip_gain() {
        let token = Uuid::new_v4().to_string();
        let cache = std::env::temp_dir().join(format!("sam-split-{token}.cache"));
        let wav = std::env::temp_dir().join(format!("sam-split-{token}.wav"));
        let samples = vec![0.5_f32; 100];
        crate::engine::source::cache::write_cache(
            &cache,
            &crate::engine::source::cache::CacheHeader { channels: 1, sample_rate: 48000, frames: 100 },
            &samples,
        ).unwrap();
        let session = test_session(
            &cache,
            serde_json::json!([{
                "id": "clip", "sourceFileId": "source", "startSample": 20,
                "endSample": 80, "sourceOffsetSample": 10, "gainDb": -6.0206
            }]),
        );
        let peaks = render_raw_track_region(&session, &session.tracks[0], 0, 100, &wav).unwrap();
        let decoded = decode_file(&wav).unwrap();
        assert_eq!(decoded.samples.len(), 100);
        assert!(decoded.samples[..20].iter().all(|sample| sample.abs() < 1e-6));
        assert!((decoded.samples[30] - 0.25).abs() < 0.002);
        assert!(decoded.samples[80..].iter().all(|sample| sample.abs() < 1e-6));
        assert!(!peaks.is_empty());
        let _ = fs::remove_file(cache);
        let _ = fs::remove_file(wav);
    }

    #[test]
    fn accepted_split_is_one_undoable_history_entry() {
        let session = test_session(Path::new("unused"), serde_json::json!([]));
        let mut project = MixProject {
            session,
            history: Vec::new(),
            redo_stack: Vec::new(),
            chat_messages: Vec::new(),
        };
        let before_tracks = project.session.tracks.clone();
        let mut after_tracks = before_tracks.clone();
        let mut extracted = after_tracks[0].clone();
        extracted.id = "extracted".into();
        extracted.name = "Extracted - guitar".into();
        after_tracks.push(extracted);
        record_patch(
            &mut project,
            vec![JsonPatchOp {
                op: "replace".into(),
                path: "/tracks".into(),
                value: Some(serde_json::json!(after_tracks)),
            }],
            vec![JsonPatchOp {
                op: "replace".into(),
                path: "/tracks".into(),
                value: Some(serde_json::json!(before_tracks)),
            }],
            HistorySource::User,
            Some("Split track".into()),
        )
        .unwrap();
        assert_eq!(project.history.len(), 1);
        assert_eq!(project.session.tracks.len(), 2);
        crate::actions::undo(&mut project).unwrap();
        assert_eq!(project.session.tracks.len(), 1);
        crate::actions::redo(&mut project).unwrap();
        assert_eq!(project.session.tracks.len(), 2);
    }
}
