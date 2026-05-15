//! Autonomous mix orchestrator.
//!
//! Runs a sequence of narrowly-scoped LLM turns (stages) on a session.
//! Each stage has its own prompt template and its own action cap. State
//! lives on the Tauri AppHandle so the frontend can drive Pause/Stop and
//! the orchestrator emits per-stage events.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::{
    actions::{apply_actions, clamp_actions, validate_actions},
    capabilities::skill_catalog,
    config::Config,
    model::{HistorySource, MixAction, MixProject, MixSession},
    store::SessionStore,
};

use crate::assistant::{
    ai_stem_preservation_block, build_capability_snapshot, expand_skills_from_actions,
    extract_json_object, mixing_fundamentals_block, ollama_generate, profile_block, sections_block,
    substitute_quoted, AccumulatingObserver, LlmObserver,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoMixStage {
    RawSessionPrep,
    PrepIntent,
    StaticBalance,
    CleanupFilters,
    SubtractiveEq,
    Dynamics,
    TonalEnhancement,
    DepthSpace,
    SectionAutomation,
    MixBusLoudness,
}

impl AutoMixStage {
    pub fn id(&self) -> &'static str {
        match self {
            Self::RawSessionPrep => "raw_session_prep",
            Self::PrepIntent => "prep_intent",
            Self::StaticBalance => "static_balance",
            Self::CleanupFilters => "cleanup_filters",
            Self::SubtractiveEq => "subtractive_eq",
            Self::Dynamics => "dynamics",
            Self::TonalEnhancement => "tonal_enhancement",
            Self::DepthSpace => "depth_space",
            Self::SectionAutomation => "section_automation",
            Self::MixBusLoudness => "mix_bus_loudness",
        }
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::RawSessionPrep => "Raw session prep",
            Self::PrepIntent => "Prep / intent",
            Self::StaticBalance => "Static balance",
            Self::CleanupFilters => "Cleanup filters",
            Self::SubtractiveEq => "Subtractive EQ",
            Self::Dynamics => "Dynamics control",
            Self::TonalEnhancement => "Tonal enhancement",
            Self::DepthSpace => "Depth & space",
            Self::SectionAutomation => "Section automation",
            Self::MixBusLoudness => "Mix bus / loudness",
        }
    }
    fn skills(&self) -> &'static [&'static str] {
        match self {
            Self::RawSessionPrep => &["session_prep", "balance", "tonal_eq", "dynamics", "space_depth"],
            Self::PrepIntent => &["balance", "tonal_eq", "dynamics", "space_depth", "mastering"],
            Self::StaticBalance => &["balance"],
            Self::CleanupFilters => &["tonal_eq"],
            Self::SubtractiveEq => &["tonal_eq"],
            Self::Dynamics => &["dynamics"],
            Self::TonalEnhancement => &["tonal_eq"],
            Self::DepthSpace => &["space_depth", "balance"],
            Self::SectionAutomation => &["region_automation", "balance"],
            Self::MixBusLoudness => &["mastering", "balance"],
        }
    }
    fn instructions(&self) -> &'static str {
        match self {
            Self::RawSessionPrep => {
                "Stage 0 — RAW SESSION PREP. \
                 Treat this as a real raw multitrack session that may contain 30-80 tracks, alternates, doubles, room mics, overdubs, noisy takes, and repeated instruments. \
                 First organize, then create a practical rough-mix starting point. Use rename_track and set_track_role to classify tracks using concise names such as Kick In, Snare Top, OH L, Room, Bass DI, Lead Vocal, BV L, Guitar L. \
                 Mute only obvious junk/empty/duplicate alternate tracks when silencePercent is very high or the name clearly indicates an alternate that should not play. \
                 Use set_track_pan for a sensible layout: kick/snare/bass/lead vocal center; overheads/rooms/stereo pairs wide; backing vocals and guitars spread; keep low-end centered. \
                 Use conservative set_track_gain to establish a rough hierarchy; do not simply raise/lower everything equally. \
                 Add only basic safety processing where obvious: high-pass rumble on non-kick/non-bass tracks, light compression on uneven vocals/bass/drums, modest reverb/delay sends for vocals/rooms/support. \
                 This stage may emit many actions, but each must be purposeful and reversible. Do not delete tracks."
            }
            Self::PrepIntent => {
                "Stage 1 — PREP / INTENT. \
                 Identify the musical hierarchy from names, roles, and audio analysis: lead elements, groove, low-end foundation, support layers, and texture. \
                 This stage is mostly observational. Emit no actions unless something is objectively unsafe, such as a clearly clipping track that needs a small gain reduction. \
                 Do NOT rebalance, EQ, compress, add sends, or master. If the session is already-mixed AI stems, preserve that hierarchy."
            }
            Self::StaticBalance => {
                "Stage 2 — STATIC BALANCE. \
                 Use faders and pan only to establish musical hierarchy: lead/hook, groove, low-end foundation, support layers, and texture. \
                 Use set_track_gain (absolute) and set_track_pan only. Do NOT target equal loudness across tracks. \
                 For already-mixed AI stems, make only tiny moves (usually +/-0.5 to +/-1.5 dB, never more than +/-2 dB unless clipping/headroom demands it). \
                 Backing vocals, doubles, pads, rooms, and effects may intentionally sit far below lead vocals. \
                 DO NOT use EQ, compression, sends, filters, automation, or master actions in this stage."
            }
            Self::CleanupFilters => {
                "Stage 3 — CLEANUP FILTERS. \
                 Apply high-pass filters only where rumble or low-frequency masking is likely. Do not filter every non-bass stem automatically when the source is already mixed. \
                 Typical high-pass range is 60–120 Hz for non-bass, non-kick, non-sub tracks. \
                 Apply a low-pass at 16–18 kHz to harsh top-end sources only when their `spectralCentroidHz` > 5000 and `bandEnergy.high` > 0.35. \
                 Pick slopeDbOct = 12 unless the track is obviously rumbly (then 24). \
                 Use set_high_pass and set_low_pass. \
                 DO NOT use EQ band, compression, sends, gain, or pan actions in this stage."
            }
            Self::SubtractiveEq => {
                "Stage 4 — SUBTRACTIVE EQ / MASKING. \
                 For each track with a measurable problem, apply ONE narrow EQ cut: \
                 - mud at 200-400 Hz when bandEnergy.low > 0.55 AND bandEnergy.mid < 0.3 \
                 - boxiness at 400-700 Hz when bandEnergy.mid > 0.6 \
                 - harshness at 2-5 kHz when spectralCentroidHz > 3500 AND the track is not a cymbal or hi-hat \
                 Use cuts to create separation between competing sources. No boosts in this stage. Use set_eq_band with negative gainDb."
            }
            Self::Dynamics => {
                "Stage 5 — DYNAMICS CONTROL. \
                 Apply set_compressor ONLY to tracks where dynamicRangeDb > 10 AND silencePercent < 30. \
                 Use compression to solve uneven dynamics, preserve transients, or add intentional glue/character. Use the per-role compressor presets in the philosophy block. Skip everything else. \
                 No EQ, no sends, no gain in this stage."
            }
            Self::TonalEnhancement => {
                "Stage 6 — TONAL ENHANCEMENT. \
                 Gentle boosts only: presence on vocals (set_eq_band at 2.5-4 kHz +1 to +2 dB), \
                 air on cymbals/strings/synths (high shelf at 10-12 kHz), \
                 weight on bass (low shelf at 80-100 Hz). \
                 Do not boost the same range on many tracks. No cuts in this stage. No new actions on tracks that already got tonal moves earlier."
            }
            Self::DepthSpace => {
                "Stage 7 — DEPTH & SPACE. \
                 Use reverb/delay sends to place sources front-to-back, not just to add effects. Vary by role: \
                 leads get enough space without losing focus, bass/kick stay dry, overheads/rooms only get small short reverb. \
                 Use set_reverb_send and set_delay_send. Don't touch anything else."
            }
            Self::SectionAutomation => {
                "Stage 8 — SECTION AUTOMATION. \
                 Only act when sections were detected. Use create_region and apply_section_automation to fix section-level energy problems, such as a chorus that is unintentionally quieter than an adjacent verse. \
                 Preserve intentional arrangement contrast. Keep rides subtle: usually +/-0.5 to +/-2 dB, rarely more than +/-3 dB. \
                 Do not use EQ, compression, sends, filters, or master actions in this stage."
            }
            Self::MixBusLoudness => {
                "Stage 9 — MIX BUS / LOUDNESS. \
                 Use master/headroom analysis. If headroom < 3 dB → adjust_master_gain by a negative delta. \
                 If integrated LUFS is more than 2 dB off the profile target and headroom allows it → adjust_master_gain to move toward it. \
                 Do not use master gain to fix track-level balance problems. \
                 Cap any single move at ±4 dB."
            }
        }
    }
    fn max_actions(&self) -> usize {
        match self {
            Self::RawSessionPrep => 250,
            Self::PrepIntent => 10,
            Self::StaticBalance => 200,
            Self::CleanupFilters => 200,
            Self::SubtractiveEq => 60,
            Self::Dynamics => 50,
            Self::TonalEnhancement => 40,
            Self::DepthSpace => 80,
            Self::SectionAutomation => 40,
            Self::MixBusLoudness => 20,
        }
    }

    fn action_schema(&self) -> &'static str {
        match self {
            Self::RawSessionPrep => {
                r#"Allowed action objects for this stage:
- {"tool":"rename_track","trackId":"tk0","name":"Lead Vocal"}
- {"tool":"set_track_role","trackId":"tk0","role":"lead_vocal"}
- {"tool":"mute_track","trackId":"tk0","muted":true}
- {"tool":"set_track_gain","trackId":"tk0","gainDb":-7.0}
- {"tool":"set_track_pan","trackId":"tk0","pan":0.0}
- {"tool":"set_high_pass","trackId":"tk0","frequencyHz":80,"slopeDbOct":12}
- {"tool":"set_compressor","trackId":"tk0","thresholdDb":-18,"ratio":2.5,"attackMs":20,"releaseMs":160,"kneeDb":6,"makeupDb":0}
- {"tool":"set_reverb_send","trackId":"tk0","levelDb":-24}
- {"tool":"set_delay_send","trackId":"tk0","levelDb":-28}"#
            }
            Self::PrepIntent => {
                r#"Allowed action objects for this stage:
- {"tool":"set_track_gain","trackId":"tk0","gainDb":-2.0}"#
            }
            Self::StaticBalance => {
                r#"Allowed action objects for this stage:
- {"tool":"set_track_gain","trackId":"tk0","gainDb":-7.0}
- {"tool":"set_track_pan","trackId":"tk0","pan":0.0}"#
            }
            Self::CleanupFilters => {
                r#"Allowed action objects for this stage:
- {"tool":"set_high_pass","trackId":"tk0","frequencyHz":80,"slopeDbOct":12}
- {"tool":"set_low_pass","trackId":"tk0","frequencyHz":18000,"slopeDbOct":12}"#
            }
            Self::SubtractiveEq | Self::TonalEnhancement => {
                r#"Allowed action objects for this stage:
- {"tool":"set_eq_band","trackId":"tk0","band":1,"frequencyHz":400,"gainDb":-2.0,"q":1.0}"#
            }
            Self::Dynamics => {
                r#"Allowed action objects for this stage:
- {"tool":"set_compressor","trackId":"tk0","thresholdDb":-18.0,"ratio":2.0,"attackMs":20.0,"releaseMs":160.0,"kneeDb":6.0,"makeupDb":0.0}"#
            }
            Self::DepthSpace => {
                r#"Allowed action objects for this stage:
- {"tool":"set_reverb_send","trackId":"tk0","levelDb":-22.0}
- {"tool":"set_delay_send","trackId":"tk0","levelDb":-24.0}"#
            }
            Self::SectionAutomation => {
                r#"Allowed action objects for this stage:
- {"tool":"create_region","name":"Chorus 1","startSample":0,"endSample":480000,"trackIds":["tk0"]}
- {"tool":"apply_section_automation","regionId":"rg0","targetTrackId":"tk0","param":"gainDb","value":1.0}"#
            }
            Self::MixBusLoudness => {
                r#"Allowed action objects for this stage:
- {"tool":"adjust_master_gain","deltaDb":-2.0}
- {"tool":"set_master_gain","gainDb":-2.0}"#
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageReport {
    pub stage_id: String,
    pub display_name: String,
    pub status: String, // "running" | "complete" | "skipped" | "error"
    pub action_count: usize,
    pub explanation: Option<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub tokens: u32,
    pub elapsed_ms: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StageEnvelope {
    #[serde(default)]
    actions: Vec<MixAction>,
    #[serde(default)]
    rationale: Option<String>,
}

const ACTION_TIMEOUT_MS: u64 = 600_000;

pub async fn run_stage(
    config: &Config,
    store: Arc<std::sync::Mutex<SessionStore>>,
    session_id: &str,
    stage: AutoMixStage,
    observer: Arc<dyn LlmObserver>,
) -> Result<StageReport, String> {
    let mut project: MixProject = store
        .lock()
        .map_err(|e| e.to_string())?
        .get_project(session_id)?;

    let base_url = config.ollama_base_url.trim().trim_end_matches('/').to_string();
    let model = config.ollama_model.trim().to_string();
    if base_url.is_empty() || model.is_empty() {
        return Err("Ollama URL or model not configured.".into());
    }

    let accumulator = Arc::new(AccumulatingObserver::new(observer));
    let observer: Arc<dyn LlmObserver> = accumulator.clone();

    let prompt = build_stage_prompt(&project.session, stage);
    let track_aliases: Vec<(String, String)> = project
        .session
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("tk{i}"), t.id.clone()))
        .collect();
    let aliased_prompt = substitute_quoted(&prompt, &track_aliases, true);

    let phase = stage.id();
    let call = match ollama_generate(&base_url, &model, &aliased_prompt, ACTION_TIMEOUT_MS, phase, observer.as_ref()).await {
        Some(c) => c,
        None => {
            return Ok(StageReport {
                stage_id: stage.id().into(),
                display_name: stage.display_name().into(),
                status: "error".into(),
                action_count: 0,
                explanation: None,
                warnings: Vec::new(),
                error: Some(format!("Ollama did not respond ({model}).")),
                tokens: 0,
                elapsed_ms: 0,
            });
        }
    };
    let raw_real = substitute_quoted(&call.response, &track_aliases, false);
    let extracted = extract_json_object(&raw_real).unwrap_or(raw_real.clone());

    let env = match parse_stage_envelope(&project.session, &extracted) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[auto-mix:{}] could not parse stage output: {e}\n{extracted}", stage.id());
            return Ok(StageReport {
                stage_id: stage.id().into(),
                display_name: stage.display_name().into(),
                status: "error".into(),
                action_count: 0,
                explanation: None,
                warnings: Vec::new(),
                error: Some(format!("could not parse stage output: {e}")),
                tokens: 0,
                elapsed_ms: 0,
            });
        }
    };

    let mut actions = env.actions;
    let actions_total = actions.len();
    if actions.len() > stage.max_actions() {
        actions.truncate(stage.max_actions());
    }
    let mut warnings = limit_ai_stem_moves(&project.session, &mut actions);
    warnings.extend(clamp_actions(&mut actions));
    if let Err(message) = validate_actions(&project.session, &actions) {
        return Ok(StageReport {
            stage_id: stage.id().into(),
            display_name: stage.display_name().into(),
            status: "error".into(),
            action_count: 0,
            explanation: None,
            warnings,
            error: Some(message),
            tokens: 0,
            elapsed_ms: 0,
        });
    }
    let _ = expand_skills_from_actions(stage.skills().iter().map(|s| s.to_string()).collect(), &actions);

    let n = actions.len();
    let explanation = env
        .rationale
        .unwrap_or_else(|| format!("Auto-mix stage: {}.", stage.display_name()));
    if n > 0 {
        apply_actions(&mut project, &actions, HistorySource::Assistant, Some(format!("[auto] {explanation}")))?;
    }
    store.lock().map_err(|e| e.to_string())?.save(&project)?;

    let stats = accumulator.snapshot();
    Ok(StageReport {
        stage_id: stage.id().into(),
        display_name: stage.display_name().into(),
        status: if n == 0 { "skipped" } else { "complete" }.into(),
        action_count: n,
        explanation: Some(if actions_total > n {
            format!("{explanation} (truncated {} → {} actions)", actions_total, n)
        } else {
            explanation
        }),
        warnings,
        error: None,
        tokens: stats.prompt_tokens + stats.response_tokens,
        elapsed_ms: stats.elapsed_ms,
    })
}

fn build_stage_prompt(session: &MixSession, stage: AutoMixStage) -> String {
    let allowed_skills: Vec<String> = stage.skills().iter().map(|s| (*s).to_string()).collect();
    let snapshot = build_capability_snapshot(session, &allowed_skills);
    let profile = profile_block(&session.mixer_profile);
    let fundamentals = mixing_fundamentals_block();
    let ai_preservation = ai_stem_preservation_block(session);
    let sections = sections_block(&session.sections, session.bpm);
    let _ = skill_catalog();
    format!(
        "{profile}{fundamentals}{ai_preservation}You are running ONE narrow stage of an automated mix. Return ONLY JSON \
         {{\"actions\": [...], \"rationale\": \"...\"}}. NO extra prose.\n\n\
         STAGE INSTRUCTIONS:\n{instr}\n\n\
         ACTION SCHEMA:\n{schema}\n\n\
         Hard output rules:\n\
         - Use ONLY the exact `tool` strings shown in ACTION SCHEMA.\n\
         - Do NOT use skill names as tools: never output balance, tonal_eq, dynamics, mastering, space_depth, or namespaced tools like balance.set_track_gain.\n\
         - Do NOT wrap action fields in `params` or `arguments`.\n\
         - Numeric fields must be JSON numbers, not strings or arrays.\n\
         - Every action must include every field shown for its tool; do not omit frequencyHz, levelDb, thresholdDb, ratio, attackMs, releaseMs, kneeDb, or makeupDb.\n\
         - Track ids are short tokens like `tk0`, `tk1`. Up to {max} actions in this stage; the \
         orchestrator will truncate if you emit more. If nothing in this stage applies, return \
         {{\"actions\": []}}.\n\n\
         Capability snapshot:\n{snap}\n\n\
         Tracks:\n{tracks}\n\n\
         {sections}",
        profile = profile,
        fundamentals = fundamentals,
        ai_preservation = ai_preservation,
        instr = stage.instructions(),
        schema = stage.action_schema(),
        max = stage.max_actions(),
        snap = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()),
        tracks = serde_json::to_string(
            &session
                .tracks
                .iter()
                .map(|t| serde_json::json!({"id": t.id, "name": t.name, "role": t.role, "aiGenerated": t.ai_generated, "gainDb": t.gain_db, "pan": t.pan}))
                .collect::<Vec<_>>()
        )
        .unwrap_or_else(|_| "[]".into()),
        sections = sections,
    )
}

fn limit_ai_stem_moves(session: &MixSession, actions: &mut [MixAction]) -> Vec<String> {
    if session.tracks.is_empty() {
        return Vec::new();
    }
    let ai_count = session.tracks.iter().filter(|track| track.ai_generated).count();
    if ai_count * 2 < session.tracks.len() {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    for action in actions {
        match action {
            MixAction::SetTrackGain { track_id, gain_db } => {
                let Some(track) = session.tracks.iter().find(|track| track.id == *track_id) else {
                    continue;
                };
                if !track.ai_generated {
                    continue;
                }
                let max_down = allowed_ai_gain_reduction(session, track);
                let min = track.gain_db - max_down;
                let max = track.gain_db + 2.0;
                let before = *gain_db;
                *gain_db = gain_db.clamp(min, max);
                if (*gain_db - before).abs() > f32::EPSILON {
                    warnings.push(format!(
                        "clamped AI-derived stem gain for '{}' from {:.1} dB to {:.1} dB",
                        track.name, before, *gain_db
                    ));
                }
            }
            MixAction::AdjustTrackGain { track_id, delta_db } => {
                let Some(track) = session.tracks.iter().find(|track| track.id == *track_id) else {
                    continue;
                };
                if !track.ai_generated {
                    continue;
                }
                let max_down = allowed_ai_gain_reduction(session, track);
                let before = *delta_db;
                *delta_db = delta_db.clamp(-max_down, 2.0);
                if (*delta_db - before).abs() > f32::EPSILON {
                    warnings.push(format!(
                        "clamped AI-derived stem gain delta for '{}' from {:.1} dB to {:.1} dB",
                        track.name, before, *delta_db
                    ));
                }
            }
            MixAction::SetCompressor { track_id, ratio, attack_ms, release_ms, .. } => {
                let Some(track) = session.tracks.iter().find(|track| track.id == *track_id) else {
                    continue;
                };
                if !track.ai_generated {
                    continue;
                }
                let before = (*ratio, *attack_ms, *release_ms);
                *ratio = ratio.min(2.0);
                *attack_ms = attack_ms.max(20.0);
                *release_ms = release_ms.max(150.0);
                if before != (*ratio, *attack_ms, *release_ms) {
                    warnings.push(format!("softened compressor on AI-derived stem '{}'", track.name));
                }
            }
            _ => {}
        }
    }
    warnings
}

fn allowed_ai_gain_reduction(session: &MixSession, track: &crate::model::Track) -> f32 {
    let current_peak = session
        .source_files
        .iter()
        .find(|source| source.id == track.source_file_id)
        .map(|source| source.analysis.peak_db + track.gain_db)
        .unwrap_or(-12.0);
    if current_peak > -1.0 {
        (current_peak + 3.0).max(2.0)
    } else {
        2.0
    }
}

fn parse_stage_envelope(session: &MixSession, raw: &str) -> Result<StageEnvelope, String> {
    let normalized_raw = normalize_json_quotes(raw);
    let mut value: Value = match serde_json::from_str(&normalized_raw) {
        Ok(value) => value,
        Err(first_error) => {
            let repaired = strip_invalid_numeric_closing_quotes(&normalized_raw);
            if repaired == normalized_raw {
                return Err(first_error.to_string());
            }
            serde_json::from_str(&repaired).map_err(|_| first_error.to_string())?
        }
    };
    if value.is_array() {
        value = serde_json::json!({ "actions": value });
    }
    normalize_stage_value(session, &mut value);
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn normalize_json_quotes(raw: &str) -> String {
    raw.replace(['“', '”'], "\"")
}

fn strip_invalid_numeric_closing_quotes(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = raw.chars().collect();

    for (idx, ch) in chars.iter().copied().enumerate() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            let prev = out.chars().rev().find(|c| !c.is_whitespace());
            let next = chars.iter().skip(idx + 1).find(|c| !c.is_whitespace()).copied();
            if prev.is_some_and(|c| c.is_ascii_digit()) && matches!(next, Some(',') | Some('}') | Some(']')) {
                continue;
            }
            in_string = true;
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

fn normalize_stage_value(session: &MixSession, value: &mut Value) {
    normalize_object_keys(value);
    let Some(actions) = value.get_mut("actions").and_then(Value::as_array_mut) else {
        return;
    };
    for action in actions {
        normalize_object_keys(action);
        normalize_action(session, action);
    }
}

fn normalize_object_keys(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    rename_key(obj, "track_id", "trackId");
    rename_key(obj, "id", "trackId");
    rename_key(obj, "target_track_id", "targetTrackId");
    rename_key(obj, "region_id", "regionId");
    rename_key(obj, "track_ids", "trackIds");
    rename_key(obj, "start_sample", "startSample");
    rename_key(obj, "end_sample", "endSample");
    rename_key(obj, "gain_db", "gainDb");
    rename_key(obj, "delta_db", "deltaDb");
    rename_key(obj, "ai_generated", "aiGenerated");
    rename_key(obj, "frequency", "frequencyHz");
    rename_key(obj, "frequency_hz", "frequencyHz");
    rename_key(obj, "frequencyHZ", "frequencyHz");
    rename_key(obj, "freqHz", "frequencyHz");
    rename_key(obj, "freq_hz", "frequencyHz");
    rename_key(obj, "freq", "frequencyHz");
    rename_key(obj, "hz", "frequencyHz");
    rename_key(obj, "slope", "slopeDbOct");
    rename_key(obj, "slope_db_oct", "slopeDbOct");
    rename_key(obj, "slopeDbPerOctave", "slopeDbOct");
    rename_key(obj, "slopeDbOctave", "slopeDbOct");
    rename_key(obj, "bandIndex", "band");
    rename_key(obj, "band_index", "band");
    rename_key(obj, "bandId", "band");
    rename_key(obj, "band_id", "band");
    rename_key(obj, "threshold_db", "thresholdDb");
    rename_key(obj, "attack_ms", "attackMs");
    rename_key(obj, "release_ms", "releaseMs");
    rename_key(obj, "knee_db", "kneeDb");
    rename_key(obj, "makeup_db", "makeupDb");
    rename_key(obj, "level_db", "levelDb");
    rename_key(obj, "reverbDb", "levelDb");
    rename_key(obj, "delayDb", "levelDb");
    rename_key(obj, "delay_ms", "delayMs");
    rename_key(obj, "feedback_pct", "feedbackPct");
    rename_key(obj, "wet_db", "wetDb");
    rename_key(obj, "processor_id", "processorId");
    rename_key(obj, "param_id", "paramId");
}

fn rename_key(obj: &mut serde_json::Map<String, Value>, from: &str, to: &str) {
    if obj.contains_key(to) {
        return;
    }
    if let Some(value) = obj.remove(from) {
        obj.insert(to.to_string(), value);
    }
}

fn normalize_action(session: &MixSession, action: &mut Value) {
    let Some(obj) = action.as_object_mut() else {
        return;
    };
    flatten_params(obj);
    normalize_object_map_keys(obj);
    coerce_numeric_fields(obj);
    normalize_tool_name(obj);
    let tool = obj.get("tool").and_then(Value::as_str).unwrap_or_default();
    match tool {
        "set_eq_band" => normalize_eq_action(session, obj),
        "set_high_pass" | "set_low_pass" => {
            obj.entry("slopeDbOct".to_string()).or_insert(Value::from(12));
        }
        "set_compressor" => normalize_compressor_action(session, obj),
        _ => {}
    }
}

fn flatten_params(obj: &mut serde_json::Map<String, Value>) {
    let Some(params) = obj.remove("params") else {
        return;
    };
    let Value::Object(params) = params else {
        return;
    };
    for (key, value) in params {
        obj.entry(key).or_insert(value);
    }
}

fn normalize_object_map_keys(obj: &mut serde_json::Map<String, Value>) {
    rename_key(obj, "track_id", "trackId");
    rename_key(obj, "id", "trackId");
    rename_key(obj, "target_track_id", "targetTrackId");
    rename_key(obj, "region_id", "regionId");
    rename_key(obj, "track_ids", "trackIds");
    rename_key(obj, "start_sample", "startSample");
    rename_key(obj, "end_sample", "endSample");
    rename_key(obj, "gain_db", "gainDb");
    rename_key(obj, "delta_db", "deltaDb");
    rename_key(obj, "ai_generated", "aiGenerated");
    rename_key(obj, "frequency", "frequencyHz");
    rename_key(obj, "frequency_hz", "frequencyHz");
    rename_key(obj, "frequencyHZ", "frequencyHz");
    rename_key(obj, "freqHz", "frequencyHz");
    rename_key(obj, "freq_hz", "frequencyHz");
    rename_key(obj, "freq", "frequencyHz");
    rename_key(obj, "hz", "frequencyHz");
    rename_key(obj, "slope", "slopeDbOct");
    rename_key(obj, "slope_db_oct", "slopeDbOct");
    rename_key(obj, "slopeDbPerOctave", "slopeDbOct");
    rename_key(obj, "slopeDbOctave", "slopeDbOct");
    rename_key(obj, "bandIndex", "band");
    rename_key(obj, "band_index", "band");
    rename_key(obj, "bandId", "band");
    rename_key(obj, "band_id", "band");
    rename_key(obj, "threshold_db", "thresholdDb");
    rename_key(obj, "attack_ms", "attackMs");
    rename_key(obj, "release_ms", "releaseMs");
    rename_key(obj, "knee_db", "kneeDb");
    rename_key(obj, "makeup_db", "makeupDb");
    rename_key(obj, "level_db", "levelDb");
    rename_key(obj, "reverbDb", "levelDb");
    rename_key(obj, "delayDb", "levelDb");
    rename_key(obj, "delay_ms", "delayMs");
    rename_key(obj, "feedback_pct", "feedbackPct");
    rename_key(obj, "wet_db", "wetDb");
    rename_key(obj, "processor_id", "processorId");
    rename_key(obj, "param_id", "paramId");
}

fn normalize_tool_name(obj: &mut serde_json::Map<String, Value>) {
    if let Some(function) = obj.get("function").and_then(Value::as_str).map(str::to_string) {
        obj.insert("tool".to_string(), Value::from(function));
    }
    let Some(tool) = obj.get("tool").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let bare = tool.rsplit('.').next().unwrap_or(&tool);
    let bare_snake = camel_to_snake(bare);
    let normalized = match bare_snake.as_str() {
        "balance" if obj.contains_key("pan") => "set_track_pan",
        "balance" if obj.contains_key("gainDb") => "set_track_gain",
        "set_pan" => "set_track_pan",
        "pan" => "set_track_pan",
        "set_gain" => "set_track_gain",
        "track_gain" => "set_track_gain",
        "high_pass" => "set_high_pass",
        "low_pass" => "set_low_pass",
        "tonal_eq" => "set_eq_band",
        "eq" => "set_eq_band",
        "compressor" => "set_compressor",
        "reverb_send" => "set_reverb_send",
        "delay_send" => "set_delay_send",
        other => other,
    };
    if normalized != tool {
        obj.insert("tool".to_string(), Value::from(normalized));
    }
}

fn camel_to_snake(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (idx, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if idx > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn coerce_numeric_fields(obj: &mut serde_json::Map<String, Value>) {
    for key in [
        "gainDb",
        "deltaDb",
        "pan",
        "frequencyHz",
        "slopeDbOct",
        "band",
        "q",
        "thresholdDb",
        "ratio",
        "attackMs",
        "releaseMs",
        "kneeDb",
        "makeupDb",
        "levelDb",
        "delayMs",
        "feedbackPct",
        "wetDb",
    ] {
        if let Some(text) = obj.get(key).and_then(Value::as_str).map(str::trim) {
            if let Ok(n) = text.parse::<i64>() {
                obj.insert(key.to_string(), Value::from(n));
            } else if let Ok(n) = text.parse::<f64>() {
                obj.insert(key.to_string(), Value::from(n));
            }
        } else if let Some(first) = obj
            .get(key)
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find(|item| item.is_number()))
            .cloned()
        {
            obj.insert(key.to_string(), first);
        }
    }
}

fn normalize_eq_action(session: &MixSession, obj: &mut serde_json::Map<String, Value>) {
    let track_id = obj.get("trackId").and_then(Value::as_str);
    let band = obj.get("band").and_then(Value::as_u64).map(|b| b as usize);
    let current_band = track_id
        .and_then(|id| session.tracks.iter().find(|t| t.id == id))
        .and_then(|track| band.and_then(|b| track.chain.eq.get(b)));

    if !obj.contains_key("frequencyHz") {
        let fallback = current_band.map(|b| b.frequency_hz).or_else(|| band.map(default_eq_frequency));
        if let Some(frequency) = fallback {
            obj.insert("frequencyHz".to_string(), Value::from(frequency));
        }
    }
    if !obj.contains_key("q") {
        obj.insert("q".to_string(), Value::from(current_band.map(|b| b.q).unwrap_or(1.0)));
    }
}

fn default_eq_frequency(band: usize) -> f32 {
    match band {
        0 => 100.0,
        1 => 400.0,
        2 => 2500.0,
        3 => 8000.0,
        _ => 1000.0,
    }
}

fn normalize_compressor_action(session: &MixSession, obj: &mut serde_json::Map<String, Value>) {
    let current = obj
        .get("trackId")
        .and_then(Value::as_str)
        .and_then(|id| session.tracks.iter().find(|t| t.id == id))
        .map(|track| &track.chain.compressor);
    if !obj.contains_key("attackMs") {
        obj.insert("attackMs".to_string(), Value::from(current.map(|c| c.attack_ms).unwrap_or(20.0)));
    }
    if !obj.contains_key("releaseMs") {
        obj.insert("releaseMs".to_string(), Value::from(current.map(|c| c.release_ms).unwrap_or(160.0)));
    }
    if !obj.contains_key("kneeDb") {
        obj.insert("kneeDb".to_string(), Value::from(current.map(|c| c.knee_db).unwrap_or(6.0)));
    }
    if !obj.contains_key("makeupDb") {
        obj.insert("makeupDb".to_string(), Value::from(current.map(|c| c.makeup_db).unwrap_or(0.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{default_master, make_track};
    use crate::actions::{clamp_actions, validate_actions};
    use crate::assistant::{extract_json_object, LlmCallStats};
    use std::sync::Mutex;

    fn test_session() -> MixSession {
        let mut track = make_track("source".into(), "Lead Vocal".into(), 0);
        track.id = "track-a".into();
        MixSession {
            id: "session".into(),
            name: "Session".into(),
            sample_rate: 48_000,
            bpm: None,
            source_files: Vec::new(),
            tracks: vec![track],
            buses: Vec::new(),
            master: default_master(),
            regions: Vec::new(),
            markers: Vec::new(),
            sections: Vec::new(),
            mixer_profile: Default::default(),
        }
    }

    fn rich_test_session() -> MixSession {
        let roles = [
            ("kick", "Kick", -8.0, -18.0, 72.0, 0.82, 0.12, 0.06, 2.0, 14.0),
            ("snare", "Snare", -7.0, -20.0, 2100.0, 0.22, 0.55, 0.23, 3.0, 13.0),
            ("bass", "Bass", -6.5, -19.0, 130.0, 0.78, 0.18, 0.04, 1.0, 11.5),
            ("lead_vocal", "Lead Vocal", -9.5, -22.0, 3100.0, 0.12, 0.62, 0.26, 4.0, 12.5),
            ("guitar", "Guitar", -10.0, -24.0, 4200.0, 0.08, 0.66, 0.26, 6.0, 15.0),
            ("keys", "Keys", -12.0, -25.0, 900.0, 0.38, 0.58, 0.04, 5.0, 14.0),
        ];
        let mut tracks = Vec::new();
        let mut source_files = Vec::new();
        for (i, (role, name, peak, rms, centroid, low, mid, high, silence, dr)) in roles.iter().enumerate() {
            let source_id = format!("source-{i}");
            let mut track = make_track(source_id.clone(), (*name).into(), i);
            track.id = format!("track-{i}");
            track.role = Some((*role).into());
            track.ai_generated = true;
            source_files.push(crate::model::SourceFile {
                id: source_id,
                original_name: format!("{name}.wav"),
                cache_path: String::new(),
                peak_path: String::new(),
                duration_samples: 48_000 * 120,
                sample_rate: 48_000,
                channels: 2,
                analysis: crate::model::TrackAnalysis {
                    peak_db: *peak,
                    rms_db: *rms,
                    lufs_estimate: *rms,
                    spectral_centroid_hz: *centroid,
                    low_energy: *low,
                    mid_energy: *mid,
                    high_energy: *high,
                    silence_percent: *silence,
                    dynamic_range_db: *dr,
                },
                peak_preview: Vec::new(),
            });
            tracks.push(track);
        }
        MixSession {
            id: "offline-smoke".into(),
            name: "Offline smoke".into(),
            sample_rate: 48_000,
            bpm: Some(120.0),
            source_files,
            tracks,
            buses: Vec::new(),
            master: default_master(),
            regions: Vec::new(),
            markers: Vec::new(),
            sections: Vec::new(),
            mixer_profile: Default::default(),
        }
    }

    struct TestObserver {
        text: Mutex<String>,
    }

    impl TestObserver {
        fn new() -> Self {
            Self { text: Mutex::new(String::new()) }
        }
    }

    impl LlmObserver for TestObserver {
        fn chunk(&self, _phase: &str, text: &str) {
            self.text.lock().unwrap().push_str(text);
        }

        fn stats(&self, _phase: &str, _stats: &LlmCallStats) {}
    }

    #[test]
    fn parses_band_index_alias() {
        let raw = r#"{"actions":[{"tool":"set_eq_band","trackId":"track-a","bandIndex":1,"frequencyHz":400,"q":1.0,"gainDb":-2.0}],"rationale":"cut"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert_eq!(env.actions.len(), 1);
        match &env.actions[0] {
            MixAction::SetEqBand { band, frequency_hz, gain_db, .. } => {
                assert_eq!(*band, 1);
                assert_eq!(*frequency_hz, 400.0);
                assert_eq!(*gain_db, -2.0);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn fills_missing_eq_frequency_from_current_band() {
        let raw = r#"{"actions":[{"tool":"set_eq_band","trackId":"track-a","band":2,"gainDb":-2,"q":1.0}],"rationale":"cut"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        match &env.actions[0] {
            MixAction::SetEqBand { band, frequency_hz, .. } => {
                assert_eq!(*band, 2);
                assert_eq!(*frequency_hz, 2500.0);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn repairs_invalid_numeric_closing_quote() {
        let raw = r#"{"actions":[{"tool":"set_compressor","trackId":"track-a","thresholdDb":-18","ratio":2,"attackMs":100,"releaseMs":160,"kneeDb":6,"makeupDb":0}],"rationale":"compress"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        match &env.actions[0] {
            MixAction::SetCompressor { threshold_db, ratio, .. } => {
                assert_eq!(*threshold_db, -18.0);
                assert_eq!(*ratio, 2.0);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn repairs_smart_json_quotes() {
        let raw = "{\"actions\":[{\"tool\":\"set_track_gain\",\"trackId\":\"track-a\",\"gainDb\":0.5}],\"rationale\":\"small move.”}";
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetTrackGain { gain_db, .. } if *gain_db == 0.5));
    }

    #[test]
    fn normalizes_prefixed_tool_and_id_field() {
        let raw = r#"{"actions":[{"tool":"balance.set_track_gain","id":"track-a","gainDb":-7.0},{"tool":"balance.set_pan","id":"track-a","pan":0.4}],"rationale":"balance"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert_eq!(env.actions.len(), 2);
        assert!(matches!(&env.actions[0], MixAction::SetTrackGain { track_id, gain_db } if track_id == "track-a" && *gain_db == -7.0));
        assert!(matches!(&env.actions[1], MixAction::SetTrackPan { track_id, pan } if track_id == "track-a" && *pan == 0.4));
    }

    #[test]
    fn normalizes_balance_tool_and_numeric_strings() {
        let raw = r#"{"actions":[{"tool":"balance","trackId":"track-a","gainDb":"-11.7"},{"tool":"balance","trackId":"track-a","pan":"0.5"}],"rationale":"balance"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetTrackGain { gain_db, .. } if *gain_db == -11.7));
        assert!(matches!(&env.actions[1], MixAction::SetTrackPan { pan, .. } if *pan == 0.5));
    }

    #[test]
    fn normalizes_skill_tool_function_and_band_id() {
        let raw = r#"{"actions":[{"tool":"tonal_eq","function":"set_eq_band","trackId":"track-a","bandId":1,"gainDb":-3,"q":1}],"rationale":"cut"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetEqBand { band, frequency_hz, gain_db, .. } if *band == 1 && *frequency_hz == 400.0 && *gain_db == -3.0));
    }

    #[test]
    fn flattens_nested_params() {
        let raw = r#"{"actions":[{"tool":"set_compressor","trackId":"track-a","params":{"enabled":true,"thresholdDb":-18.0,"ratio":2.0,"attackMs":200.0,"releaseMs":400.0,"kneeDb":6.0,"makeupDb":0.0}}],"rationale":"compress"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetCompressor { threshold_db, attack_ms, release_ms, .. } if *threshold_db == -18.0 && *attack_ms == 200.0 && *release_ms == 400.0));
    }

    #[test]
    fn normalizes_send_level_aliases() {
        let raw = r#"{"actions":[{"tool":"set_reverb_send","trackId":"track-a","reverbDb":-20},{"tool":"set_delay_send","trackId":"track-a","delayDb":-22}],"rationale":"space"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetReverbSend { level_db, .. } if *level_db == -20.0));
        assert!(matches!(&env.actions[1], MixAction::SetDelaySend { level_db, .. } if *level_db == -22.0));
    }

    #[test]
    fn extracts_first_balanced_object_with_extra_closing_brace() {
        let raw = r#"{"actions":[{"tool":"set_reverb_send","trackId":"track-a","levelDb":-22.0}]}}"#;
        let extracted = extract_json_object(raw).expect("balanced object extracted");
        let env = parse_stage_envelope(&test_session(), &extracted).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetReverbSend { level_db, .. } if *level_db == -22.0));
    }

    #[test]
    fn normalizes_numeric_array_to_first_value() {
        let raw = r#"{"actions":[{"tool":"set_high_pass","trackId":"track-a","frequencyHz":80,"slopeDbOct":[12,24]}],"rationale":"filter"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetHighPass { slope_db_oct, .. } if *slope_db_oct == 12));
    }

    #[test]
    fn normalizes_camel_case_tool_names() {
        let raw = r#"{"actions":[{"tool":"setTrackGain","trackId":"track-a","gainDb":-7.0}],"rationale":"gain"}"#;
        let env = parse_stage_envelope(&test_session(), raw).expect("stage envelope parses");
        assert!(matches!(&env.actions[0], MixAction::SetTrackGain { gain_db, .. } if *gain_db == -7.0));
    }

    #[test]
    fn clamps_ai_stem_gain_moves() {
        let mut session = test_session();
        session.tracks[0].ai_generated = true;
        session.source_files.push(crate::model::SourceFile {
            id: "source".into(),
            original_name: "Lead Vocal.wav".into(),
            cache_path: String::new(),
            peak_path: String::new(),
            duration_samples: 48_000,
            sample_rate: 48_000,
            channels: 2,
            analysis: crate::model::TrackAnalysis {
                peak_db: -8.0,
                rms_db: -20.0,
                lufs_estimate: -20.0,
                spectral_centroid_hz: 2000.0,
                low_energy: 0.2,
                mid_energy: 0.6,
                high_energy: 0.2,
                silence_percent: 0.0,
                dynamic_range_db: 8.0,
            },
            peak_preview: Vec::new(),
        });
        let mut actions = vec![
            MixAction::SetTrackGain { track_id: "track-a".into(), gain_db: -7.0 },
            MixAction::AdjustTrackGain { track_id: "track-a".into(), delta_db: 6.0 },
        ];
        let warnings = limit_ai_stem_moves(&session, &mut actions);
        assert_eq!(warnings.len(), 2);
        assert!(matches!(&actions[0], MixAction::SetTrackGain { gain_db, .. } if *gain_db == -2.0));
        assert!(matches!(&actions[1], MixAction::AdjustTrackGain { delta_db, .. } if *delta_db == 2.0));
    }

    #[tokio::test]
    #[ignore = "requires local Ollama and intentionally calls the selected LLM"]
    async fn ollama_gpt_oss_20b_all_auto_mix_stages_smoke() {
        let base_url = std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".into());
        let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "gpt-oss:20b".into());
        let session = rich_test_session();
        let track_aliases: Vec<(String, String)> = session
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("tk{i}"), t.id.clone()))
            .collect();

        for stage in [
            AutoMixStage::PrepIntent,
            AutoMixStage::StaticBalance,
            AutoMixStage::CleanupFilters,
            AutoMixStage::SubtractiveEq,
            AutoMixStage::Dynamics,
            AutoMixStage::TonalEnhancement,
            AutoMixStage::DepthSpace,
            AutoMixStage::SectionAutomation,
            AutoMixStage::MixBusLoudness,
        ] {
            let prompt = substitute_quoted(&build_stage_prompt(&session, stage), &track_aliases, true);
            let observer = TestObserver::new();
            let call = ollama_generate(&base_url, &model, &prompt, 600_000, stage.id(), &observer)
                .await
                .unwrap_or_else(|| panic!("{}: Ollama did not respond", stage.id()));
            let raw_real = substitute_quoted(&call.response, &track_aliases, false);
            let extracted = extract_json_object(&raw_real).unwrap_or(raw_real);
            let env = parse_stage_envelope(&session, &extracted)
                .unwrap_or_else(|err| panic!("{}: parse failed: {err}\n{extracted}", stage.id()));
            let mut actions = env.actions;
            if actions.len() > stage.max_actions() {
                actions.truncate(stage.max_actions());
            }
            let warnings = clamp_actions(&mut actions);
            validate_actions(&session, &actions)
                .unwrap_or_else(|err| panic!("{}: validation failed: {err}\nwarnings: {warnings:?}\nactions: {actions:?}", stage.id()));
            eprintln!(
                "[auto-mix smoke] {} ok: {} actions, {} tokens",
                stage.id(),
                actions.len(),
                call.stats.prompt_tokens + call.stats.response_tokens
            );
        }
    }
}
