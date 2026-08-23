//! Bounce selected audio tracks through the offline DSP graph into a new track.

use std::{collections::HashSet, fs, path::Path};

use hound::{SampleFormat, WavSpec, WavWriter};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    actions::record_patch,
    defaults::make_track,
    engine::render::{render_session_to_buffer, RenderedMix},
    model::{ClipRegion, HistorySource, JsonPatchOp, MixProject, MixSession, TrackKind},
    AppState,
};

#[derive(Debug, Clone)]
pub struct CreateMixTrackOptions {
    pub name: Option<String>,
    pub mono: bool,
    pub include_master: bool,
    pub mute_sources: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMixTrackResult {
    pub project: MixProject,
    pub mix_track_id: String,
    pub mix_track_name: String,
    pub source_track_ids: Vec<String>,
    pub channels: u16,
    pub included_master: bool,
    pub sources_muted: bool,
    pub mix_track_muted: bool,
}

pub fn create_mix_track_and_sync(
    state: &AppState,
    session_id: &str,
    requested_track_ids: &[String],
    options: CreateMixTrackOptions,
) -> Result<CreateMixTrackResult, String> {
    if requested_track_ids.is_empty() {
        return Err("Select at least one audio track before creating a mix track.".into());
    }

    let store = state.store.lock().map_err(|error| error.to_string())?;
    let mut project = store.get_project(session_id)?;
    let requested: HashSet<&str> = requested_track_ids.iter().map(String::as_str).collect();
    let source_tracks: Vec<_> = project
        .session
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio && requested.contains(track.id.as_str()))
        .collect();
    if source_tracks.is_empty() {
        return Err("The selection does not contain any audio tracks to mix.".into());
    }
    let source_track_ids: Vec<String> =
        source_tracks.iter().map(|track| track.id.clone()).collect();
    let source_track_names: Vec<String> = source_tracks
        .iter()
        .map(|track| track.name.clone())
        .collect();
    let render_session =
        prepare_mix_track_session(&project.session, &source_track_ids, options.include_master)?;
    let rendered = render_session_to_buffer(&render_session)?;
    if rendered.samples.iter().all(|sample| sample.abs() <= 1.0e-7) {
        return Err(
            "The selected tracks rendered silence. Check their mute and clip states.".into(),
        );
    }

    let requested_name = options
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_mix_track_name(&source_track_names));
    let mix_track_name = unique_track_name(&project, &requested_name);
    let temporary_wav =
        std::env::temp_dir().join(format!("automixer-mix-track-{}.wav", Uuid::new_v4()));
    let channels = if options.mono { 1 } else { 2 };
    write_rendered_wav(&rendered, &temporary_wav, options.mono)?;
    let source_result =
        store.import_source_standalone(session_id, &temporary_wav, project.session.sample_rate);
    let _ = fs::remove_file(&temporary_wav);
    let source = source_result?;

    let before_tracks = project.session.tracks.clone();
    let before_sources = project.session.source_files.clone();
    let mut after_tracks = before_tracks.clone();
    if options.mute_sources {
        let selected: HashSet<&str> = source_track_ids.iter().map(String::as_str).collect();
        for track in &mut after_tracks {
            if selected.contains(track.id.as_str()) {
                track.muted = true;
            }
        }
    }

    let mut mix_track = make_track(
        source.id.clone(),
        mix_track_name.clone(),
        after_tracks.len(),
    );
    let mix_track_id = mix_track.id.clone();
    mix_track.role = Some("mix".into());
    mix_track.ai_generated = true;
    // Never double the audible mix. If sources remain untouched, the bounce starts
    // muted; if the caller asks to mute sources, the bounce becomes the audible copy.
    mix_track.muted = !options.mute_sources;
    mix_track.clips_materialized = true;
    mix_track.clips = vec![ClipRegion {
        id: Uuid::new_v4().to_string(),
        source_file_id: Some(source.id.clone()),
        name: Some(mix_track_name.clone()),
        start_sample: 0,
        end_sample: source.duration_samples,
        source_offset_sample: 0,
        gain_db: 0.0,
    }];
    after_tracks.push(mix_track);
    let mut after_sources = before_sources.clone();
    after_sources.push(source);

    let explanation = format!(
        "Created {} from {} selected audio track{}{}",
        mix_track_name,
        source_track_ids.len(),
        if source_track_ids.len() == 1 { "" } else { "s" },
        if options.include_master {
            " with master processing"
        } else {
            ""
        }
    );
    record_patch(
        &mut project,
        vec![
            JsonPatchOp {
                op: "replace".into(),
                path: "/sourceFiles".into(),
                value: Some(serde_json::json!(after_sources)),
            },
            JsonPatchOp {
                op: "replace".into(),
                path: "/tracks".into(),
                value: Some(serde_json::json!(after_tracks)),
            },
        ],
        vec![
            JsonPatchOp {
                op: "replace".into(),
                path: "/sourceFiles".into(),
                value: Some(serde_json::json!(before_sources)),
            },
            JsonPatchOp {
                op: "replace".into(),
                path: "/tracks".into(),
                value: Some(serde_json::json!(before_tracks)),
            },
        ],
        HistorySource::Assistant,
        Some(explanation),
    )?;
    store.save(&project)?;
    if let Ok(mut audio) = state.audio.lock() {
        audio.bind_session_sources(&project.session)?;
        crate::commands::sync_session_to_engine(&mut audio, &project.session);
        audio.publish_automation(&project.session);
    }

    Ok(CreateMixTrackResult {
        project,
        mix_track_id,
        mix_track_name,
        source_track_ids,
        channels,
        included_master: options.include_master,
        sources_muted: options.mute_sources,
        mix_track_muted: !options.mute_sources,
    })
}

fn prepare_mix_track_session(
    session: &MixSession,
    track_ids: &[String],
    include_master: bool,
) -> Result<MixSession, String> {
    let selected: HashSet<&str> = track_ids.iter().map(String::as_str).collect();
    let mut render_session = session.clone();
    render_session
        .tracks
        .retain(|track| track.kind == TrackKind::Audio && selected.contains(track.id.as_str()));
    if render_session.tracks.is_empty() {
        return Err("None of the selected audio tracks are available to render.".into());
    }
    // The explicit selection defines the bounce. A solo on one selected source should
    // not silently remove its selected neighbors from the rendered track.
    for track in &mut render_session.tracks {
        track.solo = false;
    }
    if !include_master {
        // Keep the full per-track chain and shared sends, but neutralize mastering so
        // the new track can pass through the project's master exactly once on playback.
        render_session.master.gain_db = 0.0;
        render_session.master.limiter.ceiling_db = 0.0;
    }
    Ok(render_session)
}

fn write_rendered_wav(rendered: &RenderedMix, output: &Path, mono: bool) -> Result<(), String> {
    let output_channels = if mono { 1 } else { 2 };
    let spec = WavSpec {
        channels: output_channels,
        sample_rate: rendered.sample_rate,
        bits_per_sample: 24,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(output, spec).map_err(|error| error.to_string())?;
    let input_channels = rendered.channels.max(1) as usize;
    for frame in rendered.samples.chunks(input_channels) {
        let left = frame.first().copied().unwrap_or(0.0);
        let right = frame.get(1).copied().unwrap_or(left);
        if mono {
            write_sample(&mut writer, (left + right) * 0.5)?;
        } else {
            write_sample(&mut writer, left)?;
            write_sample(&mut writer, right)?;
        }
    }
    writer.finalize().map_err(|error| error.to_string())
}

fn write_sample(
    writer: &mut WavWriter<std::io::BufWriter<std::fs::File>>,
    sample: f32,
) -> Result<(), String> {
    writer
        .write_sample((sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32)
        .map_err(|error| error.to_string())
}

fn default_mix_track_name(source_names: &[String]) -> String {
    if source_names.len() == 1 {
        format!("{} · Mix", source_names[0])
    } else {
        "Selected Tracks · Mix".into()
    }
}

fn unique_track_name(project: &MixProject, requested: &str) -> String {
    if !project
        .session
        .tracks
        .iter()
        .any(|track| track.name == requested)
    {
        return requested.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{requested} {suffix}");
        if !project
            .session
            .tracks
            .iter()
            .any(|track| track.name == candidate)
        {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_project() -> MixProject {
        serde_json::from_value(serde_json::json!({
            "session": {
                "id": "session", "name": "Song", "albumId": "album",
                "sampleRate": 48000, "tempoPercent": 100.0, "bpm": null,
                "sourceFiles": [], "videoSourceFiles": [],
                "tracks": [
                    {"id":"one","kind":"audio","name":"Dialogue A","role":null,"sourceFileId":"s1","startSample":0,"gainDb":0.0,"pan":0.0,"muted":false,"solo":true,"color":"#fff","chain":{"highPass":{"enabled":false,"frequencyHz":40.0,"slopeDbOct":12},"lowPass":{"enabled":false,"frequencyHz":18000.0,"slopeDbOct":12},"eq":[],"compressor":{"enabled":false,"thresholdDb":-18.0,"ratio":2.0,"attackMs":20.0,"releaseMs":160.0,"kneeDb":6.0,"makeupDb":0.0}},"sends":{"reverbDb":-60.0,"delayDb":-60.0},"automation":[],"clips":[]},
                    {"id":"two","kind":"audio","name":"Dialogue B","role":null,"sourceFileId":"s2","startSample":0,"gainDb":0.0,"pan":0.0,"muted":false,"solo":false,"color":"#fff","chain":{"highPass":{"enabled":false,"frequencyHz":40.0,"slopeDbOct":12},"lowPass":{"enabled":false,"frequencyHz":18000.0,"slopeDbOct":12},"eq":[],"compressor":{"enabled":false,"thresholdDb":-18.0,"ratio":2.0,"attackMs":20.0,"releaseMs":160.0,"kneeDb":6.0,"makeupDb":0.0}},"sends":{"reverbDb":-60.0,"delayDb":-60.0},"automation":[],"clips":[]},
                    {"id":"video","kind":"video","name":"Camera","role":"video","sourceFileId":"sv","startSample":0,"gainDb":0.0,"pan":0.0,"muted":false,"solo":false,"color":"#fff","chain":{"highPass":{"enabled":false,"frequencyHz":40.0,"slopeDbOct":12},"lowPass":{"enabled":false,"frequencyHz":18000.0,"slopeDbOct":12},"eq":[],"compressor":{"enabled":false,"thresholdDb":-18.0,"ratio":2.0,"attackMs":20.0,"releaseMs":160.0,"kneeDb":6.0,"makeupDb":0.0}},"sends":{"reverbDb":-60.0,"delayDb":-60.0},"automation":[],"clips":[]}
                ],
                "buses": [], "master":{"gainDb":-3.0,"limiter":{"enabled":true,"ceilingDb":-1.0}},
                "regions":[], "markers":[], "sections":[], "videoCanvas":{"width":1280,"height":720,"background":"#000000"}
            },
            "history":[], "redoStack":[], "chatMessages":[]
        })).expect("test project")
    }

    #[test]
    fn selected_audio_tracks_define_neutral_master_render() {
        let project = test_project();
        let render = prepare_mix_track_session(
            &project.session,
            &["one".into(), "two".into(), "video".into()],
            false,
        )
        .unwrap();
        assert_eq!(render.tracks.len(), 2);
        assert!(render.tracks.iter().all(|track| !track.solo));
        assert_eq!(render.master.gain_db, 0.0);
        assert_eq!(render.master.limiter.ceiling_db, 0.0);
    }

    #[test]
    fn generated_mix_track_names_do_not_collide() {
        let mut project = test_project();
        project.session.tracks[0].name = "Selected Tracks · Mix".into();
        project.session.tracks[1].name = "Selected Tracks · Mix 2".into();
        assert_eq!(
            unique_track_name(&project, "Selected Tracks · Mix"),
            "Selected Tracks · Mix 3"
        );
    }
}
