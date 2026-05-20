use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::model::{
    AutomatableParam, AutomationLane, AutomationPoint, CurveType, HistoryEntry, HistorySource,
    JsonPatchOp, MixAction, MixProject, MixSession, Region,
};

pub fn apply_actions(
    project: &mut MixProject,
    actions: &[MixAction],
    source: HistorySource,
    explanation: Option<String>,
) -> Result<Option<HistoryEntry>, String> {
    validate_actions(&project.session, actions)?;

    let mut forward_patch = Vec::new();
    let mut inverse_patch = Vec::new();

    for action in actions {
        apply_action(&mut project.session, action, &mut forward_patch, &mut inverse_patch)?;
    }

    if forward_patch.is_empty() {
        return Ok(None);
    }

    let entry = HistoryEntry {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().timestamp_millis(),
        source,
        explanation,
        forward_patch,
        inverse_patch,
    };
    project.history.push(entry.clone());
    project.redo_stack.clear();
    Ok(Some(entry))
}

pub fn undo(project: &mut MixProject) -> Result<Option<HistoryEntry>, String> {
    let Some(entry) = project.history.pop() else {
        return Ok(None);
    };
    apply_patch_ops(&mut project.session, &entry.inverse_patch)?;
    project.redo_stack.push(entry.clone());
    Ok(Some(entry))
}

pub fn redo(project: &mut MixProject) -> Result<Option<HistoryEntry>, String> {
    let Some(entry) = project.redo_stack.pop() else {
        return Ok(None);
    };
    apply_patch_ops(&mut project.session, &entry.forward_patch)?;
    project.history.push(entry.clone());
    Ok(Some(entry))
}

pub fn record_patch(
    project: &mut MixProject,
    forward_patch: Vec<JsonPatchOp>,
    inverse_patch: Vec<JsonPatchOp>,
    source: HistorySource,
    explanation: Option<String>,
) -> Result<HistoryEntry, String> {
    apply_patch_ops(&mut project.session, &forward_patch)?;
    let entry = HistoryEntry {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().timestamp_millis(),
        source,
        explanation,
        forward_patch,
        inverse_patch,
    };
    project.history.push(entry.clone());
    project.redo_stack.clear();
    Ok(entry)
}

/// Clamp out-of-range numeric parameters into their safe envelopes and emit
/// human-readable warnings. The model occasionally proposes moves that exceed
/// our guardrails (e.g. +15 dB gain); rather than rejecting the whole turn,
/// we apply the clamped value and surface the adjustment to the user.
pub fn clamp_actions(actions: &mut [MixAction]) -> Vec<String> {
    let mut warnings = Vec::new();
    for action in actions.iter_mut() {
        match action {
            MixAction::SetTrackGain { gain_db, .. } => {
                clamp_field(gain_db, -24.0, 24.0, "gainDb", &mut warnings);
            }
            MixAction::AdjustTrackGain { delta_db, .. } => {
                clamp_field(delta_db, -12.0, 12.0, "deltaDb", &mut warnings);
            }
            MixAction::SetTrackPan { pan, .. } => {
                clamp_field(pan, -1.0, 1.0, "pan", &mut warnings);
            }
            MixAction::SetHighPass { frequency_hz, .. }
            | MixAction::SetLowPass { frequency_hz, .. } => {
                clamp_field(frequency_hz, 20.0, 20000.0, "frequencyHz", &mut warnings);
            }
            MixAction::SetEqBand { band, frequency_hz, gain_db, q, .. } => {
                let original = *band;
                *band = (*band).min(3);
                if *band != original {
                    warnings.push(format!("band clamped from {original} to {band}"));
                }
                clamp_field(frequency_hz, 20.0, 20000.0, "frequencyHz", &mut warnings);
                clamp_field(gain_db, -12.0, 12.0, "gainDb", &mut warnings);
                clamp_field(q, 0.2, 10.0, "q", &mut warnings);
            }
            MixAction::SetCompressor { threshold_db, ratio, attack_ms, release_ms, knee_db, makeup_db, .. } => {
                clamp_field(threshold_db, -60.0, 0.0, "thresholdDb", &mut warnings);
                clamp_field(ratio, 1.0, 20.0, "ratio", &mut warnings);
                clamp_field(attack_ms, 1.0, 200.0, "attackMs", &mut warnings);
                clamp_field(release_ms, 20.0, 1000.0, "releaseMs", &mut warnings);
                clamp_field(knee_db, 0.0, 24.0, "kneeDb", &mut warnings);
                clamp_field(makeup_db, -12.0, 12.0, "makeupDb", &mut warnings);
            }
            MixAction::SetReverbSend { level_db, .. }
            | MixAction::SetDelaySend { level_db, .. } => {
                clamp_field(level_db, -60.0, 0.0, "levelDb", &mut warnings);
            }
            MixAction::SetRegionGain { gain_db, .. } => {
                clamp_field(gain_db, -24.0, 24.0, "gainDb", &mut warnings);
            }
            MixAction::SetMasterGain { gain_db } => {
                clamp_field(gain_db, -24.0, 12.0, "gainDb", &mut warnings);
            }
            MixAction::AdjustMasterGain { delta_db } => {
                clamp_field(delta_db, -12.0, 12.0, "deltaDb", &mut warnings);
            }
            MixAction::ApplySectionAutomation { value, .. } => {
                clamp_field(value, -60.0, 20000.0, "value", &mut warnings);
            }
            _ => {}
        }
    }
    warnings
}

fn clamp_field(value: &mut f32, min: f32, max: f32, label: &str, warnings: &mut Vec<String>) {
    if !value.is_finite() {
        warnings.push(format!("{label} was not finite — reset to 0"));
        *value = 0.0;
        return;
    }
    if *value < min {
        warnings.push(format!("{label} {:.2} clamped to {:.2}", *value, min));
        *value = min;
    } else if *value > max {
        warnings.push(format!("{label} {:.2} clamped to {:.2}", *value, max));
        *value = max;
    }
}

pub fn validate_actions(session: &MixSession, actions: &[MixAction]) -> Result<(), String> {
    for action in actions {
        match action {
            MixAction::CreateRegion { start_sample, end_sample, .. } => {
                if end_sample <= start_sample {
                    return Err("endSample must be greater than startSample".into());
                }
            }
            MixAction::DeleteTrack { track_id }
            | MixAction::RenameTrack { track_id, .. }
            | MixAction::SetTrackRole { track_id, .. }
            | MixAction::SetTrackPan { track_id, .. }
            | MixAction::MuteTrack { track_id, .. }
            | MixAction::SoloTrack { track_id, .. }
            | MixAction::SetTrackAiGenerated { track_id, .. }
            | MixAction::SetReverbSend { track_id, .. }
            | MixAction::SetDelaySend { track_id, .. }
            | MixAction::SetTrackGain { track_id, .. }
            | MixAction::AdjustTrackGain { track_id, .. }
            | MixAction::SetCompressor { track_id, .. } => {
                require_track(session, track_id)?;
            }
            MixAction::SetHighPass { track_id, slope_db_oct, .. }
            | MixAction::SetLowPass { track_id, slope_db_oct, .. } => {
                require_track(session, track_id)?;
                if ![12, 24].contains(slope_db_oct) {
                    return Err("slopeDbOct must be 12 or 24".into());
                }
            }
            MixAction::SetEqBand { track_id, band, .. } => {
                require_track(session, track_id)?;
                if *band > 3 {
                    return Err("band must be 0 through 3".into());
                }
            }
            MixAction::SetProcessorParam { target_id, .. } => {
                require_track(session, target_id)?;
            }
            MixAction::SetRegionGain { region_id, track_id, .. } => {
                require_track(session, track_id)?;
                require_region(session, region_id)?;
            }
            MixAction::ApplySectionAutomation { region_id, track_id, .. } => {
                require_track(session, track_id)?;
                require_region(session, region_id)?;
            }
            MixAction::SetMasterGain { .. } | MixAction::AdjustMasterGain { .. } => {}
            MixAction::Undo | MixAction::Redo | MixAction::RenderMix => {}
        }
    }
    Ok(())
}

fn apply_action(
    session: &mut MixSession,
    action: &MixAction,
    forward: &mut Vec<JsonPatchOp>,
    inverse: &mut Vec<JsonPatchOp>,
) -> Result<(), String> {
    match action {
        MixAction::CreateRegion { name, start_sample, end_sample, track_ids } => {
            let region = Region {
                id: Uuid::new_v4().to_string(),
                name: name.clone(),
                start_sample: *start_sample,
                end_sample: *end_sample,
                track_ids: track_ids.clone(),
            };
            let index = session.regions.len();
            session.regions.push(region.clone());
            add_forward_inverse(forward, inverse, format!("/regions/{index}"), json!(region));
        }
        MixAction::DeleteTrack { track_id } => {
            let index = track_index(session, track_id)?;
            let removed = session.tracks.remove(index);
            forward.push(JsonPatchOp { op: "remove".into(), path: format!("/tracks/{index}"), value: None });
            inverse.insert(0, JsonPatchOp { op: "add".into(), path: format!("/tracks/{index}"), value: Some(json!(removed)) });
        }
        MixAction::RenameTrack { track_id, name } => replace(session, forward, inverse, &track_path(session, track_id, "name")?, json!(name.trim().chars().take(80).collect::<String>()))?,
        MixAction::SetTrackRole { track_id, role } => {
            let value = role
                .as_ref()
                .map(|role| role.trim().chars().take(40).collect::<String>())
                .filter(|role| !role.is_empty());
            replace(session, forward, inverse, &track_path(session, track_id, "role")?, json!(value))?;
        }
        MixAction::SetTrackGain { track_id, gain_db } => replace(session, forward, inverse, &track_path(session, track_id, "gainDb")?, json!(gain_db))?,
        MixAction::AdjustTrackGain { track_id, delta_db } => {
            let track = require_track(session, track_id)?;
            let next = (track.gain_db + delta_db).clamp(-24.0, 24.0);
            replace(session, forward, inverse, &track_path(session, track_id, "gainDb")?, json!(next))?;
        }
        MixAction::SetTrackPan { track_id, pan } => replace(session, forward, inverse, &track_path(session, track_id, "pan")?, json!(pan))?,
        MixAction::MuteTrack { track_id, muted } => replace(session, forward, inverse, &track_path(session, track_id, "muted")?, json!(muted))?,
        MixAction::SoloTrack { track_id, solo } => replace(session, forward, inverse, &track_path(session, track_id, "solo")?, json!(solo))?,
        MixAction::SetTrackAiGenerated { track_id, ai_generated } => replace(session, forward, inverse, &track_path(session, track_id, "aiGenerated")?, json!(ai_generated))?,
        MixAction::SetHighPass { track_id, frequency_hz, slope_db_oct } => {
            let base = track_path(session, track_id, "chain/highPass")?;
            replace(session, forward, inverse, &format!("{base}/enabled"), json!(true))?;
            replace(session, forward, inverse, &format!("{base}/frequencyHz"), json!(frequency_hz))?;
            replace(session, forward, inverse, &format!("{base}/slopeDbOct"), json!(slope_db_oct))?;
        }
        MixAction::SetLowPass { track_id, frequency_hz, slope_db_oct } => {
            let base = track_path(session, track_id, "chain/lowPass")?;
            replace(session, forward, inverse, &format!("{base}/enabled"), json!(true))?;
            replace(session, forward, inverse, &format!("{base}/frequencyHz"), json!(frequency_hz))?;
            replace(session, forward, inverse, &format!("{base}/slopeDbOct"), json!(slope_db_oct))?;
        }
        MixAction::SetEqBand { track_id, band, frequency_hz, gain_db, q } => {
            let base = track_path(session, track_id, &format!("chain/eq/{band}"))?;
            replace(session, forward, inverse, &format!("{base}/frequencyHz"), json!(frequency_hz))?;
            replace(session, forward, inverse, &format!("{base}/gainDb"), json!(gain_db))?;
            replace(session, forward, inverse, &format!("{base}/q"), json!(q))?;
        }
        MixAction::SetCompressor { track_id, threshold_db, ratio, attack_ms, release_ms, knee_db, makeup_db } => {
            let base = track_path(session, track_id, "chain/compressor")?;
            replace(session, forward, inverse, &format!("{base}/enabled"), json!(true))?;
            replace(session, forward, inverse, &format!("{base}/thresholdDb"), json!(threshold_db))?;
            replace(session, forward, inverse, &format!("{base}/ratio"), json!(ratio))?;
            replace(session, forward, inverse, &format!("{base}/attackMs"), json!(attack_ms))?;
            replace(session, forward, inverse, &format!("{base}/releaseMs"), json!(release_ms))?;
            replace(session, forward, inverse, &format!("{base}/kneeDb"), json!(knee_db))?;
            replace(session, forward, inverse, &format!("{base}/makeupDb"), json!(makeup_db))?;
        }
        MixAction::SetReverbSend { track_id, level_db } => replace(session, forward, inverse, &track_path(session, track_id, "sends/reverbDb")?, json!(level_db))?,
        MixAction::SetDelaySend { track_id, level_db } => replace(session, forward, inverse, &track_path(session, track_id, "sends/delayDb")?, json!(level_db))?,
        MixAction::SetProcessorParam { target_id, processor_id, param_id, value } => {
            let path = processor_param_path(session, target_id, processor_id, param_id)?;
            replace(session, forward, inverse, &path, json!(value))?;
        }
        MixAction::SetMasterGain { gain_db } => replace(session, forward, inverse, "/master/gainDb", json!(gain_db))?,
        MixAction::AdjustMasterGain { delta_db } => {
            let next = (session.master.gain_db + delta_db).clamp(-24.0, 12.0);
            replace(session, forward, inverse, "/master/gainDb", json!(next))?;
        }
        MixAction::SetRegionGain { region_id, track_id, gain_db } => add_automation(session, forward, inverse, track_id, region_id, AutomatableParam::GainDb, *gain_db)?,
        MixAction::ApplySectionAutomation { region_id, track_id, param, value } => add_automation(session, forward, inverse, track_id, region_id, param.clone(), *value)?,
        MixAction::Undo | MixAction::Redo | MixAction::RenderMix => {}
    }
    Ok(())
}

fn add_automation(
    session: &mut MixSession,
    forward: &mut Vec<JsonPatchOp>,
    inverse: &mut Vec<JsonPatchOp>,
    track_id: &str,
    region_id: &str,
    param: AutomatableParam,
    value: f32,
) -> Result<(), String> {
    let track_index = track_index(session, track_id)?;
    let region = require_region(session, region_id)?.clone();
    let lane = AutomationLane {
        id: Uuid::new_v4().to_string(),
        param,
        region_id: Some(region_id.to_string()),
        points: vec![
            AutomationPoint { sample: region.start_sample, value },
            AutomationPoint { sample: region.end_sample, value },
        ],
        curve: CurveType::Linear,
    };
    let lane_index = session.tracks[track_index].automation.len();
    session.tracks[track_index].automation.push(lane.clone());
    add_forward_inverse(forward, inverse, format!("/tracks/{track_index}/automation/{lane_index}"), json!(lane));
    Ok(())
}

fn replace<T: Serialize>(
    session: &mut MixSession,
    forward: &mut Vec<JsonPatchOp>,
    inverse: &mut Vec<JsonPatchOp>,
    path: &str,
    value: T,
) -> Result<(), String> {
    let mut root = serde_json::to_value(session.clone()).map_err(|error| error.to_string())?;
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    let previous = pointer(&root, path)?.clone();
    if previous == value {
        return Ok(());
    }
    *pointer_mut(&mut root, path)? = value.clone();
    *session = serde_json::from_value(root).map_err(|error| error.to_string())?;
    forward.push(JsonPatchOp { op: "replace".into(), path: path.to_string(), value: Some(value) });
    inverse.insert(0, JsonPatchOp { op: "replace".into(), path: path.to_string(), value: Some(previous) });
    Ok(())
}

fn add_forward_inverse(forward: &mut Vec<JsonPatchOp>, inverse: &mut Vec<JsonPatchOp>, path: String, value: Value) {
    forward.push(JsonPatchOp { op: "add".into(), path: path.clone(), value: Some(value) });
    inverse.insert(0, JsonPatchOp { op: "remove".into(), path, value: None });
}

fn apply_patch_ops(session: &mut MixSession, ops: &[JsonPatchOp]) -> Result<(), String> {
    let mut root = serde_json::to_value(session.clone()).map_err(|error| error.to_string())?;
    for op in ops {
        match op.op.as_str() {
            "replace" => *pointer_mut(&mut root, &op.path)? = op.value.clone().unwrap_or(Value::Null),
            "add" => add_value(&mut root, &op.path, op.value.clone().unwrap_or(Value::Null))?,
            "remove" => remove_value(&mut root, &op.path)?,
            other => return Err(format!("Unsupported patch op {other}")),
        }
    }
    *session = serde_json::from_value(root).map_err(|error| error.to_string())?;
    Ok(())
}

fn add_value(root: &mut Value, path: &str, value: Value) -> Result<(), String> {
    let (parent_path, key) = split_pointer(path)?;
    let parent = pointer_mut(root, &parent_path)?;
    if let Value::Array(items) = parent {
        let index = key.parse::<usize>().map_err(|_| format!("Invalid array index {key}"))?;
        items.insert(index, value);
        Ok(())
    } else if let Value::Object(map) = parent {
        map.insert(key, value);
        Ok(())
    } else {
        Err(format!("Cannot add at {path}"))
    }
}

fn remove_value(root: &mut Value, path: &str) -> Result<(), String> {
    let (parent_path, key) = split_pointer(path)?;
    let parent = pointer_mut(root, &parent_path)?;
    if let Value::Array(items) = parent {
        let index = key.parse::<usize>().map_err(|_| format!("Invalid array index {key}"))?;
        items.remove(index);
        Ok(())
    } else if let Value::Object(map) = parent {
        map.remove(&key);
        Ok(())
    } else {
        Err(format!("Cannot remove at {path}"))
    }
}

fn processor_param_path(session: &MixSession, track_id: &str, processor_id: &str, param_id: &str) -> Result<String, String> {
    let base = match processor_id {
        "track_balance" if param_id == "gainDb" => "gainDb".to_string(),
        "track_balance" if param_id == "pan" => "pan".to_string(),
        "sends" if param_id == "reverbDb" => "sends/reverbDb".to_string(),
        "sends" if param_id == "delayDb" => "sends/delayDb".to_string(),
        "filters" if param_id == "highPass.frequencyHz" => "chain/highPass/frequencyHz".to_string(),
        "filters" if param_id == "lowPass.frequencyHz" => "chain/lowPass/frequencyHz".to_string(),
        "compressor" => format!("chain/compressor/{param_id}"),
        "eq_4band" if param_id.starts_with("band") => {
            let mut chars = param_id.chars();
            let _ = chars.next();
            let _ = chars.next();
            let _ = chars.next();
            let _ = chars.next();
            let band = chars.next().ok_or_else(|| format!("Unknown processor param {processor_id}.{param_id}"))?;
            let field = param_id.split('.').nth(1).ok_or_else(|| format!("Unknown processor param {processor_id}.{param_id}"))?;
            format!("chain/eq/{band}/{field}")
        }
        _ => return Err(format!("Unknown processor param {processor_id}.{param_id}")),
    };
    track_path(session, track_id, &base)
}

fn track_path(session: &MixSession, track_id: &str, suffix: &str) -> Result<String, String> {
    Ok(format!("/tracks/{}/{}", track_index(session, track_id)?, suffix))
}

fn require_track<'a>(session: &'a MixSession, track_id: &str) -> Result<&'a crate::model::Track, String> {
    session.tracks.iter().find(|track| track.id == track_id).ok_or_else(|| format!("Unknown track {track_id}"))
}

fn require_region<'a>(session: &'a MixSession, region_id: &str) -> Result<&'a crate::model::Region, String> {
    session.regions.iter().find(|region| region.id == region_id).ok_or_else(|| format!("Unknown region {region_id}"))
}

fn track_index(session: &MixSession, track_id: &str) -> Result<usize, String> {
    session.tracks.iter().position(|track| track.id == track_id).ok_or_else(|| format!("Unknown track {track_id}"))
}

fn pointer<'a>(root: &'a Value, path: &str) -> Result<&'a Value, String> {
    root.pointer(path).ok_or_else(|| format!("Invalid patch path {path}"))
}

fn pointer_mut<'a>(root: &'a mut Value, path: &str) -> Result<&'a mut Value, String> {
    root.pointer_mut(path).ok_or_else(|| format!("Invalid patch path {path}"))
}

fn split_pointer(path: &str) -> Result<(String, String), String> {
    let Some((parent, key)) = path.rsplit_once('/') else {
        return Err(format!("Invalid patch path {path}"));
    };
    Ok((if parent.is_empty() { "/".into() } else { parent.into() }, key.replace("~1", "/").replace("~0", "~")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{default_master, make_track};

    fn fresh_session() -> MixSession {
        let mut s = MixSession {
            id: "s".into(),
            name: "test".into(),
            sample_rate: 48_000,
            bpm: None,
            source_files: Vec::new(),
            tracks: Vec::new(),
            buses: Vec::new(),
            master: default_master(),
            regions: Vec::new(),
            markers: Vec::new(),
            sections: Vec::new(),
            mixer_profile: Default::default(),
            video_canvas: Default::default(),
        };
        s.tracks.push(make_track("src1".into(), "Vocal".into(), 0));
        s.tracks.push(make_track("src2".into(), "Bass".into(), 1));
        s
    }

    fn project(session: MixSession) -> MixProject {
        MixProject { session, history: Vec::new(), redo_stack: Vec::new(), chat_messages: Vec::new() }
    }

    #[test]
    fn clamp_brings_out_of_range_gain_into_envelope() {
        let mut actions = vec![MixAction::SetTrackGain { track_id: "x".into(), gain_db: 99.0 }];
        let warnings = clamp_actions(&mut actions);
        assert_eq!(warnings.len(), 1);
        if let MixAction::SetTrackGain { gain_db, .. } = &actions[0] {
            assert!((*gain_db - 24.0).abs() < 1e-6);
        } else {
            panic!("expected SetTrackGain");
        }
    }

    #[test]
    fn validate_accepts_in_range_gain() {
        let session = fresh_session();
        let id = session.tracks[0].id.clone();
        let ok = vec![MixAction::SetTrackGain { track_id: id, gain_db: -3.0 }];
        assert!(validate_actions(&session, &ok).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_track() {
        let session = fresh_session();
        let bad = vec![MixAction::SetTrackGain { track_id: "ghost".into(), gain_db: 0.0 }];
        assert!(validate_actions(&session, &bad).is_err());
    }

    #[test]
    fn validate_rejects_bad_eq_band_index() {
        let session = fresh_session();
        let id = session.tracks[0].id.clone();
        let bad = vec![MixAction::SetEqBand {
            track_id: id,
            band: 7,
            frequency_hz: 1000.0,
            gain_db: 0.0,
            q: 1.0,
        }];
        assert!(validate_actions(&session, &bad).is_err());
    }

    #[test]
    fn clamp_brings_out_of_range_eq_band_into_envelope() {
        let mut actions = vec![MixAction::SetEqBand {
            track_id: "x".into(),
            band: 4,
            frequency_hz: 4000.0,
            gain_db: -1.5,
            q: 1.0,
        }];
        let warnings = clamp_actions(&mut actions);
        assert!(warnings.iter().any(|warning| warning.contains("band clamped from 4 to 3")));
        assert!(matches!(&actions[0], MixAction::SetEqBand { band, .. } if *band == 3));
    }

    #[test]
    fn validate_rejects_bad_slope() {
        let session = fresh_session();
        let id = session.tracks[0].id.clone();
        let bad = vec![MixAction::SetHighPass {
            track_id: id,
            frequency_hz: 80.0,
            slope_db_oct: 18,
        }];
        assert!(validate_actions(&session, &bad).is_err());
    }

    #[test]
    fn apply_then_undo_round_trip() {
        let mut p = project(fresh_session());
        let id = p.session.tracks[0].id.clone();
        let original_gain = p.session.tracks[0].gain_db;
        apply_actions(
            &mut p,
            &[MixAction::SetTrackGain { track_id: id.clone(), gain_db: -6.0 }],
            HistorySource::User,
            None,
        )
        .unwrap();
        assert!((p.session.tracks[0].gain_db - (-6.0)).abs() < 1e-6);
        undo(&mut p).unwrap();
        assert!((p.session.tracks[0].gain_db - original_gain).abs() < 1e-6);
    }

    #[test]
    fn apply_undo_redo_round_trip() {
        let mut p = project(fresh_session());
        let id = p.session.tracks[0].id.clone();
        apply_actions(
            &mut p,
            &[MixAction::SetTrackPan { track_id: id, pan: 0.5 }],
            HistorySource::User,
            None,
        )
        .unwrap();
        undo(&mut p).unwrap();
        assert!((p.session.tracks[0].pan - 0.0).abs() < 1e-6);
        redo(&mut p).unwrap();
        assert!((p.session.tracks[0].pan - 0.5).abs() < 1e-6);
    }
}
