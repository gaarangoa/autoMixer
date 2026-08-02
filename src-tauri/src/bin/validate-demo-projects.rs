use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use automixer_lib::{audio, model::TrackKind, store::SessionStore};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn main() {
    if let Err(error) = run() {
        eprintln!("validate-demo-projects: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let data_root = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));
    let data_root = data_root
        .canonicalize()
        .map_err(|error| format!("resolve {}: {error}", data_root.display()))?;
    let album_path = data_root.join("albums").join("AutoMixer Capability Demos");
    let bundles_root = data_root.join("project-bundles");
    let renders_root = data_root.join("test-renders");
    fs::create_dir_all(&renders_root).map_err(|error| error.to_string())?;

    let store = SessionStore::new(data_root.join(".demo-validation-store"));
    let album = store.open_album(&album_path)?;
    let sessions = store.list_sessions(&album.id)?;
    if sessions.len() != 2 {
        return Err(format!("expected 2 demo songs, found {}", sessions.len()));
    }

    let podcast = sessions
        .iter()
        .find(|session| session.name == "AMI Podcast Multicam Demo")
        .ok_or_else(|| "AMI demo is missing".to_string())?;
    let podcast_audio = podcast
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .count();
    let podcast_video = podcast
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .count();
    if podcast_audio != 4 || podcast_video != 5 || podcast.video_source_files.len() != 5 {
        return Err(format!(
            "AMI shape mismatch: {podcast_audio} audio, {podcast_video} video tracks, {} video sources",
            podcast.video_source_files.len()
        ));
    }
    let current_podcast = store.get_project(&podcast.id)?;

    let music = sessions
        .iter()
        .find(|session| session.name == "Slakh Multitrack Music Demo")
        .ok_or_else(|| "Slakh demo is missing".to_string())?;
    let music_audio = music
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .count();
    if music_audio != 10 {
        return Err(format!("expected 10 Slakh stems, found {music_audio}"));
    }

    let temp_parent = env::temp_dir().join(format!("automixer-demo-validation-{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_parent).map_err(|error| error.to_string())?;
    let imported_store = SessionStore::new(temp_parent.join("store"));
    imported_store.create_album_in(&temp_parent, "Bundle Import Validation".into())?;
    let imported_podcast =
        imported_store.import_project_bundle(&bundles_root.join("AMI Podcast Demo"))?;
    let imported_music =
        imported_store.import_project_bundle(&bundles_root.join("Slakh Music Demo"))?;

    let podcast_render = renders_root.join("ami-podcast-raw-mix.wav");
    let current_podcast_render = renders_root.join("ami-podcast-current-mix.wav");
    let music_render = renders_root.join("slakh-raw-mix.wav");
    audio::render_mix(&imported_podcast.session, &podcast_render)?;
    audio::render_mix(&current_podcast.session, &current_podcast_render)?;
    audio::render_mix(&imported_music.session, &music_render)?;

    fs::remove_dir_all(&temp_parent)
        .map_err(|error| format!("clean validation workspace: {error}"))?;

    let report = json!({
        "status": "passed",
        "album": album_path,
        "checks": {
            "albumOpen": true,
            "songCount": sessions.len(),
            "podcastAudioTracks": podcast_audio,
            "podcastVideoTracks": podcast_video,
            "podcastVideoSources": podcast.video_source_files.len(),
            "musicAudioTracks": music_audio,
            "portableBundleImports": 2,
            "audioRenders": [podcast_render, current_podcast_render, music_render]
        }
    });
    fs::write(
        data_root.join("validation-report.json"),
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    write_checksums(&data_root)?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}

fn write_checksums(data_root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    for relative in ["demo-sources", "project-bundles", "test-renders"] {
        collect_files(&data_root.join(relative), &mut files)?;
    }
    files.sort();
    let mut output = String::new();
    for path in files {
        let relative = path
            .strip_prefix(data_root)
            .map_err(|error| error.to_string())?;
        let mut file = fs::File::open(&path).map_err(|error| error.to_string())?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        output.push_str(&format!(
            "{:x}  {}\n",
            hasher.finalize(),
            relative.to_string_lossy()
        ));
    }
    fs::write(data_root.join("checksums.sha256"), output).map_err(|error| error.to_string())
}

fn collect_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            collect_files(&path, output)?;
        } else if path.is_file() {
            output.push(path);
        }
    }
    Ok(())
}
