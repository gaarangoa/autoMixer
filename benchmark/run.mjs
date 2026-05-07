#!/usr/bin/env node
import { readFile, writeFile } from "node:fs/promises";

const ROOT = new URL("./", import.meta.url);
const OLLAMA_URL = (process.env.OLLAMA_URL ?? "http://localhost:11434").replace(/\/+$/, "");
const MODEL = process.env.MODEL;
const LIMIT = process.env.LIMIT ? Number(process.env.LIMIT) : Infinity;
const TIERS = process.env.TIER ? process.env.TIER.split(",").map((s) => s.trim()) : null;
const SKILL_TIMEOUT_MS = Number(process.env.SKILL_TIMEOUT_MS ?? 180_000);
const ACTION_TIMEOUT_MS = Number(process.env.ACTION_TIMEOUT_MS ?? 180_000);

if (!MODEL) {
  console.error("Set MODEL=<ollama-tag> (e.g. MODEL=gemma3:4b).");
  process.exit(1);
}

const DEFAULT_TOLERANCES = {
  deltaDb: 1.0,
  gainDb: 1.5,
  frequencyHz: 0.4,
  ratio: 0.8,
  attackMs: 30,
  releaseMs: 80,
  kneeDb: 4,
  makeupDb: 2,
  q: 0.3,
  levelDb: 4,
  pan: 0.25,
  value: 1.5,
};

const SKILL_CATALOG = [
  { id: "balance", name: "Balance", whenToUse: "Use for level, mute, solo, and pan moves." },
  { id: "tonal_eq", name: "Tonal EQ", whenToUse: "Use for brightness, warmth, mud, harshness, rumble, and presence." },
  { id: "dynamics", name: "Dynamics", whenToUse: "Use for compression, punch, control, and steadiness." },
  { id: "space_depth", name: "Space And Depth", whenToUse: "Use for reverb, delay, dry/close, and depth moves." },
  { id: "region_automation", name: "Region Automation", whenToUse: "Use for section-scoped moves on selected regions." },
  { id: "safety_undo", name: "Safety And Undo", whenToUse: "Use for undo and redo requests." },
  { id: "render_export", name: "Render Export", whenToUse: "Use when the user asks to render or export the current mix." },
];

async function loadJSON(name) {
  return JSON.parse(await readFile(new URL(name, ROOT), "utf8"));
}

function applyAliases(text, aliases, toAlias) {
  let out = text;
  for (const [alias, real] of aliases) {
    const from = toAlias ? `"${real}"` : `"${alias}"`;
    const to = toAlias ? `"${alias}"` : `"${real}"`;
    out = out.split(from).join(to);
  }
  return out;
}

function buildCapabilitySnapshot(session, selectedSkills) {
  const tracks = session.tracks.map((t) => {
    const params = {};
    if (selectedSkills.includes("balance")) {
      params.gainDb = { current: t.gainDb, min: -24, max: 24, unit: "dB" };
      params.pan = { current: t.pan, min: -1, max: 1 };
    }
    if (selectedSkills.includes("tonal_eq")) {
      params.eq = [
        { band: 0, frequencyHz: { current: 80, min: 20, max: 20000 }, gainDb: { current: 0, min: -12, max: 12 }, q: { current: 0.7, min: 0.2, max: 10 } },
        { band: 1, frequencyHz: { current: 300, min: 20, max: 20000 }, gainDb: { current: 0, min: -12, max: 12 }, q: { current: 1.0, min: 0.2, max: 10 } },
        { band: 2, frequencyHz: { current: 3000, min: 20, max: 20000 }, gainDb: { current: 0, min: -12, max: 12 }, q: { current: 1.0, min: 0.2, max: 10 } },
        { band: 3, frequencyHz: { current: 10000, min: 20, max: 20000 }, gainDb: { current: 0, min: -12, max: 12 }, q: { current: 0.7, min: 0.2, max: 10 } },
      ];
      params.highPass = { enabled: false, frequencyHz: { current: 40, min: 20, max: 20000 }, slopeDbOct: [12, 24] };
      params.lowPass = { enabled: false, frequencyHz: { current: 18000, min: 20, max: 20000 }, slopeDbOct: [12, 24] };
    }
    if (selectedSkills.includes("dynamics")) {
      params.compressor = {
        enabled: false,
        thresholdDb: { current: -18, min: -60, max: 0 },
        ratio: { current: 2, min: 1, max: 20 },
        attackMs: { current: 10, min: 1, max: 200 },
        releaseMs: { current: 120, min: 20, max: 1000 },
        kneeDb: { current: 6, min: 0, max: 24 },
        makeupDb: { current: 0, min: -12, max: 12 },
      };
    }
    if (selectedSkills.includes("space_depth")) {
      params.sends = {
        reverbDb: { current: -60, min: -60, max: 0 },
        delayDb: { current: -60, min: -60, max: 0 },
      };
    }
    return { id: t.id, name: t.name, role: t.role, params, audio: t.audio };
  });
  return { selectedSkills, tracks, regions: session.regions };
}

function buildSkillPrompt(userText, session, selectedTrackIds, selectedRegionIds) {
  return [
    "You are a mix engineer routing user requests to skills.",
    "Return JSON {\"selectedSkillIds\":[\"...\"]} only.",
    "",
    `Available skills:\n${JSON.stringify({ skills: SKILL_CATALOG })}`,
    "",
    `Tracks:\n${JSON.stringify(session.tracks.map((t) => ({ id: t.id, name: t.name, role: t.role })))}`,
    "",
    `Selected track ids: ${JSON.stringify(selectedTrackIds)}`,
    `Selected region ids: ${JSON.stringify(selectedRegionIds)}`,
    "",
    `Request: ${userText}`,
    "",
    "Pick 1-3 skills that best fit. If undo/redo, include safety_undo. If render/export, include render_export. If section/region/chorus/verse, include region_automation.",
  ].join("\n");
}

function buildActionPrompt(userText, session, selectedSkills, selectedTrackIds, selectedRegionIds) {
  const snapshot = buildCapabilitySnapshot(session, selectedSkills);
  const tracksMin = session.tracks.map((t) => ({ id: t.id, name: t.name, role: t.role, gainDb: t.gainDb, pan: t.pan }));
  return [
    "You are an assistant mix engineer. Return ONLY a JSON object with this exact shape:",
    '{ "actions": [ <action>, ... ], "rationale": "...", "perActionNotes": [ "...", ... ] }',
    "",
    "Each <action> is a flat JSON object — NO wrapper key like `tool_params`. The discriminator is the `tool` field (snake_case). Every other field uses camelCase. Track ids are short tokens like `tk0`, `tk1` (and region ids like `rg0`); use them exactly as they appear in the Tracks list. Never invent or modify them.",
    "",
    "Allowed `tool` values and their exact field shapes (do NOT use skill names like \"tonal_eq\" or \"dynamics\"):",
    '- {"tool":"set_track_gain","trackId":"...","gainDb":-3.0}',
    '- {"tool":"adjust_track_gain","trackId":"...","deltaDb":1.5}',
    '- {"tool":"set_track_pan","trackId":"...","pan":-0.3}',
    '- {"tool":"mute_track","trackId":"...","muted":true}',
    '- {"tool":"solo_track","trackId":"...","solo":true}',
    '- {"tool":"set_high_pass","trackId":"...","frequencyHz":80,"slopeDbOct":12}',
    '- {"tool":"set_low_pass","trackId":"...","frequencyHz":18000,"slopeDbOct":12}',
    '- {"tool":"set_eq_band","trackId":"...","band":0,"frequencyHz":120,"gainDb":-2,"q":1.0}',
    '- {"tool":"set_compressor","trackId":"...","thresholdDb":-18,"ratio":3,"attackMs":10,"releaseMs":120,"kneeDb":6,"makeupDb":1}',
    '- {"tool":"set_reverb_send","trackId":"...","levelDb":-18}',
    '- {"tool":"set_delay_send","trackId":"...","levelDb":-22}',
    '- {"tool":"set_region_gain","regionId":"...","trackId":"...","gainDb":-1.5}',
    '- {"tool":"apply_section_automation","regionId":"...","trackId":"...","param":"gainDb","value":-2}',
    '- {"tool":"undo"} | {"tool":"redo"} | {"tool":"render_mix"}',
    "",
    `Selected skills: ${JSON.stringify(selectedSkills)}`,
    "",
    `Capability snapshot:\n${JSON.stringify(snapshot)}`,
    "",
    `Tracks:\n${JSON.stringify(tracksMin)}`,
    "",
    `Selected track ids: ${JSON.stringify(selectedTrackIds)}`,
    `Selected region ids: ${JSON.stringify(selectedRegionIds)}`,
    "",
    "Routing guidance: frequency/EQ/low/mid/high/air/presence/bright/dark/harsh/muddy/body requests should use EQ/filter actions, not gain. Vocal upfront/presence/clarity requests usually combine a subtle level move with presence EQ and light compression.",
    "",
    "Audio analysis per track is included under track.audio. Use it to ground decisions: spectralCentroidHz < 1500 = dark, > 3500 = bright; bandEnergy.low/mid/high are normalized shares (sum ~ 1) — high low_energy with low high_energy = muddy; lufs around -23 LUFS is broadcast loudness; dynamicRangeDb < 6 = heavily compressed; peakDb close to 0 indicates limited headroom.",
    "",
    `Request: ${userText}`,
  ].join("\n");
}

async function ollamaGenerate(prompt, timeoutMs) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const resp = await fetch(`${OLLAMA_URL}/api/generate`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model: MODEL, prompt, stream: false }),
      signal: ctrl.signal,
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const body = await resp.json();
    return body.response;
  } finally {
    clearTimeout(timer);
  }
}

function extractJsonObject(raw) {
  if (!raw) return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const stripped = trimmed
    .replace(/^```json\s*/i, "")
    .replace(/^```\s*/i, "")
    .replace(/```\s*$/i, "")
    .trim();
  const start = stripped.indexOf("{");
  const end = stripped.lastIndexOf("}");
  if (start < 0 || end <= start) return null;
  return stripped.slice(start, end + 1);
}

function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out`)), ms)),
  ]);
}

function tolerance(field) {
  return DEFAULT_TOLERANCES[field] ?? 1.0;
}

function withinTolerance(field, expected, actual) {
  if (typeof expected !== "number" || typeof actual !== "number") return expected === actual;
  if (field === "frequencyHz") {
    if (expected === 0) return actual === 0;
    return Math.abs(actual - expected) / expected <= tolerance(field);
  }
  return Math.abs(actual - expected) <= tolerance(field);
}

function resolveTrack(session, expectedAction) {
  if (expectedAction.trackRole) {
    return session.tracks.find((t) => t.role === expectedAction.trackRole);
  }
  if (expectedAction.trackName) {
    return session.tracks.find((t) => t.name.toLowerCase() === expectedAction.trackName.toLowerCase());
  }
  return undefined;
}

function resolveRegion(session, expectedAction) {
  if (expectedAction.regionName) {
    return session.regions.find((r) => r.name === expectedAction.regionName);
  }
  return undefined;
}

function gradeCase(caseDef, sessions, attempt) {
  const session = sessions[caseDef.session];
  const expectedSkills = caseDef.expected.skills ?? [];
  const expectedActions = caseDef.expected.actions ?? [];
  const skillsHit = expectedSkills.length === 0
    ? 1
    : expectedSkills.filter((s) => attempt.selectedSkills.includes(s)).length / expectedSkills.length;

  let toolHits = 0;
  let targetHits = 0;
  let valuesHit = 0;
  let valuesTotal = 0;

  for (const expected of expectedActions) {
    const acceptableTools = new Set([expected.tool, ...(expected.acceptableTools ?? [])]);
    const targetTrack = resolveTrack(session, expected);
    const targetRegion = resolveRegion(session, expected);

    const matched = (attempt.actions ?? []).find((act) => {
      if (!acceptableTools.has(act.tool)) return false;
      if (targetTrack && act.trackId && act.trackId !== targetTrack.id) return false;
      if (expected.tool === "set_processor_param" && act.targetId !== targetTrack?.id) return false;
      if (targetRegion && act.regionId && act.regionId !== targetRegion.id) return false;
      return true;
    });

    if (matched) {
      toolHits += 1;
      targetHits += 1;
      for (const [field, value] of Object.entries(expected)) {
        if (["tool", "trackRole", "trackName", "regionName", "acceptableTools"].includes(field)) continue;
        valuesTotal += 1;
        if (withinTolerance(field, value, matched[field])) {
          valuesHit += 1;
        }
      }
    } else {
      const sameToolAct = (attempt.actions ?? []).find((act) => acceptableTools.has(act.tool));
      if (sameToolAct) toolHits += 0.5;
    }
  }

  const toolsHitFrac = expectedActions.length === 0 ? 1 : toolHits / expectedActions.length;
  const targetHitFrac = expectedActions.length === 0 ? 1 : targetHits / expectedActions.length;
  const valuesHitFrac = valuesTotal === 0 ? 1 : valuesHit / valuesTotal;
  const passed = skillsHit >= 0.5 && toolsHitFrac >= 0.7 && targetHitFrac === 1 && valuesHitFrac >= 0.6;
  return { skillsHit, toolsHit: toolsHitFrac, targetHit: targetHitFrac, valuesHit: valuesHitFrac, passed };
}

async function runCase(caseDef, sessions) {
  const session = sessions[caseDef.session];
  if (!session) throw new Error(`Unknown session ${caseDef.session}`);

  const trackAliases = session.tracks.map((t, i) => [`tk${i}`, t.id]);
  const regionAliases = (session.regions ?? []).map((r, i) => [`rg${i}`, r.id]);

  // 1. Skill routing
  let selectedSkills = [];
  let skillRaw = null;
  let skillError = null;
  try {
    let prompt = buildSkillPrompt(caseDef.userText, session, caseDef.selectedTrackIds, caseDef.selectedRegionIds);
    prompt = applyAliases(prompt, trackAliases, true);
    prompt = applyAliases(prompt, regionAliases, true);
    const raw = await withTimeout(ollamaGenerate(prompt, SKILL_TIMEOUT_MS), SKILL_TIMEOUT_MS + 1000, "skill call");
    skillRaw = raw;
    const extracted = extractJsonObject(raw);
    if (!extracted) throw new Error("no JSON object in response");
    const parsed = JSON.parse(extracted);
    selectedSkills = (parsed.selectedSkillIds ?? parsed.selected_skill_ids ?? []).slice().sort();
  } catch (err) {
    skillError = err.message;
    selectedSkills = caseDef.expected.skills ?? [];
  }

  // 2. Action generation
  let actions = [];
  let actionRaw = null;
  let actionError = null;
  try {
    let prompt = buildActionPrompt(caseDef.userText, session, selectedSkills, caseDef.selectedTrackIds, caseDef.selectedRegionIds);
    prompt = applyAliases(prompt, trackAliases, true);
    prompt = applyAliases(prompt, regionAliases, true);
    const raw = await withTimeout(ollamaGenerate(prompt, ACTION_TIMEOUT_MS), ACTION_TIMEOUT_MS + 1000, "action call");
    actionRaw = raw;
    let unaliased = applyAliases(raw, trackAliases, false);
    unaliased = applyAliases(unaliased, regionAliases, false);
    const extracted = extractJsonObject(unaliased);
    if (!extracted) throw new Error("no JSON object in response");
    const parsed = JSON.parse(extracted);
    actions = parsed.actions ?? [];
  } catch (err) {
    actionError = err.message;
  }

  return { selectedSkills, actions, skillRaw, skillError, actionRaw, actionError };
}

function pad(s, n) {
  s = String(s);
  return s.length >= n ? s : s + " ".repeat(n - s.length);
}

function summarize(results) {
  const buckets = {};
  for (const r of results) {
    const tier = r.case.complexity;
    buckets[tier] ??= { total: 0, passed: 0, skillsSum: 0, toolsSum: 0, targetSum: 0, valuesSum: 0 };
    const b = buckets[tier];
    b.total += 1;
    if (r.score.passed) b.passed += 1;
    b.skillsSum += r.score.skillsHit;
    b.toolsSum += r.score.toolsHit;
    b.targetSum += r.score.targetHit;
    b.valuesSum += r.score.valuesHit;
  }
  console.log("\n=== Summary ===");
  console.log(pad("tier", 12) + pad("pass", 10) + pad("skills", 10) + pad("tools", 10) + pad("target", 10) + pad("values", 10));
  for (const [tier, b] of Object.entries(buckets)) {
    console.log(
      pad(tier, 12) +
      pad(`${b.passed}/${b.total}`, 10) +
      pad((b.skillsSum / b.total).toFixed(2), 10) +
      pad((b.toolsSum / b.total).toFixed(2), 10) +
      pad((b.targetSum / b.total).toFixed(2), 10) +
      pad((b.valuesSum / b.total).toFixed(2), 10)
    );
  }
}

async function main() {
  const sessions = await loadJSON("sessions.json");
  const cases = await loadJSON("cases.json");

  const filtered = cases
    .filter((c) => (TIERS ? TIERS.includes(c.complexity) : true))
    .slice(0, LIMIT);

  console.log(`Running ${filtered.length}/${cases.length} cases against ${MODEL} at ${OLLAMA_URL}`);
  const results = [];
  for (const caseDef of filtered) {
    const t0 = Date.now();
    const attempt = await runCase(caseDef, sessions);
    const score = gradeCase(caseDef, sessions, attempt);
    const ms = Date.now() - t0;
    const flag = score.passed ? "PASS" : "FAIL";
    console.log(
      `${pad(caseDef.id, 4)} ${pad(caseDef.complexity, 9)} ${flag}  ` +
      `skills=${score.skillsHit.toFixed(2)} tools=${score.toolsHit.toFixed(2)} ` +
      `target=${score.targetHit.toFixed(2)} values=${score.valuesHit.toFixed(2)}  ${ms}ms` +
      (attempt.skillError ? `  [skillErr: ${attempt.skillError}]` : "") +
      (attempt.actionError ? `  [actionErr: ${attempt.actionError}]` : "")
    );
    results.push({ case: caseDef, attempt, score, ms });
  }
  summarize(results);

  const outFile = new URL(`./results-${MODEL.replace(/[^a-z0-9.-]+/gi, "_")}.json`, ROOT);
  await writeFile(outFile, JSON.stringify({ model: MODEL, ollamaUrl: OLLAMA_URL, results }, null, 2));
  console.log(`\nDetails written to ${outFile.pathname}`);
}

main().then(() => process.exit(0)).catch((err) => {
  console.error(err);
  process.exit(1);
});
