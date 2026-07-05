use std::{
    fs,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    actions::record_patch,
    defaults::{default_master, make_track},
    engine::source::{import_to_session_rate, write_to_cache, ImportedAudio},
    model::{ClipRegion, HistorySource, JsonPatchOp, MixAlbum, MixProject, MixSession, SourceFile, TrackAnalysis, TrackKind, VideoClipRegion, VideoLayout, VideoSourceFile},
};

/// A recently opened/created album: its id, display name, and on-disk folder.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecentAlbum {
    pub id: String,
    pub name: String,
    pub path: String,
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() { "Untitled Album".to_string() } else { trimmed.to_string() }
}

/// A non-colliding album folder named after `name` inside `parent`.
fn unique_album_dir(parent: &Path, name: &str) -> PathBuf {
    let base = sanitize_name(name);
    let mut candidate = parent.join(&base);
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{base} {n}"));
        n += 1;
    }
    candidate
}

/// Given an album folder, its songs/ dir, or a song file inside it, return the
/// album folder (the dir containing album.json).
fn resolve_album_dir(path: &Path) -> Option<PathBuf> {
    let mut dir = if path.is_file() { path.parent()?.to_path_buf() } else { path.to_path_buf() };
    for _ in 0..3 {
        if dir.join("album.json").is_file() {
            return Some(dir);
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    None
}

pub struct SessionStore {
    data_dir: PathBuf,
    /// album id -> on-disk album folder (user-chosen, document model).
    album_index: std::sync::Mutex<std::collections::HashMap<String, PathBuf>>,
}

impl SessionStore {
    pub fn new(data_dir: PathBuf) -> Self {
        let store = Self { data_dir, album_index: std::sync::Mutex::new(std::collections::HashMap::new()) };
        let _ = store.init();
        store.index_recent_albums();
        let _ = store.migrate_legacy_albums();
        let _ = store.migrate_legacy_sessions();
        store
    }

    pub fn init(&self) -> Result<(), String> {
        fs::create_dir_all(self.albums_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.sources_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.peaks_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.videos_dir()).map_err(|error| error.to_string())?;
        fs::create_dir_all(self.renders_dir()).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn create_session(&self, album_id: &str, name: String) -> Result<MixProject, String> {
        self.init()?;
        let session = MixSession {
            id: Uuid::new_v4().to_string(),
            name,
            album_id: album_id.to_string(),
            sample_rate: 48000,
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
        let project = MixProject { session, history: Vec::new(), redo_stack: Vec::new(), chat_messages: Vec::new() };
        self.save(&project)?;
        self.add_song_to_album(album_id, &project.session.id)?;
        Ok(project)
    }

    /// Every song across all albums (used by the headless web/agent surface).
    pub fn list_all_sessions(&self) -> Result<Vec<MixSession>, String> {
        self.init()?;
        let mut out = Vec::new();
        for album in self.list_albums()? {
            out.extend(self.list_sessions(&album.id)?);
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Create a song in the default album (for callers without an album context).
    pub fn create_session_default(&self, name: String) -> Result<MixProject, String> {
        let album_id = self.default_album_id()?;
        self.create_session(&album_id, name)
    }

    /// List the songs (sessions) in one album, in the album's stored order.
    pub fn list_sessions(&self, album_id: &str) -> Result<Vec<MixSession>, String> {
        self.init()?;
        let album = self.get_album(album_id)?;
        let songs_dir = self.album_dir(album_id).ok_or_else(|| format!("Album {album_id} not open"))?.join("songs");
        let mut by_id: std::collections::HashMap<String, MixSession> = std::collections::HashMap::new();
        if songs_dir.is_dir() {
            for entry in fs::read_dir(&songs_dir).map_err(|error| error.to_string())? {
                let path = entry.map_err(|error| error.to_string())?.path();
                if path.extension().and_then(|item| item.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(raw) = fs::read_to_string(&path) {
                    if let Ok(project) = serde_json::from_str::<MixProject>(&raw) {
                        by_id.insert(project.session.id.clone(), project.session);
                    }
                }
            }
        }
        // Ordered by song_order first, then any stragglers by name.
        let mut sessions: Vec<MixSession> = Vec::new();
        for id in &album.song_order {
            if let Some(s) = by_id.remove(id) {
                sessions.push(s);
            }
        }
        let mut rest: Vec<MixSession> = by_id.into_values().collect();
        rest.sort_by(|a, b| a.name.cmp(&b.name));
        sessions.extend(rest);
        Ok(sessions)
    }

    pub fn get_project(&self, session_id: &str) -> Result<MixProject, String> {
        self.init()?;
        let path = self
            .locate_session_file(session_id)
            .ok_or_else(|| format!("Session {session_id} not found"))?;
        let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
        serde_json::from_str(&raw).map_err(|error| error.to_string())
    }

    pub fn save(&self, project: &MixProject) -> Result<(), String> {
        self.init()?;
        // Path follows the session's album folder. If the album isn't known (legacy
        // or orphan), fall back to (and adopt) the default app-managed album.
        let dir = match self.album_dir(&project.session.album_id) {
            Some(d) => d,
            None => {
                let id = self.default_album_id()?;
                self.album_dir(&id).ok_or_else(|| "default album folder missing".to_string())?
            }
        };
        let songs_dir = dir.join("songs");
        fs::create_dir_all(&songs_dir).map_err(|error| error.to_string())?;
        let path = songs_dir.join(format!("{}.json", project.session.id));
        fs::write(path, serde_json::to_string_pretty(project).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }

    // ---- Album folders (document model — user-chosen locations) -------------

    fn recents_path(&self) -> PathBuf {
        self.data_dir.join("recents.json")
    }

    fn read_recents(&self) -> Vec<RecentAlbum> {
        fs::read_to_string(self.recents_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn write_recents(&self, recents: &[RecentAlbum]) {
        if let Ok(json) = serde_json::to_string_pretty(recents) {
            let _ = fs::write(self.recents_path(), json);
        }
    }

    /// Record an album at the front of the recents list (deduped, capped).
    fn add_recent(&self, id: &str, name: &str, path: &Path) {
        let mut recents = self.read_recents();
        recents.retain(|r| r.id != id && Path::new(&r.path) != path);
        recents.insert(0, RecentAlbum { id: id.to_string(), name: name.to_string(), path: path.to_string_lossy().to_string() });
        recents.truncate(24);
        self.write_recents(&recents);
    }

    /// Load every recent album's folder into the id->path index (dropping any
    /// whose folder/manifest has gone missing).
    fn index_recent_albums(&self) {
        let recents = self.read_recents();
        let mut still_valid = Vec::new();
        if let Ok(mut index) = self.album_index.lock() {
            for r in recents {
                let dir = PathBuf::from(&r.path);
                if dir.join("album.json").is_file() {
                    index.insert(r.id.clone(), dir);
                    still_valid.push(r);
                }
            }
        }
        self.write_recents(&still_valid);
    }

    fn album_dir(&self, album_id: &str) -> Option<PathBuf> {
        self.album_index.lock().ok()?.get(album_id).cloned()
    }

    fn register_album(&self, id: &str, dir: &Path) {
        if let Ok(mut index) = self.album_index.lock() {
            index.insert(id.to_string(), dir.to_path_buf());
        }
    }

    fn album_manifest_path(&self, album_id: &str) -> Option<PathBuf> {
        self.album_dir(album_id).map(|d| d.join("album.json"))
    }

    /// Find the on-disk file for a session by scanning every open album folder.
    fn locate_session_file(&self, session_id: &str) -> Option<PathBuf> {
        let dirs: Vec<PathBuf> = self.album_index.lock().ok()?.values().cloned().collect();
        for dir in dirs {
            let candidate = dir.join("songs").join(format!("{session_id}.json"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// All known (recent) albums, freshest first; drops any whose folder vanished.
    pub fn list_albums(&self) -> Result<Vec<MixAlbum>, String> {
        self.init()?;
        let mut albums = Vec::new();
        for r in self.read_recents() {
            if let Ok(album) = self.get_album(&r.id) {
                albums.push(album);
            }
        }
        Ok(albums)
    }

    pub fn list_recents(&self) -> Result<Vec<RecentAlbum>, String> {
        Ok(self.read_recents())
    }

    pub fn get_album(&self, album_id: &str) -> Result<MixAlbum, String> {
        let manifest = self.album_manifest_path(album_id).ok_or_else(|| format!("Album {album_id} not open"))?;
        let raw = fs::read_to_string(manifest).map_err(|_| format!("Album {album_id} not found on disk"))?;
        serde_json::from_str(&raw).map_err(|error| error.to_string())
    }

    fn save_album(&self, album: &MixAlbum) -> Result<(), String> {
        let dir = self.album_dir(&album.id).ok_or_else(|| format!("Album {} not open", album.id))?;
        fs::create_dir_all(dir.join("songs")).map_err(|error| error.to_string())?;
        fs::write(
            dir.join("album.json"),
            serde_json::to_string_pretty(album).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        self.add_recent(&album.id, &album.name, &dir);
        Ok(())
    }

    /// Create a new album folder named `name` inside `parent_dir` (document model).
    pub fn create_album_in(&self, parent_dir: &Path, name: String) -> Result<MixAlbum, String> {
        let folder = unique_album_dir(parent_dir, &name);
        fs::create_dir_all(folder.join("songs")).map_err(|error| error.to_string())?;
        let album = MixAlbum { id: Uuid::new_v4().to_string(), name, song_order: Vec::new() };
        self.register_album(&album.id, &folder);
        self.save_album(&album)?;
        Ok(album)
    }

    /// Open an existing album from its folder on disk.
    pub fn open_album(&self, path: &Path) -> Result<MixAlbum, String> {
        // Accept either the album folder, or a song file / songs dir inside it.
        let dir = resolve_album_dir(path).ok_or_else(|| format!("No album.json found at {}", path.display()))?;
        let raw = fs::read_to_string(dir.join("album.json")).map_err(|error| error.to_string())?;
        let album: MixAlbum = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        self.register_album(&album.id, &dir);
        self.add_recent(&album.id, &album.name, &dir);
        Ok(album)
    }

    pub fn rename_album(&self, album_id: &str, new_name: String) -> Result<MixAlbum, String> {
        let mut album = self.get_album(album_id)?;
        album.name = new_name;
        self.save_album(&album)?;
        Ok(album)
    }

    /// Forget an album (remove from recents/index). Leaves the folder on disk.
    pub fn delete_album(&self, album_id: &str) -> Result<(), String> {
        if let Ok(mut index) = self.album_index.lock() {
            index.remove(album_id);
        }
        let recents: Vec<RecentAlbum> = self.read_recents().into_iter().filter(|r| r.id != album_id).collect();
        self.write_recents(&recents);
        Ok(())
    }

    fn add_song_to_album(&self, album_id: &str, session_id: &str) -> Result<(), String> {
        let mut album = self.get_album(album_id)?;
        if !album.song_order.iter().any(|id| id == session_id) {
            album.song_order.push(session_id.to_string());
        }
        self.save_album(&album)
    }

    /// A default app-managed album (in the data dir) for callers without a chosen
    /// location (the headless web/agent surface, or orphan sessions).
    fn default_album_id(&self) -> Result<String, String> {
        if let Some(first) = self.read_recents().into_iter().next() {
            if self.album_dir(&first.id).is_some() {
                return Ok(first.id);
            }
        }
        Ok(self.create_album_in(&self.albums_dir(), "My Album".to_string())?.id)
    }

    /// Register any pre-existing app-data albums (from the earlier in-app model)
    /// into recents so users keep their albums after the move to the document model.
    fn migrate_legacy_albums(&self) -> Result<(), String> {
        let root = self.albums_dir();
        if !root.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(&root).map_err(|error| error.to_string())? {
            let dir = entry.map_err(|error| error.to_string())?.path();
            let manifest = dir.join("album.json");
            if !manifest.is_file() {
                continue;
            }
            if let Ok(raw) = fs::read_to_string(&manifest) {
                if let Ok(album) = serde_json::from_str::<MixAlbum>(&raw) {
                    self.register_album(&album.id, &dir);
                    self.add_recent(&album.id, &album.name, &dir);
                }
            }
        }
        Ok(())
    }

    /// One-time migration: wrap pre-album `sessions/{id}.json` files into a default
    /// album. No-ops once any album exists.
    fn migrate_legacy_sessions(&self) -> Result<(), String> {
        let legacy = self.sessions_dir();
        if !legacy.is_dir() {
            return Ok(());
        }
        if !self.read_recents().is_empty() {
            return Ok(());
        }
        let mut legacy_files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&legacy).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().and_then(|i| i.to_str()) == Some("json") {
                legacy_files.push(path);
            }
        }
        if legacy_files.is_empty() {
            return Ok(());
        }
        let album = self.create_album_in(&self.albums_dir(), "My Album".to_string())?;
        for path in legacy_files {
            if let Ok(raw) = fs::read_to_string(&path) {
                if let Ok(mut project) = serde_json::from_str::<MixProject>(&raw) {
                    project.session.album_id = album.id.clone();
                    self.save(&project)?;
                    self.add_song_to_album(&album.id, &project.session.id)?;
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(())
    }

    pub fn add_source_file(&self, session_id: &str, source_path: &Path) -> Result<MixProject, String> {
        self.add_source_file_at(session_id, source_path, 0)
    }

    /// Import a wav into the cache (analysis + peaks) and return the SourceFile
    /// WITHOUT touching any session — used by transforms (e.g. tempo stretch) that
    /// register the file themselves.
    pub fn import_source_standalone(&self, source_path: &Path, session_rate: u32) -> Result<SourceFile, String> {
        let (source, _imported) = self.import_source(source_path, session_rate)?;
        Ok(source)
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
        let extension = video_path.extension().and_then(|item| item.to_str()).unwrap_or("mp4");
        let destination = self.videos_dir().join(format!("{source_id}.{extension}"));
        fs::copy(video_path, &destination)
            .map_err(|error| format!("Could not store rendered video: {error}"))?;

        let duration_samples = ((duration_ms as f64 / 1000.0) * sample_rate as f64).round() as u64;
        // Snapshot the pre-edit source as the pristine on the FIRST replace; later
        // replaces leave the original snapshot alone so a revert always lands back
        // on the raw recording, not on a previously graded version.
        if clip.pristine_video_source_file_id.is_none() {
            clip.pristine_video_source_file_id = Some(clip.video_source_file_id.clone());
            clip.pristine_source_offset_ms = Some(clip.source_offset_ms);
            clip.pristine_duration_samples = Some(clip.end_sample.saturating_sub(clip.start_sample));
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
        let extension = video_path.extension().and_then(|item| item.to_str()).unwrap_or("mp4");
        let destination = self.videos_dir().join(format!("{source_id}.{extension}"));
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
        let pristine_duration = clip.pristine_duration_samples.take().unwrap_or_else(
            || clip.end_sample.saturating_sub(clip.start_sample),
        );
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
            pristine_video_source_file_id: None,
            pristine_source_offset_ms: None,
            pristine_duration_samples: None,
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
        project.session.name = new_name;
        self.save(&project)?;
        Ok(project)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        if let Some(path) = self.locate_session_file(session_id) {
            // albums/{album_id}/songs/{id}.json → album id is the grandparent dir name.
            let album_id = path
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string());
            fs::remove_file(&path).map_err(|error| error.to_string())?;
            if let Some(aid) = album_id {
                if let Ok(mut album) = self.get_album(&aid) {
                    album.song_order.retain(|id| id != session_id);
                    let _ = self.save_album(&album);
                }
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
        project.session.album_id = self.default_album_id()?;

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
        self.add_song_to_album(&project.session.album_id, &project.session.id)?;
        Ok(project)
    }

    pub fn renders_dir(&self) -> PathBuf {
        self.data_dir.join("renders")
    }

    fn albums_dir(&self) -> PathBuf {
        self.data_dir.join("albums")
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
