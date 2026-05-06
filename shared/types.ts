export type Id = string;

export type TrackAnalysis = {
  peakDb: number;
  rmsDb: number;
  lufsEstimate: number;
  spectralCentroidHz: number;
  lowEnergy: number;
  midEnergy: number;
  highEnergy: number;
  silencePercent: number;
  dynamicRangeDb: number;
};

export type SourceFile = {
  id: Id;
  originalName: string;
  storedName: string;
  mimeType: string;
  sizeBytes: number;
  durationSamples?: number;
  sampleRate?: number;
  channels?: number;
  analysis?: TrackAnalysis;
};

export type EqBand = {
  type: "low_shelf" | "peak" | "high_shelf";
  frequencyHz: number;
  gainDb: number;
  q: number;
};

export type FilterState = {
  enabled: boolean;
  frequencyHz: number;
  slopeDbOct: 12 | 24;
};

export type CompressorState = {
  enabled: boolean;
  thresholdDb: number;
  ratio: number;
  attackMs: number;
  releaseMs: number;
  kneeDb: number;
  makeupDb: number;
};

export type TrackChain = {
  highPass: FilterState;
  lowPass: FilterState;
  eq: EqBand[];
  compressor: CompressorState;
};

export type Sends = {
  reverbDb: number;
  delayDb: number;
};

export type AutomatableParam =
  | "gainDb"
  | "pan"
  | "highPass.frequencyHz"
  | "lowPass.frequencyHz"
  | "sends.reverbDb"
  | "sends.delayDb";

export type AutomationPoint = {
  sample: number;
  value: number;
};

export type AutomationLane = {
  id: Id;
  param: AutomatableParam;
  regionId?: Id;
  points: AutomationPoint[];
  curve: "linear" | "exponential" | "hold";
};

export type ClipRegion = {
  id: Id;
  startSample: number;
  endSample: number;
  gainDb: number;
};

export type Track = {
  id: Id;
  name: string;
  role?: string;
  sourceFileId: Id;
  startSample: number;
  gainDb: number;
  pan: number;
  muted: boolean;
  solo: boolean;
  color: string;
  chain: TrackChain;
  sends: Sends;
  automation: AutomationLane[];
  clips: ClipRegion[];
};

export type Region = {
  id: Id;
  name: string;
  startSample: number;
  endSample: number;
  trackIds?: Id[];
};

export type Marker = {
  id: Id;
  name: string;
  sample: number;
};

export type MasterChannel = {
  gainDb: number;
  limiter: {
    enabled: boolean;
    ceilingDb: number;
  };
};

export type MixSession = {
  id: Id;
  name: string;
  sampleRate: number;
  bpm?: number;
  sourceFiles: SourceFile[];
  tracks: Track[];
  regions: Region[];
  markers: Marker[];
  master: MasterChannel;
};

export type JsonPatch = {
  op: "add" | "replace" | "remove";
  path: string;
  value?: unknown;
};

export type HistoryEntry = {
  id: Id;
  timestamp: number;
  source: "user" | "assistant";
  explanation?: string;
  forwardPatch: JsonPatch[];
  inversePatch: JsonPatch[];
};

export type MixProject = {
  session: MixSession;
  history: HistoryEntry[];
  redoStack: HistoryEntry[];
};

export type MixAction =
  | { tool: "create_region"; name: string; startSample: number; endSample: number; trackIds?: Id[] }
  | { tool: "set_track_gain"; trackId: Id; gainDb: number }
  | { tool: "adjust_track_gain"; trackId: Id; deltaDb: number }
  | { tool: "set_track_pan"; trackId: Id; pan: number }
  | { tool: "mute_track"; trackId: Id; muted: boolean }
  | { tool: "solo_track"; trackId: Id; solo: boolean }
  | { tool: "set_high_pass"; trackId: Id; frequencyHz: number; slopeDbOct: 12 | 24 }
  | { tool: "set_low_pass"; trackId: Id; frequencyHz: number; slopeDbOct: 12 | 24 }
  | { tool: "set_eq_band"; trackId: Id; band: number; frequencyHz: number; gainDb: number; q: number }
  | { tool: "set_compressor"; trackId: Id; thresholdDb: number; ratio: number; attackMs: number; releaseMs: number; kneeDb: number; makeupDb: number }
  | { tool: "set_reverb_send"; trackId: Id; levelDb: number }
  | { tool: "set_delay_send"; trackId: Id; levelDb: number }
  | { tool: "set_processor_param"; targetId: Id; processorId: string; paramId: string; value: number }
  | { tool: "set_region_gain"; regionId: Id; trackId: Id; gainDb: number }
  | { tool: "apply_section_automation"; regionId: Id; trackId: Id; param: AutomatableParam; value: number }
  | { tool: "undo" }
  | { tool: "redo" }
  | { tool: "render_mix" };

export type SkillCard = {
  skillId: string;
  displayName: string;
  whenToUse: string;
  musicalIntents: string[];
  summaryActions: string[];
  requiredContext: string[];
};

export type SkillCatalog = {
  skills: SkillCard[];
};

export type ParamDescriptor = {
  paramId: string;
  label: string;
  unit: "db" | "hz" | "ratio" | "ms" | "normalized" | "boolean";
  min: number;
  max: number;
  default: number;
  current?: number;
  safeStep: number;
  automatable: boolean;
  semanticTags: string[];
};

export type ProcessorDescriptor = {
  processorId: string;
  displayName: string;
  purpose: string;
  params: ParamDescriptor[];
};

export type TrackCapability = {
  trackId: Id;
  name: string;
  role?: string;
  processors: ProcessorDescriptor[];
};

export type CapabilitySnapshot = {
  selectedSkills: string[];
  tracks: TrackCapability[];
  actions: string[];
};

export type AssistantRequest = {
  sessionId: Id;
  userText: string;
  selectedTrackIds: Id[];
  selectedRegionIds: Id[];
  selectedTimeRange?: { startSample: number; endSample: number };
};

export type AssistantResponse =
  | {
      status: "ok";
      explanation: string;
      actions: MixAction[];
      warnings: string[];
      selectedSkills: string[];
      session: MixSession;
      history: HistoryEntry[];
    }
  | { status: "clarification"; question: string; reason: string }
  | { status: "err"; kind: string; message: string; rawModelOutput?: string };
