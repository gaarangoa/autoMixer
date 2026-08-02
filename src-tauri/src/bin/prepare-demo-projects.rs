use std::{
    env, fs,
    path::{Path, PathBuf},
};

use automixer_lib::{
    model::{Marker, Region, TrackKind},
    store::SessionStore,
};
use uuid::Uuid;

const DEMO_SECONDS: f64 = 120.0;

fn main() {
    if let Err(error) = run() {
        eprintln!("prepare-demo-projects: {error}");
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
    let source_root = data_root.join("demo-sources");
    let albums_root = data_root.join("albums");
    let bundles_root = data_root.join("project-bundles");
    fs::create_dir_all(&albums_root).map_err(|error| error.to_string())?;
    fs::create_dir_all(&bundles_root).map_err(|error| error.to_string())?;

    let expected_album = albums_root.join("AutoMixer Capability Demos");
    if expected_album.exists() {
        return Err(format!(
            "{} already exists; move or remove that one demo album before rebuilding",
            expected_album.display()
        ));
    }
    for name in ["AMI Podcast Demo", "Slakh Music Demo"] {
        let bundle = bundles_root.join(name);
        if bundle.exists() {
            return Err(format!(
                "{} already exists; move or remove that one bundle before rebuilding",
                bundle.display()
            ));
        }
    }

    let store = SessionStore::new(data_root.join(".demo-store"));
    let album = store.create_album_in(&albums_root, "AutoMixer Capability Demos".into())?;

    let podcast = build_podcast_project(&store, &album.id, &source_root.join("ami-es2002a"))?;
    store.export_project_bundle(&podcast.session.id, &bundles_root.join("AMI Podcast Demo"))?;

    let music = build_music_project(&store, &album.id, &source_root.join("slakh-track00001"))?;
    store.export_project_bundle(&music.session.id, &bundles_root.join("Slakh Music Demo"))?;

    println!("album={}", expected_album.display());
    println!(
        "podcast_bundle={}",
        bundles_root.join("AMI Podcast Demo").display()
    );
    println!(
        "music_bundle={}",
        bundles_root.join("Slakh Music Demo").display()
    );
    Ok(())
}

fn build_podcast_project(
    store: &SessionStore,
    album_id: &str,
    source_dir: &Path,
) -> Result<automixer_lib::model::MixProject, String> {
    let mut project = store.create_session(album_id, "AMI Podcast Multicam Demo".into())?;
    for name in [
        "speaker-a-david.wav",
        "speaker-b-project-manager.wav",
        "speaker-c-craig.wav",
        "speaker-d-andrew.wav",
    ] {
        project =
            store.add_source_file(&project.session.id, &source_dir.join("audio").join(name))?;
    }

    let cameras = [
        ("Camera A — David", "camera-a-david.mp4"),
        ("Camera B — Project Manager", "camera-b-project-manager.mp4"),
        ("Camera C — Craig", "camera-c-craig.mp4"),
        ("Camera D — Andrew", "camera-d-andrew.mp4"),
        ("Camera 5 — Room Overview", "camera-room-overview.mp4"),
    ];
    for (track_name, file_name) in cameras {
        project = store.create_video_track(&project.session.id)?;
        let track_id = project
            .session
            .tracks
            .last()
            .map(|track| track.id.clone())
            .ok_or_else(|| "video track was not created".to_string())?;
        {
            let track = project
                .session
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .ok_or_else(|| format!("track {track_id} disappeared"))?;
            track.name = track_name.into();
            track.kind = TrackKind::Video;
            track.role = Some("video".into());
        }
        store.save(&project)?;
        project = store.add_video_recording_clip(
            &project.session.id,
            &track_id,
            &source_dir.join("video").join(file_name),
            file_name.into(),
            "video/mp4".into(),
            0,
            (DEMO_SECONDS * 1000.0) as u64,
            0,
        )?;
    }

    let sample_rate = project.session.sample_rate as u64;
    project.session.markers = vec![
        marker("Participant introductions", 4.0, sample_rate),
        marker("Project briefing", 60.0, sample_rate),
        marker("Team discussion", 70.0, sample_rate),
    ];
    project.session.regions = vec![Region {
        id: Uuid::new_v4().to_string(),
        name: "Two-minute pilot excerpt".into(),
        start_sample: 0,
        end_sample: (DEMO_SECONDS * sample_rate as f64) as u64,
        track_ids: None,
    }];
    project.chat_messages = vec![serde_json::json!({
        "role": "assistant",
        "content": "Demo ready. Try: Create a professional speaker-aware multicam edit. Use the room overview briefly for context, cut to the active speaker, avoid rapid cuts, and preserve the isolated headset mix."
    })];
    store.save(&project)?;
    Ok(project)
}

fn build_music_project(
    store: &SessionStore,
    album_id: &str,
    source_dir: &Path,
) -> Result<automixer_lib::model::MixProject, String> {
    let mut project = store.create_session(album_id, "Slakh Multitrack Music Demo".into())?;
    for name in [
        "drums.wav",
        "electric-bass.wav",
        "bright-acoustic-piano.wav",
        "distortion-guitar-1.wav",
        "distortion-guitar-2.wav",
        "jazz-guitar.wav",
        "choir-aahs.wav",
        "percussive-organ.wav",
        "harmonica-organ-1.wav",
        "harmonica-organ-2.wav",
    ] {
        project =
            store.add_source_file(&project.session.id, &source_dir.join("stems").join(name))?;
    }

    project.session.mixer_profile.genre = Some("rock".into());
    project.session.mixer_profile.custom_notes =
        Some("Capability demo: preserve dynamics and make restrained, explainable changes.".into());
    let sample_rate = project.session.sample_rate as u64;
    project.session.regions = vec![Region {
        id: Uuid::new_v4().to_string(),
        name: "Two-minute multitrack excerpt".into(),
        start_sample: 0,
        end_sample: (DEMO_SECONDS * sample_rate as f64) as u64,
        track_ids: None,
    }];
    project.chat_messages = vec![serde_json::json!({
        "role": "assistant",
        "content": "Demo ready. Try: Build a balanced professional rock mix. Give the drums and bass a solid foundation, create space between the guitars and organs, keep the piano audible, and explain every processing decision."
    })];
    store.save(&project)?;
    Ok(project)
}

fn marker(name: &str, seconds: f64, sample_rate: u64) -> Marker {
    Marker {
        id: Uuid::new_v4().to_string(),
        name: name.into(),
        sample: (seconds * sample_rate as f64) as u64,
    }
}
