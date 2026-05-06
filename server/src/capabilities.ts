import type {
  CapabilitySnapshot,
  MixSession,
  ParamDescriptor,
  ProcessorDescriptor,
  SkillCatalog,
  Track
} from "../../shared/types";

export const skillCatalog: SkillCatalog = {
  skills: [
    {
      skillId: "balance",
      displayName: "Balance",
      whenToUse: "Foreground/background level, pan, mute, solo, simple mix balance moves.",
      musicalIntents: ["louder", "quieter", "upfront", "behind", "wider", "center", "mute", "solo"],
      summaryActions: ["set_track_gain", "adjust_track_gain", "set_track_pan", "mute_track", "solo_track"],
      requiredContext: ["track"]
    },
    {
      skillId: "tonal_eq",
      displayName: "Tonal EQ",
      whenToUse: "Brightness, harshness, muddiness, body, rumble, air, tonal conflicts.",
      musicalIntents: ["bright", "dark", "muddy", "harsh", "thin", "body", "rumble", "air"],
      summaryActions: ["set_high_pass", "set_low_pass", "set_eq_band", "set_processor_param"],
      requiredContext: ["track"]
    },
    {
      skillId: "dynamics",
      displayName: "Dynamics",
      whenToUse: "Punch, control, sustain, consistency, transient and compression requests.",
      musicalIntents: ["punch", "control", "sustain", "even", "compress", "tight"],
      summaryActions: ["set_compressor", "set_processor_param"],
      requiredContext: ["track"]
    },
    {
      skillId: "space_depth",
      displayName: "Space And Depth",
      whenToUse: "Reverb, delay, front/back placement, ambience and depth.",
      musicalIntents: ["reverb", "delay", "space", "depth", "ambience", "farther", "closer"],
      summaryActions: ["set_reverb_send", "set_delay_send", "set_processor_param"],
      requiredContext: ["track"]
    },
    {
      skillId: "region_automation",
      displayName: "Region Automation",
      whenToUse: "Changes limited to a chorus, verse, hook, selected time range, or named region.",
      musicalIntents: ["chorus", "verse", "hook", "section", "only here", "selected part"],
      summaryActions: ["create_region", "set_region_gain", "apply_section_automation"],
      requiredContext: ["region_or_time_range"]
    },
    {
      skillId: "analysis_reader",
      displayName: "Analysis Reader",
      whenToUse: "Interpret level, frequency, masking, or clipping analysis before choosing a move.",
      musicalIntents: ["masking", "clipping", "too loud", "frequency", "balance"],
      summaryActions: [],
      requiredContext: ["analysis"]
    },
    {
      skillId: "render_export",
      displayName: "Render Export",
      whenToUse: "Export, bounce, render, download, clipping readiness.",
      musicalIntents: ["export", "render", "bounce", "wav", "download"],
      summaryActions: ["render_mix"],
      requiredContext: ["session"]
    },
    {
      skillId: "safety_undo",
      displayName: "Safety And Undo",
      whenToUse: "Undo, redo, safety checks, or requests that need clarification.",
      musicalIntents: ["undo", "redo", "revert", "safe", "too much"],
      summaryActions: ["undo", "redo"],
      requiredContext: ["history"]
    }
  ]
};

export function selectSkills(userText: string): string[] {
  const text = userText.toLowerCase();
  const selected = new Set<string>();
  for (const skill of skillCatalog.skills) {
    if (skill.musicalIntents.some((intent) => text.includes(intent))) {
      selected.add(skill.skillId);
    }
  }
  if (/\b(wider|left|right|center|pan|loud|quiet|up|down|mute|solo)\b/.test(text)) selected.add("balance");
  if (/\b(eq|bright|dark|mud|harsh|thin|air|rumble|body)\b/.test(text)) selected.add("tonal_eq");
  if (/\b(comp|punch|tight|dynamic|sustain|transient)\b/.test(text)) selected.add("dynamics");
  if (/\b(reverb|delay|space|wet|dry|room|ambience)\b/.test(text)) selected.add("space_depth");
  if (/\b(chorus|verse|hook|bridge|section|region|selected)\b/.test(text)) selected.add("region_automation");
  if (/\b(undo|redo|revert)\b/.test(text)) selected.add("safety_undo");
  if (/\b(export|render|bounce|wav|download)\b/.test(text)) selected.add("render_export");
  if (!selected.size) selected.add("balance");
  return Array.from(selected).slice(0, 3);
}

export function buildCapabilitySnapshot(session: MixSession, selectedSkills: string[]): CapabilitySnapshot {
  return {
    selectedSkills,
    tracks: session.tracks.map((track) => ({
      trackId: track.id,
      name: track.name,
      role: track.role,
      processors: processorsFor(track, selectedSkills)
    })),
    actions: actionsFor(selectedSkills)
  };
}

function processorsFor(track: Track, selectedSkills: string[]): ProcessorDescriptor[] {
  const processors: ProcessorDescriptor[] = [];
  if (selectedSkills.includes("balance")) {
    processors.push({
      processorId: "track_balance",
      displayName: "Track Balance",
      purpose: "Set foreground/background level, stereo position, mute and solo state.",
      params: [
        param("gainDb", "Gain", "db", -24, 24, 0, track.gainDb, 1, true, ["level", "foreground"]),
        param("pan", "Pan", "normalized", -1, 1, 0, track.pan, 0.15, true, ["width", "stereo"])
      ]
    });
  }
  if (selectedSkills.includes("tonal_eq")) {
    processors.push({
      processorId: "eq_4band",
      displayName: "4 Band EQ",
      purpose: "Shape tone with shelves and peaks; remove rumble, mud, harshness, or add presence/air.",
      params: track.chain.eq.flatMap((band, index) => [
        param(`band${index}.frequencyHz`, `Band ${index} Frequency`, "hz", 20, 20000, band.frequencyHz, band.frequencyHz, 250, true, ["tone"]),
        param(`band${index}.gainDb`, `Band ${index} Gain`, "db", -12, 12, 0, band.gainDb, 1.5, true, ["tone"]),
        param(`band${index}.q`, `Band ${index} Q`, "ratio", 0.2, 10, band.q, band.q, 0.3, true, ["tone"])
      ])
    });
    processors.push({
      processorId: "filters",
      displayName: "High/Low Pass Filters",
      purpose: "Remove low rumble or excessive high-frequency content.",
      params: [
        param("highPass.frequencyHz", "High Pass Frequency", "hz", 20, 1000, 40, track.chain.highPass.frequencyHz, 20, true, ["rumble", "cleanup"]),
        param("lowPass.frequencyHz", "Low Pass Frequency", "hz", 1000, 20000, 18000, track.chain.lowPass.frequencyHz, 500, true, ["darken", "smooth"])
      ]
    });
  }
  if (selectedSkills.includes("dynamics")) {
    processors.push({
      processorId: "compressor",
      displayName: "Compressor",
      purpose: "Control dynamics, add punch, smooth uneven performance, or increase sustain.",
      params: [
        param("thresholdDb", "Threshold", "db", -60, 0, -18, track.chain.compressor.thresholdDb, 3, true, ["control", "punch"]),
        param("ratio", "Ratio", "ratio", 1, 20, 2, track.chain.compressor.ratio, 0.5, true, ["control"]),
        param("attackMs", "Attack", "ms", 1, 200, 20, track.chain.compressor.attackMs, 5, true, ["transient", "punch"]),
        param("releaseMs", "Release", "ms", 20, 1000, 160, track.chain.compressor.releaseMs, 30, true, ["sustain"])
      ]
    });
  }
  if (selectedSkills.includes("space_depth")) {
    processors.push({
      processorId: "sends",
      displayName: "Reverb And Delay Sends",
      purpose: "Move tracks deeper, add ambience, or create echo without changing dry tone.",
      params: [
        param("reverbDb", "Reverb Send", "db", -60, 6, -60, track.sends.reverbDb, 3, true, ["depth", "space"]),
        param("delayDb", "Delay Send", "db", -60, 6, -60, track.sends.delayDb, 3, true, ["echo", "space"])
      ]
    });
  }
  return processors;
}

function actionsFor(selectedSkills: string[]): string[] {
  const actions = new Set<string>();
  for (const skill of selectedSkills) {
    const card = skillCatalog.skills.find((item) => item.skillId === skill);
    card?.summaryActions.forEach((action) => actions.add(action));
  }
  return Array.from(actions);
}

function param(
  paramId: string,
  label: string,
  unit: ParamDescriptor["unit"],
  min: number,
  max: number,
  defaultValue: number,
  current: number,
  safeStep: number,
  automatable: boolean,
  semanticTags: string[]
): ParamDescriptor {
  return { paramId, label, unit, min, max, default: defaultValue, current, safeStep, automatable, semanticTags };
}
