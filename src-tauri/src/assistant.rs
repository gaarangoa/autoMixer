use serde::Deserialize;
use serde_json::json;
use tokio::time::{timeout, Duration};

use crate::{
    actions::{apply_actions, clamp_actions, redo, undo, validate_actions},
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
    observer_inner: std::sync::Arc<dyn LlmObserver>,
) -> Result<(AssistantResponse, MixProject), String> {
    let accumulator = std::sync::Arc::new(AccumulatingObserver::new(observer_inner.clone()));
    let observer: std::sync::Arc<dyn LlmObserver> = accumulator.clone();
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

    let selected_skills = match model_select_skills(&base_url, &model, &request, &project.session, observer.as_ref()).await {
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
                tokens: None,
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
                tokens: None,
            },
            project,
        ));
    }

    if selected_skills.iter().any(|s| s == "critique") {
        return match try_model_critique(&base_url, &model, &request, &project.session, &selected_skills, observer.as_ref()).await {
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
        try_model_actions(&base_url, &model, &request, &project.session, &selected_skills, observer.as_ref()).await;

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

    let mut actions = actions;
    let warnings = clamp_actions(&mut actions);
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

    let stats = accumulator.snapshot();
    Ok((
        AssistantResponse::Ok {
            explanation,
            actions,
            warnings,
            selected_skills,
            session: project.session.clone(),
            history: project.history.clone(),
            rationale,
            per_action_notes,
            tokens: Some(crate::model::TurnTokens {
                prompt: stats.prompt_tokens,
                response: stats.response_tokens,
                elapsed_ms: stats.elapsed_ms,
            }),
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

#[derive(Deserialize, Default)]
struct GenerateResponse {
    response: String,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
    #[serde(default)]
    done: bool,
}

#[derive(serde::Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
}

#[derive(Debug, Default, Clone)]
pub struct LlmCallStats {
    pub prompt_tokens: u32,
    pub response_tokens: u32,
    pub elapsed_ms: u32,
}

pub struct LlmCall {
    pub response: String,
    pub stats: LlmCallStats,
}

/// Callback invoked for each streamed chunk and on completion. `phase` is one
/// of "skill", "action", "repair", "critique"; `kind` is "chunk", "stats".
pub trait LlmObserver: Send + Sync {
    fn chunk(&self, phase: &str, text: &str);
    fn stats(&self, phase: &str, stats: &LlmCallStats);
}

pub struct NoopObserver;
impl LlmObserver for NoopObserver {
    fn chunk(&self, _phase: &str, _text: &str) {}
    fn stats(&self, _phase: &str, _stats: &LlmCallStats) {}
}

/// Aggregates token + time stats across multiple LLM calls in one turn,
/// while forwarding to an inner observer (typically Tauri-emitting).
pub struct AccumulatingObserver {
    inner: std::sync::Arc<dyn LlmObserver>,
    pub totals: std::sync::Mutex<LlmCallStats>,
}

impl AccumulatingObserver {
    pub fn new(inner: std::sync::Arc<dyn LlmObserver>) -> Self {
        Self { inner, totals: std::sync::Mutex::new(LlmCallStats::default()) }
    }
    pub fn snapshot(&self) -> LlmCallStats {
        self.totals.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl LlmObserver for AccumulatingObserver {
    fn chunk(&self, phase: &str, text: &str) {
        self.inner.chunk(phase, text);
    }
    fn stats(&self, phase: &str, stats: &LlmCallStats) {
        if let Ok(mut t) = self.totals.lock() {
            t.prompt_tokens += stats.prompt_tokens;
            t.response_tokens += stats.response_tokens;
            t.elapsed_ms += stats.elapsed_ms;
        }
        self.inner.stats(phase, stats);
    }
}

/// Extract the first complete JSON object from a free-form model response.
/// Some models wrap output in fences/prose, and some emit multiple objects.
pub fn extract_json_object(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in stripped.char_indices() {
        if start.is_none() {
            if ch == '{' {
                start = Some(idx);
                depth = 1;
            }
            continue;
        }

        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                let begin = start?;
                return Some(stripped[begin..=idx].to_string());
            }
        }
    }
    None
}

pub async fn ollama_generate(
    base_url: &str,
    model: &str,
    prompt: &str,
    timeout_ms: u64,
    phase: &str,
    observer: &dyn LlmObserver,
) -> Option<LlmCall> {
    use futures_util::StreamExt;
    if base_url.is_empty() || model.is_empty() {
        return None;
    }
    let started = std::time::Instant::now();
    let client = reqwest::Client::new();
    let resp = timeout(
        Duration::from_millis(timeout_ms),
        client
            .post(format!("{base_url}/api/generate"))
            .json(&GenerateRequest { model, prompt, stream: true })
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut accumulated = String::new();
    let mut stats = LlmCallStats::default();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.ok()?;
        let s = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        buffer.push_str(s);
        // Ollama streams one JSON object per line.
        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].to_string();
            buffer.drain(..=newline);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(part) = serde_json::from_str::<GenerateResponse>(trimmed) {
                if !part.response.is_empty() {
                    observer.chunk(phase, &part.response);
                    accumulated.push_str(&part.response);
                }
                if part.done {
                    stats.prompt_tokens = part.prompt_eval_count.unwrap_or(0);
                    stats.response_tokens = part.eval_count.unwrap_or(0);
                }
            }
        }
    }
    stats.elapsed_ms = started.elapsed().as_millis() as u32;
    observer.stats(phase, &stats);
    Some(LlmCall { response: accumulated, stats })
}

async fn model_select_skills(
    base_url: &str,
    model: &str,
    request: &AssistantRequest,
    session: &MixSession,
    observer: &dyn LlmObserver,
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
    let call = ollama_generate(base_url, model, &prompt, SKILL_TIMEOUT_MS, "skill", observer).await?;
    eprintln!("[assistant] skill raw response:\n{}", call.response);
    let extracted = extract_json_object(&call.response)?;
    #[derive(Deserialize)]
    struct SkillEnvelope {
        #[serde(rename = "selectedSkillIds", alias = "selected_skill_ids")]
        ids: Vec<String>,
    }
    let _ = call.stats; // observer already received the stats
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
    observer: &dyn LlmObserver,
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
    let profile_preamble = profile_block(&session.mixer_profile);
    let fundamentals = mixing_fundamentals_block();
    let ai_preservation = ai_stem_preservation_block(session);
    let prompt = format!(
        "{profile_preamble}{fundamentals}{ai_preservation}You are an assistant mix engineer. Return ONLY a JSON object with this exact shape:\n\
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
         - {{\"tool\":\"set_master_gain\",\"gainDb\":-2.0}}  // sets master fader, applies to whole mix\n\
         - {{\"tool\":\"adjust_master_gain\",\"deltaDb\":-3.0}}  // relative master move (use this to fix headroom)\n\
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
         {}\
         Routing guidance: frequency/EQ/low/mid/high/air/presence/bright/dark/harsh/muddy/body \
         requests should use EQ/filter actions, not gain. Vocal upfront/presence/clarity requests usually \
         combine a subtle level move with presence EQ and light compression. Whole-mix loudness or \
         headroom problems must use set_master_gain or adjust_master_gain — do NOT solve them by \
         touching every track gain.\n\n\
         Audio analysis per track is included under track.audio. Use it to ground decisions: \
         spectralCentroidHz < 1500 = dark, > 3500 = bright; bandEnergy.low/mid/high are normalized \
         shares of energy (sum ≈ 1) — high low_energy with low high_energy = muddy; lufs around \
         -23 LUFS is broadcast loudness, lower means quieter; dynamicRangeDb < 6 is heavily \
         compressed material, > 14 is highly dynamic; peakDb close to 0 indicates limited headroom. \
         IMPORTANT: silencePercent > 50 means the track has long silent gaps (intermittent content like \
         percussion hits or backing vox stabs) — do NOT compress that as if it were dynamic-range \
         problem; compression of silence does nothing. A high dynamicRangeDb only justifies compression \
         when silencePercent is also low (< 30).\n\n\
         Per-role compressor starting points (use these as defaults, then adjust to taste — \
         do NOT apply identical settings to every track; vary by role):\n\
         - lead vocal:    threshold -18, ratio 2.5–3, attack 8–15 ms, release 80–120 ms, knee 6, makeup 1\n\
         - backing vocal: threshold -20, ratio 2,    attack 12 ms,   release 150 ms,    knee 8, makeup 1\n\
         - kick/snare:    threshold -12, ratio 4,    attack 5–10 ms, release 60 ms,     knee 2, makeup 2\n\
         - drum bus:      threshold -14, ratio 3,    attack 20 ms,   release 80 ms,     knee 4, makeup 1\n\
         - bass:          threshold -16, ratio 4,    attack 20 ms,   release 100 ms,    knee 4, makeup 2\n\
         - electric gtr:  threshold -18, ratio 2.5,  attack 15 ms,   release 80 ms,     knee 6, makeup 1\n\
         - acoustic gtr:  threshold -22, ratio 2,    attack 20 ms,   release 150 ms,    knee 8, makeup 1\n\
         - keys/synth:    threshold -22, ratio 2,    attack 25 ms,   release 200 ms,    knee 8, makeup 0\n\
         - percussion:    threshold -14, ratio 3,    attack 5 ms,    release 60 ms,     knee 4, makeup 1\n\
         Reverb/delay sends should also vary by role — vocals usually -16 to -20 dB reverb, \
         drums dry or short room -24 dB, bass usually NO reverb, leads -14 to -18 dB. Don't stamp \
         identical send levels across all tracks — that flattens the mix.\n\n\
         Anti-template rule: if you are emitting more than three actions of the same tool type, \
         each one MUST have parameters that differ in a way the role/audio analysis justifies. \
         Identical settings copy-pasted across tracks is a bug, not a mix.\n\n\
         Restraint rules (apply regardless of profile):\n\
         - If you cannot point to a specific measurement that justifies a move, DO NOT make it.\n\
         - Prefer the smallest move that solves the problem. A 1 dB cut beats a 4 dB cut when both \
           address the issue.\n\
         - Cuts before boosts. To brighten, first cut competing low-mids; only then add high-shelf.\n\
         - Never compress just because a track exists. Only compress when dynamicRangeDb > 10 \
           (and silencePercent < 30 for that track). Otherwise leave the compressor untouched.\n\
         - For the master fader, ±3 dB is a large move. Single moves above 4 dB on master \
           require explicit measurement justification in the rationale (e.g. headroom < 1 dB).\n\
         - One coherent turn beats five micro-adjustments. Aim for 3–8 actions total per turn, \
           each with a clearly stated reason in perActionNotes.\n\n\
         LUFS target follow-through: if a recent critique exists with a target loudness or \
         headroom in its recommendedNextSteps, your job is to MOVE TOWARD that target, not just \
         add unrelated changes. Compare the critique's measured headroom/lufs against its target — \
         if they differ by more than 2 dB, the move is required, not optional.\n\n\
         AI-generated stems: tracks where `aiGenerated` is true are NOT clean recordings — they \
         are produced by a model (e.g. Suno) or extracted by a stem-separator (e.g. demucs). They \
         carry codec/separation artifacts, residual bleed from other parts, and a steady low-level \
         noise floor instead of true silence. Treat them differently:\n\
         - Use BROADER EQ Q (0.5–1.0, never above 2.0). Surgical narrow-Q cuts amplify artifacts.\n\
         - Use GENTLER compression (ratio ≤ 2:1, slow attack ≥ 20 ms, longer release ≥ 150 ms). \
           Heavy compression pumps the noise floor.\n\
         - LOWER reverb sends by ~6 dB versus a clean recording (e.g. -22 to -28 dB instead of \
           -16 to -20). Reverb adds tail to the noise floor too.\n\
         - A low-pass around 12–14 kHz is often appropriate to tame codec/AI hiss.\n\
         - Do NOT trust silencePercent on AI tracks — the noise floor reads as non-silent. Skip \
           the silence-vs-dynamics check entirely; treat them as continuous content.\n\
         - Be cautious with high-pass on AI bass — sub-bass artifacts are not the same as recorded \
           bass, but cutting too high can remove real fundamental.\n\
         - Tonal centroid extremes on AI stems are often artifacts, not musical content — prefer \
           gentle shelves over peak EQs.\n\n\
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
                .map(|t| json!({"id": t.id, "name": t.name, "role": t.role, "aiGenerated": t.ai_generated, "gainDb": t.gain_db, "pan": t.pan}))
                .collect::<Vec<_>>()
        )
        .unwrap_or_else(|_| "[]".into()),
        request.selected_track_ids,
        request.selected_region_ids,
        sections_block(&session.sections, session.bpm),
        critique_block,
        request.user_text,
        fundamentals = fundamentals,
        ai_preservation = ai_preservation,
    );

    let prompt = substitute_quoted(&prompt, &track_aliases, true);
    let prompt = substitute_quoted(&prompt, &region_aliases, true);

    eprintln!(
        "[assistant] action prompt sections_block:\n{}",
        sections_block(&session.sections, session.bpm)
    );

    let Some(call) = ollama_generate(base_url, model, &prompt, ACTION_TIMEOUT_MS, "action", observer).await else {
        return ModelAttempt { turn: None, raw: None, parse_error: Some("Ollama did not respond within the timeout.".into()) };
    };
    let raw_aliased = call.response;
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
    let Some(repair_call) = ollama_generate(base_url, model, &repair_prompt, REPAIR_TIMEOUT_MS, "repair", observer).await else {
        return ModelAttempt {
            turn: None,
            raw: Some(raw_real),
            parse_error: Some(format!("First parse failed ({first_error}); repair pass timed out.")),
        };
    };
    let repaired_aliased = repair_call.response;
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
    observer: &dyn LlmObserver,
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

    let profile_preamble = profile_block(&session.mixer_profile);
    let prompt = format!(
        "{profile_preamble}You are a senior mix engineer giving a critical assessment. Return ONLY a JSON object with this exact shape:\n\
         {{\n  \"mixScore\": <0-10>,\n  \"summary\": \"<2-4 sentence overall verdict>\",\n  \"headroomDb\": <number>,\n  \"integratedLufsEstimate\": <number>,\n  \"truePeakDbEstimate\": <number>,\n  \"mixIssues\": [{{\"category\": \"...\", \"severity\": \"low|medium|high\", \"message\": \"...\", \"suggestedSkills\": [\"...\"]}}],\n  \"perTrack\": [{{\"trackId\": \"tk0\", \"trackName\": \"...\", \"rating\": <0-10>, \"issues\": [...], \"strengths\": [\"...\"]}}],\n  \"recommendedNextSteps\": [\"<short action>\", ...]\n}}\n\n\
         Use ONLY trackId values from the Tracks list (tk0, tk1, ...). Categories should be one of: balance, tonality, dynamics, space, headroom, mono_compatibility, arrangement. Severity reflects how much it hurts the mix. \
         Every entry in `mixIssues` AND every entry in `perTrack[].issues` MUST be a full object \
         {{\"category\":\"...\",\"severity\":\"low|medium|high\",\"message\":\"...\"}}. Do NOT emit issues as plain strings.\n\n\
         Use the audio analysis values to ground the critique. Reference the numbers when calling out issues (e.g. \"vocal centroid is 1100 Hz — too dark\"). Don't praise things that aren't there; if a track sounds fine, leave issues empty.\n\n\
         Suggested skills must come from this set: balance, tonal_eq, dynamics, space_depth, mastering, region_automation. \
         If headroom or whole-mix loudness is the issue, suggest the `mastering` skill (set_master_gain / adjust_master_gain), not per-track gains.\n\n\
         Silence vs dynamics: if a track has silencePercent > 50, its high dynamicRangeDb is an \
         artifact of silent gaps, not loud-vs-quiet musical content — do NOT label it as a dynamics \
         problem and do NOT recommend compression for it. Only flag dynamics when silencePercent < 30 \
         AND dynamicRangeDb > 14.\n\n\
         Concrete next steps: when you mention a target loudness or headroom, give a specific delta. \
         Example: \"Lower master by 3 dB to reach 4 dB headroom\" instead of \"increase headroom\". \
         When a previous critique's targets weren't met (compare measured to its prior recommendations \
         if visible in user_text), say so explicitly.\n\n\
         Per-section critique: when session.sections has measurements, compare sections \
         to each other. Flag with severity=high when measured LUFS of a chorus is at or below \
         the LUFS of adjacent verses (energy inversion). Flag with severity=medium when two \
         sections sharing a label (e.g. two verses) have nearly identical LUFS AND nearly \
         identical spectralCentroidHz AND nearly identical bandEnergy (within 5%) — this is a \
         repeat that lacks variation and should be flagged in the `arrangement` category. \
         Flag with severity=low when an outro/intro has higher LUFS than the chorus (likely \
         a measurement-vs-arrangement issue). Always reference the section labels and the \
         numbers in the message.\n\n\
         AI-generated stems (track.aiGenerated == true): do NOT critique them as if they were \
         clean recordings. Their tonal anomalies, broad-band noise, harshness, or unusual dynamic \
         range are USUALLY stem-separation/AI generation artifacts, not mix problems. Specifically: \
         do not flag high silencePercent as a balance problem on AI tracks (it's noise floor, not \
         silence); do not flag bright/dark spectral centroid as a tonal problem unless it is \
         obviously wrong for the role; do not flag wide dynamic range as a dynamics problem. \
         Issues you DO call out on AI tracks should focus on what the engineer can actually fix: \
         level relative to the rest of the mix, gross EQ pockets that clash with other tracks, \
         and how much reverb to add (less than usual). Mention briefly in the message that the \
         track is AI-generated when this changes the recommendation.\n\n\
         Master bus analysis:\n{}\n\n\
         Per-track capability snapshot (current parameter values + audio analysis):\n{}\n\n\
         Tracks summary:\n{}\n\n\
         Selected track ids: {:?}\n\
         User request: {}\n\n\
         {}\
         Audio interpretation guide: spectralCentroidHz < 1500 = dark, > 3500 = bright; bandEnergy.low > 0.6 with high < 0.1 = muddy; lufs around -23 = broadcast loudness; dynamicRangeDb < 6 = squashed, > 14 = highly dynamic; peakDb above -1 = limited headroom; truePeakDbEstimate above 0 dBTP = inter-sample clipping risk on lossy codecs.\n",
        master_block,
        serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()),
        serde_json::to_string(
            &session
                .tracks
                .iter()
                .map(|t| json!({"id": t.id, "name": t.name, "role": t.role, "aiGenerated": t.ai_generated, "gainDb": t.gain_db, "pan": t.pan}))
                .collect::<Vec<_>>()
        )
        .unwrap_or_else(|_| "[]".into()),
        request.selected_track_ids,
        request.user_text,
        sections_block(&session.sections, session.bpm),
    );

    let prompt = substitute_quoted(&prompt, &track_aliases, true);
    let _ = selected_skills; // currently unused but kept for future skill-scoped critique
    let crit_call = ollama_generate(base_url, model, &prompt, ACTION_TIMEOUT_MS, "critique", observer)
        .await
        .ok_or_else(|| CritiqueError { message: "Ollama did not respond within the timeout.".into(), raw: None })?;
    let raw_aliased = crit_call.response;
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
pub fn substitute_quoted(text: &str, pairs: &[(String, String)], to_alias: bool) -> String {
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

pub fn profile_block(profile: &crate::model::MixerProfile) -> String {
    let target_lufs = match profile.loudness_target.as_str() {
        "broadcast" => "-23 LUFS integrated",
        "streaming" => "-14 LUFS integrated",
        "loud" => "-10 LUFS integrated (loudness war level — only when the song's style demands it)",
        _ => "-14 LUFS integrated",
    };
    let aggressiveness_rule = match profile.aggressiveness.as_str() {
        "subtle" => "Default move size is 0.5–1.5 dB. Never exceed 3 dB in one shot. Prefer the smallest move that solves the problem; if you cannot describe a measurable problem, do not act on that track.",
        "moderate" => "Default move size is 1–3 dB. Avoid moves larger than 4 dB unless a measurement clearly demands it. Prefer one carefully-chosen action over three small simultaneous ones.",
        "bold" => "Move size can reach 4–6 dB when the measurements support it, but still justify every move with a number from the analysis.",
        _ => "Default move size is 1–3 dB.",
    };
    let eq_rule = match profile.eq_philosophy.as_str() {
        "corrective_only" => "EQ: only correct measurable problems (mud at 200–400 Hz with high low_energy, harshness at 2–5 kHz with high mid+high). Prefer cuts to boosts. Use broad Q (0.7–1.0). No tonal shaping for taste.",
        "tonal_shaping" => "EQ: corrective AND gentle tonal shaping (presence lifts at 2–4 kHz, air at 10–12 kHz, low-mid warmth at 200 Hz). Cuts before boosts. Q between 0.7 and 1.5.",
        "sculpting" => "EQ: shaping is encouraged where it serves the genre. Narrow surgical cuts (Q 2–4) and broad shaping boosts both allowed. Aggressive moves OK when supported by analysis.",
        _ => "",
    };
    let comp_rule = match profile.compression_philosophy.as_str() {
        "transparent_glue" => "Compression: only for glue (≤ 2 dB GR) and to control individual peaks. Threshold sits just above the average level. Ratio 2:1, slow attack, medium release.",
        "character" => "Compression: shape the source as well as control it. Up to 4 dB GR on individual tracks, 1–2 dB on master bus. Vary attack/release by role.",
        "aggressive" => "Compression: heavy GR (5–8 dB on drums, 3–4 dB on vocals), pumping is acceptable for genre fit. Parallel compression encouraged.",
        _ => "",
    };
    let space_rule = match profile.space.as_str() {
        "dry" => "Reverb/delay: minimal. Most tracks ≤ -30 dB send. Only the leads get -22 to -26 dB.",
        "tasteful" => "Reverb/delay: -18 to -24 dB sends on leads and supporting elements. Bass, kick, snare bottoms stay dry.",
        "lush" => "Reverb/delay: -14 to -22 dB sends, longer tails acceptable. Wide stereo space.",
        _ => "",
    };
    let stereo_rule = match profile.stereo_treatment.as_str() {
        "narrow" => "Stereo: keep the image centered and focused. Avoid extreme pans (> 0.5).",
        "natural" => "Stereo: pan to taste with the LCR principle. Most elements between ±0.4; doubled guitars / synths can go wider.",
        "wide" => "Stereo: full LCR. Doubles fully panned, supporting elements out to ±0.7. Mono compatibility still required.",
        _ => "",
    };
    let mut extras = Vec::new();
    if let Some(eng) = &profile.reference_engineer {
        extras.push(format!("Reference engineer: {eng} — channel that style."));
    }
    if let Some(genre) = &profile.genre {
        extras.push(format!("Genre: {genre}. Decisions should suit the genre's conventions."));
    }
    if let Some(notes) = &profile.custom_notes {
        if !notes.trim().is_empty() {
            extras.push(format!("Producer notes: {notes}"));
        }
    }
    format!(
        "Mix philosophy for this session (preset `{}`):\n\
         - Aggressiveness: {aggressiveness_rule}\n\
         - {eq_rule}\n\
         - {comp_rule}\n\
         - {space_rule}\n\
         - {stereo_rule}\n\
         - Loudness target: {target_lufs}.\n\
         {extras}\n\
         These rules override any default tendency to be aggressive. When in doubt, do less — \
         a small turn that's verifiably correct is better than a big turn that might be wrong.\n\n",
        profile.preset_id,
        extras = if extras.is_empty() { String::new() } else { format!("- {}\n", extras.join("\n- ")) },
    )
}

pub fn mixing_fundamentals_block() -> &'static str {
    "Mixing fundamentals:\n\
     - Start with balance and musical hierarchy. The listener should understand what is lead, \
       support, rhythm, low-end foundation, and texture. Do not make every source equally loud.\n\
     - Preserve the emotional focus of the song. If a move makes the hook, lead vocal, groove, \
       or main motif less clear, it is probably the wrong move.\n\
     - Fix level and arrangement masking before reaching for EQ or compression. A 1 dB fader \
       move or pan change is often better than a processor.\n\
     - EQ is for separation and tone. Cut masking frequencies before boosting; use boosts only \
       when they serve a clear role such as presence, air, weight, or excitement.\n\
     - Carve complementary spaces: do not boost the same range on every track. If vocal presence \
       needs 3 kHz, avoid adding competing 3 kHz energy to guitars/keys unless there is a reason.\n\
     - Compression must solve a dynamic problem, add intentional character, or glue a role. Do \
       not compress tracks just because a stage allows compression.\n\
     - Transients matter. Preserve punch on drums, consonants on vocals, and articulation on \
       rhythmic instruments unless the style intentionally wants smoothing.\n\
     - Depth comes from level, pan, EQ, transient shape, and sends together. Use less reverb/delay \
       on elements that should stay forward; use darker/wetter treatment for elements that should \
       sit behind.\n\
     - Low end is a hierarchy, not a pile-up. Kick, bass, sub, and low pads should have distinct \
       jobs; avoid adding low-frequency weight to multiple sources at once.\n\
     - Stereo width should support contrast. Keep anchors centered; use width for doubles, pads, \
       rooms, and ear candy. Do not widen everything.\n\
     - Master moves cannot repair a bad balance. Use master gain mainly for headroom/loudness; \
       fix track-level problems at the track or section level.\n\
     - Prefer small, reversible moves. A/B mentally against the starting point; if the benefit is \
       not measurable or musically obvious, leave it alone.\n\n"
}

pub fn ai_stem_preservation_block(session: &crate::model::MixSession) -> String {
    if session.tracks.is_empty() {
        return String::new();
    }
    let ai_count = session.tracks.iter().filter(|track| track.ai_generated).count();
    let all_ai = ai_count == session.tracks.len();
    let mostly_ai = ai_count * 2 >= session.tracks.len();
    if !mostly_ai {
        return String::new();
    }
    let scope = if all_ai { "all" } else { "most" };
    format!(
        "Already-mixed AI stem preservation rule ({scope} tracks have aiGenerated=true):\n\
         Assume these are stems from an already mixed source, not raw multitrack recordings. \
         Preserve the existing musical hierarchy unless the user explicitly asks for a rebalance. \
         Do NOT force all tracks toward similar peak or LUFS values. Lead vocals may naturally be \
         much louder than backing vocals; backing vocals, doubles, pads, rooms, effects, and support \
         layers may be intentionally low. Kick, snare, bass, and lead elements may dominate peaks.\n\
         For AI-derived stems, prefer tiny changes: usually +/-0.5 to +/-1.5 dB, rarely more than \
         +/-2 dB unless there is clipping, severe headroom trouble, or an explicit user request. \
         Gain staging should mostly protect headroom and fix obvious imbalance; it should not \
         normalize an already mixed arrangement.\n\n"
    )
}

pub fn sections_block(sections: &[crate::model::MixSection], bpm: Option<f32>) -> String {
    if sections.is_empty() {
        return String::new();
    }
    let bpm_str = bpm
        .filter(|v| *v > 0.0)
        .map(|v| format!(" (bpm {v:.0})"))
        .unwrap_or_default();
    let items: Vec<_> = sections
        .iter()
        .map(|s| {
            let mut obj = serde_json::Map::new();
            obj.insert("start".into(), json!(round1(s.start)));
            obj.insert("end".into(), json!(round1(s.end)));
            obj.insert("label".into(), json!(s.label));
            if let Some(a) = &s.analysis {
                obj.insert("lufs".into(), json!(a.lufs));
                obj.insert("peakDb".into(), json!(a.peak_db));
                obj.insert("rmsDb".into(), json!(a.rms_db));
                obj.insert("spectralCentroidHz".into(), json!(a.spectral_centroid_hz));
                obj.insert(
                    "bandEnergy".into(),
                    json!({"low": a.low_energy, "mid": a.mid_energy, "high": a.high_energy}),
                );
                obj.insert("dynamicRangeDb".into(), json!(a.dynamic_range_db));
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    format!(
        "Detected song structure{bpm_str}: {}\n\
         Each section has its own LUFS / peak / centroid / band-energy when analysis is available. \
         Reason per-section: compare values across sections (e.g. \"chorus is 1.2 LUFS quieter than \
         verse — lift master 2 dB in the chorus\"; \"bridge centroid is 4500 Hz vs verse 2100 Hz — \
         tame the bridge with a high shelf\"). When the user references verse/chorus/bridge/etc., \
         map to these section boundaries. To scope a move to a section, create a region with \
         create_region using its start/end (seconds), then use set_region_gain or \
         apply_section_automation against it. Two sections with the same label are likely repeats \
         (verse 1 vs verse 2) — flag dramatic differences between repeats as arrangement issues.\n\n",
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".into()),
    )
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
pub fn build_capability_snapshot(session: &MixSession, selected: &[String]) -> serde_json::Value {
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
        track_obj.insert("aiGenerated".into(), json!(t.ai_generated));
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

pub fn expand_skills_from_actions(mut selected: Vec<String>, actions: &[MixAction]) -> Vec<String> {
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
                MixAction::SetTrackAiGenerated { ai_generated, .. } => format!("{} {name} as AI-generated", if *ai_generated { "marked" } else { "unmarked" }),
                MixAction::SetEqBand { .. } => format!("adjusted EQ on {name}"),
                MixAction::SetHighPass { .. } => format!("cleaned low rumble on {name}"),
                MixAction::SetLowPass { .. } => format!("softened top end on {name}"),
                MixAction::SetCompressor { .. } => format!("set compression on {name}"),
                MixAction::SetReverbSend { .. } => format!("changed reverb depth on {name}"),
                MixAction::SetDelaySend { .. } => format!("changed delay send on {name}"),
                MixAction::SetRegionGain { .. } | MixAction::ApplySectionAutomation { .. } => format!("created a section-scoped move for {name}"),
                MixAction::SetMasterGain { gain_db } => format!("set master gain to {gain_db} dB"),
                MixAction::AdjustMasterGain { delta_db } => format!("{} master by {} dB", if *delta_db > 0.0 { "raised" } else { "lowered" }, delta_db.abs()),
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
        MixAction::SetTrackAiGenerated { .. } => "set_track_ai_generated",
        MixAction::SetHighPass { .. } => "set_high_pass",
        MixAction::SetLowPass { .. } => "set_low_pass",
        MixAction::SetEqBand { .. } => "set_eq_band",
        MixAction::SetCompressor { .. } => "set_compressor",
        MixAction::SetReverbSend { .. } => "set_reverb_send",
        MixAction::SetDelaySend { .. } => "set_delay_send",
        MixAction::SetProcessorParam { .. } => "set_processor_param",
        MixAction::SetRegionGain { .. } => "set_region_gain",
        MixAction::ApplySectionAutomation { .. } => "apply_section_automation",
        MixAction::SetMasterGain { .. } => "set_master_gain",
        MixAction::AdjustMasterGain { .. } => "adjust_master_gain",
        MixAction::Undo => "undo",
        MixAction::Redo => "redo",
        MixAction::RenderMix => "render_mix",
    }
}

fn contains_any(text: &str, words: &[&str]) -> bool {
    let lower = text.to_lowercase();
    words.iter().any(|word| lower.contains(word))
}
