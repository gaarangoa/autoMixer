use std::{
    fs,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    actions::record_patch,
    defaults::{default_master, make_track},
    engine::source::{import_to_session_rate, write_to_cache, ImportedAudio},
    model::{ClipRegion, HistorySource, JsonPatchOp, MixProject, MixSession, SourceFile, TrackAnalysis, TrackKind, VideoClipRegion, VideoLayout, VideoSourceFile},
};

pub struct SessionStore {
    data_dir: PathBuf,
}

impl SessionStore {
    pub fn new(data_dir: PathBuf) -> Self {
        let store = Self { data_dir };
        let _ = store.init();
        store
    }

    pub fn init(&self) -> Result<(), String> {
        fs::create_dir_all(self.sessions_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.sources_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.peaks_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.videos_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.renders_dir()).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn create_session(&self, name: String) -> Result<MixProject, String> {
        self.init()?;
        let session = MixSession {
            id: Uuid::new_v4().to_string(),
            name,
            sample_rate: 48000,
            bpm: None,
            source_files: Vec::new(),
            video_source_files: Vec::new(),
            tracks: Vec::new(),
            buses: Vec::new(),
            master: default_master(),
            regions: Vec::new(),
            markers: Vec::new(),
            sections: Vec::new(),
            mixer_profile: crate::model::MixerProfile::default(),
            video_canvas: crate::model::VideoCanvas::default(),
        };
        let project = MixProject { session, history: Vec::new(), redo_stack: Vec::new(), chat_messages: Vec::new() };
        self.save(&project)?;
        Ok(project)
    }

    pub fn list_sessions(&self) -> Result<Vec<MixSession>, String> {
        self.init()?;
        let mut sessions = Vec::new();
        for entry in fs::read_dir(self.sessions_dir()).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().and_then(|item| item.to_str()) != Some("json") {
                continue;
            }
            let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
            let project: MixProject = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
            sessions.push(project.session);
        }
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(sessions)
    }

    pub fn get_project(&self, session_id: &str) -> Result<MixProject, String> {
        self.init()?;
        let path = self.sessions_dir().join(format!("{session_id}.json"));
        let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
        serde_json::from_str(&raw).map_err(|error| error.to_string())
    }

    pub fn save(&self, project: &MixProject) -> Result<(), String> {
        self.init()?;
        let path = self.sessions_dir().join(format!("{}.json", project.session.id));
        fs::write(path, serde_json::to_string_pretty(project).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }

    pub fn add_source_file(&self, session_id: &str, source_path: &Path) -> Result<MixProject, String> {
        self.add_source_file_at(session_id, source_path, 0)
    }

    pub fn add_source_file_at(&self, session_id: &str, source_path: &Path, start_sample: u64) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let session_rate = project.session.sample_rate;
        let (source, _imported) = self.import_source(source_path, session_rate)?;
        let source_id = source.id.clone();

        let track_name = strip_extension(&source.original_name);
        let mut track = make_track(source_id, track_name, project.session.tracks.len());
        track.start_sample = start_sample;
        project.session.source_files.push(source);
        project.session.tracks.push(track);
        self.save(&project)?;
        Ok(project)
    }

    pub fn create_recording_track(&self, session_id: &str, channels: u16) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let source = self.create_silent_source(project.session.sample_rate, "Recording", channels)?;
        let source_id = source.id.clone();
        let track_index = project.session.tracks.len();
        let label = if channels >= 2 { "Stereo Recording" } else { "Recording" };
        let track = make_track(source_id, format!("{} {}", label, track_index + 1), track_index);
        project.session.source_files.push(source);
        project.session.tracks.push(track);
        self.save(&project)?;
        Ok(project)
    }

    pub fn create_video_track(&self, session_id: &str) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let source = self.create_silent_source(project.session.sample_rate, "Video Placeholder", 1)?;
        let source_id = source.id.clone();
        let mut track = make_track(source_id, format!("Video {}", project.session.tracks.len() + 1), project.session.tracks.len());
        track.kind = TrackKind::Video;
        track.role = Some("video".into());
        track.solo = false;
        track.record_camera_audio = true;
        project.session.source_files.push(source);
        project.session.tracks.push(track);
        self.save(&project)?;
        Ok(project)
    }

    /// Create a new video track holding a video file rendered by the agent edit.
    /// The track carries a silent audio placeholder (the rendered mp4's audio is the
    /// baked mix, played only as a muted preview), so it does not double the engine output.
    pub fn add_rendered_video_track(
        &self,
        session_id: &str,
        video_path: &Path,
        name: String,
        start_sample: u64,
        duration_ms: u64,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let placeholder = self.create_silent_source(project.session.sample_rate, &name, 1)?;
        let placeholder_id = placeholder.id.clone();
        let mut track = make_track(placeholder_id, name.clone(), project.session.tracks.len());
        track.kind = TrackKind::Video;
        track.role = Some("video".into());
        track.solo = false;
        track.record_camera_audio = false;

        let source_id = Uuid::new_v4().to_string();
        let extension = video_path.extension().and_then(|item| item.to_str()).unwrap_or("mp4");
        let destination = self.videos_dir().join(format!("{source_id}.{extension}"));
        fs::copy(video_path, &destination)
            .map_err(|error| format!("Could not store rendered video: {error}"))?;
        let original_name = video_path
            .file_name()
            .and_then(|item| item.to_str())
            .map(|item| item.to_string())
            .unwrap_or_else(|| name.clone());
        let source = VideoSourceFile {
            id: source_id.clone(),
            original_name,
            path: destination.to_string_lossy().to_string(),
            mime_type: "video/mp4".into(),
            duration_ms,
        };
        let duration_samples = ((duration_ms as f64 / 1000.0) * project.session.sample_rate as f64).round() as u64;
        track.video_clips.push(VideoClipRegion {
            id: Uuid::new_v4().to_string(),
            video_source_file_id: source_id.clone(),
            name: Some(name.clone()),
            start_sample,
            end_sample: start_sample + duration_samples.max(1),
            source_offset_ms: 0,
            // The agent edit is a finished, full-frame composite — fill the canvas
            // rather than falling back to a small picture-in-picture default.
            layout: Some(VideoLayout::default()),
        });
        project.session.source_files.push(placeholder);
        project.session.video_source_files.push(source);
        project.session.tracks.push(track);
        self.save(&project)?;
        Ok(project)
    }

    /// Swap the video file behind an existing clip in place (keeps the clip id, start
    /// position and layout). Used to update an agent-edit track after a no-agent re-render.
    pub fn replace_track_video(
        &self,
        session_id: &str,
        track_id: &str,
        clip_id: &str,
        video_path: &Path,
        duration_ms: u64,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let sample_rate = project.session.sample_rate;
        let track = project
            .session
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        let clip = track
            .video_clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
            .ok_or_else(|| format!("Unknown video clip {clip_id}"))?;

        let source_id = Uuid::new_v4().to_string();
        let extension = video_path.extension().and_then(|item| item.to_str()).unwrap_or("mp4");
        let destination = self.videos_dir().join(format!("{source_id}.{extension}"));
        fs::copy(video_path, &destination)
            .map_err(|error| format!("Could not store rendered video: {error}"))?;

        let duration_samples = ((duration_ms as f64 / 1000.0) * sample_rate as f64).round() as u64;
        clip.video_source_file_id = source_id.clone();
        clip.source_offset_ms = 0;
        clip.end_sample = clip.start_sample + duration_samples.max(1);

        let original_name = video_path
            .file_name()
            .and_then(|item| item.to_str())
            .map(|item| item.to_string())
            .unwrap_or_else(|| "Agent Edit".to_string());
        project.session.video_source_files.push(VideoSourceFile {
            id: source_id,
            original_name,
            path: destination.to_string_lossy().to_string(),
            mime_type: "video/mp4".into(),
            duration_ms,
        });
        self.save(&project)?;
        Ok(project)
    }

    pub fn replace_track_audio(
        &self,
        session_id: &str,
        track_id: &str,
        source_path: &Path,
        start_sample: u64,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let session_rate = project.session.sample_rate;
        let (source, _imported) = self.import_source(source_path, session_rate)?;
        let source_id = source.id.clone();
        let track = project
            .session
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        track.source_file_id = source_id;
        track.start_sample = start_sample;
        track.name = strip_extension(&source.original_name);
        project.session.source_files.push(source);
        self.save(&project)?;
        Ok(project)
    }

    pub fn add_recording_clip(
        &self,
        session_id: &str,
        track_id: &str,
        source_path: &Path,
        start_sample: u64,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let session_rate = project.session.sample_rate;
        let (source, _imported) = self.import_source(source_path, session_rate)?;
        let source_id = source.id.clone();
        let duration = source.duration_samples;
        let clip_name = strip_extension(&source.original_name);
        let existing_source = project
            .session
            .source_files
            .iter()
            .find(|source| source.id == project.session.tracks.iter().find(|track| track.id == track_id).map(|track| track.source_file_id.as_str()).unwrap_or(""))
            .cloned();
        let track = project
            .session
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        // Latency compensation: shift the new clip earlier by the configured ms so the
        // recorded transient lands where it was actually played, not when the buffer
        // arrived. Positive ms => earlier; negative => later.
        let offset_samples = (track.input_latency_ms as f64 * session_rate as f64 / 1000.0).round() as i64;
        let adjusted_start = if offset_samples >= 0 {
            start_sample.saturating_sub(offset_samples as u64)
        } else {
            start_sample.saturating_add((-offset_samples) as u64)
        };
        if track.clips.is_empty() {
            if let Some(existing) = existing_source.as_ref().filter(|source| source.original_name != "Recording") {
                track.clips.push(ClipRegion {
                    id: Uuid::new_v4().to_string(),
                    source_file_id: Some(existing.id.clone()),
                    name: Some(strip_extension(&existing.original_name)),
                    start_sample: track.start_sample,
                    end_sample: track.start_sample + existing.duration_samples,
                    source_offset_sample: 0,
                    gain_db: 0.0,
                });
            }
        }
        track.clips.push(ClipRegion {
            id: Uuid::new_v4().to_string(),
            source_file_id: Some(source_id.clone()),
            name: Some(clip_name),
            start_sample: adjusted_start,
            end_sample: adjusted_start + duration,
            source_offset_sample: 0,
            gain_db: 0.0,
        });
        project.session.source_files.push(source);
        self.save(&project)?;
        Ok(project)
    }

    pub fn add_video_recording_clip(
        &self,
        session_id: &str,
        track_id: &str,
        video_path: &Path,
        original_name: String,
        mime_type: String,
        start_sample: u64,
        duration_ms: u64,
        source_offset_ms: u64,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let track = project
            .session
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        if track.kind != TrackKind::Video {
            return Err("Record video into a video track.".into());
        }
        let source_id = Uuid::new_v4().to_string();
        let extension = video_path.extension().and_then(|item| item.to_str()).unwrap_or("webm");
        let destination = self.videos_dir().join(format!("{source_id}.{extension}"));
        fs::copy(video_path, &destination)
            .map_err(|error| format!("Could not save video recording: {error}"))?;
        let source = VideoSourceFile {
            id: source_id.clone(),
            original_name: original_name.clone(),
            path: destination.to_string_lossy().to_string(),
            mime_type,
            duration_ms,
        };
        let playable_ms = duration_ms.saturating_sub(source_offset_ms).max(1);
        let duration_samples = ((playable_ms as f64 / 1000.0) * project.session.sample_rate as f64).round() as u64;
        // Latency compensation (see add_recording_clip).
        let offset_samples = (track.input_latency_ms as f64 * project.session.sample_rate as f64 / 1000.0).round() as i64;
        let adjusted_start = if offset_samples >= 0 {
            start_sample.saturating_sub(offset_samples as u64)
        } else {
            start_sample.saturating_add((-offset_samples) as u64)
        };
        track.video_clips.push(VideoClipRegion {
            id: Uuid::new_v4().to_string(),
            video_source_file_id: source_id.clone(),
            name: Some(strip_extension(&original_name)),
            start_sample: adjusted_start,
            end_sample: adjusted_start + duration_samples.max(1),
            source_offset_ms,
            layout: None,
        });
        project.session.video_source_files.push(source);
        self.save(&project)?;
        Ok(project)
    }

    pub fn delete_clip(&self, session_id: &str, track_id: &str, clip_id: &str) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let track_index = project
            .session
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        let track = &mut project.session.tracks[track_index];
        let before_clips = track.clips.clone();
        let before = track.clips.len();
        track.clips.retain(|clip| clip.id != clip_id);
        if track.clips.len() == before {
            return Err(format!("Unknown clip {clip_id}"));
        }
        let after_clips = track.clips.clone();
        project.session.tracks[track_index].clips = before_clips.clone();
        record_patch(
            &mut project,
            vec![JsonPatchOp {
                op: "replace".into(),
                path: format!("/tracks/{track_index}/clips"),
                value: Some(serde_json::json!(after_clips)),
            }],
            vec![JsonPatchOp {
                op: "replace".into(),
                path: format!("/tracks/{track_index}/clips"),
                value: Some(serde_json::json!(before_clips)),
            }],
            HistorySource::User,
            Some("Deleted recording clip".into()),
        )?;
        self.save(&project)?;
        Ok(project)
    }

    pub fn delete_clip_range(
        &self,
        session_id: &str,
        track_id: &str,
        start_sample: u64,
        end_sample: u64,
    ) -> Result<MixProject, String> {
        if end_sample <= start_sample {
            return Err("Selection end must be after selection start.".to_string());
        }
        let mut project = self.get_project(session_id)?;
        let track_index = project
            .session
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        if project.session.tracks[track_index].kind == crate::model::TrackKind::Video {
            return self.delete_video_clip_range(session_id, track_id, start_sample, end_sample);
        }
        if project.session.tracks[track_index].clips.is_empty() {
            let source_id = project.session.tracks[track_index].source_file_id.clone();
            let source = project
                .session
                .source_files
                .iter()
                .find(|source| source.id == source_id)
                .ok_or_else(|| format!("Unknown source {source_id}"))?;
            let track = &mut project.session.tracks[track_index];
            track.clips.push(ClipRegion {
                id: Uuid::new_v4().to_string(),
                source_file_id: Some(source.id.clone()),
                name: Some(track.name.clone()),
                start_sample: track.start_sample,
                end_sample: track.start_sample + source.duration_samples,
                source_offset_sample: 0,
                gain_db: 0.0,
            });
        }
        let track = &mut project.session.tracks[track_index];
        let before_clips = track.clips.clone();
        let mut changed = false;
        let mut next = Vec::with_capacity(track.clips.len());
        for clip in track.clips.drain(..) {
            let clip_start = clip.start_sample;
            let clip_end = clip.end_sample;
            if end_sample <= clip_start || start_sample >= clip_end {
                next.push(clip);
                continue;
            }
            changed = true;
            if start_sample > clip_start {
                let mut left = clip.clone();
                left.end_sample = start_sample.min(clip_end);
                next.push(left);
            }
            if end_sample < clip_end {
                let mut right = clip;
                right.id = Uuid::new_v4().to_string();
                right.start_sample = end_sample;
                right.source_offset_sample = right
                    .source_offset_sample
                    .saturating_add(end_sample.saturating_sub(clip_start));
                next.push(right);
            }
        }
        if !changed {
            return Err("No recorded clip audio in selected range.".to_string());
        }
        next.sort_by_key(|clip| (clip.start_sample, clip.end_sample));
        track.clips = next;
        let after_clips = track.clips.clone();
        project.session.tracks[track_index].clips = before_clips.clone();
        record_patch(
            &mut project,
            vec![JsonPatchOp {
                op: "replace".into(),
                path: format!("/tracks/{track_index}/clips"),
                value: Some(serde_json::json!(after_clips)),
            }],
            vec![JsonPatchOp {
                op: "replace".into(),
                path: format!("/tracks/{track_index}/clips"),
                value: Some(serde_json::json!(before_clips)),
            }],
            HistorySource::User,
            Some("Deleted selected track range".into()),
        )?;
        self.save(&project)?;
        Ok(project)
    }

    fn delete_video_clip_range(
        &self,
        session_id: &str,
        track_id: &str,
        start_sample: u64,
        end_sample: u64,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let track_index = project
            .session
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        let track = &mut project.session.tracks[track_index];
        let before_clips = track.video_clips.clone();
        let mut changed = false;
        let mut next = Vec::with_capacity(track.video_clips.len());
        for clip in track.video_clips.drain(..) {
            let clip_start = clip.start_sample;
            let clip_end = clip.end_sample;
            if end_sample <= clip_start || start_sample >= clip_end {
                next.push(clip);
                continue;
            }
            changed = true;
            if start_sample > clip_start {
                let mut left = clip.clone();
                left.end_sample = start_sample.min(clip_end);
                next.push(left);
            }
            if end_sample < clip_end {
                let mut right = clip;
                right.id = Uuid::new_v4().to_string();
                right.start_sample = end_sample;
                right.source_offset_ms = right
                    .source_offset_ms
                    .saturating_add(((end_sample.saturating_sub(clip_start) as f64 / project.session.sample_rate as f64) * 1000.0).round() as u64);
                next.push(right);
            }
        }
        if !changed {
            return Err("No recorded video clip in selected range.".to_string());
        }
        track.video_clips = next;
        let after_clips = track.video_clips.clone();
        project.session.tracks[track_index].video_clips = before_clips.clone();
        record_patch(
            &mut project,
            vec![JsonPatchOp {
                op: "replace".into(),
                path: format!("/tracks/{track_index}/videoClips"),
                value: Some(serde_json::json!(after_clips)),
            }],
            vec![JsonPatchOp {
                op: "replace".into(),
                path: format!("/tracks/{track_index}/videoClips"),
                value: Some(serde_json::json!(before_clips)),
            }],
            HistorySource::User,
            Some("Deleted selected video range".into()),
        )?;
        self.save(&project)?;
        Ok(project)
    }

    fn import_source(&self, source_path: &Path, session_rate: u32) -> Result<(SourceFile, ImportedAudio), String> {
        let source_id = Uuid::new_v4().to_string();
        let original_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("audio")
            .to_string();

        let imported = import_to_session_rate(source_path, session_rate)
            .map_err(|e| format!("import {original_name}: {e}"))?;
        let cache_path = self.sources_dir().join(format!("{source_id}.f32cache"));
        write_to_cache(&cache_path, &imported)?;

        let peak_path = self.peaks_dir().join(format!("{source_id}.peaks.json"));
        fs::write(
            &peak_path,
            serde_json::to_string(&imported.peaks).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let analysis = analyze_imported(&imported);
        let peak_preview = imported.peaks.preview.clone();

        let source = SourceFile {
            id: source_id.clone(),
            original_name: original_name.clone(),
            cache_path: cache_path.to_string_lossy().to_string(),
            peak_path: peak_path.to_string_lossy().to_string(),
            duration_samples: imported.frames,
            sample_rate: imported.sample_rate,
            channels: imported.channels,
            analysis,
            peak_preview,
        };
        Ok((source, imported))
    }

    pub fn rename_session(&self, session_id: &str, new_name: String) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        project.session.name = new_name;
        self.save(&project)?;
        Ok(project)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let path = self.sessions_dir().join(format!("{session_id}.json"));
        if path.exists() {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Write a self-contained bundle directory: project.json + audio cache +
    /// peak files. Source paths inside the bundle are relative so the
    /// directory can be moved or copied between machines.
    pub fn export_project_bundle(&self, session_id: &str, bundle_dir: &Path) -> Result<(), String> {
        let project = self.get_project(session_id)?;
        fs::create_dir_all(bundle_dir.join("sources")).map_err(|error| error.to_string())?;
        fs::create_dir_all(bundle_dir.join("peaks")).map_err(|error| error.to_string())?;

        let mut bundled = project.clone();
        for src in &mut bundled.session.source_files {
            let cache_src = PathBuf::from(&src.cache_path);
            let cache_rel = format!("sources/{}.f32cache", src.id);
            let cache_dst = bundle_dir.join(&cache_rel);
            fs::copy(&cache_src, &cache_dst)
                .map_err(|e| format!("copy cache for {}: {e}", src.original_name))?;
            src.cache_path = cache_rel;

            let peak_src = PathBuf::from(&src.peak_path);
            let peak_rel = format!("peaks/{}.peaks.json", src.id);
            let peak_dst = bundle_dir.join(&peak_rel);
            fs::copy(&peak_src, &peak_dst)
                .map_err(|e| format!("copy peaks for {}: {e}", src.original_name))?;
            src.peak_path = peak_rel;
        }
        fs::create_dir_all(bundle_dir.join("videos")).map_err(|error| error.to_string())?;
        for src in &mut bundled.session.video_source_files {
            let video_src = PathBuf::from(&src.path);
            let extension = video_src.extension().and_then(|item| item.to_str()).unwrap_or("webm");
            let video_rel = format!("videos/{}.{}", src.id, extension);
            let video_dst = bundle_dir.join(&video_rel);
            fs::copy(&video_src, &video_dst)
                .map_err(|e| format!("copy video for {}: {e}", src.original_name))?;
            src.path = video_rel;
        }

        let manifest = serde_json::json!({
            "version": 1,
            "appName": "AutoMixer",
            "sessionId": bundled.session.id,
            "sessionName": bundled.session.name,
        });
        fs::write(
            bundle_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            bundle_dir.join("project.json"),
            serde_json::to_string_pretty(&bundled).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Import a bundle directory into the app's data directory and register
    /// the session under a fresh id (so re-imports don't overwrite prior copies).
    pub fn import_project_bundle(&self, bundle_dir: &Path) -> Result<MixProject, String> {
        self.init()?;
        let project_path = bundle_dir.join("project.json");
        let raw = fs::read_to_string(&project_path).map_err(|e| {
            format!("Could not read {} (not a project bundle?): {e}", project_path.display())
        })?;
        let mut project: MixProject = serde_json::from_str(&raw).map_err(|error| error.to_string())?;

        project.session.id = Uuid::new_v4().to_string();

        for src in &mut project.session.source_files {
            let cache_src = bundle_dir.join(&src.cache_path);
            let cache_dst = self.sources_dir().join(format!("{}.f32cache", src.id));
            fs::copy(&cache_src, &cache_dst)
                .map_err(|e| format!("import cache for {}: {e}", src.original_name))?;
            src.cache_path = cache_dst.to_string_lossy().to_string();

            let peak_src = bundle_dir.join(&src.peak_path);
            let peak_dst = self.peaks_dir().join(format!("{}.peaks.json", src.id));
            fs::copy(&peak_src, &peak_dst)
                .map_err(|e| format!("import peaks for {}: {e}", src.original_name))?;
            src.peak_path = peak_dst.to_string_lossy().to_string();
        }
        for src in &mut project.session.video_source_files {
            let video_src = bundle_dir.join(&src.path);
            let extension = video_src.extension().and_then(|item| item.to_str()).unwrap_or("webm");
            let video_dst = self.videos_dir().join(format!("{}.{}", src.id, extension));
            fs::copy(&video_src, &video_dst)
                .map_err(|e| format!("import video for {}: {e}", src.original_name))?;
            src.path = video_dst.to_string_lossy().to_string();
        }

        self.save(&project)?;
        Ok(project)
    }

    pub fn renders_dir(&self) -> PathBuf {
        self.data_dir.join("renders")
    }

    pub fn videos_dir(&self) -> PathBuf {
        self.data_dir.join("recordings")
    }

    fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    fn sources_dir(&self) -> PathBuf {
        self.data_dir.join("sources")
    }

    fn peaks_dir(&self) -> PathBuf {
        self.data_dir.join("peaks")
    }

    fn create_silent_source(&self, sample_rate: u32, original_name: &str, channels: u16) -> Result<SourceFile, String> {
        let source_id = Uuid::new_v4().to_string();
        let frames = sample_rate as u64;
        let channels = channels.max(1).min(2);
        let samples = vec![0.0_f32; frames as usize * channels as usize];
        let cache_path = self.sources_dir().join(format!("{source_id}.f32cache"));
        crate::engine::source::cache::write_cache(
            &cache_path,
            &crate::engine::source::cache::CacheHeader {
                channels,
                sample_rate,
                frames,
            },
            &samples,
        )?;
        let peaks = crate::engine::source::peaks::build_peaks(&samples, channels, sample_rate);
        let peak_path = self.peaks_dir().join(format!("{source_id}.peaks.json"));
        fs::write(
            &peak_path,
            serde_json::to_string(&peaks).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(SourceFile {
            id: source_id,
            original_name: original_name.into(),
            cache_path: cache_path.to_string_lossy().to_string(),
            peak_path: peak_path.to_string_lossy().to_string(),
            duration_samples: frames,
            sample_rate,
            channels,
            analysis: analyze_samples(&samples, channels, sample_rate),
            peak_preview: peaks.preview,
        })
    }
}

fn analyze_imported(imported: &ImportedAudio) -> TrackAnalysis {
    analyze_samples(&imported.samples, imported.channels, imported.sample_rate)
}

fn analyze_samples(samples: &[f32], channels: u16, sample_rate: u32) -> TrackAnalysis {
    let a = crate::engine::source::analysis::analyze(
        samples,
        channels,
        sample_rate,
    );
    TrackAnalysis {
        peak_db: a.peak_db,
        rms_db: a.rms_db,
        lufs_estimate: a.lufs,
        spectral_centroid_hz: a.spectral_centroid_hz,
        low_energy: a.low_energy,
        mid_energy: a.mid_energy,
        high_energy: a.high_energy,
        silence_percent: a.silence_percent,
        dynamic_range_db: a.dynamic_range_db,
    }
}

fn strip_extension(name: &str) -> String {
    name.rsplit_once('.').map(|(base, _)| base).unwrap_or(name).to_string()
}
