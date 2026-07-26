use std::{
    fs,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    actions::record_patch,
    defaults::{default_master, make_track},
    engine::source::{import_to_session_rate, write_to_cache, ImportedAudio},
    model::{
        ClipRegion, HistorySource, JsonPatchOp, MixAlbum, MixProject, MixSession, SourceFile,
        TrackAnalysis, TrackKind, VideoClipRegion, VideoLayout, VideoSourceFile,
    },
};

pub(crate) const SCRATCH_SESSION_MINIMUM_TIMELINE_SECONDS: f64 = 180.0;
const ALBUM_MANIFEST: &str = "album.json";
const SONG_MANIFEST: &str = "song.json";
const AUDIO_DIR: &str = "Audio";
const PEAKS_DIR: &str = "Peaks";
const RECORDINGS_DIR: &str = "Recordings";
const VIDEO_DIR: &str = "Video";
const RENDERS_DIR: &str = "Renders";

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

fn unique_named_dir(parent: &Path, name: &str) -> PathBuf {
    let base = sanitize_name(name);
    let mut candidate = parent.join(&base);
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{base} {n}"));
        n += 1;
    }
    candidate
}

/// Given an album folder, a song folder, or a manifest inside either one,
/// return the directory containing album.json.
fn resolve_album_dir(path: &Path) -> Option<PathBuf> {
    let mut dir = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path.to_path_buf()
    };
    for _ in 0..3 {
        if dir.join(ALBUM_MANIFEST).is_file() {
            return Some(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    None
}

pub struct SessionStore {
    data_dir: PathBuf,
    /// Zero or one explicitly opened album. This is intentionally not persisted:
    /// AutoMixer starts without a library and waits for the user to open a folder.
    album_index: std::sync::Mutex<std::collections::HashMap<String, PathBuf>>,
}

impl SessionStore {
    pub fn new(data_dir: PathBuf) -> Self {
        let store = Self {
            data_dir,
            album_index: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        let _ = store.init();
        store
    }

    pub fn init(&self) -> Result<(), String> {
        fs::create_dir_all(&self.data_dir).map_err(|error| error.to_string())
    }

    /// Create a task-safe store handle that knows only about the album currently
    /// open in this process. This is deliberately ephemeral and never persisted.
    pub fn clone_open_handle(&self) -> Result<Self, String> {
        let album_index = self
            .album_index
            .lock()
            .map_err(|error| error.to_string())?
            .clone();
        Ok(Self {
            data_dir: self.data_dir.clone(),
            album_index: std::sync::Mutex::new(album_index),
        })
    }

    pub fn create_session(&self, album_id: &str, name: String) -> Result<MixProject, String> {
        let album_dir = self
            .album_dir(album_id)
            .ok_or_else(|| "Open or create an album before creating a song.".to_string())?;
        let song_dir = unique_named_dir(&album_dir, &name);
        self.create_song_layout(&song_dir)?;
        let session = MixSession {
            id: Uuid::new_v4().to_string(),
            name,
            album_id: album_id.to_string(),
            sample_rate: 48000,
            minimum_timeline_seconds: Some(SCRATCH_SESSION_MINIMUM_TIMELINE_SECONDS),
            tempo_percent: 100.0,
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
        let project = MixProject {
            session,
            history: Vec::new(),
            redo_stack: Vec::new(),
            chat_messages: Vec::new(),
        };
        fs::write(
            song_dir.join(SONG_MANIFEST),
            serde_json::to_string_pretty(&project).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        self.add_song_to_album(album_id, &project.session.id)?;
        Ok(project)
    }

    pub fn list_all_sessions(&self) -> Result<Vec<MixSession>, String> {
        let Some(album) = self.list_albums()?.into_iter().next() else {
            return Ok(Vec::new());
        };
        self.list_sessions(&album.id)
    }

    pub fn list_sessions(&self, album_id: &str) -> Result<Vec<MixSession>, String> {
        let album = self.get_album(album_id)?;
        let album_dir = self
            .album_dir(album_id)
            .ok_or_else(|| format!("Album {album_id} is not open"))?;
        let mut by_id: std::collections::HashMap<String, MixSession> =
            std::collections::HashMap::new();
        for path in self.song_manifest_paths(&album_dir)? {
            if let Ok(raw) = fs::read_to_string(path) {
                if let Ok(project) = serde_json::from_str::<MixProject>(&raw) {
                    by_id.insert(project.session.id.clone(), project.session);
                }
            }
        }
        let mut sessions = Vec::new();
        for id in &album.song_order {
            if let Some(session) = by_id.remove(id) {
                sessions.push(session);
            }
        }
        let mut rest: Vec<MixSession> = by_id.into_values().collect();
        rest.sort_by(|a, b| a.name.cmp(&b.name));
        sessions.extend(rest);
        Ok(sessions)
    }

    pub fn get_project(&self, session_id: &str) -> Result<MixProject, String> {
        let path = self
            .locate_session_file(session_id)
            .ok_or_else(|| format!("Session {session_id} not found in the open album"))?;
        let raw = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let mut project: MixProject =
            serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        if let Some(song_dir) = path.parent() {
            Self::resolve_project_paths(&mut project, song_dir);
        }
        Ok(project)
    }

    pub fn save(&self, project: &MixProject) -> Result<(), String> {
        let path = self
            .locate_session_file(&project.session.id)
            .ok_or_else(|| format!("Session {} not found in the open album", project.session.id))?;
        let song_dir = path
            .parent()
            .ok_or_else(|| "Song project has no parent directory.".to_string())?;
        let mut portable = project.clone();
        Self::make_project_paths_relative(&mut portable, song_dir);
        fs::write(
            path,
            serde_json::to_string_pretty(&portable).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    fn album_dir(&self, album_id: &str) -> Option<PathBuf> {
        self.album_index.lock().ok()?.get(album_id).cloned()
    }

    fn register_album(&self, id: &str, dir: &Path) {
        if let Ok(mut index) = self.album_index.lock() {
            index.clear();
            index.insert(id.to_string(), dir.to_path_buf());
        }
    }

    fn album_manifest_path(&self, album_id: &str) -> Option<PathBuf> {
        self.album_dir(album_id).map(|dir| dir.join(ALBUM_MANIFEST))
    }

    fn song_manifest_paths(&self, album_dir: &Path) -> Result<Vec<PathBuf>, String> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(album_dir).map_err(|error| error.to_string())? {
            let child = entry.map_err(|error| error.to_string())?.path();
            let manifest = child.join(SONG_MANIFEST);
            if child.is_dir() && manifest.is_file() {
                paths.push(manifest);
            }
        }
        // Read-only compatibility with the former `songs/<id>.json` layout.
        let legacy = album_dir.join("songs");
        if legacy.is_dir() {
            for entry in fs::read_dir(legacy).map_err(|error| error.to_string())? {
                let path = entry.map_err(|error| error.to_string())?.path();
                if path.extension().and_then(|item| item.to_str()) == Some("json") {
                    paths.push(path);
                }
            }
        }
        Ok(paths)
    }

    fn locate_session_file(&self, session_id: &str) -> Option<PathBuf> {
        let dirs: Vec<PathBuf> = self.album_index.lock().ok()?.values().cloned().collect();
        for dir in dirs {
            for candidate in self.song_manifest_paths(&dir).ok()? {
                if let Ok(raw) = fs::read_to_string(&candidate) {
                    if let Ok(project) = serde_json::from_str::<MixProject>(&raw) {
                        if project.session.id == session_id {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
        None
    }

    /// Returns only the album explicitly opened in this process.
    pub fn list_albums(&self) -> Result<Vec<MixAlbum>, String> {
        let ids: Vec<String> = self
            .album_index
            .lock()
            .map_err(|error| error.to_string())?
            .keys()
            .cloned()
            .collect();
        let mut albums = Vec::new();
        for id in ids {
            albums.push(self.get_album(&id)?);
        }
        Ok(albums)
    }

    pub fn get_album(&self, album_id: &str) -> Result<MixAlbum, String> {
        let manifest = self
            .album_manifest_path(album_id)
            .ok_or_else(|| format!("Album {album_id} is not open"))?;
        let raw = fs::read_to_string(manifest)
            .map_err(|_| format!("Album {album_id} not found on disk"))?;
        serde_json::from_str(&raw).map_err(|error| error.to_string())
    }

    fn save_album(&self, album: &MixAlbum) -> Result<(), String> {
        let dir = self
            .album_dir(&album.id)
            .ok_or_else(|| format!("Album {} is not open", album.id))?;
        fs::write(
            dir.join(ALBUM_MANIFEST),
            serde_json::to_string_pretty(album).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    pub fn create_album_in(&self, parent_dir: &Path, name: String) -> Result<MixAlbum, String> {
        fs::create_dir_all(parent_dir).map_err(|error| error.to_string())?;
        let folder = unique_named_dir(parent_dir, &name);
        fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let album = MixAlbum {
            id: Uuid::new_v4().to_string(),
            name,
            song_order: Vec::new(),
        };
        self.register_album(&album.id, &folder);
        self.save_album(&album)?;
        Ok(album)
    }

    pub fn open_album(&self, path: &Path) -> Result<MixAlbum, String> {
        let dir = resolve_album_dir(path)
            .ok_or_else(|| format!("No album.json found at {}", path.display()))?;
        let raw =
            fs::read_to_string(dir.join(ALBUM_MANIFEST)).map_err(|error| error.to_string())?;
        let album: MixAlbum = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        self.register_album(&album.id, &dir);
        Ok(album)
    }

    pub fn rename_album(&self, album_id: &str, new_name: String) -> Result<MixAlbum, String> {
        let mut album = self.get_album(album_id)?;
        let old_dir = self
            .album_dir(album_id)
            .ok_or_else(|| format!("Album {album_id} is not open"))?;
        let parent = old_dir
            .parent()
            .ok_or_else(|| "Album folder has no parent directory.".to_string())?;
        let desired = parent.join(sanitize_name(&new_name));
        let new_dir = if desired == old_dir {
            old_dir.clone()
        } else if desired.exists() {
            unique_named_dir(parent, &new_name)
        } else {
            desired
        };
        if new_dir != old_dir {
            fs::rename(&old_dir, &new_dir)
                .map_err(|error| format!("Could not rename album folder: {error}"))?;
            self.register_album(album_id, &new_dir);
        }
        album.name = new_name;
        self.save_album(&album)?;
        Ok(album)
    }

    pub fn close_album(&self, album_id: &str) -> Result<(), String> {
        if let Ok(mut index) = self.album_index.lock() {
            index.remove(album_id);
        }
        Ok(())
    }

    fn add_song_to_album(&self, album_id: &str, session_id: &str) -> Result<(), String> {
        let mut album = self.get_album(album_id)?;
        if !album.song_order.iter().any(|id| id == session_id) {
            album.song_order.push(session_id.to_string());
        }
        self.save_album(&album)
    }

    fn create_song_layout(&self, song_dir: &Path) -> Result<(), String> {
        for child in [AUDIO_DIR, PEAKS_DIR, RECORDINGS_DIR, VIDEO_DIR, RENDERS_DIR] {
            fs::create_dir_all(song_dir.join(child)).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn resolve_project_paths(project: &mut MixProject, song_dir: &Path) {
        for source in &mut project.session.source_files {
            let cache = PathBuf::from(&source.cache_path);
            if cache.is_relative() {
                source.cache_path = song_dir.join(cache).to_string_lossy().to_string();
            }
            let peaks = PathBuf::from(&source.peak_path);
            if peaks.is_relative() {
                source.peak_path = song_dir.join(peaks).to_string_lossy().to_string();
            }
        }
        for source in &mut project.session.video_source_files {
            let path = PathBuf::from(&source.path);
            if path.is_relative() {
                source.path = song_dir.join(path).to_string_lossy().to_string();
            }
        }
    }

    fn make_project_paths_relative(project: &mut MixProject, song_dir: &Path) {
        for source in &mut project.session.source_files {
            source.cache_path = Self::relative_if_inside(&source.cache_path, song_dir);
            source.peak_path = Self::relative_if_inside(&source.peak_path, song_dir);
        }
        for source in &mut project.session.video_source_files {
            source.path = Self::relative_if_inside(&source.path, song_dir);
        }
    }

    fn relative_if_inside(value: &str, song_dir: &Path) -> String {
        match Path::new(value).strip_prefix(song_dir) {
            Ok(relative) => relative.to_string_lossy().to_string(),
            Err(_) => value.to_string(),
        }
    }

    fn rebase_project_paths(project: &mut MixProject, old_dir: &Path, new_dir: &Path) {
        for source in &mut project.session.source_files {
            source.cache_path = Self::rebase_path(&source.cache_path, old_dir, new_dir);
            source.peak_path = Self::rebase_path(&source.peak_path, old_dir, new_dir);
        }
        for source in &mut project.session.video_source_files {
            source.path = Self::rebase_path(&source.path, old_dir, new_dir);
        }
    }

    fn rebase_path(value: &str, old_dir: &Path, new_dir: &Path) -> String {
        match Path::new(value).strip_prefix(old_dir) {
            Ok(relative) => new_dir.join(relative).to_string_lossy().to_string(),
            Err(_) => value.to_string(),
        }
    }

    pub fn song_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        self.locate_session_file(session_id)
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .ok_or_else(|| format!("Session {session_id} not found in the open album"))
    }

    fn audio_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        Ok(self.song_dir(session_id)?.join(AUDIO_DIR))
    }

    fn peaks_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        Ok(self.song_dir(session_id)?.join(PEAKS_DIR))
    }

    pub fn recordings_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        Ok(self.song_dir(session_id)?.join(RECORDINGS_DIR))
    }

    pub fn videos_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        Ok(self.song_dir(session_id)?.join(VIDEO_DIR))
    }

    pub fn renders_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        Ok(self.song_dir(session_id)?.join(RENDERS_DIR))
    }

    pub fn add_source_file(
        &self,
        session_id: &str,
        source_path: &Path,
    ) -> Result<MixProject, String> {
        self.add_source_file_at(session_id, source_path, 0)
    }

    /// Import a wav into the cache (analysis + peaks) and return the SourceFile
    /// WITHOUT touching any session — used by transforms (e.g. tempo stretch) that
    /// register the file themselves.
    pub fn import_source_standalone(
        &self,
        session_id: &str,
        source_path: &Path,
        session_rate: u32,
    ) -> Result<SourceFile, String> {
        let (source, _imported) = self.import_source(session_id, source_path, session_rate)?;
        Ok(source)
    }

    pub fn add_source_file_at(
        &self,
        session_id: &str,
        source_path: &Path,
        start_sample: u64,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let session_rate = project.session.sample_rate;
        let (source, _imported) = self.import_source(session_id, source_path, session_rate)?;
        let source_id = source.id.clone();

        let track_name = strip_extension(&source.original_name);
        let mut track = make_track(source_id, track_name, project.session.tracks.len());
        track.start_sample = start_sample;
        project.session.minimum_timeline_seconds = None;
        project.session.source_files.push(source);
        project.session.tracks.push(track);
        self.save(&project)?;
        Ok(project)
    }

    pub fn create_recording_track(
        &self,
        session_id: &str,
        channels: u16,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        if project.session.minimum_timeline_seconds.is_none()
            && project.session.tracks.is_empty()
            && project.session.source_files.is_empty()
            && project.session.video_source_files.is_empty()
        {
            project.session.minimum_timeline_seconds =
                Some(SCRATCH_SESSION_MINIMUM_TIMELINE_SECONDS);
        }
        let source = self.create_silent_source(
            session_id,
            project.session.sample_rate,
            "Recording",
            channels,
        )?;
        let source_id = source.id.clone();
        let track_index = project.session.tracks.len();
        let label = if channels >= 2 {
            "Stereo Recording"
        } else {
            "Recording"
        };
        let track = make_track(
            source_id,
            format!("{} {}", label, track_index + 1),
            track_index,
        );
        project.session.source_files.push(source);
        project.session.tracks.push(track);
        self.save(&project)?;
        Ok(project)
    }

    pub fn create_video_track(&self, session_id: &str) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        if project.session.minimum_timeline_seconds.is_none()
            && project.session.tracks.is_empty()
            && project.session.source_files.is_empty()
            && project.session.video_source_files.is_empty()
        {
            project.session.minimum_timeline_seconds =
                Some(SCRATCH_SESSION_MINIMUM_TIMELINE_SECONDS);
        }
        let source = self.create_silent_source(
            session_id,
            project.session.sample_rate,
            "Video Placeholder",
            1,
        )?;
        let source_id = source.id.clone();
        let mut track = make_track(
            source_id,
            format!("Video {}", project.session.tracks.len() + 1),
            project.session.tracks.len(),
        );
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
        let placeholder =
            self.create_silent_source(session_id, project.session.sample_rate, &name, 1)?;
        let placeholder_id = placeholder.id.clone();
        let mut track = make_track(placeholder_id, name.clone(), project.session.tracks.len());
        track.kind = TrackKind::Video;
        track.role = Some("video".into());
        track.solo = false;
        track.record_camera_audio = false;

        let source_id = Uuid::new_v4().to_string();
        let extension = video_path
            .extension()
            .and_then(|item| item.to_str())
            .unwrap_or("mp4");
        let destination = self
            .videos_dir(session_id)?
            .join(format!("{source_id}.{extension}"));
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
        let duration_samples =
            ((duration_ms as f64 / 1000.0) * project.session.sample_rate as f64).round() as u64;
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
            pristine_video_source_file_id: None,
            pristine_source_offset_ms: None,
            pristine_duration_samples: None,
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
        let extension = video_path
            .extension()
            .and_then(|item| item.to_str())
            .unwrap_or("mp4");
        let destination = self
            .videos_dir(session_id)?
            .join(format!("{source_id}.{extension}"));
        fs::copy(video_path, &destination)
            .map_err(|error| format!("Could not store rendered video: {error}"))?;

        let duration_samples = ((duration_ms as f64 / 1000.0) * sample_rate as f64).round() as u64;
        // Snapshot the pre-edit source as the pristine on the FIRST replace; later
        // replaces leave the original snapshot alone so a revert always lands back
        // on the raw recording, not on a previously graded version.
        if clip.pristine_video_source_file_id.is_none() {
            clip.pristine_video_source_file_id = Some(clip.video_source_file_id.clone());
            clip.pristine_source_offset_ms = Some(clip.source_offset_ms);
            clip.pristine_duration_samples =
                Some(clip.end_sample.saturating_sub(clip.start_sample));
        }
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

    /// Add — or, if one already exists, replace in place — the single canonical
    /// "Agent video edit" output track. Every agent `edit_video` call used to push a
    /// brand-new track, so re-running stacked identical copies; this upserts instead.
    /// Existing agent-edit tracks are detected by name (the canonical name or the
    /// legacy "Agent Edit N" the manual button produced); the first is reused and any
    /// extras are removed so the session converges to exactly one agent-edit lane.
    pub fn upsert_agent_video_track(
        &self,
        session_id: &str,
        video_path: &Path,
        start_sample: u64,
        duration_ms: u64,
    ) -> Result<MixProject, String> {
        const AGENT_EDIT_NAME: &str = "Agent video edit";
        let is_agent_edit = |t: &crate::model::Track| {
            t.kind == TrackKind::Video
                && (t.name == AGENT_EDIT_NAME || t.name.starts_with("Agent Edit"))
        };

        let mut project = self.get_project(session_id)?;
        let sample_rate = project.session.sample_rate;
        let agent_indices: Vec<usize> = project
            .session
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| is_agent_edit(t))
            .map(|(i, _)| i)
            .collect();

        let Some(&keep) = agent_indices.first() else {
            // No agent-edit track yet — create the canonical one.
            return self.add_rendered_video_track(
                session_id,
                video_path,
                AGENT_EDIT_NAME.to_string(),
                start_sample,
                duration_ms,
            );
        };

        // Stage the new render into the videos dir.
        let source_id = Uuid::new_v4().to_string();
        let extension = video_path
            .extension()
            .and_then(|item| item.to_str())
            .unwrap_or("mp4");
        let destination = self
            .videos_dir(session_id)?
            .join(format!("{source_id}.{extension}"));
        fs::copy(video_path, &destination)
            .map_err(|error| format!("Could not store rendered video: {error}"))?;
        let original_name = video_path
            .file_name()
            .and_then(|item| item.to_str())
            .map(|item| item.to_string())
            .unwrap_or_else(|| AGENT_EDIT_NAME.to_string());
        let duration_samples = ((duration_ms as f64 / 1000.0) * sample_rate as f64).round() as u64;

        // Drop the duplicate agent-edit tracks (everything after the first). Remove
        // from the end so the lower `keep` index stays valid.
        for &idx in agent_indices.iter().skip(1).rev() {
            project.session.tracks.remove(idx);
        }

        let track = &mut project.session.tracks[keep];
        track.name = AGENT_EDIT_NAME.to_string();
        // Converge to a single full-frame clip pointing at the fresh render.
        track.video_clips.truncate(1);
        if let Some(clip) = track.video_clips.first_mut() {
            clip.video_source_file_id = source_id.clone();
            clip.source_offset_ms = 0;
            clip.start_sample = start_sample;
            clip.end_sample = start_sample + duration_samples.max(1);
            clip.name = Some(AGENT_EDIT_NAME.to_string());
            clip.layout = Some(VideoLayout::default());
            clip.pristine_video_source_file_id = None;
            clip.pristine_source_offset_ms = None;
            clip.pristine_duration_samples = None;
        } else {
            track.video_clips.push(VideoClipRegion {
                id: Uuid::new_v4().to_string(),
                video_source_file_id: source_id.clone(),
                name: Some(AGENT_EDIT_NAME.to_string()),
                start_sample,
                end_sample: start_sample + duration_samples.max(1),
                source_offset_ms: 0,
                layout: Some(VideoLayout::default()),
                pristine_video_source_file_id: None,
                pristine_source_offset_ms: None,
                pristine_duration_samples: None,
            });
        }
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

    /// Restore a video clip to its original (un-graded) recording by swapping its
    /// source-id, offset and duration back to the pristine snapshot saved on the
    /// first effects render. No-op if no pristine snapshot exists — meaning the
    /// clip has never been re-rendered, so it's already the original.
    pub fn revert_clip_to_pristine(
        &self,
        session_id: &str,
        track_id: &str,
        clip_id: &str,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
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
        let Some(pristine_id) = clip.pristine_video_source_file_id.take() else {
            return Ok(project); // Nothing to revert.
        };
        let pristine_offset = clip.pristine_source_offset_ms.take().unwrap_or(0);
        let pristine_duration = clip
            .pristine_duration_samples
            .take()
            .unwrap_or_else(|| clip.end_sample.saturating_sub(clip.start_sample));
        clip.video_source_file_id = pristine_id;
        clip.source_offset_ms = pristine_offset;
        clip.end_sample = clip.start_sample + pristine_duration.max(1);
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
        let (source, _imported) = self.import_source(session_id, source_path, session_rate)?;
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
        let (source, _imported) = self.import_source(session_id, source_path, session_rate)?;
        let source_id = source.id.clone();
        let duration = source.duration_samples;
        let clip_name = strip_extension(&source.original_name);
        let existing_source = project
            .session
            .source_files
            .iter()
            .find(|source| {
                source.id
                    == project
                        .session
                        .tracks
                        .iter()
                        .find(|track| track.id == track_id)
                        .map(|track| track.source_file_id.as_str())
                        .unwrap_or("")
            })
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
        let offset_samples =
            (track.input_latency_ms as f64 * session_rate as f64 / 1000.0).round() as i64;
        let adjusted_start = if offset_samples >= 0 {
            start_sample.saturating_sub(offset_samples as u64)
        } else {
            start_sample.saturating_add((-offset_samples) as u64)
        };
        if track.clips.is_empty() && !track.clips_materialized {
            if let Some(existing) = existing_source
                .as_ref()
                .filter(|source| source.original_name != "Recording")
            {
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
        track.clips_materialized = true;
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
        let extension = video_path
            .extension()
            .and_then(|item| item.to_str())
            .unwrap_or("webm");
        let destination = self
            .videos_dir(session_id)?
            .join(format!("{source_id}.{extension}"));
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
        let duration_samples =
            ((playable_ms as f64 / 1000.0) * project.session.sample_rate as f64).round() as u64;
        // Latency compensation (see add_recording_clip).
        let offset_samples = (track.input_latency_ms as f64 * project.session.sample_rate as f64
            / 1000.0)
            .round() as i64;
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
            pristine_video_source_file_id: None,
            pristine_source_offset_ms: None,
            pristine_duration_samples: None,
        });
        project.session.video_source_files.push(source);
        self.save(&project)?;
        Ok(project)
    }

    pub fn delete_clip(
        &self,
        session_id: &str,
        track_id: &str,
        clip_id: &str,
    ) -> Result<MixProject, String> {
        let mut project = self.get_project(session_id)?;
        let track_index = project
            .session
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or_else(|| format!("Unknown track {track_id}"))?;
        let track = &mut project.session.tracks[track_index];
        let before_clips = track.clips.clone();
        let before_materialized = track.clips_materialized;
        let before = track.clips.len();
        track.clips.retain(|clip| clip.id != clip_id);
        if track.clips.len() == before {
            return Err(format!("Unknown clip {clip_id}"));
        }
        track.clips_materialized = true;
        let after_clips = track.clips.clone();
        project.session.tracks[track_index].clips = before_clips.clone();
        project.session.tracks[track_index].clips_materialized = before_materialized;
        record_patch(
            &mut project,
            vec![
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clips"),
                    value: Some(serde_json::json!(after_clips)),
                },
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clipsMaterialized"),
                    value: Some(serde_json::json!(true)),
                },
            ],
            vec![
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clips"),
                    value: Some(serde_json::json!(before_clips)),
                },
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clipsMaterialized"),
                    value: Some(serde_json::json!(before_materialized)),
                },
            ],
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
        let before_materialized = project.session.tracks[track_index].clips_materialized;
        let before_clips = project.session.tracks[track_index].clips.clone();
        if project.session.tracks[track_index].clips.is_empty() && !before_materialized {
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
        track.clips_materialized = true;
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
        project.session.tracks[track_index].clips_materialized = before_materialized;
        record_patch(
            &mut project,
            vec![
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clips"),
                    value: Some(serde_json::json!(after_clips)),
                },
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clipsMaterialized"),
                    value: Some(serde_json::json!(true)),
                },
            ],
            vec![
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clips"),
                    value: Some(serde_json::json!(before_clips)),
                },
                JsonPatchOp {
                    op: "replace".into(),
                    path: format!("/tracks/{track_index}/clipsMaterialized"),
                    value: Some(serde_json::json!(before_materialized)),
                },
            ],
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
                right.source_offset_ms = right.source_offset_ms.saturating_add(
                    ((end_sample.saturating_sub(clip_start) as f64
                        / project.session.sample_rate as f64)
                        * 1000.0)
                        .round() as u64,
                );
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

    fn import_source(
        &self,
        session_id: &str,
        source_path: &Path,
        session_rate: u32,
    ) -> Result<(SourceFile, ImportedAudio), String> {
        let source_id = Uuid::new_v4().to_string();
        let original_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("audio")
            .to_string();

        let imported = import_to_session_rate(source_path, session_rate)
            .map_err(|e| format!("import {original_name}: {e}"))?;
        let cache_path = self
            .audio_dir(session_id)?
            .join(format!("{source_id}.f32cache"));
        write_to_cache(&cache_path, &imported)?;

        let peak_path = self
            .peaks_dir(session_id)?
            .join(format!("{source_id}.peaks.json"));
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
            pristine_source_id: None,
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
        let manifest = self
            .locate_session_file(session_id)
            .ok_or_else(|| format!("Session {session_id} not found in the open album"))?;
        let old_dir = manifest
            .parent()
            .ok_or_else(|| "Song project has no parent directory.".to_string())?
            .to_path_buf();
        let is_document_folder =
            manifest.file_name().and_then(|name| name.to_str()) == Some(SONG_MANIFEST);
        if is_document_folder {
            let album_dir = old_dir
                .parent()
                .ok_or_else(|| "Song folder has no album directory.".to_string())?;
            let desired = album_dir.join(sanitize_name(&new_name));
            let new_dir = if desired == old_dir {
                old_dir.clone()
            } else if desired.exists() {
                unique_named_dir(album_dir, &new_name)
            } else {
                desired
            };
            if new_dir != old_dir {
                fs::rename(&old_dir, &new_dir)
                    .map_err(|error| format!("Could not rename song folder: {error}"))?;
                Self::rebase_project_paths(&mut project, &old_dir, &new_dir);
            }
        }
        project.session.name = new_name;
        self.save(&project)?;
        Ok(project)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let project = self.get_project(session_id)?;
        if let Some(path) = self.locate_session_file(session_id) {
            if path.file_name().and_then(|name| name.to_str()) == Some(SONG_MANIFEST) {
                let song_dir = path
                    .parent()
                    .ok_or_else(|| "Song project has no parent directory.".to_string())?;
                fs::remove_dir_all(song_dir).map_err(|error| error.to_string())?;
            } else {
                fs::remove_file(&path).map_err(|error| error.to_string())?;
            }
            if let Ok(mut album) = self.get_album(&project.session.album_id) {
                album.song_order.retain(|id| id != session_id);
                self.save_album(&album)?;
            }
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
            let extension = video_src
                .extension()
                .and_then(|item| item.to_str())
                .unwrap_or("webm");
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

    /// Import a portable bundle as a self-contained song in the open album.
    pub fn import_project_bundle(&self, bundle_dir: &Path) -> Result<MixProject, String> {
        let project_path = bundle_dir.join("project.json");
        let raw = fs::read_to_string(&project_path).map_err(|e| {
            format!(
                "Could not read {} (not a project bundle?): {e}",
                project_path.display()
            )
        })?;
        let mut project: MixProject =
            serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let album =
            self.list_albums()?.into_iter().next().ok_or_else(|| {
                "Open or create an album before importing a song bundle.".to_string()
            })?;
        project.session.id = Uuid::new_v4().to_string();
        project.session.album_id = album.id.clone();
        let album_dir = self
            .album_dir(&album.id)
            .ok_or_else(|| "The open album folder is unavailable.".to_string())?;
        let song_dir = unique_named_dir(&album_dir, &project.session.name);
        self.create_song_layout(&song_dir)?;

        for src in &mut project.session.source_files {
            let cache_src = bundle_dir.join(&src.cache_path);
            let cache_dst = song_dir
                .join(AUDIO_DIR)
                .join(format!("{}.f32cache", src.id));
            fs::copy(&cache_src, &cache_dst)
                .map_err(|e| format!("import cache for {}: {e}", src.original_name))?;
            src.cache_path = cache_dst.to_string_lossy().to_string();

            let peak_src = bundle_dir.join(&src.peak_path);
            let peak_dst = song_dir
                .join(PEAKS_DIR)
                .join(format!("{}.peaks.json", src.id));
            fs::copy(&peak_src, &peak_dst)
                .map_err(|e| format!("import peaks for {}: {e}", src.original_name))?;
            src.peak_path = peak_dst.to_string_lossy().to_string();
        }
        for src in &mut project.session.video_source_files {
            let video_src = bundle_dir.join(&src.path);
            let extension = video_src
                .extension()
                .and_then(|item| item.to_str())
                .unwrap_or("webm");
            let video_dst = song_dir
                .join(VIDEO_DIR)
                .join(format!("{}.{}", src.id, extension));
            fs::copy(&video_src, &video_dst)
                .map_err(|e| format!("import video for {}: {e}", src.original_name))?;
            src.path = video_dst.to_string_lossy().to_string();
        }

        let mut portable = project.clone();
        Self::make_project_paths_relative(&mut portable, &song_dir);
        fs::write(
            song_dir.join(SONG_MANIFEST),
            serde_json::to_string_pretty(&portable).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        self.add_song_to_album(&project.session.album_id, &project.session.id)?;
        Ok(project)
    }

    fn create_silent_source(
        &self,
        session_id: &str,
        sample_rate: u32,
        original_name: &str,
        channels: u16,
    ) -> Result<SourceFile, String> {
        let source_id = Uuid::new_v4().to_string();
        let frames = sample_rate as u64;
        let channels = channels.max(1).min(2);
        let samples = vec![0.0_f32; frames as usize * channels as usize];
        let cache_path = self
            .audio_dir(session_id)?
            .join(format!("{source_id}.f32cache"));
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
        let peak_path = self
            .peaks_dir(session_id)?
            .join(format!("{source_id}.peaks.json"));
        fs::write(
            &peak_path,
            serde_json::to_string(&peaks).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(SourceFile {
            id: source_id,
            original_name: original_name.into(),
            pristine_source_id: None,
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
    let a = crate::engine::source::analysis::analyze(samples, channels, sample_rate);
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
    name.rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("automixer-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn album_document_is_portable_and_never_reopens_implicitly() {
        let root = test_root("album-document");
        let internal = root.join("Internal");
        let documents = root.join("Documents");
        fs::create_dir_all(&documents).expect("create test documents directory");

        let store = SessionStore::new(internal.clone());
        assert!(store.list_albums().expect("list empty store").is_empty());

        let album = store
            .create_album_in(&documents, "Studio Album".to_string())
            .expect("create album document");
        let album_dir = documents.join("Studio Album");
        assert!(album_dir.join(ALBUM_MANIFEST).is_file());
        assert!(store
            .list_sessions(&album.id)
            .expect("list empty album")
            .is_empty());

        let song = store
            .create_session(&album.id, "First Song".to_string())
            .expect("create song");
        let session_id = song.session.id.clone();
        let song_dir = album_dir.join("First Song");
        for path in [
            song_dir.join(SONG_MANIFEST),
            song_dir.join(AUDIO_DIR),
            song_dir.join(PEAKS_DIR),
            song_dir.join(RECORDINGS_DIR),
            song_dir.join(VIDEO_DIR),
            song_dir.join(RENDERS_DIR),
        ] {
            assert!(
                path.exists(),
                "expected portable song path {}",
                path.display()
            );
        }

        store
            .create_recording_track(&session_id, 1)
            .expect("create self-contained recording track");
        let video_path = song_dir.join(VIDEO_DIR).join("portable-test.mp4");
        fs::write(&video_path, b"portable video fixture").expect("write video fixture");
        let mut with_video = store
            .get_project(&session_id)
            .expect("load song for video fixture");
        with_video.session.video_source_files.push(VideoSourceFile {
            id: Uuid::new_v4().to_string(),
            original_name: "portable-test.mp4".to_string(),
            path: video_path.to_string_lossy().to_string(),
            mime_type: "video/mp4".to_string(),
            duration_ms: 1,
        });
        store
            .save(&with_video)
            .expect("save portable video fixture");
        let raw = fs::read_to_string(song_dir.join(SONG_MANIFEST)).expect("read song manifest");
        assert!(raw.contains("\"Audio/"));
        assert!(raw.contains("\"Peaks/"));
        assert!(raw.contains("\"Video/portable-test.mp4\""));
        assert!(!raw.contains(&root.to_string_lossy().to_string()));

        store.close_album(&album.id).expect("close album");
        assert!(store.list_albums().expect("list closed store").is_empty());

        let moved_album = root.join("Moved Studio Album");
        fs::rename(&album_dir, &moved_album).expect("move album document");

        let reopened_store = SessionStore::new(internal);
        assert!(
            reopened_store
                .list_albums()
                .expect("new process starts empty")
                .is_empty(),
            "an album must never reopen implicitly"
        );
        let reopened_album = reopened_store
            .open_album(&moved_album)
            .expect("open moved album");
        assert_eq!(reopened_album.id, album.id);
        let sessions = reopened_store
            .list_sessions(&album.id)
            .expect("list moved songs");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);

        let loaded = reopened_store
            .get_project(&session_id)
            .expect("load moved song");
        let source = loaded
            .session
            .source_files
            .first()
            .expect("recording source");
        assert!(Path::new(&source.cache_path).starts_with(&moved_album));
        assert!(Path::new(&source.cache_path).is_file());
        assert!(Path::new(&source.peak_path).is_file());
        let video = loaded
            .session
            .video_source_files
            .first()
            .expect("portable video source");
        assert!(Path::new(&video.path).starts_with(&moved_album));
        assert!(Path::new(&video.path).is_file());

        let renamed = reopened_store
            .rename_session(&session_id, "Renamed Song".to_string())
            .expect("rename song folder");
        assert_eq!(renamed.session.name, "Renamed Song");
        assert!(moved_album
            .join("Renamed Song")
            .join(SONG_MANIFEST)
            .is_file());
        assert!(!moved_album.join("First Song").exists());

        let renamed_album = reopened_store
            .rename_album(&album.id, "Final Album".to_string())
            .expect("rename album folder");
        assert_eq!(renamed_album.name, "Final Album");
        assert!(root.join("Final Album").join(ALBUM_MANIFEST).is_file());
        assert!(!moved_album.exists());

        fs::remove_dir_all(&root).expect("remove isolated test root");
    }
}
