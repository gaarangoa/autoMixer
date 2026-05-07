use std::{
    fs,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    defaults::{default_master, make_track},
    engine::source::{import_to_session_rate, write_to_cache, ImportedAudio},
    model::{MixProject, MixSession, SourceFile, TrackAnalysis},
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
            tracks: Vec::new(),
            buses: Vec::new(),
            master: default_master(),
            regions: Vec::new(),
            markers: Vec::new(),
        };
        let project = MixProject { session, history: Vec::new(), redo_stack: Vec::new() };
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
        let mut project = self.get_project(session_id)?;
        let session_rate = project.session.sample_rate;
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

        let track_name = strip_extension(&original_name);
        let track = make_track(source_id, track_name, project.session.tracks.len());
        project.session.source_files.push(source);
        project.session.tracks.push(track);
        self.save(&project)?;
        Ok(project)
    }

    pub fn renders_dir(&self) -> PathBuf {
        self.data_dir.join("renders")
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
}

fn analyze_imported(imported: &ImportedAudio) -> TrackAnalysis {
    let a = crate::engine::source::analysis::analyze(
        &imported.samples,
        imported.channels,
        imported.sample_rate,
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
