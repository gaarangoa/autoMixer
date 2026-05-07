use serde::Deserialize;
use serde_json::json;
use tokio::time::{timeout, Duration};

use crate::{
    actions::{apply_actions, redo, undo, validate_actions},
    capabilities::skill_catalog,
    config::Config,
    model::{AssistantRequest, AssistantResponse, HistorySource, MixAction, MixProject, MixSession},
};

const SKILL_TIMEOUT_MS: u64 = 600_000;
const ACTION_TIMEOUT_MS: u64 = 600_000;
const REPAIR_TIMEOUT_MS: u64 = 600_000;

pub async fn handle_assistant(
    config: Config,
    mut project: MixProject,
    request: AssistantRequest,
) -> Result<(AssistantResponse, MixProject), String> {
    let base_url = effective_base_url(&config, &request);
    let model = effective_model(&config, &request);

    if base_url.is_empty() || model.is_empty() {
        return Ok((
            AssistantResponse::Err {
                kind: "AgentNotReady".into(),
                message: "Set an Ollama URL and model in Settings before chatting with the assistant.".into(),
                raw_model_output: None,
            },
            project,
        ));
    }

    let selected_skills = match model_select_skills(&base_url, &model, &request, &project.session).await {
        Some(s) if !s.is_empty() => s,
        _ => {
            return Ok((
                AssistantResponse::Err {
                    kind: "AgentNotReady".into(),
                    message: format!(
                        "Could not reach the model at {base_url} ({model}) to choose skills. Check that Ollama is running and the model is pulled."
                    ),
                    raw_model_output: None,
                },
                project,
            ));
        }
    };

    if selected_skills.contains(&"safety_undo".to_string())
        && contains_any(&request.user_text, &["undo", "revert"])
    {
        let entry = undo(&mut project)?;
        return Ok((
            AssistantResponse::Ok {
                explanation: if entry.is_some() {
                    "Undid the last mix change.".into()
                } else {
                    "There was nothing to undo.".into()
                },
                actions: vec![MixAction::Undo],
                warnings: if entry.is_some() {
                    Vec::new()
                } else {
                    vec!["No history entry was available.".into()]
                },
                selected_skills,
                session: project.session.clone(),
                history: project.history.clone(),
                rationale: None,
                per_action_notes: None,
            },
            project,
        ));
    }

    if selected_skills.contains(&"safety_undo".to_string())
        && contains_any(&request.user_text, &["redo"])
    {
        let entry = redo(&mut project)?;
        return Ok((
            AssistantResponse::Ok {
                explanation: if entry.is_some() {
                    "Redid the last undone mix change.".into()
                } else {
                    "There was nothing to redo.".into()
                },
                actions: vec![MixAction::Redo],
                warnings: if entry.is_some() {
                    Vec::new()
                } else {
                    vec!["No redo entry was available.".into()]
                },
                selected_skills,
                session: project.session.clone(),
                history: project.history.clone(),
                rationale: None,
                per_action_notes: None,
            },
            project,
        ));
    }

    if selected_skills.iter().any(|s| s == "critique") {
        return match try_model_critique(&base_url, &model, &request, &project.session, &selected_skills).await {
            Ok(critique) => Ok((AssistantResponse::Critique { critique, selected_skills }, project)),
            Err(detail) => Ok((
                AssistantResponse::Err {
                    kind: "ModelInvalidJson".into(),
                    message: format!("Model {model} did not produce a valid critique: {}", detail.message),
                    raw_model_output: detail.raw,
                },
                project,
            )),
        };
    }

    let attempt =
        try_model_actions(&base_url, &model, &request, &project.session, &selected_skills).await;

    let (actions, rationale, per_action_notes) = match attempt.turn {
        Some(turn) if !turn.actions.is_empty() => (turn.actions, turn.rationale, turn.per_action_notes),
        Some(_) => {
            return Ok((
                AssistantResponse::Err {
                    kind: "ModelEmpty".into(),
                    message: format!(
                        "Model {model} returned a valid envelope but no actions. See raw output below."
                    ),
                    raw_model_output: attempt.raw,
                },
                project,
            ));
        }
        None => {
            let detail = attempt.parse_error.unwrap_or_else(|| "Unknown error".into());
            return Ok((
                AssistantResponse::Err {
                    kind: "ModelInvalidJson".into(),
                    message: format!("Model {model} did not produce a valid action envelope: {detail}"),
                    raw_model_output: attempt.raw,
                },
                project,
            ));
        }
    };

    if actions.is_empty() {
        return Ok((
            AssistantResponse::Clarification {
                question: "Which track should I change?".into(),
                reason: "The request did not clearly map to a track or selected track.".into(),
            },
            project,
        ));
    }

    if let Err(message) = validate_actions(&project.session, &actions) {
        return Ok((
            AssistantResponse::Err { kind: "InvalidActions".into(), message, raw_model_output: None },
            project,
        ));
    }
    let selected_skills = expand_skills_from_actions(selected_skills, &actions);

    let explanation = explain_actions(&actions, &project.session);
    apply_actions(
        &mut project,
        &actions,
        HistorySource::Assistant,
        Some(explanation.clone()),
    )?;

    Ok((
        AssistantResponse::Ok {
            explanation,
            actions,
            warnings: Vec::new(),
            selected_skills,
            session: project.session.clone(),
            history: project.history.clone(),
            rationale,
            per_action_notes,
        },
        project,
    ))
}

fn effective_base_url(config: &Config, request: &AssistantRequest) -> String {
    request
        .ollama_base_url
        .as_deref()
        .unwrap_or(&config.ollama_base_url)
        .trim()
        .trim_end_matches('/')
        .to_string()
}

fn effective_model(config: &Config, request: &AssistantRequest) -> String {
    request
        .ollama_model
        .as_deref()
        .unwrap_or(&config.ollama_model)
        .trim()
        .to_string()
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(serde::Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
}

/// Extract a JSON object from a free-form model response. Some models wrap
/// output in ```json fences or prose preamble; some omit anything visible
/// (gpt-oss with format:json). Try the raw string first, then look for the
/// first `{` and matching last `}`.
fn extract_json_object(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed.to_string());
    }
    let stripped = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if stripped.starts_with('{') && stripped.ends_with('}') {
        return Some(stripped.to_string());
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end > start {
        Some(trimmed[start..=end].to_string())
    } else {
        None
    }
}

async fn ollama_generate(
    base_url: &str,
    model: &str,
    prompt: &str,
    timeout_ms: u64,
) -> Option<String> {
    if base_url.is_empty() || model.is_empty() {
        return None;
    }
    let client = reqwest::Client::new();
    let resp = timeout(
        Duration::from_millis(timeout_ms),
        client
            .post(format!("{base_url}/api/generate"))
            .json(&GenerateRequest { model, prompt, stream: false })
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.json::<GenerateResponse>().await.ok()?;
    Some(body.response)
}

async fn model_select_skills(
    base_url: &str,
    model: &str,
    request: &AssistantRequest,
    session: &MixSession,
) -> Option<Vec<String>> {
    let prompt = format!(
        "You are a mix engineer routing user requests to skills. \
         Return JSON {{\"selectedSkillIds\":[\"...\"]}} only.\n\n\
         Available skills:\n{}\n\n\
         Tracks:\n{}\n\n\
         Selected track ids: {:?}\n\
         Selected region ids: {:?}\n\n\
         Request: {}\n\n\
         Pick 1-3 skills that best fit. If undo/redo, include safety_undo. \
         If render/export, include render_export. If section/region/chorus/verse, include region_automation. \
         If the user is asking for a rating, critique, review, evaluation, feedback, or score \
         (and NOT asking to apply changes), use ONLY the critique skill — do not combine it with action skills.",
        serde_json::to_string(&skill_catalog()).ok()?,
        serde_json::to_string(
            &session
                .tracks
                .iter()
                .map(|t| json!({"id": t.id, "name": t.name, "role": t.role}))
                .collect::<Vec<_>>()
        )
        .ok()?,
        request.selected_track_ids,
        request.selected_region_ids,
        request.user_text
    );
    let raw = ollama_generate(base_url, model, &prompt, SKILL_TIMEOUT_MS).await?;
    eprintln!("[assistant] skill raw response:\n{raw}");
    let extracted = extract_json_object(&raw)?;
    #[derive(Deserialize)]
    struct SkillEnvelope {
        #[serde(rename = "selectedSkillIds", alias = "selected_skill_ids")]
        ids: Vec<String>,
    }
    serde_json::from_str::<SkillEnvelope>(&extracted).ok().map(|e| {
        let mut v = e.ids;
        v.sort();
        v.dedup();
        v
    })
}

pub struct ModelTurn {
    pub actions: Vec<MixAction>,
    pub rationale: Option<String>,
    pub per_action_notes: Option<Vec<String>>,
}

pub struct ModelAttempt {
    pub turn: Option<ModelTurn>,
    pub raw: Option<String>,
    pub parse_error: Option<String>,
}

async fn try_model_actions(
    base_url: &str,
    model: &str,
    request: &AssistantRequest,
    session: &MixSession,
    selected_skills: &[String],
) -> ModelAttempt {
    let track_aliases: Vec<(String, String)> = session
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("tk{i}"), t.id.clone()))
        .collect();
    let region_aliases: Vec<(String, String)> = session
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| (format!("rg{i}"), r.id.clone()))
        .collect();
    let snapshot = build_capability_snapshot(session, selected_skills);
    let critique_block = match request.recent_critique.as_ref() {
        Some(c) => format!(
            "Recent critique you produced for this mix (use it as your guide; do not contradict your own findings unless the user disagrees):\n{}\n\n",
            serde_json::to_string(&json!({
                "mixScore": c.mix_score,
                "summary": c.summary,
                "headroomDb": c.headroom_db,
                "integratedLufsEstimate": c.integrated_lufs_estimate,
                "truePeakDbEstimate": c.true_peak_db_estimate,
                "mixIssues": c.mix_issues,
                "perTrack": c.per_track,
                "recommendedNextSteps": c.recommended_next_steps,
            })).unwrap_or_else(|_| "{}".into())
        ),
        None => String::new(),
    };
    let prompt = format!(
        "You are an assistant mix engineer. Return ONLY a JSON object with this exact shape:\n\
         {{\n  \"actions\": [ <action>, ... ],\n  \"rationale\": \"...\",\n  \"perActionNotes\": [ \"...\", ... ]\n}}\n\n\
         Each <action> is a flat JSON object — NO wrapper key like `tool_params`. \
         The discriminator is the `tool` field (snake_case). Every other field uses camelCase. \
         Track ids are short tokens like `tk0`, `tk1` (and region ids like `rg0`); use them \
         exactly as they appear in the Tracks list. Never invent or modify them.\n\n\
         Allowed `tool` values and their exact field shapes (use these tool names; do NOT use skill names like \"tonal_eq\" or \"dynamics\"):\n\
         - {{\"tool\":\"set_track_gain\",\"trackId\":\"...\",\"gainDb\":-3.0}}\n\
         - {{\"tool\":\"adjust_track_gain\",\"trackId\":\"...\",\"deltaDb\":1.5}}\n\
         - {{\"tool\":\"set_track_pan\",\"trackId\":\"...\",\"pan\":-0.3}}\n\
         - {{\"tool\":\"mute_track\",\"trackId\":\"...\",\"muted\":true}}\n\
         - {{\"tool\":\"solo_track\",\"trackId\":\"...\",\"solo\":true}}\n\
         - {{\"tool\":\"set_high_pass\",\"trackId\":\"...\",\"frequencyHz\":80,\"slopeDbOct\":12}}\n\
         - {{\"tool\":\"set_low_pass\",\"trackId\":\"...\",\"frequencyHz\":18000,\"slopeDbOct\":12}}\n\
         - {{\"tool\":\"set_eq_band\",\"trackId\":\"...\",\"band\":0,\"frequencyHz\":120,\"gainDb\":-2,\"q\":1.0}}\n\
         - {{\"tool\":\"set_compressor\",\"trackId\":\"...\",\"thresholdDb\":-18,\"ratio\":3,\"attackMs\":10,\"releaseMs\":120,\"kneeDb\":6,\"makeupDb\":1}}\n\
         - {{\"tool\":\"set_reverb_send\",\"trackId\":\"...\",\"levelDb\":-18}}\n\
         - {{\"tool\":\"set_delay_send\",\"trackId\":\"...\",\"levelDb\":-22}}\n\
         - {{\"tool\":\"set_region_gain\",\"regionId\":\"...\",\"trackId\":\"...\",\"gainDb\":-1.5}}\n\
         - {{\"tool\":\"undo\"}} | {{\"tool\":\"redo\"}} | {{\"tool\":\"render_mix\"}}\n\n\
         Example of a valid response for \"add presence and tame mud on the vocal\":\n\
         {{\"actions\":[\
         {{\"tool\":\"set_eq_band\",\"trackId\":\"<vocal-id>\",\"band\":1,\"frequencyHz\":300,\"gainDb\":-2.0,\"q\":1.1}},\
         {{\"tool\":\"set_eq_band\",\"trackId\":\"<vocal-id>\",\"band\":2,\"frequencyHz\":3000,\"gainDb\":1.5,\"q\":0.8}},\
         {{\"tool\":\"set_compressor\",\"trackId\":\"<vocal-id>\",\"thresholdDb\":-18,\"ratio\":2.5,\"attackMs\":12,\"releaseMs\":120,\"kneeDb\":6,\"makeupDb\":1.0}}],\
         \"rationale\":\"Cleans low-mid mud and adds presence; light compression evens dynamics.\",\
         \"perActionNotes\":[\"Cuts boxy 300 Hz buildup.\",\"Lifts presence at 3 kHz for clarity.\",\"Light glue compression.\"]}}\n\n\
         Selected skills: {}\n\n\
         Capability snapshot for selected skills:\n{}\n\n\
         Tracks:\n{}\n\n\
         Selected track ids: {:?}\n\
         Selected region ids: {:?}\n\n\
         Routing guidance: frequency/EQ/low/mid/high/air/presence/bright/dark/harsh/muddy/body \
         requests should use EQ/filter actions, not gain. Vocal upfront/presence/clarity requests usually \
         combine a subtle level move with presence EQ and light compression.\n\n\
         Audio analysis per track is included under track.audio. Use it to ground decisions: \
         spectralCentroidHz < 1500 = dark, > 3500 = bright; bandEnergy.low/mid/high are normalized \
         shares of energy (sum ≈ 1) — high low_energy with low high_energy = muddy; lufs around \
         -23 LUFS is broadcast loudness, lower means quieter; dynamicRangeDb < 6 is heavily \
         compressed material, > 14 is highly dynamic; peakDb close to 0 indicates limited headroom.\n\n\
         rationale: 1-3 sentences explaining the musical reason for the chosen moves. \
         perActionNotes: one short note per action in the same order. Both are required.\n\n\
         {}\
         Request: {}\n",
        serde_json::to_string(selected_skills).unwrap_or_else(|_| "[]".into()),
        serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()),
        serde_json::to_string(
            &session
                .tracks
                .iter()
                .map(|t| json!({"id": t.id, "name": t.name, "role": t.role, "gainDb": t.gain_db, "pan": t.pan}))
                .collect::<Vec<_>>()
        )
        .unwrap_or_else(|_| "[]".into()),
        request.selected_track_ids,
        request.selected_region_ids,
        critique_block,
        request.user_text
    );

    let prompt = substitute_quoted(&prompt, &track_aliases, true);
    let prompt = substitute_quoted(&prompt, &region_aliases, true);

    let Some(raw_aliased) = ollama_generate(base_url, model, &prompt, ACTION_TIMEOUT_MS).await else {
        return ModelAttempt { turn: None, raw: None, parse_error: Some("Ollama did not respond within the timeout.".into()) };
    };
    eprintln!("[assistant] action raw response:\n{raw_aliased}");
    let raw_real = substitute_quoted(&raw_aliased, &track_aliases, false);
    let raw_real = substitute_quoted(&raw_real, &region_aliases, false);

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ActionEnvelope {
        actions: Vec<MixAction>,
        #[serde(default)]
        rationale: Option<String>,
        #[serde(default)]
        per_action_notes: Option<Vec<String>>,
    }
    let raw_real_extracted = extract_json_object(&raw_real).unwrap_or_else(|| raw_real.clone());
    let first_error = match serde_json::from_str::<ActionEnvelope>(&raw_real_extracted) {
        Ok(env) => {
            return ModelAttempt {
                turn: Some(ModelTurn {
                    actions: env.actions,
                    rationale: env.rationale,
                    per_action_notes: env.per_action_notes,
                }),
                raw: Some(raw_real),
                parse_error: None,
            };
        }
        Err(error) => error.to_string(),
    };

    let repair_prompt = format!(
        "Your previous response was not valid JSON conforming to the action schema. \
         Re-emit ONLY a JSON object {{\"actions\":[...], \"rationale\": \"...\", \"perActionNotes\": [\"...\"]}} \
         matching the schema. Keep the short track ids (tk0, tk1, ...) and region ids (rg0, rg1, ...) exactly as before. \
         Do not include any other text. Original output to repair:\n{}",
        raw_aliased
    );
    let Some(repaired_aliased) = ollama_generate(base_url, model, &repair_prompt, REPAIR_TIMEOUT_MS).await else {
        return ModelAttempt {
            turn: None,
            raw: Some(raw_real),
            parse_error: Some(format!("First parse failed ({first_error}); repair pass timed out.")),
        };
    };
    eprintln!("[assistant] repair raw response:\n{repaired_aliased}");
    let repaired_real = substitute_quoted(&repaired_aliased, &track_aliases, false);
    let repaired_real = substitute_quoted(&repaired_real, &region_aliases, false);
    let repaired_extracted = extract_json_object(&repaired_real).unwrap_or_else(|| repaired_real.clone());
    match serde_json::from_str::<ActionEnvelope>(&repaired_extracted) {
        Ok(env) => ModelAttempt {
            turn: Some(ModelTurn {
                actions: env.actions,
                rationale: env.rationale,
                per_action_notes: env.per_action_notes,
            }),
            raw: Some(repaired_real),
            parse_error: None,
        },
        Err(error) => ModelAttempt {
            turn: None,
            raw: Some(repaired_real),
            parse_error: Some(format!("First parse failed ({first_error}); repair parse failed ({error}).")),
        },
    }
}

pub struct CritiqueError {
    pub message: String,
    pub raw: Option<String>,
}

async fn try_model_critique(
    base_url: &str,
    model: &str,
    request: &AssistantRequest,
    session: &MixSession,
    selected_skills: &[String],
) -> Result<crate::model::MixCritique, CritiqueError> {
    if session.tracks.is_empty() {
        return Err(CritiqueError {
            message: "Session has no tracks to critique.".into(),
            raw: None,
        });
    }
    let track_aliases: Vec<(String, String)> = session
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("tk{i}"), t.id.clone()))
        .collect();

    // Render the session offline and analyze the master bus output.
    let rendered = crate::engine::render::render_session_to_buffer(session)
        .map_err(|e| CritiqueError { message: format!("offline render failed: {e}"), raw: None })?;
    let master = crate::engine::source::analysis::analyze(
        &rendered.samples,
        rendered.channels,
        rendered.sample_rate,
    );
    let true_peak_db = compute_true_peak_db(&rendered.samples);
    let headroom_db = (-master.peak_db).max(0.0);

    // Per-track snapshot mirrors the action-pipeline format so the model has
    // the same vocabulary it already knows.
    let snapshot = build_capability_snapshot(
        session,
        &["balance".into(), "tonal_eq".into(), "dynamics".into(), "space_depth".into()],
    );
    let master_block = json!({
        "peakDb": round1(master.peak_db),
        "rmsDb": round1(master.rms_db),
        "lufs": round1(master.lufs),
        "spectralCentroidHz": round0(master.spectral_centroid_hz),
        "bandEnergy": {
            "low": round2(master.low_energy),
            "mid": round2(master.mid_energy),
            "high": round2(master.high_energy),
        },
        "silencePercent": round1(master.silence_percent),
        "dynamicRangeDb": round1(master.dynamic_range_db),
        "truePeakDbEstimate": round1(true_peak_db),
        "headroomDb": round1(headroom_db),
    });

    let prompt = format!(
        "You are a senior mix engineer giving a critical assessment. Return ONLY a JSON object with this exact shape:\n\
         {{\n  \"mixScore\": <0-10>,\n  \"summary\": \"<2-4 sentence overall verdict>\",\n  \"headroomDb\": <number>,\n  \"integratedLufsEstimate\": <number>,\n  \"truePeakDbEstimate\": <number>,\n  \"mixIssues\": [{{\"category\": \"...\", \"severity\": \"low|medium|high\", \"message\": \"...\", \"suggestedSkills\": [\"...\"]}}],\n  \"perTrack\": [{{\"trackId\": \"tk0\", \"trackName\": \"...\", \"rating\": <0-10>, \"issues\": [...], \"strengths\": [\"...\"]}}],\n  \"recommendedNextSteps\": [\"<short action>\", ...]\n}}\n\n\
         Use ONLY trackId values from the Tracks list (tk0, tk1, ...). Categories should be one of: balance, tonality, dynamics, space, headroom, mono_compatibility. Severity reflects how much it hurts the mix.\n\n\
         Use the audio analysis values to ground the critique. Reference the numbers when calling out issues (e.g. \"vocal centroid is 1100 Hz — too dark\"). Don't praise things that aren't there; if a track sounds fine, leave issues empty.\n\n\
         Suggested skills must come from this set: balance, tonal_eq, dynamics, space_depth, region_automation.\n\n\
         Master bus analysis:\n{}\n\n\
         Per-track capability snapshot (current parameter values + audio analysis):\n{}\n\n\
         Tracks summary:\n{}\n\n\
         Selected track ids: {:?}\n\
         User request: {}\n\n\
         Audio interpretation guide: spectralCentroidHz < 1500 = dark, > 3500 = bright; bandEnergy.low > 0.6 with high < 0.1 = muddy; lufs around -23 = broadcast loudness; dynamicRangeDb < 6 = squashed, > 14 = highly dynamic; peakDb above -1 = limited headroom; truePeakDbEstimate above 0 dBTP = inter-sample clipping risk on lossy codecs.\n",
        master_block,
        serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()),
        serde_json::to_string(
            &session
                .tracks
                .iter()
                .map(|t| json!({"id": t.id, "name": t.name, "role": t.role, "gainDb": t.gain_db, "pan": t.pan}))
                .collect::<Vec<_>>()
        )
        .unwrap_or_else(|_| "[]".into()),
        request.selected_track_ids,
        request.user_text
    );

    let prompt = substitute_quoted(&prompt, &track_aliases, true);
    let _ = selected_skills; // currently unused but kept for future skill-scoped critique
    let raw_aliased = ollama_generate(base_url, model, &prompt, ACTION_TIMEOUT_MS)
        .await
        .ok_or_else(|| CritiqueError { message: "Ollama did not respond within the timeout.".into(), raw: None })?;
    eprintln!("[assistant] critique raw response:\n{raw_aliased}");

    let raw_real = substitute_quoted(&raw_aliased, &track_aliases, false);
    let extracted = extract_json_object(&raw_real).unwrap_or_else(|| raw_real.clone());
    serde_json::from_str::<crate::model::MixCritique>(&extracted).map_err(|err| CritiqueError {
        message: err.to_string(),
        raw: Some(raw_real),
    })
}

/// Cheap true-peak estimator: 4× linear oversampling and take the max abs sample.
/// Not ITU-R BS.1770 grade, but close enough to flag inter-sample clipping risk.
fn compute_true_peak_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let mut max = 0.0_f32;
    for w in samples.windows(2) {
        let a = w[0];
        let b = w[1];
        let q1 = a + 0.25 * (b - a);
        let q2 = a + 0.5 * (b - a);
        let q3 = a + 0.75 * (b - a);
        let m = a.abs().max(b.abs()).max(q1.abs()).max(q2.abs()).max(q3.abs());
        if m > max {
            max = m;
        }
    }
    if max <= 0.0 {
        -120.0
    } else {
        20.0 * max.log10()
    }
}

/// Replace quoted string values inside `text` between alias and real form.
/// `pairs` is `[(alias, real)]`. When `to_alias` is true, replaces `"real"` with `"alias"`;
/// otherwise replaces `"alias"` with `"real"`. The surrounding quotes ensure we only
/// touch full JSON string values, not substrings.
fn substitute_quoted(text: &str, pairs: &[(String, String)], to_alias: bool) -> String {
    let mut out = text.to_string();
    for (alias, real) in pairs {
        let (from, to) = if to_alias {
            (format!("\"{real}\""), format!("\"{alias}\""))
        } else {
            (format!("\"{alias}\""), format!("\"{real}\""))
        };
        out = out.replace(&from, &to);
    }
    out
}

fn round0(x: f32) -> f32 {
    if x.is_finite() { x.round() } else { 0.0 }
}
fn round1(x: f32) -> f32 {
    if x.is_finite() { (x * 10.0).round() / 10.0 } else { 0.0 }
}
fn round2(x: f32) -> f32 {
    if x.is_finite() { (x * 100.0).round() / 100.0 } else { 0.0 }
}

/// Compact, skill-scoped snapshot of currently relevant processors / parameters.
fn build_capability_snapshot(session: &MixSession, selected: &[String]) -> serde_json::Value {
    use std::collections::HashMap;
    let by_source: HashMap<&str, &crate::model::SourceFile> =
        session.source_files.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut tracks_json = Vec::new();
    for t in &session.tracks {
        let mut params = serde_json::Map::new();
        if selected.iter().any(|s| s == "balance") {
            params.insert("gainDb".into(), json!({"current": t.gain_db, "min": -24.0, "max": 24.0, "unit": "dB"}));
            params.insert("pan".into(), json!({"current": t.pan, "min": -1.0, "max": 1.0}));
            params.insert("muted".into(), json!({"current": t.muted}));
            params.insert("solo".into(), json!({"current": t.solo}));
        }
        if selected.iter().any(|s| s == "tonal_eq") {
            params.insert("highPass".into(), json!({
                "enabled": t.chain.high_pass.enabled,
                "frequencyHz": {"current": t.chain.high_pass.frequency_hz, "min": 20.0, "max": 20000.0},
                "slopeDbOct": [12, 24],
            }));
            params.insert("lowPass".into(), json!({
                "enabled": t.chain.low_pass.enabled,
                "frequencyHz": {"current": t.chain.low_pass.frequency_hz, "min": 20.0, "max": 20000.0},
                "slopeDbOct": [12, 24],
            }));
            let bands: Vec<_> = t.chain.eq.iter().enumerate().map(|(i, b)| {
                json!({
                    "band": i,
                    "frequencyHz": {"current": b.frequency_hz, "min": 20.0, "max": 20000.0},
                    "gainDb": {"current": b.gain_db, "min": -12.0, "max": 12.0, "safeStep": 1.0},
                    "q": {"current": b.q, "min": 0.2, "max": 10.0},
                })
            }).collect();
            params.insert("eq".into(), json!(bands));
        }
        if selected.iter().any(|s| s == "dynamics") {
            let c = &t.chain.compressor;
            params.insert("compressor".into(), json!({
                "enabled": c.enabled,
                "thresholdDb": {"current": c.threshold_db, "min": -60.0, "max": 0.0},
                "ratio": {"current": c.ratio, "min": 1.0, "max": 20.0},
                "attackMs": {"current": c.attack_ms, "min": 1.0, "max": 200.0},
                "releaseMs": {"current": c.release_ms, "min": 20.0, "max": 1000.0},
                "kneeDb": {"current": c.knee_db, "min": 0.0, "max": 24.0},
                "makeupDb": {"current": c.makeup_db, "min": -12.0, "max": 12.0},
            }));
        }
        if selected.iter().any(|s| s == "space_depth") {
            params.insert("sends".into(), json!({
                "reverbDb": {"current": t.sends.reverb_db, "min": -60.0, "max": 0.0},
                "delayDb": {"current": t.sends.delay_db, "min": -60.0, "max": 0.0},
            }));
        }
        let mut track_obj = serde_json::Map::new();
        track_obj.insert("id".into(), json!(t.id));
        track_obj.insert("name".into(), json!(t.name));
        track_obj.insert("role".into(), json!(t.role));
        track_obj.insert("params".into(), json!(params));
        if let Some(src) = by_source.get(t.source_file_id.as_str()) {
            let a = &src.analysis;
            let duration_seconds = if src.sample_rate > 0 {
                src.duration_samples as f32 / src.sample_rate as f32
            } else {
                0.0
            };
            track_obj.insert(
                "audio".into(),
                json!({
                    "durationSeconds": round1(duration_seconds),
                    "channels": src.channels,
                    "sampleRate": src.sample_rate,
                    "peakDb": round1(a.peak_db),
                    "rmsDb": round1(a.rms_db),
                    "lufs": round1(a.lufs_estimate),
                    "spectralCentroidHz": round0(a.spectral_centroid_hz),
                    "bandEnergy": {
                        "low": round2(a.low_energy),
                        "mid": round2(a.mid_energy),
                        "high": round2(a.high_energy),
                    },
                    "silencePercent": round1(a.silence_percent),
                    "dynamicRangeDb": round1(a.dynamic_range_db),
                }),
            );
        }
        tracks_json.push(serde_json::Value::Object(track_obj));
    }
    json!({
        "selectedSkills": selected,
        "tracks": tracks_json,
        "regions": session.regions,
    })
}

pub async fn list_ollama_models(base_url: String) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Tags {
        models: Vec<TagModel>,
    }
    #[derive(Deserialize)]
    struct TagModel {
        name: Option<String>,
    }

    let base_url = base_url.trim().trim_end_matches('/').to_string();
    let client = reqwest::Client::new();
    let response = timeout(Duration::from_millis(4500), client.get(format!("{base_url}/api/tags")).send())
        .await
        .map_err(|_| format!("Timed out connecting to Ollama at {base_url}"))?
        .map_err(|_| format!("Could not connect to Ollama at {base_url}"))?;
    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {}", response.status()));
    }
    let tags = response.json::<Tags>().await.map_err(|error| error.to_string())?;
    Ok(tags.models.into_iter().filter_map(|model| model.name).collect())
}

fn expand_skills_from_actions(mut selected: Vec<String>, actions: &[MixAction]) -> Vec<String> {
    let catalog = crate::capabilities::skill_catalog();
    let mut additions: Vec<String> = Vec::new();
    for action in actions {
        let tool = action_name(action);
        for skill in &catalog.skills {
            if skill.summary_actions.iter().any(|a| a == tool) && !selected.contains(&skill.skill_id) && !additions.contains(&skill.skill_id) {
                additions.push(skill.skill_id.clone());
            }
        }
    }
    selected.extend(additions);
    selected.sort();
    selected.dedup();
    selected
}

fn explain_actions(actions: &[MixAction], session: &MixSession) -> String {
    let phrases = actions
        .iter()
        .map(|action| {
            let track_id = match action {
                MixAction::SetTrackGain { track_id, .. }
                | MixAction::AdjustTrackGain { track_id, .. }
                | MixAction::SetTrackPan { track_id, .. }
                | MixAction::MuteTrack { track_id, .. }
                | MixAction::SoloTrack { track_id, .. }
                | MixAction::SetHighPass { track_id, .. }
                | MixAction::SetLowPass { track_id, .. }
                | MixAction::SetEqBand { track_id, .. }
                | MixAction::SetCompressor { track_id, .. }
                | MixAction::SetReverbSend { track_id, .. }
                | MixAction::SetDelaySend { track_id, .. }
                | MixAction::SetRegionGain { track_id, .. }
                | MixAction::ApplySectionAutomation { track_id, .. } => Some(track_id),
                MixAction::SetProcessorParam { target_id, .. } => Some(target_id),
                _ => None,
            };
            let name = track_id
                .and_then(|id| session.tracks.iter().find(|track| &track.id == id))
                .map(|track| track.name.as_str())
                .unwrap_or("the mix");
            match action {
                MixAction::AdjustTrackGain { delta_db, .. } => format!("{} {name} by {} dB", if *delta_db > 0.0 { "raised" } else { "lowered" }, delta_db.abs()),
                MixAction::SetTrackGain { gain_db, .. } => format!("set {name} to {gain_db} dB"),
                MixAction::SetTrackPan { .. } => format!("moved {name} in the stereo field"),
                MixAction::MuteTrack { muted, .. } => format!("{} {name}", if *muted { "muted" } else { "unmuted" }),
                MixAction::SoloTrack { solo, .. } => format!("{} {name}", if *solo { "soloed" } else { "unsoloed" }),
                MixAction::SetEqBand { .. } => format!("adjusted EQ on {name}"),
                MixAction::SetHighPass { .. } => format!("cleaned low rumble on {name}"),
                MixAction::SetLowPass { .. } => format!("softened top end on {name}"),
                MixAction::SetCompressor { .. } => format!("set compression on {name}"),
                MixAction::SetReverbSend { .. } => format!("changed reverb depth on {name}"),
                MixAction::SetDelaySend { .. } => format!("changed delay send on {name}"),
                MixAction::SetRegionGain { .. } | MixAction::ApplySectionAutomation { .. } => format!("created a section-scoped move for {name}"),
                MixAction::DeleteTrack { .. } => format!("deleted {name}"),
                MixAction::RenderMix => "prepared the current mix for render".into(),
                _ => format!("updated {name}"),
            }
        })
        .collect::<Vec<_>>();
    format!("I {}.", phrases.join(", "))
}

fn action_name(action: &MixAction) -> &'static str {
    match action {
        MixAction::CreateRegion { .. } => "create_region",
        MixAction::DeleteTrack { .. } => "delete_track",
        MixAction::SetTrackGain { .. } => "set_track_gain",
        MixAction::AdjustTrackGain { .. } => "adjust_track_gain",
        MixAction::SetTrackPan { .. } => "set_track_pan",
        MixAction::MuteTrack { .. } => "mute_track",
        MixAction::SoloTrack { .. } => "solo_track",
        MixAction::SetHighPass { .. } => "set_high_pass",
        MixAction::SetLowPass { .. } => "set_low_pass",
        MixAction::SetEqBand { .. } => "set_eq_band",
        MixAction::SetCompressor { .. } => "set_compressor",
        MixAction::SetReverbSend { .. } => "set_reverb_send",
        MixAction::SetDelaySend { .. } => "set_delay_send",
        MixAction::SetProcessorParam { .. } => "set_processor_param",
        MixAction::SetRegionGain { .. } => "set_region_gain",
        MixAction::ApplySectionAutomation { .. } => "apply_section_automation",
        MixAction::Undo => "undo",
        MixAction::Redo => "redo",
        MixAction::RenderMix => "render_mix",
    }
}

fn contains_any(text: &str, words: &[&str]) -> bool {
    let lower = text.to_lowercase();
    words.iter().any(|word| lower.contains(word))
}
