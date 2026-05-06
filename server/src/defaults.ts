import type { CompressorState, EqBand, MasterChannel, Track, TrackChain } from "../../shared/types";

const COLORS = ["#4f8cff", "#d95f5f", "#2f9e6e", "#c28a2c", "#8b6bd8", "#3aa6a6", "#d26ca3"];

export function defaultEq(): EqBand[] {
  return [
    { type: "low_shelf", frequencyHz: 100, gainDb: 0, q: 0.7 },
    { type: "peak", frequencyHz: 400, gainDb: 0, q: 1 },
    { type: "peak", frequencyHz: 2500, gainDb: 0, q: 1 },
    { type: "high_shelf", frequencyHz: 8000, gainDb: 0, q: 0.7 }
  ];
}

export function defaultCompressor(): CompressorState {
  return {
    enabled: false,
    thresholdDb: -18,
    ratio: 2,
    attackMs: 20,
    releaseMs: 160,
    kneeDb: 6,
    makeupDb: 0
  };
}

export function defaultChain(): TrackChain {
  return {
    highPass: { enabled: false, frequencyHz: 40, slopeDbOct: 12 },
    lowPass: { enabled: false, frequencyHz: 18000, slopeDbOct: 12 },
    eq: defaultEq(),
    compressor: defaultCompressor()
  };
}

export function defaultMaster(): MasterChannel {
  return {
    gainDb: 0,
    limiter: {
      enabled: true,
      ceilingDb: -1
    }
  };
}

export function makeTrack(id: string, sourceFileId: string, name: string, index: number): Track {
  return {
    id,
    name,
    role: inferRole(name),
    sourceFileId,
    startSample: 0,
    gainDb: 0,
    pan: 0,
    muted: false,
    solo: false,
    color: COLORS[index % COLORS.length],
    chain: defaultChain(),
    sends: {
      reverbDb: -60,
      delayDb: -60
    },
    automation: [],
    clips: []
  };
}

export function inferRole(name: string): string | undefined {
  const lower = name.toLowerCase();
  if (lower.includes("kick")) return "kick";
  if (lower.includes("snare")) return "snare";
  if (lower.includes("bass")) return "bass";
  if (lower.includes("vocal") || lower.includes("vox") || lower.includes("lead")) return "lead_vocal";
  if (lower.includes("guitar") || lower.includes("gtr")) return "guitar";
  if (lower.includes("drum")) return "drums";
  if (lower.includes("keys") || lower.includes("piano") || lower.includes("synth")) return "keys";
  return undefined;
}
