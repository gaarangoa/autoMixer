import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AbJudgeResponse, AgentColorGrade, AgentVideoEffects, AgentVideoScriptEntry, AssistantRequest, AssistantResponse, JsonPatch, MixAction, MixerProfile, MixProject, MixSession, ProfilePreset, SkillCatalog, VideoFilterPreset } from "../../shared/types";

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
export type ExportAspect = "original" | "square" | "portrait916";
// "fast" copies the preview cache or transcodes with -preset veryfast. "high" uses
// -preset slow -crf 17 -b:a 320k. When a script is present, "high" also re-renders
// from the original camera sources instead of copying the preview cache.
export type ExportQuality = "fast" | "high";

export const api = {
  config: () => tauriInvoke<{ ollamaBaseUrl: string; ollamaModel: string }>("get_config"),
  restartApp: () => tauriInvoke<void>("restart_app"),
  cancelAgent: () => tauriInvoke<void>("cancel_agent"),
  // Lists models from the server at baseUrl — Ollama, vLLM, or llama.cpp (the
  // backend auto-detects the protocol). `provider` is the detected server kind.
  ollamaModels: (baseUrl: string) => tauriInvoke<{ models: string[]; provider: string }>("list_ollama_models", { baseUrl }),
  inputDevices: () => tauriInvoke<{ devices: string[] }>("list_input_devices"),
  inputDeviceChannelCount: (inputDevice?: string) => tauriInvoke<number>("list_input_device_channels", { inputDevice }),
  skills: () => tauriInvoke<SkillCatalog>("get_skill_catalog"),
  sessions: () => tauriInvoke<MixSession[]>("list_sessions"),
  createSession: (name: string) => tauriInvoke<MixProject>("create_session", { name }),
  getSession: (id: string) => tauriInvoke<MixProject>("get_project", { sessionId: id }),
  importFiles: (sessionId: string, paths: string[]) => tauriInvoke<MixProject>("import_audio_files", { sessionId, paths }),
  createRecordingTrack: (sessionId: string, channels: 1 | 2 = 1) => tauriInvoke<MixProject>("create_recording_track", { sessionId, channels }),
  createVideoTrack: (sessionId: string) => tauriInvoke<MixProject>("create_video_track", { sessionId }),
  addRenderedVideoTrack: (sessionId: string, videoPath: string, name: string, startSample: number, durationMs: number) =>
    tauriInvoke<MixProject>("add_rendered_video_track", { sessionId, videoPath, name, startSample, durationMs }),
  replaceRenderedVideoTrack: (sessionId: string, trackId: string, clipId: string, videoPath: string, durationMs: number) =>
    tauriInvoke<MixProject>("replace_rendered_video_track", { sessionId, trackId, clipId, videoPath, durationMs }),
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
  startRecording: (sessionId: string, startSample: number, targetTrackId?: string, inputDevice?: string, inputGainDb?: number, inputChannels?: number[]) =>
    tauriInvoke<void>("start_recording", { sessionId, startSample, targetTrackId, inputDevice, inputGainDb, inputChannels }),
  recordingMeters: () => tauriInvoke<{ peaks: number[]; channelPeaks: number[] }>("poll_recording_meters"),
  stopRecording: (sessionId: string) => tauriInvoke<MixProject>("stop_recording", { sessionId }),
  startInputMonitor: (inputDevice?: string) => tauriInvoke<void>("start_input_monitor", { inputDevice }),
  inputMonitorMeters: () => tauriInvoke<{ peaks: number[]; channelPeaks: number[] }>("poll_input_monitor_meters"),
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
  renderVideoMix: (sessionId: string, outputPath: string, startSample?: number, endSample?: number, trackIds?: string[], aspectRatio?: ExportAspect, quality?: ExportQuality) =>
    tauriInvoke<{ path: string }>("render_video_mix", { sessionId, outputPath, startSample, endSample, trackIds, aspectRatio, quality }),
  exportRenderedVideo: (sourcePath: string, outputPath: string, aspectRatio?: ExportAspect, quality?: ExportQuality) =>
    tauriInvoke<{ path: string }>("export_rendered_video", { sourcePath, outputPath, aspectRatio, quality }),
  // Flexible export: any aspect ("16:9","9:16","1:1","4:5","original",…), long-edge px,
  // and fit (letterbox) vs fill (crop). Re-encodes at high quality.
  exportVideo: (sourcePath: string, outputPath: string, aspect: string, maxDimension?: number, mode?: "fit" | "fill") =>
    tauriInvoke<{ path: string }>("export_video", { sourcePath, outputPath, aspect, maxDimension, mode }),
  renderAutoVideoEdit: (sessionId: string, outputPath: string, startSample: number | undefined, endSample: number | undefined, trackIds: string[], sampleIntervalSeconds: number) =>
    tauriInvoke<{ path: string }>("render_auto_video_edit", { sessionId, outputPath, startSample, endSample, trackIds, sampleIntervalSeconds }),
  renderAgentVideoEdit: (sessionId: string, outputPath: string | undefined, startSample: number | undefined, endSample: number | undefined, trackIds: string[], sampleIntervalSeconds: number, ollamaBaseUrl: string, visionModel: string, editModel: string, instructions: string, planOnly?: boolean) =>
    tauriInvoke<{ path: string; script: AgentVideoScriptEntry[]; lookPreset?: VideoFilterPreset; colorGrade?: AgentColorGrade; videoEffects?: AgentVideoEffects }>("render_agent_video_edit", { sessionId, outputPath, startSample, endSample, trackIds, sampleIntervalSeconds, ollamaBaseUrl, visionModel, editModel, instructions, planOnly }),
  applyClipEffects: (sessionId: string, trackId: string, clipId: string, instructions: string, ollamaBaseUrl?: string, visionModel?: string) =>
    tauriInvoke<{ project: MixProject; lookPreset?: VideoFilterPreset; colorGrade?: AgentColorGrade; videoEffects?: AgentVideoEffects; sourceSummary: string }>("apply_clip_effects", { sessionId, trackId, clipId, instructions, ollamaBaseUrl, visionModel }),
  revertClipVideo: (sessionId: string, trackId: string, clipId: string) =>
    tauriInvoke<MixProject>("revert_clip_video", { sessionId, trackId, clipId }),
  renderVideoFromScript: (sessionId: string, sourceTrackIds: string[], startSample: number | undefined, endSample: number | undefined, script: AgentVideoScriptEntry[], lookPreset?: VideoFilterPreset, colorGrade?: AgentColorGrade, videoEffects?: AgentVideoEffects, quality?: ExportQuality) =>
    tauriInvoke<{ path: string; durationMs: number }>("render_video_from_script", { sessionId, sourceTrackIds, startSample, endSample, script, lookPreset, colorGrade, videoEffects, quality }),
  rerenderAgentEdit: (sessionId: string, trackId: string, clipId: string, sourceTrackIds: string[], startSample: number | undefined, endSample: number | undefined, script: AgentVideoScriptEntry[], lookPreset?: VideoFilterPreset, colorGrade?: AgentColorGrade, videoEffects?: AgentVideoEffects) =>
    tauriInvoke<MixProject>("rerender_agent_edit", { sessionId, trackId, clipId, sourceTrackIds, startSample, endSample, script, lookPreset, colorGrade, videoEffects }),
  fitCanvasToFootage: (sessionId: string) => tauriInvoke<MixProject>("fit_canvas_to_footage", { sessionId }),
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
  // Read / set the embedded Hermes agent's orchestration model (any
  // OpenAI-compatible endpoint). Setting it restarts the agent sidecar.
  getHermesModel: () => tauriInvoke<{ baseUrl: string; model: string; provider: string }>("get_hermes_model"),
  setHermesModel: (baseUrl: string, model: string) => tauriInvoke<void>("set_hermes_model", { baseUrl, model }),
  // Forget the agent's conversation for a session so the next turn starts fresh.
  clearChat: (sessionId: string) => tauriInvoke<void>("clear_chat", { sessionId }),
  // Read / set the video-edit skill's vision-model endpoint (e.g. Qwen3-VL on the Spark).
  getVideoModel: () => tauriInvoke<{ baseUrl: string; model: string }>("get_video_model"),
  setVideoModel: (baseUrl: string, model: string) => tauriInvoke<void>("set_video_model", { baseUrl, model }),
  // Push the user's track selection so the video skill edits only selected tracks.
  setVideoSelection: (sessionId: string, trackIds: string[]) => tauriInvoke<void>("set_video_selection", { sessionId, trackIds }),
  getVideoSelection: (sessionId: string) => tauriInvoke<string[]>("get_video_selection", { sessionId }),
  // Fired when an external agent (the Hermes control surface) mutates a session
  // out from under the UI, so the frontend can refresh its project state.
  onSessionExternallyUpdated: (cb: (event: { sessionId: string; project: MixProject }) => void): Promise<UnlistenFn> =>
    listen<{ sessionId: string; project: MixProject }>("session:externally-updated", e => cb(e.payload)),
  // Fired by the control surface when an agent video edit finishes rendering.
  onVideoRendered: (cb: (event: { sessionId: string; path: string; cuts: number; lookPreset?: string }) => void): Promise<UnlistenFn> =>
    listen<{ sessionId: string; path: string; cuts: number; lookPreset?: string }>("video:rendered", e => cb(e.payload)),
  onPlayhead: (cb: (event: PlayheadEvent) => void): Promise<UnlistenFn> =>
    listen<PlayheadEvent>("engine:playhead", e => cb(e.payload)),
  onMeters: (cb: (event: MetersEvent) => void): Promise<UnlistenFn> =>
    listen<MetersEvent>("engine:meters", e => cb(e.payload)),
};
