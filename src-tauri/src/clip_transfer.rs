use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::{
    actions::record_patch,
    commands::sync_session_to_engine,
    model::{ClipRegion, HistorySource, JsonPatchOp, MixProject, SourceFile, Track, TrackKind},
    AppState,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferClipResponse {
    pub project: MixProject,
    pub track_id: String,
    pub clip_id: String,
}

fn materialize_audio_track(track: &mut Track, sources: &[SourceFile]) -> Result<(), String> {
    if track.clips_materialized {
        return Ok(());
    }
    if track.clips.is_empty() {
        let source = sources
            .iter()
            .find(|source| source.id == track.source_file_id)
            .ok_or_else(|| format!("Audio source for `{}` is missing.", track.name))?;
        if source.duration_samples > 0 {
            track.clips.push(ClipRegion {
                id: format!("legacy-{}", track.id),
                source_file_id: Some(source.id.clone()),
                name: Some(track.name.clone()),
                start_sample: track.start_sample,
                end_sample: track.start_sample.saturating_add(source.duration_samples),
                source_offset_sample: 0,
                gain_db: 0.0,
            });
        }
    }
    track.clips_materialized = true;
    Ok(())
}

fn transfer_tracks(
    tracks: &[Track],
    sources: &[SourceFile],
    source_track_id: &str,
    destination_track_id: &str,
    clip_id: &str,
    start_sample: u64,
    copy: bool,
) -> Result<(Vec<Track>, String), String> {
    let source_index = tracks
        .iter()
        .position(|track| track.id == source_track_id)
        .ok_or_else(|| format!("Unknown source track {source_track_id}"))?;
    let destination_index = tracks
        .iter()
        .position(|track| track.id == destination_track_id)
        .ok_or_else(|| format!("Unknown destination track {destination_track_id}"))?;
    if tracks[source_index].kind != tracks[destination_index].kind {
        return Err(
            "Audio clips can only move to audio tracks, and video clips to video tracks.".into(),
        );
    }

    let mut next = tracks.to_vec();
    match next[source_index].kind {
        TrackKind::Audio => {
            materialize_audio_track(&mut next[source_index], sources)?;
            if destination_index != source_index {
                materialize_audio_track(&mut next[destination_index], sources)?;
            }

            let clip_index = next[source_index]
                .clips
                .iter()
                .position(|clip| clip.id == clip_id)
                .ok_or_else(|| format!("Unknown audio clip {clip_id}"))?;
            let mut clip = next[source_index].clips[clip_index].clone();
            let duration = clip.end_sample.saturating_sub(clip.start_sample);
            let end_sample = start_sample
                .checked_add(duration)
                .ok_or_else(|| "The clip would extend beyond the timeline limit.".to_string())?;

            if !copy {
                next[source_index].clips.remove(clip_index);
            } else {
                clip.id = Uuid::new_v4().to_string();
            }
            clip.start_sample = start_sample;
            clip.end_sample = end_sample;
            let returned_id = clip.id.clone();
            next[destination_index].clips.push(clip);
            next[source_index].clips_materialized = true;
            next[destination_index].clips_materialized = true;
            next[source_index]
                .clips
                .sort_by_key(|clip| (clip.start_sample, clip.end_sample));
            if destination_index != source_index {
                next[destination_index]
                    .clips
                    .sort_by_key(|clip| (clip.start_sample, clip.end_sample));
            }
            Ok((next, returned_id))
        }
        TrackKind::Video => {
            let clip_index = next[source_index]
                .video_clips
                .iter()
                .position(|clip| clip.id == clip_id)
                .ok_or_else(|| format!("Unknown video clip {clip_id}"))?;
            let mut clip = next[source_index].video_clips[clip_index].clone();
            let duration = clip.end_sample.saturating_sub(clip.start_sample);
            let end_sample = start_sample
                .checked_add(duration)
                .ok_or_else(|| "The clip would extend beyond the timeline limit.".to_string())?;

            if !copy {
                next[source_index].video_clips.remove(clip_index);
            } else {
                clip.id = Uuid::new_v4().to_string();
            }
            clip.start_sample = start_sample;
            clip.end_sample = end_sample;
            let returned_id = clip.id.clone();
            next[destination_index].video_clips.push(clip);
            next[source_index]
                .video_clips
                .sort_by_key(|clip| (clip.start_sample, clip.end_sample));
            if destination_index != source_index {
                next[destination_index]
                    .video_clips
                    .sort_by_key(|clip| (clip.start_sample, clip.end_sample));
            }
            Ok((next, returned_id))
        }
    }
}

#[tauri::command]
pub fn transfer_clip(
    state: State<'_, AppState>,
    session_id: String,
    source_track_id: String,
    destination_track_id: String,
    clip_id: String,
    start_sample: u64,
    copy: bool,
) -> Result<TransferClipResponse, String> {
    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(&session_id)?;
    let before_tracks = project.session.tracks.clone();
    let (after_tracks, returned_clip_id) = transfer_tracks(
        &before_tracks,
        &project.session.source_files,
        &source_track_id,
        &destination_track_id,
        &clip_id,
        start_sample,
        copy,
    )?;
    let verb = if copy { "Copied" } else { "Moved" };
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
        Some(format!("{verb} clip to track")),
    )?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }
    Ok(TransferClipResponse {
        project,
        track_id: destination_track_id,
        clip_id: returned_clip_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        defaults::{default_master, make_track},
        model::{MixSession, MixerProfile, TrackAnalysis, VideoCanvas, VideoClipRegion},
    };

    fn source(id: &str, duration_samples: u64) -> SourceFile {
        SourceFile {
            id: id.into(),
            original_name: format!("{id}.wav"),
            pristine_source_id: None,
            cache_path: String::new(),
            peak_path: String::new(),
            duration_samples,
            sample_rate: 48_000,
            channels: 1,
            analysis: TrackAnalysis {
                peak_db: 0.0,
                rms_db: 0.0,
                lufs_estimate: 0.0,
                spectral_centroid_hz: 0.0,
                low_energy: 0.0,
                mid_energy: 0.0,
                high_energy: 0.0,
                silence_percent: 0.0,
                dynamic_range_db: 0.0,
            },
            peak_preview: Vec::new(),
        }
    }

    fn audio_track(id: &str, source_id: &str) -> Track {
        let mut track = make_track(source_id.into(), id.into(), 0);
        track.id = id.into();
        track
    }

    #[test]
    fn move_preserves_audio_clip_data_and_leaves_source_silent() {
        let sources = vec![source("one", 1000), source("two", 1000)];
        let mut source_track = audio_track("source", "one");
        source_track.clips_materialized = true;
        source_track.clips.push(ClipRegion {
            id: "clip".into(),
            source_file_id: Some("one".into()),
            name: Some("Take".into()),
            start_sample: 100,
            end_sample: 500,
            source_offset_sample: 75,
            gain_db: -2.5,
        });
        let mut destination = audio_track("destination", "two");
        destination.clips_materialized = true;
        let (tracks, returned_id) = transfer_tracks(
            &[source_track, destination],
            &sources,
            "source",
            "destination",
            "clip",
            900,
            false,
        )
        .unwrap();
        assert_eq!(returned_id, "clip");
        assert!(tracks[0].clips.is_empty());
        assert!(tracks[0].clips_materialized);
        assert_eq!(tracks[1].clips[0].start_sample, 900);
        assert_eq!(tracks[1].clips[0].end_sample, 1300);
        assert_eq!(tracks[1].clips[0].source_offset_sample, 75);
        assert_eq!(tracks[1].clips[0].gain_db, -2.5);
    }

    #[test]
    fn copy_keeps_source_and_assigns_a_new_id() {
        let sources = vec![source("one", 1000)];
        let source_track = audio_track("source", "one");
        let mut destination = audio_track("destination", "one");
        destination.clips_materialized = true;
        let (tracks, returned_id) = transfer_tracks(
            &[source_track, destination],
            &sources,
            "source",
            "destination",
            "legacy-source",
            1200,
            true,
        )
        .unwrap();
        assert_eq!(tracks[0].clips.len(), 1);
        assert_eq!(tracks[0].clips[0].id, "legacy-source");
        assert_ne!(returned_id, "legacy-source");
        assert_eq!(tracks[1].clips[0].start_sample, 1200);
    }

    #[test]
    fn legacy_destination_is_materialized_before_append() {
        let sources = vec![source("one", 1000), source("two", 500)];
        let source_track = audio_track("source", "one");
        let destination = audio_track("destination", "two");
        let (tracks, _) = transfer_tracks(
            &[source_track, destination],
            &sources,
            "source",
            "destination",
            "legacy-source",
            1500,
            false,
        )
        .unwrap();
        assert_eq!(tracks[1].clips.len(), 2);
        assert!(tracks[1]
            .clips
            .iter()
            .any(|clip| clip.id == "legacy-destination"));
    }

    #[test]
    fn rejects_cross_kind_transfer() {
        let sources = vec![source("one", 1000)];
        let source_track = audio_track("source", "one");
        let mut video_track = audio_track("video", "one");
        video_track.kind = TrackKind::Video;
        video_track.video_clips.push(VideoClipRegion {
            id: "video-clip".into(),
            video_source_file_id: "video-source".into(),
            name: None,
            start_sample: 0,
            end_sample: 100,
            source_offset_ms: 0,
            layout: None,
            pristine_video_source_file_id: None,
            pristine_source_offset_ms: None,
            pristine_duration_samples: None,
        });
        assert!(transfer_tracks(
            &[source_track, video_track],
            &sources,
            "source",
            "video",
            "legacy-source",
            0,
            false,
        )
        .is_err());
    }

    #[test]
    fn transfer_is_one_undoable_history_entry() {
        let sources = vec![source("one", 1000), source("two", 1000)];
        let before_tracks = vec![
            audio_track("source", "one"),
            audio_track("destination", "two"),
        ];
        let (after_tracks, _) = transfer_tracks(
            &before_tracks,
            &sources,
            "source",
            "destination",
            "legacy-source",
            300,
            false,
        )
        .unwrap();
        let mut project = MixProject {
            session: MixSession {
                id: "session".into(),
                name: "Session".into(),
                album_id: String::new(),
                sample_rate: 48_000,
                minimum_timeline_seconds: None,
                tempo_percent: 100.0,
                bpm: None,
                time_signature: Default::default(),
                project_start_bar: 1,
                source_files: sources,
                video_source_files: Vec::new(),
                tracks: before_tracks.clone(),
                buses: Vec::new(),
                master: default_master(),
                regions: Vec::new(),
                markers: Vec::new(),
                sections: Vec::new(),
                mixer_profile: MixerProfile::default(),
                video_canvas: VideoCanvas::default(),
            },
            history: Vec::new(),
            redo_stack: Vec::new(),
            chat_messages: Vec::new(),
        };
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
            Some("Moved clip".into()),
        )
        .unwrap();
        assert_eq!(project.history.len(), 1);
        assert!(project.session.tracks[0].clips.is_empty());
        crate::actions::undo(&mut project).unwrap();
        assert!(!project.session.tracks[0].clips_materialized);
        crate::actions::redo(&mut project).unwrap();
        assert!(project.session.tracks[0].clips_materialized);
    }
}
