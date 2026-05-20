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
  cachePath: string;
  peakPath: string;
  durationSamples: number;
  sampleRate: number;
  channels: number;
  analysis: TrackAnalysis;
  peakPreview: number[];
};

export type VideoSourceFile = {
  id: Id;
  originalName: string;
  path: string;
  mimeType: string;
  durationMs: number;
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
  sourceFileId?: Id;
  name?: string;
  startSample: number;
  endSample: number;
  sourceOffsetSample?: number;
  gainDb: number;
};

export type VideoClipRegion = {
  id: Id;
  videoSourceFileId: Id;
  name?: string;
  startSample: number;
  endSample: number;
  sourceOffsetMs?: number;
  layout?: VideoLayout;
};

export type VideoFilterPreset = "none" | "warm" | "cool" | "mono" | "punch" | "dream";

export type VideoLayout = {
  x: number;
  y: number;
  width: number;
  height: number;
  cropTop: number;
  cropRight: number;
  cropBottom: number;
  cropLeft: number;
  opacity: number;
  rotation: number;
  zIndex: number;
  brightness: number;
  contrast: number;
  saturation: number;
  blur: number;
  preset: VideoFilterPreset;
};

export type VideoCanvas = {
  width: number;
  height: number;
  background: string;
};

export type Track = {
  id: Id;
  kind?: "audio" | "video";
  name: string;
  role?: string;
  sourceFileId: Id;
  startSample: number;
  gainDb: number;
  pan: number;
  muted: boolean;
  solo: boolean;
  aiGenerated?: boolean;
  color: string;
  chain: TrackChain;
  sends: Sends;
  automation: AutomationLane[];
  clips: ClipRegion[];
  videoClips?: VideoClipRegion[];
  cameraDeviceId?: string;
  recordCameraAudio?: boolean;
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

export type Bus = {
  id: Id;
  name: string;
  gainDb: number;
};

export type MixSession = {
  id: Id;
  name: string;
  sampleRate: number;
  bpm?: number;
  sourceFiles: SourceFile[];
  videoSourceFiles?: VideoSourceFile[];
  tracks: Track[];
  buses: Bus[];
  regions: Region[];
  markers: Marker[];
  master: MasterChannel;
  sections?: MixSection[];
  mixerProfile?: MixerProfile;
  videoCanvas?: VideoCanvas;
};

export type MixerProfile = {
  presetId: string;
  aggressiveness: "subtle" | "moderate" | "bold";
  eqPhilosophy: "corrective_only" | "tonal_shaping" | "sculpting";
  compressionPhilosophy: "transparent_glue" | "character" | "aggressive";
  stereoTreatment: "narrow" | "natural" | "wide";
  space: "dry" | "tasteful" | "lush";
  loudnessTarget: "broadcast" | "streaming" | "loud";
  genre?: string;
  referenceEngineer?: string;
  customNotes?: string;
};

export type ProfilePreset = {
  id: string;
  displayName: string;
  summary: string;
  profile: MixerProfile;
};

export type MixSection = {
  start: number;
  end: number;
  label: string;
  analysis?: SectionAnalysis;
};

export type SectionAnalysis = {
  peakDb: number;
  rmsDb: number;
  lufs: number;
  spectralCentroidHz: number;
  lowEnergy: number;
  midEnergy: number;
  highEnergy: number;
  dynamicRangeDb: number;
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
  chatMessages?: unknown[];
};

export type MixAction =
  | { tool: "create_region"; name: string; startSample: number; endSample: number; trackIds?: Id[] }
  | { tool: "delete_track"; trackId: Id }
  | { tool: "rename_track"; trackId: Id; name: string }
  | { tool: "set_track_role"; trackId: Id; role?: string }
  | { tool: "set_track_gain"; trackId: Id; gainDb: number }
  | { tool: "adjust_track_gain"; trackId: Id; deltaDb: number }
  | { tool: "set_track_pan"; trackId: Id; pan: number }
  | { tool: "mute_track"; trackId: Id; muted: boolean }
  | { tool: "solo_track"; trackId: Id; solo: boolean }
  | { tool: "set_track_ai_generated"; trackId: Id; aiGenerated: boolean }
  | { tool: "set_high_pass"; trackId: Id; frequencyHz: number; slopeDbOct: 12 | 24 }
  | { tool: "set_low_pass"; trackId: Id; frequencyHz: number; slopeDbOct: 12 | 24 }
  | { tool: "set_eq_band"; trackId: Id; band: number; frequencyHz: number; gainDb: number; q: number }
  | { tool: "set_compressor"; trackId: Id; thresholdDb: number; ratio: number; attackMs: number; releaseMs: number; kneeDb: number; makeupDb: number }
  | { tool: "set_reverb_send"; trackId: Id; levelDb: number }
  | { tool: "set_delay_send"; trackId: Id; levelDb: number }
  | { tool: "set_master_gain"; gainDb: number }
  | { tool: "adjust_master_gain"; deltaDb: number }
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
  ollamaBaseUrl?: string;
  ollamaModel?: string;
  recentCritique?: MixCritique;
};

export type CritiqueSeverity = "low" | "medium" | "high";

export type CritiqueIssue = {
  category: string;
  severity: CritiqueSeverity;
  message: string;
  suggestedSkills?: string[];
};

export type TrackCritique = {
  trackId: Id;
  trackName: string;
  rating: number;
  issues: CritiqueIssue[];
  strengths: string[];
};

export type MixCritique = {
  mixScore: number;
  summary: string;
  headroomDb: number;
  integratedLufsEstimate: number;
  truePeakDbEstimate: number;
  mixIssues: CritiqueIssue[];
  perTrack: TrackCritique[];
  recommendedNextSteps: string[];
};

export type AbJudgeIssue = {
  category: string;
  severity: CritiqueSeverity;
  message: string;
};

export type AbJudgeResponse = {
  provider: string;
  model: string;
  winner: "before" | "after" | "tie";
  confidence: number;
  summary: string;
  improvements: string[];
  regressions: string[];
  mixIssuesBefore: AbJudgeIssue[];
  mixIssuesAfter: AbJudgeIssue[];
  recommendedNextMoves: string[];
  clipStart: number;
  clipDuration: number;
  promptTokens?: number;
  outputTokens?: number;
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
      rationale?: string;
      perActionNotes?: string[];
    }
  | { status: "clarification"; question: string; reason: string }
  | { status: "critique"; critique: MixCritique; selectedSkills: string[] }
  | { status: "err"; kind: string; message: string; rawModelOutput?: string };
