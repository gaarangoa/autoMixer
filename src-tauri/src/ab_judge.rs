use std::{env, fs, path::Path};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hound::{SampleFormat, WavSpec, WavWriter};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    assistant::extract_json_object,
    engine::{
        render::{render_session_to_buffer_with_bypass, RenderedMix},
        source::analysis::{analyze, AudioAnalysis},
    },
    model::MixSession,
};

const DEFAULT_MODEL: &str = "gemini-flash-latest";
const CLIP_SECONDS: f32 = 45.0;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbJudgeOptions {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbJudgeIssue {
    pub category: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbJudgeResponse {
    pub provider: String,
    pub model: String,
    pub winner: String,
    pub confidence: f32,
    pub summary: String,
    pub improvements: Vec<String>,
    pub regressions: Vec<String>,
    pub mix_issues_before: Vec<AbJudgeIssue>,
    pub mix_issues_after: Vec<AbJudgeIssue>,
    pub recommended_next_moves: Vec<String>,
    pub clip_start: f32,
    pub clip_duration: f32,
    pub prompt_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiEnvelope {
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: GeminiContent,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsage {
    #[serde(default)]
    prompt_token_count: Option<u32>,
    #[serde(default)]
    candidates_token_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelJudgePayload {
    winner: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    improvements: Vec<String>,
    #[serde(default)]
    regressions: Vec<String>,
    #[serde(default)]
    mix_issues_before: Vec<AbJudgeIssue>,
    #[serde(default)]
    mix_issues_after: Vec<AbJudgeIssue>,
    #[serde(default)]
    recommended_next_moves: Vec<String>,
}

struct LocalQcMetrics {
    lufs: f32,
    peak_db: f32,
    rms_db: f32,
    dynamic_range_db: f32,
    spectral_centroid_hz: f32,
    low_energy: f32,
    mid_energy: f32,
    high_energy: f32,
    stereo_width: f32,
    correlation: f32,
}

impl LocalQcMetrics {
    fn from_render(render: &RenderedMix, start: f32, duration: f32) -> Self {
        let samples = clip_samples(render, start, duration);
        let analysis = analyze(&samples, render.channels, render.sample_rate);
        let (stereo_width, correlation) = stereo_metrics(&samples, render.channels);
        Self::from_analysis(analysis, stereo_width, correlation)
    }

    fn from_analysis(a: AudioAnalysis, stereo_width: f32, correlation: f32) -> Self {
        Self {
            lufs: a.lufs,
            peak_db: a.peak_db,
            rms_db: a.rms_db,
            dynamic_range_db: a.dynamic_range_db,
            spectral_centroid_hz: a.spectral_centroid_hz,
            low_energy: a.low_energy,
            mid_energy: a.mid_energy,
            high_energy: a.high_energy,
            stereo_width,
            correlation,
        }
    }
}

pub async fn judge_session(
    session: &MixSession,
    temp_dir: &Path,
    options: AbJudgeOptions,
) -> Result<AbJudgeResponse, String> {
    let provider = options.provider.unwrap_or_else(|| "gemini".into());
    if provider == "local" {
        return judge_session_local(session);
    }
    if provider != "gemini" {
        return Err(format!("Unsupported A/B judge provider `{provider}`."));
    }

    let model = options.model.unwrap_or_else(|| DEFAULT_MODEL.into());
    let api_key = options
        .api_key
        .filter(|key| !key.trim().is_empty())
        .or_else(|| env::var("GEMINI_API_KEY").ok())
        .ok_or_else(|| {
            "Missing Gemini API key. Enter one in settings or set GEMINI_API_KEY.".to_string()
        })?;

    let before = render_session_to_buffer_with_bypass(session, true)?;
    let after = render_session_to_buffer_with_bypass(session, false)?;
    let (clip_start, clip_duration) = choose_clip_window(session, &after);

    fs::create_dir_all(temp_dir).map_err(|e| e.to_string())?;
    let before_path = temp_dir.join(format!("ab-before-{}.wav", session.id));
    let after_path = temp_dir.join(format!("ab-after-{}.wav", session.id));
    write_clip_wav(&before, clip_start, clip_duration, &before_path)?;
    write_clip_wav(&after, clip_start, clip_duration, &after_path)?;

    let before_b64 = STANDARD.encode(fs::read(&before_path).map_err(|e| e.to_string())?);
    let after_b64 = STANDARD.encode(fs::read(&after_path).map_err(|e| e.to_string())?);

    let prompt = format!(
        "You are a strict, skeptical mix QC engineer doing an A/B judgment. This is quality control, not coaching.\n\
         Audio A is the unprocessed original/bypass render. Audio B is the current processed mix.\n\
         Do NOT be encouraging, complimentary, optimistic, or polite. Do not use vague praise such as amazing, great, much better, professional, polished, strong, or improved unless you cite the exact audible evidence.\n\
         The processed mix B is not presumed better. Assume B may be worse unless the audio proves otherwise.\n\
         Ignore loudness advantage: if B is louder but not clearer, more balanced, or less fatiguing, treat that as a regression. Do not reward volume, density, or hyped brightness.\n\
         Judge concrete defects only: vocal/instrument hierarchy, low-end control, mud, harshness, masking, transient punch, dynamics, stereo image, depth, noise, clicks, clipping, artifacts, phasey/washed ambience, and over-processing.\n\
         For raw multitrack sessions, be extra harsh: if B still sounds disorganized, crowded, unbalanced, harsh, muddy, or like all tracks are fighting each other, choose winner='before' or 'tie'.\n\
         Choose winner='after' only when B is clearly better on multiple concrete criteria and has no serious regressions. Choose winner='before' when B buries lead elements, worsens harshness/mud, reduces punch/depth, or is merely louder. Use winner='tie' when differences are small or inconclusive.\n\
         Require at least two concrete critical observations in mixIssuesAfter unless B is genuinely clean. Keep summary blunt and technical.\n\
         Return ONLY JSON with this shape:\n\
         {{\"winner\":\"before|after|tie\",\"confidence\":0.0,\"summary\":\"...\",\"improvements\":[\"...\"],\"regressions\":[\"...\"],\"mixIssuesBefore\":[{{\"category\":\"...\",\"severity\":\"low|medium|high\",\"message\":\"...\"}}],\"mixIssuesAfter\":[{{\"category\":\"...\",\"severity\":\"low|medium|high\",\"message\":\"...\"}}],\"recommendedNextMoves\":[\"...\"]}}\n\n\
         Session: {} tracks, clip starts at {:.1}s and lasts {:.1}s. Most stems AI-generated: {}.",
        session.tracks.len(),
        clip_start,
        clip_duration,
        session.tracks.iter().filter(|t| t.ai_generated).count() * 2 >= session.tracks.len().max(1)
    );

    let body = json!({
        "contents": [{
            "role": "user",
            "parts": [
                { "text": prompt },
                { "text": "Audio A: original/bypass render." },
                { "inline_data": { "mime_type": "audio/wav", "data": before_b64 } },
                { "text": "Audio B: current processed mix." },
                { "inline_data": { "mime_type": "audio/wav", "data": after_b64 } }
            ]
        }],
        "generationConfig": {
            "temperature": 0.2,
            "response_mime_type": "application/json"
        }
    });

    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    let response = reqwest::Client::new()
        .post(url)
        .header("X-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Gemini request failed: {e}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Gemini returned {status}: {text}"));
    }

    let envelope: GeminiEnvelope = serde_json::from_str(&text)
        .map_err(|e| format!("Could not parse Gemini response envelope: {e}: {text}"))?;
    let raw = envelope
        .candidates
        .first()
        .and_then(|candidate| {
            candidate
                .content
                .parts
                .iter()
                .find_map(|part| part.text.clone())
        })
        .ok_or_else(|| "Gemini response did not include text output.".to_string())?;
    let extracted = extract_json_object(&raw).unwrap_or(raw);
    let mut payload: ModelJudgePayload = serde_json::from_str(&extracted)
        .map_err(|e| format!("Could not parse A/B judge JSON: {e}: {extracted}"))?;
    enforce_strict_judgment(&mut payload);

    Ok(AbJudgeResponse {
        provider,
        model,
        winner: normalize_winner(&payload.winner),
        confidence: payload.confidence.clamp(0.0, 1.0),
        summary: payload.summary,
        improvements: payload.improvements,
        regressions: payload.regressions,
        mix_issues_before: payload.mix_issues_before,
        mix_issues_after: payload.mix_issues_after,
        recommended_next_moves: payload.recommended_next_moves,
        clip_start,
        clip_duration,
        prompt_tokens: envelope
            .usage_metadata
            .as_ref()
            .and_then(|u| u.prompt_token_count),
        output_tokens: envelope
            .usage_metadata
            .as_ref()
            .and_then(|u| u.candidates_token_count),
    })
}

fn judge_session_local(session: &MixSession) -> Result<AbJudgeResponse, String> {
    let before = render_session_to_buffer_with_bypass(session, true)?;
    let after = render_session_to_buffer_with_bypass(session, false)?;
    let (clip_start, clip_duration) = choose_clip_window(session, &after);
    let before_metrics = LocalQcMetrics::from_render(&before, clip_start, clip_duration);
    let after_metrics = LocalQcMetrics::from_render(&after, clip_start, clip_duration);
    let raw_like = session.tracks.len() >= 24
        || session
            .tracks
            .iter()
            .filter(|track| !track.clips.is_empty())
            .count()
            >= 8;

    let mut before_issues = Vec::new();
    let mut after_issues = Vec::new();
    push_metric_issues("before", &before_metrics, &mut before_issues);
    push_metric_issues("after", &after_metrics, &mut after_issues);

    let mut improvements = Vec::new();
    let mut regressions = Vec::new();
    let mut score = 0.0_f32;

    let lufs_delta = after_metrics.lufs - before_metrics.lufs;
    if after_metrics.peak_db > -0.3 {
        regressions.push(format!(
            "Processed mix peaks too close to clipping ({:.1} dBFS).",
            after_metrics.peak_db
        ));
        score -= 2.0;
    } else if before_metrics.peak_db > -0.3 && after_metrics.peak_db < -1.0 {
        improvements.push(format!(
            "Processed mix improves peak headroom from {:.1} to {:.1} dBFS.",
            before_metrics.peak_db, after_metrics.peak_db
        ));
        score += 1.0;
    }

    if lufs_delta > 2.0 {
        regressions.push(format!(
            "Processed mix is {:.1} dB louder; local QC does not count loudness as quality.",
            lufs_delta
        ));
        score -= 0.8;
    } else if lufs_delta < -4.0 {
        regressions.push(format!(
            "Processed mix is {:.1} dB quieter, which may undercut impact.",
            lufs_delta.abs()
        ));
        score -= 0.5;
    }

    let high_delta = after_metrics.high_energy - before_metrics.high_energy;
    if high_delta > 0.08 || after_metrics.high_energy > 0.46 {
        regressions.push(format!(
            "Processed mix has more high-band energy ({:.2} -> {:.2}), possible harshness/fatigue.",
            before_metrics.high_energy, after_metrics.high_energy
        ));
        score -= 1.0;
    } else if before_metrics.high_energy > 0.46
        && after_metrics.high_energy < before_metrics.high_energy - 0.04
    {
        improvements.push("Processed mix reduces excessive high-band energy.".into());
        score += 0.8;
    }

    let low_delta = after_metrics.low_energy - before_metrics.low_energy;
    if low_delta > 0.10 || after_metrics.low_energy > 0.58 {
        regressions.push(format!(
            "Processed mix has heavier low-band concentration ({:.2} -> {:.2}), possible mud/boom.",
            before_metrics.low_energy, after_metrics.low_energy
        ));
        score -= 0.9;
    } else if before_metrics.low_energy > 0.58
        && after_metrics.low_energy < before_metrics.low_energy - 0.05
    {
        improvements.push("Processed mix reduces excessive low-band buildup.".into());
        score += 0.8;
    }

    if after_metrics.dynamic_range_db < before_metrics.dynamic_range_db - 4.0 {
        regressions.push(format!(
            "Processed mix loses dynamic range ({:.1} -> {:.1} dB), possible over-compression.",
            before_metrics.dynamic_range_db, after_metrics.dynamic_range_db
        ));
        score -= 1.2;
    } else if before_metrics.dynamic_range_db > 18.0
        && after_metrics.dynamic_range_db < before_metrics.dynamic_range_db - 1.5
    {
        improvements.push("Processed mix reins in excessive dynamic range.".into());
        score += 0.7;
    }

    if after_metrics.correlation < 0.05 {
        regressions.push(format!("Processed mix has very low stereo correlation ({:.2}), possible phasey or unstable image.", after_metrics.correlation));
        score -= 1.0;
    } else if before_metrics.correlation < 0.05 && after_metrics.correlation > 0.15 {
        improvements.push("Processed mix improves stereo correlation stability.".into());
        score += 0.6;
    }

    if raw_like && regressions.is_empty() && improvements.len() < 2 {
        regressions.push("Raw-session QC found no strong objective evidence that processing organized the session.".into());
        score -= 0.6;
    }

    if after_issues.len() < 2 {
        after_issues.push(AbJudgeIssue {
            category: "qc".into(),
            severity: "medium".into(),
            message: "Local QC is objective-only; use a listening pass for vocal hierarchy and musical intent.".into(),
        });
    }

    let winner = if score >= 1.4 && regressions.len() <= 1 {
        "after"
    } else if score <= -0.8 || regressions.len() > improvements.len() {
        "before"
    } else {
        "tie"
    };
    let confidence = (0.52 + score.abs().min(3.0) * 0.12).clamp(0.52, 0.86);
    let summary = format!(
        "Local QC: A {:.1} LUFS/{:.1} dBFS peak/{:.1} RMS vs B {:.1} LUFS/{:.1} dBFS peak/{:.1} RMS. High energy {:.2}->{:.2}, mid {:.2}->{:.2}, low {:.2}->{:.2}, DR {:.1}->{:.1} dB, width {:.2}->{:.2}, corr {:.2}->{:.2}. Verdict: {}.",
        before_metrics.lufs,
        before_metrics.peak_db,
        before_metrics.rms_db,
        after_metrics.lufs,
        after_metrics.peak_db,
        after_metrics.rms_db,
        before_metrics.high_energy,
        after_metrics.high_energy,
        before_metrics.mid_energy,
        after_metrics.mid_energy,
        before_metrics.low_energy,
        after_metrics.low_energy,
        before_metrics.dynamic_range_db,
        after_metrics.dynamic_range_db,
        before_metrics.stereo_width,
        after_metrics.stereo_width,
        before_metrics.correlation,
        after_metrics.correlation,
        winner
    );

    Ok(AbJudgeResponse {
        provider: "local".into(),
        model: "local-qc-v1".into(),
        winner: winner.into(),
        confidence,
        summary,
        improvements,
        regressions,
        mix_issues_before: before_issues,
        mix_issues_after: after_issues,
        recommended_next_moves: local_next_moves(&after_metrics),
        clip_start,
        clip_duration,
        prompt_tokens: None,
        output_tokens: None,
    })
}

fn normalize_winner(winner: &str) -> String {
    let lower = winner.trim().to_ascii_lowercase();
    match lower.as_str() {
        "a" | "original" | "orig" | "before" => "before".into(),
        "b" | "mix" | "processed" | "after" => "after".into(),
        _ => "tie".into(),
    }
}

fn push_metric_issues(label: &str, metrics: &LocalQcMetrics, issues: &mut Vec<AbJudgeIssue>) {
    if metrics.peak_db > -0.3 {
        issues.push(issue(
            "headroom",
            "high",
            format!(
                "{label} peak is {:.1} dBFS, too close to clipping.",
                metrics.peak_db
            ),
        ));
    } else if metrics.peak_db > -1.0 {
        issues.push(issue(
            "headroom",
            "medium",
            format!(
                "{label} peak is {:.1} dBFS; limited mastering headroom.",
                metrics.peak_db
            ),
        ));
    }
    if metrics.low_energy > 0.58 {
        issues.push(issue(
            "tonality",
            "medium",
            format!(
                "{label} low-band energy is high ({:.2}); possible mud or boom.",
                metrics.low_energy
            ),
        ));
    }
    if metrics.mid_energy > 0.68 {
        issues.push(issue(
            "tonality",
            "medium",
            format!(
                "{label} mid-band energy is concentrated ({:.2}); possible boxiness or masking.",
                metrics.mid_energy
            ),
        ));
    }
    if metrics.high_energy > 0.46 || metrics.spectral_centroid_hz > 5200.0 {
        issues.push(issue("tonality", "medium", format!("{label} high-band energy/centroid is elevated ({:.2}, {:.0} Hz); possible harshness.", metrics.high_energy, metrics.spectral_centroid_hz)));
    }
    if metrics.rms_db > -8.0 {
        issues.push(issue(
            "loudness",
            "medium",
            format!(
                "{label} RMS is high ({:.1} dBFS); possible density or limited dynamics.",
                metrics.rms_db
            ),
        ));
    }
    if metrics.dynamic_range_db < 6.0 {
        issues.push(issue(
            "dynamics",
            "medium",
            format!(
                "{label} dynamic range is only {:.1} dB; possible over-compression.",
                metrics.dynamic_range_db
            ),
        ));
    } else if metrics.dynamic_range_db > 24.0 {
        issues.push(issue(
            "dynamics",
            "medium",
            format!(
                "{label} dynamic range is {:.1} dB; likely uneven or under-controlled.",
                metrics.dynamic_range_db
            ),
        ));
    }
    if metrics.correlation < 0.05 {
        issues.push(issue(
            "stereo",
            "high",
            format!(
                "{label} stereo correlation is {:.2}; possible phase/image instability.",
                metrics.correlation
            ),
        ));
    } else if metrics.stereo_width > 1.5 {
        issues.push(issue(
            "stereo",
            "medium",
            format!(
                "{label} stereo width is high ({:.2}); verify width is not phasey or unfocused.",
                metrics.stereo_width
            ),
        ));
    }
}

fn issue(category: &str, severity: &str, message: String) -> AbJudgeIssue {
    AbJudgeIssue {
        category: category.into(),
        severity: severity.into(),
        message,
    }
}

fn local_next_moves(metrics: &LocalQcMetrics) -> Vec<String> {
    let mut moves = Vec::new();
    if metrics.peak_db > -1.0 {
        moves.push("Lower master or loud tracks to restore at least 3 dB peak headroom.".into());
    }
    if metrics.low_energy > 0.58 {
        moves.push("Reduce low-mid buildup with high-pass filters on non-bass/non-kick tracks and selective 200-400 Hz cuts.".into());
    }
    if metrics.high_energy > 0.46 || metrics.spectral_centroid_hz > 5200.0 {
        moves.push("Check cymbals, vocal presence, guitars, and limiter brightness for harshness around 3-8 kHz.".into());
    }
    if metrics.dynamic_range_db > 20.0 {
        moves.push("Use light compression or clip gain on uneven lead sources before pushing overall loudness.".into());
    }
    if metrics.correlation < 0.05 {
        moves.push(
            "Narrow or rebalance phasey stereo/room sources before adding more width.".into(),
        );
    }
    if moves.is_empty() {
        moves.push("Use a listening pass to verify vocal hierarchy, groove impact, and masking; objective QC found no dominant metric failure.".into());
    }
    moves
}

fn clip_samples(render: &RenderedMix, start_seconds: f32, duration_seconds: f32) -> Vec<f32> {
    let channels = render.channels.max(1) as usize;
    let sample_rate = render.sample_rate.max(1);
    let start_frame = (start_seconds.max(0.0) * sample_rate as f32).round() as usize;
    let frame_count = (duration_seconds.max(0.1) * sample_rate as f32).round() as usize;
    let total_frames = render.samples.len() / channels;
    let end_frame = (start_frame + frame_count).min(total_frames);
    if start_frame >= end_frame {
        return Vec::new();
    }
    render.samples[start_frame * channels..end_frame * channels].to_vec()
}

fn stereo_metrics(samples: &[f32], channels: u16) -> (f32, f32) {
    if channels < 2 || samples.len() < 4 {
        return (0.0, 1.0);
    }
    let channels = channels as usize;
    let frames = samples.len() / channels;
    let mut side_sq = 0.0_f64;
    let mut mid_sq = 0.0_f64;
    let mut l_sq = 0.0_f64;
    let mut r_sq = 0.0_f64;
    let mut lr = 0.0_f64;
    for frame in 0..frames {
        let l = samples[frame * channels] as f64;
        let r = samples[frame * channels + 1] as f64;
        let mid = (l + r) * 0.5;
        let side = (l - r) * 0.5;
        mid_sq += mid * mid;
        side_sq += side * side;
        l_sq += l * l;
        r_sq += r * r;
        lr += l * r;
    }
    let width = if mid_sq > 1.0e-12 {
        (side_sq / mid_sq).sqrt() as f32
    } else {
        0.0
    };
    let corr = if l_sq > 1.0e-12 && r_sq > 1.0e-12 {
        (lr / (l_sq.sqrt() * r_sq.sqrt())) as f32
    } else {
        1.0
    };
    (width, corr.clamp(-1.0, 1.0))
}

fn enforce_strict_judgment(payload: &mut ModelJudgePayload) {
    let lower = payload.summary.to_ascii_lowercase();
    let praise_words = [
        "amazing",
        "great",
        "much better",
        "professional",
        "polished",
        "excellent",
        "fantastic",
    ];
    let vague_praise = praise_words.iter().any(|word| lower.contains(word));
    if vague_praise && payload.regressions.is_empty() && payload.mix_issues_after.is_empty() {
        payload.winner = "tie".into();
        payload.confidence = payload.confidence.min(0.55);
        payload.regressions.push(
            "The judgment used vague praise without identifying concrete audible tradeoffs.".into(),
        );
        payload.mix_issues_after.push(AbJudgeIssue {
            category: "qc".into(),
            severity: "medium".into(),
            message: "Re-run judgment skeptically: no mix should pass on positive language alone."
                .into(),
        });
    }
    payload.summary = strip_vague_praise(&payload.summary);
    payload.improvements = payload
        .improvements
        .iter()
        .map(|item| strip_vague_praise(item))
        .filter(|item| !item.trim().is_empty())
        .collect();
}

fn strip_vague_praise(text: &str) -> String {
    let mut out = text.to_string();
    for phrase in [
        "amazing",
        "great",
        "much better",
        "professional",
        "polished",
        "excellent",
        "fantastic",
    ] {
        out = replace_case_insensitive(&out, phrase, "specific");
    }
    out
}

fn replace_case_insensitive(input: &str, needle: &str, replacement: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut result = String::new();
    let mut cursor = 0;
    let mut search_start = 0;
    while let Some(pos) = lower[search_start..].find(&needle_lower) {
        let start = search_start + pos;
        let end = start + needle.len();
        result.push_str(&input[cursor..start]);
        result.push_str(replacement);
        cursor = end;
        search_start = end;
    }
    result.push_str(&input[cursor..]);
    result
}

fn choose_clip_window(session: &MixSession, render: &RenderedMix) -> (f32, f32) {
    let duration = render.samples.len() as f32
        / render.channels.max(1) as f32
        / render.sample_rate.max(1) as f32;
    let clip_duration = CLIP_SECONDS.min(duration.max(1.0));
    if let Some(section) = session
        .sections
        .iter()
        .find(|s| s.label.to_ascii_lowercase().contains("chorus"))
        .or_else(|| {
            session
                .sections
                .iter()
                .max_by(|a, b| section_len(a).total_cmp(&section_len(b)))
        })
    {
        let midpoint = (section.start + section.end) * 0.5;
        let start =
            (midpoint - clip_duration * 0.5).clamp(0.0, (duration - clip_duration).max(0.0));
        return (start, clip_duration);
    }
    (((duration - clip_duration) * 0.5).max(0.0), clip_duration)
}

fn section_len(section: &crate::model::MixSection) -> f32 {
    (section.end - section.start).max(0.0)
}

fn write_clip_wav(
    render: &RenderedMix,
    start_seconds: f32,
    duration_seconds: f32,
    path: &Path,
) -> Result<(), String> {
    let channels = render.channels.max(1) as usize;
    let sample_rate = render.sample_rate.max(1);
    let start_frame = (start_seconds.max(0.0) * sample_rate as f32).round() as usize;
    let frame_count = (duration_seconds.max(0.1) * sample_rate as f32).round() as usize;
    let total_frames = render.samples.len() / channels;
    let end_frame = (start_frame + frame_count).min(total_frames);

    let spec = WavSpec {
        channels: render.channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for frame in start_frame..end_frame {
        for channel in 0..channels {
            let sample = render.samples[frame * channels + channel].clamp(-1.0, 1.0);
            writer
                .write_sample((sample * i16::MAX as f32) as i16)
                .map_err(|e| e.to_string())?;
        }
    }
    writer.finalize().map_err(|e| e.to_string())
}
