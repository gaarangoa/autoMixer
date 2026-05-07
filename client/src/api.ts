import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AssistantRequest, AssistantResponse, JsonPatch, MixAction, MixProject, MixSession, SkillCatalog } from "../../shared/types";

function tauriInvoke<T>(command: string, args?: Record<string, unknown>) {
  if (!("__TAURI_INTERNALS__" in window)) {
    throw new Error("This frontend is open in a browser. Use the AutoMixer desktop window launched by `npm run dev`; the Vite URL is only the frontend dev server.");
  }
  return invoke<T>(command, args);
}

export type PlayheadEvent = { sample: number; running: boolean };
export type MetersEvent = { masterPeak: number; trackPeaks: number[] };

export const api = {
  config: () => tauriInvoke<{ ollamaBaseUrl: string; ollamaModel: string }>("get_config"),
  ollamaModels: (baseUrl: string) => tauriInvoke<{ models: string[] }>("list_ollama_models", { baseUrl }),
  skills: () => tauriInvoke<SkillCatalog>("get_skill_catalog"),
  sessions: () => tauriInvoke<MixSession[]>("list_sessions"),
  createSession: (name: string) => tauriInvoke<MixProject>("create_session", { name }),
  getSession: (id: string) => tauriInvoke<MixProject>("get_project", { sessionId: id }),
  importFiles: (sessionId: string, paths: string[]) => tauriInvoke<MixProject>("import_audio_files", { sessionId, paths }),
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
  renderMix: (sessionId: string, outputPath: string) => tauriInvoke<{ path: string }>("render_mix", { sessionId, outputPath }),
  onPlayhead: (cb: (event: PlayheadEvent) => void): Promise<UnlistenFn> =>
    listen<PlayheadEvent>("engine:playhead", e => cb(e.payload)),
  onMeters: (cb: (event: MetersEvent) => void): Promise<UnlistenFn> =>
    listen<MetersEvent>("engine:meters", e => cb(e.payload)),
};
