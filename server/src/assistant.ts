import type { AssistantRequest, AssistantResponse, MixAction, MixSession, Track } from "../../shared/types";
import { applyActions, redo, undo, validateActions } from "./actions";
import { buildCapabilitySnapshot, selectSkills, skillCatalog } from "./capabilities";
import type { Config } from "./config";
import type { SessionStore } from "./store";

export async function handleAssistant(store: SessionStore, config: Config, request: AssistantRequest): Promise<AssistantResponse> {
  const project = await store.getProject(request.sessionId);
  const selectedSkills = selectSkills(request.userText);

  if (selectedSkills.includes("safety_undo") && /\bundo|revert\b/i.test(request.userText)) {
    const entry = undo(project);
    await store.save(project);
    return {
      status: "ok",
      explanation: entry ? "Undid the last mix change." : "There was nothing to undo.",
      actions: [{ tool: "undo" }],
      warnings: entry ? [] : ["No history entry was available."],
      selectedSkills,
      session: project.session,
      history: project.history
    };
  }

  if (selectedSkills.includes("safety_undo") && /\bredo\b/i.test(request.userText)) {
    const entry = redo(project);
    await store.save(project);
    return {
      status: "ok",
      explanation: entry ? "Redid the last undone mix change." : "There was nothing to redo.",
      actions: [{ tool: "redo" }],
      warnings: entry ? [] : ["No redo entry was available."],
      selectedSkills,
      session: project.session,
      history: project.history
    };
  }

  const capabilitySnapshot = buildCapabilitySnapshot(project.session, selectedSkills);
  const actions = (await tryModelActions(config, request, project.session, selectedSkills)) ?? heuristicActions(request, project.session, selectedSkills);

  if (!actions.length) {
    return {
      status: "clarification",
      question: "Which track should I change?",
      reason: "The request did not clearly map to a track or selected track."
    };
  }

  try {
    validateActions(project.session, actions);
    validateAgainstCapabilities(capabilitySnapshot.actions, actions);
    const explanation = explainActions(actions, project.session);
    const result = applyActions(project, actions, "assistant", explanation);
    if (result.entry) await store.pushHistory(project, result.entry);
    else await store.save(project);
    return {
      status: "ok",
      explanation,
      actions,
      warnings: [],
      selectedSkills,
      session: project.session,
      history: project.history
    };
  } catch (error) {
    return {
      status: "err",
      kind: "InvalidActions",
      message: error instanceof Error ? error.message : "Invalid assistant action"
    };
  }
}

async function tryModelActions(
  config: Config,
  request: AssistantRequest,
  session: MixSession,
  selectedSkills: string[]
): Promise<MixAction[] | undefined> {
  if (!config.ollamaBaseUrl) return undefined;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 4500);
  try {
    const prompt = [
      "You are an assistant mix engineer. Return JSON only.",
      "Use only these selected skills:",
      JSON.stringify(selectedSkills),
      "Available skill cards:",
      JSON.stringify(skillCatalog),
      "Tracks:",
      JSON.stringify(session.tracks.map((track) => ({ id: track.id, name: track.name, role: track.role, gainDb: track.gainDb, pan: track.pan }))),
      "Request:",
      request.userText,
      'Return shape: {"actions":[...]} using snake_case tool names. Prefer subtle moves.'
    ].join("\n");
    const response = await fetch(`${config.ollamaBaseUrl}/api/generate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: config.ollamaModel, prompt, stream: false, format: "json" }),
      signal: controller.signal
    });
    if (!response.ok) return undefined;
    const data = (await response.json()) as { response?: string };
    if (!data.response) return undefined;
    const parsed = JSON.parse(data.response) as { actions?: MixAction[] };
    return parsed.actions;
  } catch {
    return undefined;
  } finally {
    clearTimeout(timeout);
  }
}

function heuristicActions(request: AssistantRequest, session: MixSession, selectedSkills: string[]): MixAction[] {
  const text = request.userText.toLowerCase();
  const track = resolveTrack(request, session);
  if (!track) return [];

  if (selectedSkills.includes("render_export")) return [{ tool: "render_mix" }];

  const actions: MixAction[] = [];

  if (selectedSkills.includes("region_automation") && session.regions.length && request.selectedRegionIds[0]) {
    if (text.includes("louder") || text.includes("upfront") || text.includes("up")) {
      return [{ tool: "set_region_gain", regionId: request.selectedRegionIds[0], trackId: track.id, gainDb: 1.5 }];
    }
    if (text.includes("quieter") || text.includes("down") || text.includes("behind")) {
      return [{ tool: "set_region_gain", regionId: request.selectedRegionIds[0], trackId: track.id, gainDb: -1.5 }];
    }
  }

  if (text.includes("mute")) actions.push({ tool: "mute_track", trackId: track.id, muted: true });
  if (text.includes("solo")) actions.push({ tool: "solo_track", trackId: track.id, solo: true });
  if (/\b(center|centre)\b/.test(text)) actions.push({ tool: "set_track_pan", trackId: track.id, pan: 0 });
  if (/\bleft\b/.test(text)) actions.push({ tool: "set_track_pan", trackId: track.id, pan: -0.45 });
  if (/\bright\b/.test(text)) actions.push({ tool: "set_track_pan", trackId: track.id, pan: 0.45 });
  if (/\bwider|wide\b/.test(text)) {
    const direction = track.pan < 0 ? -0.6 : track.pan > 0 ? 0.6 : 0.35;
    actions.push({ tool: "set_track_pan", trackId: track.id, pan: direction });
  }
  if (/\blouder|upfront|forward|bring.*up\b/.test(text)) actions.push({ tool: "adjust_track_gain", trackId: track.id, deltaDb: 1.5 });
  if (/\bquieter|behind|back|lower|down\b/.test(text)) actions.push({ tool: "adjust_track_gain", trackId: track.id, deltaDb: -1.5 });

  if (selectedSkills.includes("tonal_eq")) {
    if (/\brumble|low end cleanup|clean up\b/.test(text)) actions.push({ tool: "set_high_pass", trackId: track.id, frequencyHz: track.role === "bass" || track.role === "kick" ? 30 : 90, slopeDbOct: 12 });
    if (/\bbright|air|presence\b/.test(text)) actions.push({ tool: "set_eq_band", trackId: track.id, band: 3, frequencyHz: 8000, gainDb: 1.5, q: 0.7 });
    if (/\bharsh|piercing\b/.test(text)) actions.push({ tool: "set_eq_band", trackId: track.id, band: 2, frequencyHz: 3500, gainDb: -1.5, q: 1.2 });
    if (/\bmud|muddy|boxy\b/.test(text)) actions.push({ tool: "set_eq_band", trackId: track.id, band: 1, frequencyHz: 350, gainDb: -1.8, q: 1.1 });
  }

  if (selectedSkills.includes("dynamics")) {
    actions.push({
      tool: "set_compressor",
      trackId: track.id,
      thresholdDb: -20,
      ratio: /\bpunch|punchier\b/.test(text) ? 3 : 2,
      attackMs: /\bpunch|punchier\b/.test(text) ? 25 : 12,
      releaseMs: 140,
      kneeDb: 6,
      makeupDb: 1
    });
  }

  if (selectedSkills.includes("space_depth")) {
    if (/\breverb|space|ambience|room|farther|deeper\b/.test(text)) actions.push({ tool: "set_reverb_send", trackId: track.id, levelDb: -18 });
    if (/\bdelay|echo\b/.test(text)) actions.push({ tool: "set_delay_send", trackId: track.id, levelDb: -20 });
    if (/\bcloser|dry\b/.test(text)) actions.push({ tool: "set_reverb_send", trackId: track.id, levelDb: -60 });
  }

  if (!actions.length && selectedSkills.includes("balance")) actions.push({ tool: "adjust_track_gain", trackId: track.id, deltaDb: 1 });
  return actions.slice(0, 4);
}

function resolveTrack(request: AssistantRequest, session: MixSession): Track | undefined {
  if (request.selectedTrackIds.length) return session.tracks.find((track) => track.id === request.selectedTrackIds[0]);
  const text = request.userText.toLowerCase();
  const byRole = session.tracks.find((track) => track.role && text.includes(track.role.replace("_", " ")));
  if (byRole) return byRole;
  const byName = session.tracks.find((track) => text.includes(track.name.toLowerCase()));
  if (byName) return byName;
  const aliases = [
    ["vocal", "lead_vocal"],
    ["vox", "lead_vocal"],
    ["kick", "kick"],
    ["bass", "bass"],
    ["snare", "snare"],
    ["guitar", "guitar"],
    ["drums", "drums"]
  ];
  for (const [word, role] of aliases) {
    if (text.includes(word)) {
      const match = session.tracks.find((track) => track.role === role || track.name.toLowerCase().includes(word));
      if (match) return match;
    }
  }
  return session.tracks[0];
}

function validateAgainstCapabilities(actions: string[], mixActions: MixAction[]) {
  for (const action of mixActions) {
    if (["undo", "redo", "render_mix"].includes(action.tool)) continue;
    if (!actions.includes(action.tool)) throw new Error(`Action ${action.tool} is not available to the selected skill set`);
  }
}

function explainActions(actions: MixAction[], session: MixSession) {
  const phrases = actions.map((action) => {
    const trackId = "trackId" in action ? action.trackId : "targetId" in action ? action.targetId : undefined;
    const track = trackId ? session.tracks.find((item) => item.id === trackId) : undefined;
    const name = track?.name ?? "the mix";
    switch (action.tool) {
      case "adjust_track_gain":
        return `${action.deltaDb > 0 ? "raised" : "lowered"} ${name} by ${Math.abs(action.deltaDb)} dB`;
      case "set_track_gain":
        return `set ${name} to ${action.gainDb} dB`;
      case "set_track_pan":
        return `moved ${name} in the stereo field`;
      case "mute_track":
        return `${action.muted ? "muted" : "unmuted"} ${name}`;
      case "solo_track":
        return `${action.solo ? "soloed" : "unsoloed"} ${name}`;
      case "set_eq_band":
        return `adjusted EQ on ${name}`;
      case "set_high_pass":
        return `cleaned low rumble on ${name}`;
      case "set_low_pass":
        return `softened top end on ${name}`;
      case "set_compressor":
        return `set compression on ${name}`;
      case "set_reverb_send":
        return `changed reverb depth on ${name}`;
      case "set_delay_send":
        return `changed delay send on ${name}`;
      case "set_region_gain":
      case "apply_section_automation":
        return `created a section-scoped move for ${name}`;
      case "render_mix":
        return "prepared the current mix for render";
      default:
        return `updated ${name}`;
    }
  });
  return `I ${phrases.join(", ")}.`;
}
