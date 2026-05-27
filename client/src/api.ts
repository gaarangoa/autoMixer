import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AbJudgeResponse, AgentVideoScriptEntry, AssistantRequest, AssistantResponse, JsonPatch, MixAction, MixerProfile, MixProject, MixSession, ProfilePreset, SkillCatalog } from "../../shared/types";

function tauriInvoke<T>(command: string, args?: Record<string, unknown>) {
  if (!("__TAURI_INTERNALS__" in window)) {
    throw new Error("This frontend is open in a browser. Use the AutoMixer desktop window launched by `npm run dev`; the Vite URL is only the frontend dev server.");
  }
  return invoke<T>(command, args);
}

export type PlayheadEvent = { sample: number; running: boolean };
export type MetersEvent = { masterPeak: number; trackPeaks: number[] };
export type AudioProgressEvent = { stage: string; message: string; elapsedSeconds: number };
export type AgentVideoProgressEvent = { stage: string; message: string; current: number; total: number; elapsedSeconds: number };
export type LlmChunkEvent = { phase: string; text: string };
export type LlmStatsEvent = { phase: string; promptTokens: number; responseTokens: number; elapsedMs: number };

export const api = {
  config: () => tauriInvoke<{ ollamaBaseUrl: string; ollamaModel: string }>("get_config"),
  ollamaModels: (baseUrl: string) => tauriInvoke<{ models: string[] }>("list_ollama_models", { baseUrl }),
  inputDevices: () => tauriInvoke<{ devices: string[] }>("list_input_devices"),
  skills: () => tauriInvoke<SkillCatalog>("get_skill_catalog"),
  sessions: () => tauriInvoke<MixSession[]>("list_sessions"),
  createSession: (name: string) => tauriInvoke<MixProject>("create_session", { name }),
  getSession: (id: string) => tauriInvoke<MixProject>("get_project", { sessionId: id }),
  importFiles: (sessionId: string, paths: string[]) => tauriInvoke<MixProject>("import_audio_files", { sessionId, paths }),
  createRecordingTrack: (sessionId: string) => tauriInvoke<MixProject>("create_recording_track", { sessionId }),
  createVideoTrack: (sessionId: string) => tauriInvoke<MixProject>("create_video_track", { sessionId }),
  applyActions: (sessionId: string, actions: MixAction[], explanation?: string) =>
    tauriInvoke<MixProject>("apply_mix_actions", { sessionId, actions, explanation }),
  undo: (sessionId: string) => tauriInvoke<MixProject>("undo_mix_action", { sessionId }),
  redo: (sessionId: string) => tauriInvoke<MixProject>("redo_mix_action", { sessionId }),
  applyPatch: (sessionId: string, forwardPatch: JsonPatch[], inversePatch: JsonPatch[], explanation?: string) =>
    tauriInvoke<MixProject>("apply_recorded_patch", { sessionId, forwardPatch, inversePatch, explanation }),
  resetSession: (sessionId: string) => tauriInvoke<MixProject>("reset_session", { sessionId }),
  assistant: (request: AssistantRequest) => tauriInvoke<AssistantResponse>("assistant_request", { request }),
  play: (sessionId: string) => tauriInvoke<void>("transport_play", { sessionId }),
  pause: () => tauriInvoke<void>("transport_pause"),
  stop: () => tauriInvoke<void>("transport_stop"),
  seek: (sample: number) => tauriInvoke<void>("transport_seek", { sample }),
  startRecording: (sessionId: string, startSample: number, targetTrackId?: string, inputDevice?: string) =>
    tauriInvoke<void>("start_recording", { sessionId, startSample, targetTrackId, inputDevice }),
  recordingMeters: () => tauriInvoke<{ peaks: number[] }>("poll_recording_meters"),
  stopRecording: (sessionId: string) => tauriInvoke<MixProject>("stop_recording", { sessionId }),
  startInputMonitor: (inputDevice?: string) => tauriInvoke<void>("start_input_monitor", { inputDevice }),
  inputMonitorMeters: () => tauriInvoke<{ peaks: number[] }>("poll_input_monitor_meters"),
  stopInputMonitor: () => tauriInvoke<void>("stop_input_monitor"),
  deleteClip: (sessionId: string, trackId: string, clipId: string) =>
    tauriInvoke<MixProject>("delete_clip", { sessionId, trackId, clipId }),
  deleteClipRange: (sessionId: string, trackId: string, startSample: number, endSample: number) =>
    tauriInvoke<MixProject>("delete_clip_range", { sessionId, trackId, startSample, endSample }),
  setMasterBypass: (enabled: boolean) => tauriInvoke<void>("set_master_bypass", { enabled }),
  setMasterGain: (sessionId: string, gainDb: number) => tauriInvoke<MixProject>("set_master_gain", { sessionId, gainDb }),
  renameSession: (sessionId: string, name: string) => tauriInvoke<MixProject>("rename_session", { sessionId, name }),
  deleteSession: (sessionId: string) => tauriInvoke<void>("delete_session", { sessionId }),
  exportProjectBundle: (sessionId: string, bundleDir: string) => tauriInvoke<void>("export_project_bundle", { sessionId, bundleDir }),
  importProjectBundle: (bundleDir: string) => tauriInvoke<MixProject>("import_project_bundle", { bundleDir }),
  saveChatMessages: (sessionId: string, messages: unknown[]) => tauriInvoke<void>("save_chat_messages", { sessionId, messages }),
  listMixerProfiles: () => tauriInvoke<ProfilePreset[]>("list_mixer_profiles"),
  setMixerProfile: (sessionId: string, profile: MixerProfile) => tauriInvoke<MixProject>("set_mixer_profile", { sessionId, profile }),
  startAutoMix: (sessionId: string, stages: string[], ollamaBaseUrl: string, ollamaModel: string) =>
    tauriInvoke<void>("start_auto_mix", { sessionId, options: { stages, ollamaBaseUrl, ollamaModel } }),
  onAutoMixEvent: (kind: "start" | "stage-start" | "stage-done" | "complete", cb: (payload: unknown) => void): Promise<UnlistenFn> =>
    listen(`auto-mix:${kind}`, (e) => cb(e.payload)),
  onMenuDetectStructure: (cb: () => void): Promise<UnlistenFn> =>
    listen("menu:detect-structure", () => cb()),
  onMenuLevelSections: (cb: () => void): Promise<UnlistenFn> =>
    listen("menu:level-sections", () => cb()),
  renderMix: (sessionId: string, outputPath: string) => tauriInvoke<{ path: string }>("render_mix", { sessionId, outputPath }),
  saveVideoRecording: (sessionId: string, trackId: string, fileName: string, mimeType: string, startSample: number, durationMs: number, dataBase64: string, createAudioTrack = false, sourceOffsetMs = 0) =>
    tauriInvoke<MixProject>("save_video_recording", { sessionId, trackId, fileName, mimeType, startSample, durationMs, dataBase64, createAudioTrack, sourceOffsetMs }),
  renderVideoMix: (sessionId: string, outputPath: string, startSample?: number, endSample?: number, trackIds?: string[]) =>
    tauriInvoke<{ path: string }>("render_video_mix", { sessionId, outputPath, startSample, endSample, trackIds }),
  exportRenderedVideo: (sourcePath: string, outputPath: string) =>
    tauriInvoke<{ path: string }>("export_rendered_video", { sourcePath, outputPath }),
  renderAutoVideoEdit: (sessionId: string, outputPath: string, startSample: number | undefined, endSample: number | undefined, trackIds: string[], sampleIntervalSeconds: number) =>
    tauriInvoke<{ path: string }>("render_auto_video_edit", { sessionId, outputPath, startSample, endSample, trackIds, sampleIntervalSeconds }),
  renderAgentVideoEdit: (sessionId: string, outputPath: string, startSample: number | undefined, endSample: number | undefined, trackIds: string[], sampleIntervalSeconds: number, ollamaBaseUrl: string, visionModel: string, editModel: string, instructions: string) =>
    tauriInvoke<{ path: string; script: AgentVideoScriptEntry[] }>("render_agent_video_edit", { sessionId, outputPath, startSample, endSample, trackIds, sampleIntervalSeconds, ollamaBaseUrl, visionModel, editModel, instructions }),
  judgeMixAb: (sessionId: string, apiKey: string, model = "gemini-flash-latest") =>
    tauriInvoke<AbJudgeResponse>("judge_mix_ab", { sessionId, options: { provider: "gemini", model, apiKey } }),
  judgeMixAbLocal: (sessionId: string) =>
    tauriInvoke<AbJudgeResponse>("judge_mix_ab", { sessionId, options: { provider: "local", model: "local-qc-v1" } }),
  analyzeMasterStructure: (sessionId: string) => tauriInvoke<MixProject>("analyze_master_structure", { sessionId }),
  onAudioProgress: (cb: (event: AudioProgressEvent) => void): Promise<UnlistenFn> =>
    listen<AudioProgressEvent>("audio:progress", e => cb(e.payload)),
  onAgentVideoProgress: (cb: (event: AgentVideoProgressEvent) => void): Promise<UnlistenFn> =>
    listen<AgentVideoProgressEvent>("agent-video:progress", e => cb(e.payload)),
  onLlmChunk: (cb: (event: LlmChunkEvent) => void): Promise<UnlistenFn> =>
    listen<LlmChunkEvent>("llm:chunk", e => cb(e.payload)),
  onLlmStats: (cb: (event: LlmStatsEvent) => void): Promise<UnlistenFn> =>
    listen<LlmStatsEvent>("llm:stats", e => cb(e.payload)),
  onLlmTurnStart: (cb: () => void): Promise<UnlistenFn> =>
    listen("llm:turn-start", () => cb()),
  onLlmTurnEnd: (cb: () => void): Promise<UnlistenFn> =>
    listen("llm:turn-end", () => cb()),
  onPlayhead: (cb: (event: PlayheadEvent) => void): Promise<UnlistenFn> =>
    listen<PlayheadEvent>("engine:playhead", e => cb(e.payload)),
  onMeters: (cb: (event: MetersEvent) => void): Promise<UnlistenFn> =>
    listen<MetersEvent>("engine:meters", e => cb(e.payload)),
};
