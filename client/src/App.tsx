import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Circle, Download, FilePlus2, FolderOpen, GitCompareArrows, MessageSquare, Mic, Pause, Pencil, Play, Plus, Power, RefreshCw, RotateCcw, RotateCw, Save, Settings, Square, Trash2, Upload } from "lucide-react";
import type { AbJudgeResponse, AssistantResponse, JsonPatch, MixAction, MixCritique, MixerProfile, MixProject, MixSession, ProfilePreset, Track } from "../../shared/types";
import { open, save } from "@tauri-apps/plugin-dialog";
import { api } from "./api";

const DEFAULT_OLLAMA_URL = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL = "gpt-oss:20b";

export function App() {
  const initialOllamaUrlRef = useRef(localStorage.getItem("autoMixer.ollamaUrl"));
  const initialOllamaModelRef = useRef(localStorage.getItem("autoMixer.ollamaModel"));
  const initialGeminiKeyRef = useRef(localStorage.getItem("autoMixer.geminiApiKey"));
  const playStartedAtRef = useRef(0);
  const pausedAtRef = useRef(0);
  const [project, setProject] = useState<MixProject>();
  const [selectedTrackIds, setSelectedTrackIds] = useState<string[]>([]);
  const [selectedRegionIds, setSelectedRegionIds] = useState<string[]>([]);
  const [selectedClip, setSelectedClip] = useState<{ trackId: string; clipId: string } | undefined>();
  const [selectedRange, setSelectedRange] = useState<{ trackId: string; start: number; end: number } | undefined>();
  const [chatText, setChatText] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [startupError, setStartupError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [recording, setRecording] = useState(false);
  const [recordingTrackId, setRecordingTrackId] = useState<string | undefined>();
  const [armedTrackId, setArmedTrackId] = useState<string | undefined>();
  const [playhead, setPlayhead] = useState(0);
  const [bypass, setBypass] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sessionMenuOpen, setSessionMenuOpen] = useState(false);
  const [sessionList, setSessionList] = useState<MixSession[]>([]);
  const [renameDraft, setRenameDraft] = useState<string | null>(null);
  const [newDraft, setNewDraft] = useState<string | null>(null);
  const [analysisProgress, setAnalysisProgress] = useState<{ stage: string; message: string; elapsedSeconds: number } | null>(null);
  const [scopedSection, setScopedSection] = useState<{ index: number; start: number; end: number; label: string } | null>(null);
  const [loopSection, setLoopSection] = useState<{ start: number; end: number } | null>(null);
  const [profilePresets, setProfilePresets] = useState<ProfilePreset[]>([]);
  const [reasoning, setReasoning] = useState<{ phase: string; text: string; tokens: { prompt: number; response: number; elapsedMs: number } | null }[]>([]);
  const [reasoningOpen, setReasoningOpen] = useState(false);
  const [streamingTurn, setStreamingTurn] = useState<{ phase: string; text: string } | null>(null);
  const [mode, setMode] = useState<"interactive" | "auto">("interactive");
  const [autoMixStages, setAutoMixStages] = useState<{ stageId: string; displayName: string; status: string; actionCount: number; warnings: string[]; error?: string; tokens: number; elapsedMs: number; explanation?: string }[]>([]);
  const [autoMixRunning, setAutoMixRunning] = useState(false);
  const [ollamaUrl, setOllamaUrl] = useState(() => initialOllamaUrlRef.current ?? DEFAULT_OLLAMA_URL);
  const [ollamaModel, setOllamaModel] = useState(() => initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL);
  const [geminiApiKey, setGeminiApiKey] = useState(() => initialGeminiKeyRef.current ?? "");
  const [modelOptions, setModelOptions] = useState<string[]>(() => [initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL]);
  const [modelStatus, setModelStatus] = useState("Not checked");
  const [modelsLoading, setModelsLoading] = useState(false);
  const [inputDevices, setInputDevices] = useState<string[]>([]);
  const [trackInputDevices, setTrackInputDevices] = useState<Record<string, string>>({});
  const [liveRecordingPeaks, setLiveRecordingPeaks] = useState<number[]>([]);

  const session = project?.session;
  const chatLogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const node = chatLogRef.current;
    if (!node) return;
    node.scrollTop = node.scrollHeight;
  }, [messages.length, busy]);

  const lastLoadedSessionRef = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (!session) return;
    if (lastLoadedSessionRef.current === session.id) return;
    lastLoadedSessionRef.current = session.id;
    const stored = (project?.chatMessages ?? []) as ChatMessage[];
    setMessages(stored);
    setArmedTrackId(undefined);
  }, [session?.id, project?.chatMessages]);

  useEffect(() => {
    if (!session) return;
    if (lastLoadedSessionRef.current !== session.id) return;
    const handle = setTimeout(() => {
      void api.saveChatMessages(session.id, messages).catch(() => undefined);
    }, 600);
    return () => clearTimeout(handle);
  }, [messages, session?.id]);

  useEffect(() => {
    void bootstrap();
  }, []);

  useEffect(() => {
    void api.listMixerProfiles().then(setProfilePresets).catch(() => undefined);
    void api.inputDevices().then((result) => setInputDevices(result.devices)).catch(() => undefined);
  }, []);

  async function applyProfilePreset(preset: ProfilePreset) {
    if (!session) return;
    try {
      const updated = await api.setMixerProfile(session.id, preset.profile);
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    }
  }

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void api.onAudioProgress((event) => {
      setAnalysisProgress(event);
      if (event.stage === "done" || event.stage === "error") {
        setTimeout(() => setAnalysisProgress(null), 4000);
      }
    }).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: (() => void)[] = [];
    const reg = (p: Promise<() => void>) => {
      void p.then((fn) => { if (cancelled) fn(); else unlisteners.push(fn); });
    };
    reg(api.onAutoMixEvent("start", () => {
      setAutoMixRunning(true);
      setAutoMixStages([]);
    }));
    reg(api.onAutoMixEvent("stage-start", (payload) => {
      const p = payload as { stageId: string; displayName: string };
      setAutoMixStages((cur) => [...cur, { stageId: p.stageId, displayName: p.displayName, status: "running", actionCount: 0, warnings: [], tokens: 0, elapsedMs: 0 }]);
    }));
    reg(api.onAutoMixEvent("stage-done", (payload) => {
      const p = payload as { stageId: string; displayName: string; status: string; actionCount: number; warnings?: string[]; error?: string; tokens: number; elapsedMs: number; explanation?: string };
      setAutoMixStages((cur) => cur.map((s) => s.stageId === p.stageId ? { ...s, ...p, warnings: p.warnings ?? [] } : s));
    }));
    reg(api.onAutoMixEvent("complete", (payload) => {
      setAutoMixRunning(false);
      const p = payload as { project?: MixProject };
      if (p?.project) setProject(p.project);
    }));
    reg(api.onLlmTurnStart(() => {
      setReasoning([]);
      setStreamingTurn(null);
    }));
    reg(api.onLlmTurnEnd(() => setStreamingTurn(null)));
    reg(api.onLlmChunk((event) => {
      setReasoning((current) => {
        const last = current[current.length - 1];
        if (last && last.phase === event.phase && !last.tokens) {
          return [...current.slice(0, -1), { ...last, text: last.text + event.text }];
        }
        return [...current, { phase: event.phase, text: event.text, tokens: null }];
      });
      // Stream to the in-flight chat bubble only for visible-to-user phases.
      if (event.phase === "action" || event.phase === "critique") {
        setStreamingTurn((current) => ({
          phase: event.phase,
          text: (current?.phase === event.phase ? current.text : "") + event.text,
        }));
      }
    }));
    reg(api.onLlmStats((event) => {
      setReasoning((current) => {
        const i = [...current].reverse().findIndex((r) => r.phase === event.phase && !r.tokens);
        if (i === -1) return current;
        const idx = current.length - 1 - i;
        return current.map((r, j) =>
          j === idx
            ? { ...r, tokens: { prompt: event.promptTokens, response: event.responseTokens, elapsedMs: event.elapsedMs } }
            : r
        );
      });
    }));
    return () => { cancelled = true; unlisteners.forEach((fn) => fn()); };
  }, []);

  const turnTokenTotal = useMemo(() => {
    return reasoning.reduce(
      (acc, r) => ({
        prompt: acc.prompt + (r.tokens?.prompt ?? 0),
        response: acc.response + (r.tokens?.response ?? 0),
        elapsedMs: acc.elapsedMs + (r.tokens?.elapsedMs ?? 0),
      }),
      { prompt: 0, response: 0, elapsedMs: 0 }
    );
  }, [reasoning]);

  useEffect(() => {
    localStorage.setItem("autoMixer.ollamaUrl", ollamaUrl);
  }, [ollamaUrl]);

  useEffect(() => {
    localStorage.setItem("autoMixer.ollamaModel", ollamaModel);
    setModelOptions((items) => items.includes(ollamaModel) ? items : [...items, ollamaModel]);
  }, [ollamaModel]);

  useEffect(() => {
    localStorage.setItem("autoMixer.geminiApiKey", geminiApiKey);
  }, [geminiApiKey]);

  useEffect(() => {
    if (!session) return;
    const raw = localStorage.getItem(`autoMixer.trackInputDevices.${session.id}`);
    if (!raw) {
      setTrackInputDevices({});
      return;
    }
    try {
      const parsed = JSON.parse(raw);
      setTrackInputDevices(parsed.devices ?? parsed);
    } catch {
      setTrackInputDevices({});
    }
  }, [session?.id]);

  useEffect(() => {
    if (!session) return;
    localStorage.setItem(
      `autoMixer.trackInputDevices.${session.id}`,
      JSON.stringify({ devices: trackInputDevices })
    );
  }, [trackInputDevices, session?.id]);

  useEffect(() => {
    if (!recording) {
      setLiveRecordingPeaks([]);
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const result = await api.recordingMeters();
        if (!cancelled && result.peaks.length > 0) {
          setLiveRecordingPeaks((items) => [...items, ...result.peaks].slice(-512));
        }
      } catch {
        // Metering is cosmetic; recording errors surface through stop/start.
      }
      if (!cancelled) window.setTimeout(tick, 80);
    };
    void tick();
    return () => { cancelled = true; };
  }, [recording]);

  useEffect(() => {
    if (!session || !armedTrackId) return;
    if (!session.tracks.some((track) => track.id === armedTrackId)) {
      setArmedTrackId(undefined);
    }
  }, [session, armedTrackId]);

  useEffect(() => {
    if (!session || !selectedClip) return;
    const track = session.tracks.find((item) => item.id === selectedClip.trackId);
    if (!track || !track.clips.some((clip) => clip.id === selectedClip.clipId)) {
      setSelectedClip(undefined);
    }
  }, [session, selectedClip]);

  useEffect(() => {
    if (!session || !selectedRange) return;
    if (!session.tracks.some((track) => track.id === selectedRange.trackId)) {
      setSelectedRange(undefined);
    }
  }, [session, selectedRange]);

  const duration = useMemo(() => {
    if (!session) return 0;
    const sources = new Map(session.sourceFiles.map((source) => [source.id, source]));
    return Math.max(0, ...session.tracks.map((track) => {
      if (track.clips.length > 0) {
        return Math.max(0, ...track.clips.map((clip) => clip.endSample / session.sampleRate));
      }
      const source = sources.get(track.sourceFileId);
      return (track.startSample + (source?.durationSamples ?? 0)) / session.sampleRate;
    }));
  }, [session]);

  useEffect(() => {
    let frame = 0;
    const tick = () => {
      if (playing) {
        const elapsed = Math.max(0, (performance.now() - playStartedAtRef.current) / 1000);
        if (loopSection && elapsed >= loopSection.end) {
          const next = loopSection.start;
          pausedAtRef.current = next;
          playStartedAtRef.current = performance.now() - next * 1000;
          setPlayhead(next);
          if (session) {
            void api.seek(Math.round(next * session.sampleRate)).catch(() => undefined);
          }
        } else if (duration > 0 && elapsed >= duration) {
          pausedAtRef.current = duration;
          setPlayhead(duration);
          setPlaying(false);
          void api.pause().catch(() => undefined);
        } else {
          setPlayhead(elapsed);
        }
      } else {
        setPlayhead(pausedAtRef.current);
      }
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing, duration, loopSection, session?.sampleRate]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.code !== "Space") return;
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return;
      event.preventDefault();
      void togglePlay();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [playing, session?.id]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key !== "Delete" && event.key !== "Backspace") return;
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable)) return;
      if (!session || busy) return;
      if (selectedRange) {
        event.preventDefault();
        void deleteClipRange(selectedRange);
        return;
      }
      if (selectedClip) {
        event.preventDefault();
        void deleteClip(selectedClip.trackId, selectedClip.clipId);
        return;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [session, selectedClip, selectedRange, busy, recording, recordingTrackId]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable)) return;
      const mod = event.metaKey || event.ctrlKey;
      if (!mod || busy || recording) return;
      const key = event.key.toLowerCase();
      if (key === "z" && !event.shiftKey) {
        event.preventDefault();
        void doUndo();
      } else if ((key === "z" && event.shiftKey) || key === "y") {
        event.preventDefault();
        void doRedo();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [session, busy, recording]);

  async function flushChat() {
    if (!session) return;
    try {
      await api.saveChatMessages(session.id, messages);
    } catch {}
  }

  async function refreshSessionList() {
    try {
      const sessions = await api.sessions();
      setSessionList(sessions);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function switchSession(sessionId: string) {
    if (!sessionId || sessionId === session?.id) return;
    await flushChat();
    setBusy(true);
    try {
      const loaded = await api.getSession(sessionId);
      setProject(loaded);
      setSelectedTrackIds([]);
      setSelectedRegionIds([]);
      pausedAtRef.current = 0;
      setPlayhead(0);
      setPlaying(false);
      await api.stop().catch(() => undefined);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function commitNewSession(name: string) {
    const trimmed = name.trim();
    if (!trimmed) return;
    await flushChat();
    setBusy(true);
    try {
      const created = await api.createSession(trimmed);
      setProject(created);
      setSelectedTrackIds([]);
      setSelectedRegionIds([]);
      await refreshSessionList();
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function commitRename(name: string) {
    if (!session) return;
    const trimmed = name.trim();
    if (!trimmed || trimmed === session.name) return;
    try {
      const updated = await api.renameSession(session.id, trimmed);
      setProject(updated);
      await refreshSessionList();
    } catch (error) {
      pushSystem(error);
    }
  }

  async function deleteCurrentSession() {
    if (!session) return;
    if (!window.confirm(`Delete session "${session.name}"? This cannot be undone.`)) return;
    setBusy(true);
    try {
      await api.deleteSession(session.id);
      const remaining = (await api.sessions()).filter((s) => s.id !== session.id);
      setSessionList(remaining);
      const next = remaining[0]
        ? await api.getSession(remaining[0].id)
        : await api.createSession("AutoMixer session");
      setProject(next);
      setSelectedTrackIds([]);
      setSelectedRegionIds([]);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function saveProjectBundle() {
    if (!session) return;
    await flushChat();
    const folder = await save({
      defaultPath: `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "project"}.amix`,
      filters: [{ name: "AutoMixer project", extensions: ["amix"] }]
    });
    if (!folder) return;
    setBusy(true);
    try {
      await api.exportProjectBundle(session.id, folder);
      setMessages((items) => [...items, { role: "system", text: `Saved bundle to ${folder}` }]);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function openProjectBundle() {
    const folder = await open({ multiple: false, directory: true, title: "Select an AutoMixer project bundle" });
    const bundleDir = Array.isArray(folder) ? folder[0] : folder;
    if (!bundleDir) return;
    await flushChat();
    setBusy(true);
    try {
      const loaded = await api.importProjectBundle(bundleDir);
      setProject(loaded);
      setSelectedTrackIds([]);
      setSelectedRegionIds([]);
      await refreshSessionList();
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function bootstrap() {
    setLoading(true);
    try {
      const [config, sessions] = await Promise.all([api.config().catch(() => undefined), api.sessions()]);
      if (config) {
        if (!initialOllamaUrlRef.current) setOllamaUrl(config.ollamaBaseUrl);
        if (!initialOllamaModelRef.current) {
          setOllamaModel(config.ollamaModel);
          setModelOptions([config.ollamaModel]);
        }
      }
      const loaded = sessions[0] ? await api.getSession(sessions[0].id) : await api.createSession("AutoMixer session");
      setProject(loaded);
      setSessionList(sessions.length > 0 ? sessions : [loaded.session]);
    } catch (error) {
      const message = error instanceof Error ? error.message : "Could not start app.";
      setStartupError(message);
      setMessages([{ role: "system", text: message }]);
    } finally {
      setLoading(false);
    }
  }

  async function loadOllamaModels() {
    setModelsLoading(true);
    setModelStatus("Checking...");
    try {
      const result = await api.ollamaModels(ollamaUrl);
      const models = result.models.filter(Boolean);
      setModelOptions(models.includes(ollamaModel) ? models : [...models, ollamaModel]);
      setModelStatus(`${models.length} model${models.length === 1 ? "" : "s"}`);
    } catch (error) {
      setModelStatus(error instanceof Error ? error.message : "Could not reach Ollama");
    } finally {
      setModelsLoading(false);
    }
  }

  async function importFiles() {
    if (!session) return;
    const selected = await open({
      multiple: true,
      filters: [{ name: "Audio", extensions: ["wav", "aif", "aiff", "flac", "mp3", "ogg"] }]
    });
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
    if (!paths.length) return;
    setBusy(true);
    try {
      const updated = await api.importFiles(session.id, paths);
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function addRecordingTrack() {
    if (!session) return;
    try {
      const updated = await api.createRecordingTrack(session.id);
      setProject(updated);
      const added = updated.session.tracks[updated.session.tracks.length - 1];
      if (added) {
        setSelectedTrackIds([added.id]);
        setArmedTrackId(added.id);
      }
    } catch (error) {
      pushSystem(error);
    }
  }

  async function updateTrack(track: Track, patch: Partial<Track>) {
    if (!session) return;
    const actions = [];
    if (patch.gainDb !== undefined) actions.push({ tool: "set_track_gain" as const, trackId: track.id, gainDb: patch.gainDb });
    if (patch.pan !== undefined) actions.push({ tool: "set_track_pan" as const, trackId: track.id, pan: patch.pan });
    if (patch.muted !== undefined) actions.push({ tool: "mute_track" as const, trackId: track.id, muted: patch.muted });
    if (patch.solo !== undefined) actions.push({ tool: "solo_track" as const, trackId: track.id, solo: patch.solo });
    if (patch.aiGenerated !== undefined) actions.push({ tool: "set_track_ai_generated" as const, trackId: track.id, aiGenerated: patch.aiGenerated });
    if (!actions.length) return;
    const updated = await api.applyActions(session.id, actions, "Manual control change");
    setProject(updated);
  }

  async function deleteTrack(track: Track) {
    if (!session) return;
    if (recording && recordingTrackId === track.id) {
      pushSystem("Stop recording before deleting the recording track.");
      return;
    }
    const confirmed = window.confirm(`Delete track "${track.name}"?`);
    if (!confirmed) return;
    setBusy(true);
    try {
      const updated = await api.applyActions(session.id, [{ tool: "delete_track", trackId: track.id }], `Deleted ${track.name}`);
      setProject(updated);
      setSelectedTrackIds((ids) => ids.filter((id) => id !== track.id));
      setArmedTrackId((id) => id === track.id ? undefined : id);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function deleteClip(trackId: string, clipId: string) {
    if (!session) return;
    const track = session.tracks.find((item) => item.id === trackId);
    const clip = track?.clips.find((item) => item.id === clipId);
    if (!track || !clip) return;
    if (recording && recordingTrackId === trackId) {
      pushSystem("Stop recording before deleting clips on the recording track.");
      return;
    }
    const confirmed = window.confirm(`Delete recording "${clip.name ?? track.name}"?`);
    if (!confirmed) return;
    setBusy(true);
    try {
      const updated = await api.deleteClip(session.id, trackId, clipId);
      setProject(updated);
      setSelectedClip(undefined);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function deleteClipRange(range: { trackId: string; start: number; end: number }) {
    if (!session) return;
    const track = session.tracks.find((item) => item.id === range.trackId);
    if (!track) return;
    const start = Math.max(0, Math.min(range.start, range.end));
    const end = Math.max(0, Math.max(range.start, range.end));
    if (end - start < 0.01) return;
    if (recording && recordingTrackId === range.trackId) {
      pushSystem("Stop recording before deleting audio on the recording track.");
      return;
    }
    const startSample = Math.round(start * session.sampleRate);
    const endSample = Math.round(end * session.sampleRate);
    setBusy(true);
    try {
      const updated = await api.deleteClipRange(session.id, range.trackId, startSample, endSample);
      setProject(updated);
      setSelectedClip(undefined);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function sendChat(overrideText?: string) {
    if (!session) return;
    const userText = (overrideText ?? chatText).trim();
    if (!userText) return;
    if (overrideText === undefined) setChatText("");
    setMessages((items) => [...items, { role: "user", text: userText }]);
    setBusy(true);
    try {
      const recentCritique = findLatestCritique(messages);
      const selectedTimeRange = scopedSection
        ? {
            startSample: Math.round(scopedSection.start * session.sampleRate),
            endSample: Math.round(scopedSection.end * session.sampleRate)
          }
        : undefined;
      const response = await api.assistant({
        sessionId: session.id,
        userText: scopedSection ? `[scope: ${scopedSection.label} ${formatTime(scopedSection.start)}–${formatTime(scopedSection.end)}] ${userText}` : userText,
        selectedTrackIds,
        selectedRegionIds,
        selectedTimeRange,
        ollamaBaseUrl: ollamaUrl,
        ollamaModel,
        recentCritique
      });
      handleAssistantResponse(response);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  function handleAssistantResponse(response: AssistantResponse) {
    if (response.status === "ok") {
      setProject((current) => current ? { ...current, session: response.session, history: response.history } : current);
      const turnEntry = response.history[response.history.length - 1];
      setMessages((items) => [
        ...items,
        {
          role: "assistant-turn",
          explanation: response.explanation,
          skills: response.selectedSkills,
          actions: response.actions,
          warnings: response.warnings,
          rationale: response.rationale,
          perActionNotes: response.perActionNotes,
          forwardPatch: turnEntry?.forwardPatch ?? [],
          inversePatch: turnEntry?.inversePatch ?? [],
          applied: true
        }
      ]);
    } else if (response.status === "clarification") {
      setMessages((items) => [...items, { role: "assistant", text: response.question }]);
    } else if (response.status === "critique") {
      setMessages((items) => [
        ...items,
        { role: "critique", critique: response.critique, skills: response.selectedSkills }
      ]);
    } else {
      const text = response.rawModelOutput
        ? `${response.message}\n\nModel output:\n${response.rawModelOutput}`
        : response.message;
      setMessages((items) => [...items, { role: "system", text }]);
    }
  }

  async function doUndo() {
    if (!session) return;
    setBusy(true);
    try {
      const updated = await api.undo(session.id);
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function doRedo() {
    if (!session) return;
    setBusy(true);
    try {
      const updated = await api.redo(session.id);
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function toggleTurn(messageIndex: number) {
    if (!session) return;
    const target = messages[messageIndex];
    if (!target || target.role !== "assistant-turn" || target.forwardPatch.length === 0) return;
    const turning = target.applied ? "off" : "on";
    const note = target.explanation ? target.explanation.split("\n")[0].slice(0, 80) : "assistant turn";
    setBusy(true);
    try {
      const updated = target.applied
        ? await api.applyPatch(session.id, target.inversePatch, target.forwardPatch, `Bypass: ${note}`)
        : await api.applyPatch(session.id, target.forwardPatch, target.inversePatch, `Restore: ${note}`);
      setProject(updated);
      setMessages((items) =>
        items.map((item, i) => (i === messageIndex && item.role === "assistant-turn" ? { ...item, applied: !item.applied } : item))
      );
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function togglePlay() {
    if (!session) return;
    if (playing) {
      await api.pause();
      pausedAtRef.current = Math.max(0, (performance.now() - playStartedAtRef.current) / 1000);
      setPlaying(false);
    } else {
      await api.play(session.id);
      playStartedAtRef.current = performance.now() - pausedAtRef.current * 1000;
      setPlaying(true);
    }
  }

  async function toggleRecording() {
    if (!session) return;
    if (recording) {
      try {
        const trackId = recordingTrackId;
        const updated = await api.stopRecording(session.id);
        setProject(updated);
        if (trackId) {
          const track = updated.session.tracks.find((item) => item.id === trackId);
          const clip = track?.clips[track.clips.length - 1];
          if (clip) {
            setSelectedTrackIds([trackId]);
            setSelectedClip({ trackId, clipId: clip.id });
          }
        }
        setRecording(false);
        setRecordingTrackId(undefined);
      } catch (error) {
        pushSystem(error);
      }
      return;
    }
    try {
      if (!armedTrackId) {
        pushSystem("Arm one track with R before recording.");
        return;
      }
      if (!session.tracks.some((track) => track.id === armedTrackId)) {
        pushSystem("The armed track is no longer available. Arm a track again.");
        setArmedTrackId(undefined);
        return;
      }
      const targetTrackId = armedTrackId;
      const startSeconds = selectedRange?.trackId === targetTrackId ? Math.min(selectedRange.start, selectedRange.end) : playhead;
      const startSample = Math.round(Math.max(0, startSeconds) * session.sampleRate);
      const inputDevice = trackInputDevices[targetTrackId];
      setLiveRecordingPeaks([]);
      if (selectedRange?.trackId === targetTrackId) {
        await seekTo(Math.max(0, startSeconds));
      }
      await api.startRecording(session.id, startSample, targetTrackId, inputDevice);
      setRecording(true);
      setRecordingTrackId(targetTrackId);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function toggleBypass() {
    const next = !bypass;
    setBypass(next);
    try {
      await api.setMasterBypass(next);
    } catch (error) {
      setBypass(!next);
      pushSystem(error);
    }
  }

  function seekTo(seconds: number) {
    if (!session) return;
    const safe = Math.max(0, Math.min(duration, seconds));
    pausedAtRef.current = safe;
    if (playing) {
      playStartedAtRef.current = performance.now() - safe * 1000;
    }
    setPlayhead(safe);
    void api.seek(Math.round(safe * session.sampleRate)).catch(() => undefined);
  }

  async function stop() {
    if (recording && session) {
      try {
        const updated = await api.stopRecording(session.id);
        setProject(updated);
        setRecording(false);
        setRecordingTrackId(undefined);
      } catch (error) {
        pushSystem(error);
      }
    }
    await api.stop();
    pausedAtRef.current = 0;
    setPlayhead(0);
    setPlaying(false);
  }

  async function resetSession() {
    if (!session) return;
    const confirmed = window.confirm("Clear all tracks, history, and chat to start a fresh session?");
    if (!confirmed) return;
    setBusy(true);
    try {
      const updated = await api.resetSession(session.id);
      setProject(updated);
      setSelectedTrackIds([]);
      setSelectedRegionIds([]);
      pausedAtRef.current = 0;
      setPlayhead(0);
      setPlaying(false);
      await api.stop().catch(() => undefined);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function balanceSongLevels() {
    if (!session?.sections?.length) return;
    const withAnalysis = session.sections.filter((s) => s.analysis);
    if (withAnalysis.length < 2) {
      pushSystem("Need at least two analyzed sections to balance levels.");
      return;
    }
    const lines = withAnalysis.map((s) => `${s.label} @ ${formatTime(s.start)}-${formatTime(s.end)} = ${s.analysis!.lufs.toFixed(1)} LUFS`);
    void sendChat(
      `Balance the song's section levels. Per-section measured loudness:\n${lines.join("\n")}\n\n` +
        `Choose a target loudness (median of chorus sections is a reasonable default), then for each section that differs by more than 1.5 dB from the target, create a region with create_region using its start/end (seconds) and use apply_section_automation on the master gainDb (or per-track if needed) to nudge it toward the target. Cap any individual move at +/-3 dB.`
    );
  }

  async function judgeCurrentMixAb() {
    if (!session) return;
    setBusy(true);
    try {
      const result = geminiApiKey.trim()
        ? await api.judgeMixAb(session.id, geminiApiKey.trim())
        : await api.judgeMixAbLocal(session.id);
      setMessages((items) => [...items, { role: "ab-judge", result }]);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    let cancelled = false;
    const unlisteners: (() => void)[] = [];
    const reg = (p: Promise<() => void>) => {
      void p.then((fn) => { if (cancelled) fn(); else unlisteners.push(fn); });
    };
    reg(api.onMenuDetectStructure(() => {
      void analyzeStructure();
    }));
    reg(api.onMenuLevelSections(() => {
      void balanceSongLevels();
    }));
    return () => { cancelled = true; unlisteners.forEach((fn) => fn()); };
  }, [session, busy]);

  async function analyzeStructure() {
    if (!session) return;
    setBusy(true);
    setAnalysisProgress({ stage: "starting", message: "Preparing analysis…", elapsedSeconds: 0 });
    try {
      const updated = await api.analyzeMasterStructure(session.id);
      setProject(updated);
      const count = updated.session.sections?.length ?? 0;
      setMessages((items) => [
        ...items,
        { role: "system", text: count > 0 ? `Detected ${count} sections (bpm ${updated.session.bpm?.toFixed(0) ?? "?"}).` : "Structure analysis returned no sections." }
      ]);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function toggleAllAi() {
    if (!session) return;
    const allOn = session.tracks.length > 0 && session.tracks.every((t) => t.aiGenerated);
    const next = !allOn;
    const actions = session.tracks.map((t) => ({ tool: "set_track_ai_generated" as const, trackId: t.id, aiGenerated: next }));
    if (actions.length === 0) return;
    setBusy(true);
    try {
      const updated = await api.applyActions(session.id, actions, next ? "Marked all tracks as AI-generated" : "Cleared AI-generated flags");
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function renderCurrentMix() {
    if (!session) return;
    const outputPath = await save({
      defaultPath: `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "automix"}.wav`,
      filters: [{ name: "WAV", extensions: ["wav"] }]
    });
    if (!outputPath) return;
    setBusy(true);
    try {
      await api.renderMix(session.id, outputPath);
      setMessages((items) => [...items, { role: "system", text: `Rendered ${outputPath}` }]);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  function pushSystem(error: unknown) {
    let text: string;
    if (error instanceof Error) text = error.message;
    else if (typeof error === "string") text = error;
    else if (error && typeof error === "object") {
      try { text = JSON.stringify(error); } catch { text = String(error); }
    } else text = String(error ?? "Unexpected error");
    setMessages((items) => [...items, { role: "system", text }]);
  }

  if (loading) return <main className="loading">Loading AutoMixer...</main>;
  if (startupError || !project || !session) {
    return (
      <main className="loading">
        <div>
          <h1>AutoMixer could not start</h1>
          <p>{startupError ?? "No session was loaded."}</p>
        </div>
      </main>
    );
  }

  return (
    <main className="app">
      <section className="mix">
        <header className="topbar">
          <div className="title-block">
            <h1>AutoMixer</h1>
            <div className="session-picker">
              <button
                className="session-current"
                onClick={() => {
                  setSessionMenuOpen((open) => {
                    if (!open) void refreshSessionList();
                    return !open;
                  });
                }}
                disabled={busy}
                title="Session menu"
              >
                <strong>{session.name}</strong>
                <ChevronDown size={14} />
              </button>
              {sessionMenuOpen ? (
                <div className="session-menu" onMouseLeave={() => { setSessionMenuOpen(false); setRenameDraft(null); setNewDraft(null); }}>
                  <div className="session-menu-section">
                    {sessionList.length === 0 ? (
                      <div className="session-menu-empty">No saved sessions</div>
                    ) : (
                      sessionList.map((entry) => (
                        <button
                          key={entry.id}
                          className={`session-menu-item ${entry.id === session.id ? "active" : ""}`}
                          onClick={() => {
                            setSessionMenuOpen(false);
                            void switchSession(entry.id);
                          }}
                        >
                          {entry.name}
                        </button>
                      ))
                    )}
                  </div>
                  <div className="session-menu-divider" />
                  {newDraft !== null ? (
                    <form
                      className="session-menu-form"
                      onSubmit={(event) => {
                        event.preventDefault();
                        const value = newDraft;
                        setNewDraft(null);
                        setSessionMenuOpen(false);
                        void commitNewSession(value);
                      }}
                    >
                      <input
                        autoFocus
                        value={newDraft}
                        onChange={(event) => setNewDraft(event.target.value)}
                        onKeyDown={(event) => { if (event.key === "Escape") setNewDraft(null); }}
                        placeholder="Session name"
                      />
                      <button type="submit">Create</button>
                    </form>
                  ) : (
                    <button className="session-menu-item" onClick={() => setNewDraft("AutoMixer session")}>
                      <FilePlus2 size={14} /> New session
                    </button>
                  )}
                  {renameDraft !== null ? (
                    <form
                      className="session-menu-form"
                      onSubmit={(event) => {
                        event.preventDefault();
                        const value = renameDraft;
                        setRenameDraft(null);
                        setSessionMenuOpen(false);
                        void commitRename(value);
                      }}
                    >
                      <input
                        autoFocus
                        value={renameDraft}
                        onChange={(event) => setRenameDraft(event.target.value)}
                        onKeyDown={(event) => { if (event.key === "Escape") setRenameDraft(null); }}
                        placeholder="New name"
                      />
                      <button type="submit">Rename</button>
                    </form>
                  ) : (
                    <button className="session-menu-item" onClick={() => setRenameDraft(session.name)}>
                      <Pencil size={14} /> Rename
                    </button>
                  )}
                  <button className="session-menu-item" onClick={() => { setSessionMenuOpen(false); void saveProjectBundle(); }}>
                    <Save size={14} /> Save project as bundle…
                  </button>
                  <button className="session-menu-item" onClick={() => { setSessionMenuOpen(false); void openProjectBundle(); }}>
                    <FolderOpen size={14} /> Open project bundle…
                  </button>
                  <div className="session-menu-divider" />
                  <button className="session-menu-item danger" onClick={() => { setSessionMenuOpen(false); void deleteCurrentSession(); }}>
                    <Trash2 size={14} /> Delete this session
                  </button>
                </div>
              ) : null}
            </div>
            <span>{session.tracks.length} tracks · {formatTime(playhead)} / {formatTime(duration)}</span>
          </div>
          <div className="transport">
            <button onClick={() => void togglePlay()} title={playing ? "Pause" : "Play"}>{playing ? <Pause size={18} /> : <Play size={18} />}</button>
            <button
              className={`record-toggle ${recording ? "active" : ""}`}
              onClick={() => void toggleRecording()}
              disabled={busy}
              title={recording ? "Stop recording" : "Record into the R-armed track"}
              aria-pressed={recording}
            >
              <Circle size={18} />
            </button>
            <button onClick={() => void addRecordingTrack()} disabled={busy} title="Add recording track">
              <Plus size={18} />
            </button>
            <button onClick={() => void stop()} title="Stop"><Square size={18} /></button>
            <button
              className={`bypass-toggle ${bypass ? "bypass-active" : ""}`}
              onClick={() => void toggleBypass()}
              title={bypass ? "Hearing ORIGINAL (no processing). Click for the mix." : "Hearing the MIX (with all processing). Click for the original."}
              aria-pressed={bypass}
            >
              <GitCompareArrows size={16} />
              <span className="bypass-label">{bypass ? "ORIG" : "MIX"}</span>
            </button>
            <button onClick={doUndo} title="Undo"><RotateCcw size={18} /></button>
            <button onClick={doRedo} title="Redo"><RotateCw size={18} /></button>
            <button
              className={`ai-bulk ${session.tracks.length > 0 && session.tracks.every((t) => t.aiGenerated) ? "active" : ""}`}
              onClick={() => void toggleAllAi()}
              disabled={busy || session.tracks.length === 0}
              title="Toggle AI-generated flag on all tracks (Suno, demucs, etc.). The agent uses gentler EQ/compression and lower reverb on AI stems."
            >
              All AI
            </button>
            <button
              onClick={() => void judgeCurrentMixAb()}
              disabled={busy || session.tracks.length === 0}
              title="Use Gemini Flash to judge ORIG versus MIX on a short representative clip"
            >
              <GitCompareArrows size={18} />
              <span>A/B</span>
            </button>
            <button onClick={() => void renderCurrentMix()} title="Export WAV"><Download size={18} /></button>
            <button className="upload" onClick={() => void importFiles()} title="Import audio">
              <Upload size={18} />
            </button>
          </div>
        </header>

        {analysisProgress ? (
          <div className={`progress-banner stage-${analysisProgress.stage}`}>
            <div className="progress-banner-spinner" aria-hidden="true" />
            <div className="progress-banner-text">
              <strong>{stageLabel(analysisProgress.stage)}</strong>
              <span>{analysisProgress.message || "…"}</span>
            </div>
            {analysisProgress.elapsedSeconds > 0 ? (
              <span className="progress-banner-time">{Math.round(analysisProgress.elapsedSeconds)}s</span>
            ) : null}
          </div>
        ) : null}

        <div className="timeline">
          <div className="daw-workspace">
            <TrackInspector
              track={selectedTrackIds.length === 1 ? session.tracks.find((track) => track.id === selectedTrackIds[0]) : undefined}
              source={selectedTrackIds.length === 1 ? session.sourceFiles.find((source) => source.id === session.tracks.find((track) => track.id === selectedTrackIds[0])?.sourceFileId) : undefined}
              sampleRate={session.sampleRate}
              inputDevices={inputDevices}
              inputDevice={selectedTrackIds.length === 1 ? trackInputDevices[selectedTrackIds[0]] ?? "" : ""}
              selectionCount={selectedTrackIds.length}
              onChange={(track, patch) => void updateTrack(track, patch)}
              onInputDeviceChange={(trackId, device) => setTrackInputDevices((current) => ({ ...current, [trackId]: device }))}
              onRefreshInputDevices={() => void api.inputDevices().then((result) => setInputDevices(result.devices)).catch(pushSystem)}
              onDelete={(track) => void deleteTrack(track)}
            />
            <div className="track-lanes">
              {session.sections && session.sections.length > 0 && duration > 0 ? (
                <SectionRibbon
                  sections={session.sections}
                  duration={duration}
                  playhead={playhead}
                  scopedIndex={scopedSection?.index ?? null}
                  loopRange={loopSection}
                  onSeek={seekTo}
                  onScope={(index) => {
                    const s = session.sections?.[index];
                    if (!s) return;
                    setScopedSection((current) =>
                      current?.index === index
                        ? null
                        : { index, start: s.start, end: s.end, label: s.label }
                    );
                  }}
                  onLoop={(index) => {
                    const s = session.sections?.[index];
                    if (!s) return;
                    setLoopSection((current) =>
                      current && Math.abs(current.start - s.start) < 0.01 && Math.abs(current.end - s.end) < 0.01
                        ? null
                        : { start: s.start, end: s.end }
                    );
                  }}
                />
              ) : null}
              {session.tracks.length === 0 ? (
                <div className="empty">
                  <Upload size={28} />
                  <span>Import stems to start mixing.</span>
                </div>
              ) : (
                session.tracks.map((track) => {
                  const sourceById = new Map(session.sourceFiles.map((item) => [item.id, item]));
                  const source = sourceById.get(track.sourceFileId);
                  const clips = track.clips.length > 0
                    ? track.clips.map((clip) => ({
                        id: clip.id,
                        name: clip.name ?? "Recording",
                        startSeconds: clip.startSample / session.sampleRate,
                        sourceSeconds: Math.max(0, (clip.endSample - clip.startSample) / session.sampleRate),
                        peaks: sourceById.get(clip.sourceFileId ?? track.sourceFileId)?.peakPreview,
                      }))
                    : [{
                        id: `legacy-${track.id}`,
                        name: track.name,
                        startSeconds: track.startSample / session.sampleRate,
                        sourceSeconds: (source?.durationSamples ?? 0) / session.sampleRate,
                        peaks: source?.peakPreview,
                      }];
                  return (
                    <TrackRow
                      key={track.id}
                      track={track}
                      selected={selectedTrackIds.includes(track.id)}
                      armed={armedTrackId === track.id}
                      playhead={playhead}
                      duration={duration}
                      clips={clips}
                      selectedClipId={selectedClip?.trackId === track.id ? selectedClip.clipId : undefined}
                      selectedRange={selectedRange?.trackId === track.id ? selectedRange : undefined}
                      recording={recordingTrackId === track.id}
                      livePeaks={recordingTrackId === track.id ? liveRecordingPeaks : undefined}
                      onSelect={() => { setSelectedTrackIds([track.id]); setSelectedClip(undefined); setSelectedRange(undefined); }}
                      onClipSelect={(clipId) => { setSelectedTrackIds([track.id]); setSelectedRange(undefined); setSelectedClip({ trackId: track.id, clipId }); }}
                      onRangeSelect={(start, end) => {
                        setSelectedTrackIds([track.id]);
                        setSelectedClip(undefined);
                        setSelectedRange({ trackId: track.id, start: Math.min(start, end), end: Math.max(start, end) });
                      }}
                      onRangeClear={() => setSelectedRange(undefined)}
                      onArm={() => setArmedTrackId((current) => current === track.id ? undefined : track.id)}
                      onSeek={(seconds) => seekTo(seconds)}
                      onChange={(patch) => void updateTrack(track, patch)}
                    />
                  );
                })
              )}
            </div>
          </div>
        </div>

        <MasterBar
          gainDb={session.master.gainDb}
          onChange={async (gainDb) => {
            const updated = await api.setMasterGain(session.id, gainDb);
            setProject(updated);
          }}
        />
      </section>

      <aside className={`assistant ${settingsOpen ? "settings-open" : ""}`}>
        <div className="assistant-head">
          <div className="assistant-title">
            <MessageSquare size={18} />
            <strong>Mix engineer</strong>
            <div className="mode-toggle">
              <button className={mode === "interactive" ? "active" : ""} onClick={() => setMode("interactive")}>Chat</button>
              <button className={mode === "auto" ? "active" : ""} onClick={() => setMode("auto")}>Auto-mix</button>
            </div>
          </div>
          <button onClick={() => setSettingsOpen((open) => !open)} title="LLM settings">
            <Settings size={18} />
          </button>
        </div>
        {settingsOpen ? (
          <div className="llm-settings">
            <label>
              Ollama URL
              <input value={ollamaUrl} onChange={(event) => setOllamaUrl(event.target.value)} placeholder={DEFAULT_OLLAMA_URL} />
            </label>
            <label>
              Model
              <select value={ollamaModel} onChange={(event) => setOllamaModel(event.target.value)}>
                {modelOptions.map((model) => <option key={model} value={model}>{model}</option>)}
              </select>
            </label>
            <label>
              Gemini API key
              <input
                value={geminiApiKey}
                onChange={(event) => setGeminiApiKey(event.target.value)}
                type="password"
                placeholder="Used for A/B audio judge"
              />
            </label>
            <div className="llm-actions">
              <button onClick={() => void loadOllamaModels()} disabled={modelsLoading} title="Refresh models">
                <RefreshCw size={16} />
              </button>
              <span>{modelStatus}</span>
            </div>
            <label>
              Mixer profile
              <select
                value={session.mixerProfile?.presetId ?? "balanced"}
                onChange={(event) => {
                  const preset = profilePresets.find((p) => p.id === event.target.value);
                  if (preset) void applyProfilePreset(preset);
                }}
              >
                {profilePresets.map((preset) => (
                  <option key={preset.id} value={preset.id}>{preset.displayName}</option>
                ))}
              </select>
            </label>
            {(() => {
              const current = profilePresets.find((p) => p.id === (session.mixerProfile?.presetId ?? "balanced"));
              return current ? <div className="profile-summary">{current.summary}</div> : null;
            })()}
          </div>
        ) : null}
        <div className={`reasoning-panel ${reasoningOpen ? "open" : ""}`}>
          <button
            type="button"
            className="reasoning-head"
            onClick={() => setReasoningOpen((open) => !open)}
            title="Show or hide live model output and token counts"
          >
            <strong>Agent reasoning</strong>
            <span>
              {turnTokenTotal.prompt > 0
                ? `${turnTokenTotal.prompt.toLocaleString()} prompt · ${turnTokenTotal.response.toLocaleString()} response tokens · ${(turnTokenTotal.elapsedMs / 1000).toFixed(1)}s`
                : busy
                  ? "waiting for first chunk…"
                  : "click to inspect"}
            </span>
          </button>
          {reasoningOpen ? (
            <div className="reasoning-body">
              {reasoning.length === 0 ? (
                <div className="reasoning-empty">
                  When you send a message, the model's raw output and per-phase token counts will appear here live.
                </div>
              ) : (
                reasoning.map((r, i) => (
                  <div key={i} className={`reasoning-phase phase-${r.phase}`}>
                    <div className="reasoning-phase-head">
                      <span className="reasoning-phase-name">{r.phase}</span>
                      {r.tokens ? (
                        <span className="reasoning-phase-stats">
                          {r.tokens.prompt.toLocaleString()} · {r.tokens.response.toLocaleString()} tok · {(r.tokens.elapsedMs / 1000).toFixed(1)}s
                        </span>
                      ) : (
                        <span className="reasoning-phase-stats streaming">streaming…</span>
                      )}
                    </div>
                    <pre className="reasoning-text">{r.text}</pre>
                  </div>
                ))
              )}
            </div>
          ) : null}
        </div>
        {mode === "auto" ? (
          <AutoMixView
            stages={autoMixStages}
            running={autoMixRunning}
            disabled={busy || !session.tracks.length}
            onStart={(stageIds) => {
              setAutoMixStages([]);
              void api.startAutoMix(session.id, stageIds, ollamaUrl, ollamaModel);
            }}
          />
        ) : (
          <>
            <div className="chat-log" ref={chatLogRef}>
              {messages.length === 0 ? (
                <div className="hint">Select a track and ask for a mix change.</div>
              ) : (
                messages.map((message, index) => {
                  if (message.role === "critique") {
                    const isLatestCritique = findLatestCritique(messages) === message.critique;
                    return (
                      <CritiqueCard
                        key={index}
                        critique={message.critique}
                        skills={message.skills}
                        session={session}
                        onApply={isLatestCritique && !busy ? () => {
                          const steps = message.critique.recommendedNextSteps;
                          const text = steps.length > 0
                            ? `Apply your recommended next steps: ${steps.map((s, i) => `(${i + 1}) ${s}`).join(" ")}`
                            : "Apply the fixes from your critique above.";
                          void sendChat(text);
                        } : undefined}
                      />
                    );
                  }
                  if (message.role === "ab-judge") {
                    const isLatestAbJudge = findLatestAbJudge(messages) === message.result;
                    return (
                      <AbJudgeCard
                        key={index}
                        result={message.result}
                        onApply={isLatestAbJudge && !busy ? () => void sendChat(buildAbJudgeFixPrompt(message.result)) : undefined}
                      />
                    );
                  }
                  if (message.role === "assistant-turn") {
                    const canToggle = message.forwardPatch.length > 0;
                    return (
                      <AssistantTurn
                        key={index}
                        message={message}
                        session={session}
                        toggleState={canToggle ? (message.applied ? "on" : "off") : "locked"}
                        toggleDisabled={busy}
                        onToggle={() => void toggleTurn(index)}
                      />
                    );
                  }
                  return <div key={index} className={`message ${message.role}`}>{message.text}</div>;
                })
              )}
              {streamingTurn ? (
                <div className="message streaming">
                  <div className="streaming-head">
                    <div className="streaming-dot" />
                    <span>agent is {streamingTurn.phase === "critique" ? "writing the critique" : "drafting actions"}…</span>
                  </div>
                  <pre className="streaming-text">{streamingTurn.text || "…"}</pre>
                </div>
              ) : null}
            </div>
            <div className="chat-selected">
              {selectedTrackIds.length === 0 ? (
                <>Scope: <strong>all tracks</strong> (click a track to narrow and arm recording)</>
              ) : (
                <>Scope: {selectedTrackIds.map((id) => session.tracks.find((track) => track.id === id)?.name).filter(Boolean).join(", ")}</>
              )}
            </div>
            {scopedSection ? (
              <div className="chat-scope">
                <span>scope: <strong>{scopedSection.label}</strong> {formatTime(scopedSection.start)}–{formatTime(scopedSection.end)}</span>
                <button type="button" onClick={() => setScopedSection(null)}>×</button>
              </div>
            ) : null}
            <form
              className="chat-input"
              onSubmit={(event) => {
                event.preventDefault();
                void sendChat();
              }}
            >
              <textarea
                value={chatText}
                onChange={(event) => setChatText(event.target.value)}
                placeholder="Make the vocal more upfront..."
                disabled={busy}
              />
              <button disabled={busy || !chatText.trim()}>{busy ? "Working" : "Send"}</button>
            </form>
          </>
        )}
      </aside>
    </main>
  );
}

function SectionRibbon({
  sections,
  duration,
  playhead,
  scopedIndex,
  loopRange,
  onSeek,
  onScope,
  onLoop
}: {
  sections: NonNullable<MixSession["sections"]>;
  duration: number;
  playhead: number;
  scopedIndex: number | null;
  loopRange: { start: number; end: number } | null;
  onSeek: (seconds: number) => void;
  onScope: (index: number) => void;
  onLoop: (index: number) => void;
}) {
  const cursorPct = duration > 0 ? Math.max(0, Math.min(100, (playhead / duration) * 100)) : 0;
  return (
    <div className="section-ribbon">
      <div className="section-ribbon-spacer">
        <span>Structure</span>
        <small>click to seek · ⇧ to scope · ⌥ to loop</small>
      </div>
      <div className="section-ribbon-track">
        {sections.map((section, index) => {
          const leftPct = duration > 0 ? (section.start / duration) * 100 : 0;
          const widthPct = duration > 0 ? Math.max(0.2, ((section.end - section.start) / duration) * 100) : 0;
          const klass = sectionClass(section.label);
          const isScoped = scopedIndex === index;
          const isLooped = loopRange && Math.abs(loopRange.start - section.start) < 0.01 && Math.abs(loopRange.end - section.end) < 0.01;
          const lufs = section.analysis?.lufs;
          const tooltipParts = [
            `${section.label}  ${formatTime(section.start)} → ${formatTime(section.end)}`,
            lufs !== undefined ? `LUFS ${lufs.toFixed(1)}` : null,
            section.analysis ? `peak ${section.analysis.peakDb.toFixed(1)} dB · centroid ${Math.round(section.analysis.spectralCentroidHz)} Hz` : null,
            "shift-click: scope to chat · alt-click: loop"
          ].filter(Boolean).join("\n");
          return (
            <button
              key={index}
              type="button"
              className={`section-band ${klass} ${isScoped ? "scoped" : ""} ${isLooped ? "looped" : ""}`}
              style={{ left: `${leftPct}%`, width: `${widthPct}%` }}
              title={tooltipParts}
              onClick={(event) => {
                event.stopPropagation();
                if (event.shiftKey) onScope(index);
                else if (event.altKey) onLoop(index);
                else onSeek(section.start);
              }}
            >
              <span>{section.label}</span>
              {lufs !== undefined ? <em>{lufs.toFixed(1)}</em> : null}
            </button>
          );
        })}
        <div className="section-playhead" style={{ left: `${cursorPct}%` }} />
      </div>
    </div>
  );
}

function sectionClass(label: string): string {
  const l = label.toLowerCase();
  if (l.includes("chorus") || l === "drop" || l === "hook") return "section-chorus";
  if (l.includes("verse")) return "section-verse";
  if (l.includes("bridge")) return "section-bridge";
  if (l === "intro" || l === "start") return "section-intro";
  if (l === "outro" || l === "end") return "section-outro";
  if (l.includes("solo") || l.includes("inst")) return "section-solo";
  if (l.includes("break")) return "section-break";
  return "section-other";
}

function TrackInspector({
  track,
  source,
  sampleRate,
  inputDevices,
  inputDevice,
  selectionCount,
  onChange,
  onInputDeviceChange,
  onRefreshInputDevices,
  onDelete
}: {
  track?: Track;
  source?: MixSession["sourceFiles"][number];
  sampleRate: number;
  inputDevices: string[];
  inputDevice: string;
  selectionCount: number;
  onChange: (track: Track, patch: Partial<Track>) => void;
  onInputDeviceChange: (trackId: string, device: string) => void;
  onRefreshInputDevices: () => void;
  onDelete: (track: Track) => void;
}) {
  if (!track) {
    return (
      <aside className="track-inspector">
        <div className="inspector-tabs">
          <button className="active">Inspector</button>
          <button>Visibility</button>
        </div>
        <div className="inspector-empty">
          <strong>{selectionCount > 1 ? "Multiple Tracks Selected" : "No Track Selected"}</strong>
          <span>{selectionCount > 1 ? "Select one track to edit recording input and mix details." : "Click a track lane to show its details."}</span>
        </div>
      </aside>
    );
  }

  const durationSeconds = (source?.durationSamples ?? 0) / sampleRate;
  const startSeconds = track.startSample / sampleRate;
  return (
    <aside className="track-inspector">
      <div className="inspector-tabs">
        <button className="active">Inspector</button>
        <button>Visibility</button>
      </div>
      <div className="inspector-track-title" style={{ borderLeftColor: track.color }}>
        <strong>{track.name}</strong>
        <span>{track.role ?? "track"}</span>
      </div>
      <div className="inspector-section">
        <div className="inspector-section-title">Record input</div>
        <label className="inspector-field">
          <span><Mic size={12} /> Input</span>
          <select value={inputDevice} onChange={(event) => onInputDeviceChange(track.id, event.target.value)} onFocus={onRefreshInputDevices}>
            <option value="">Default input</option>
            {inputDevices.map((device) => <option key={device} value={device}>{device}</option>)}
          </select>
        </label>
      </div>
      <div className="inspector-section">
        <div className="inspector-section-title">Channel</div>
        <label className="inspector-field">
          <span>Vol</span>
          <input type="range" min="-24" max="12" step="0.5" value={track.gainDb} onChange={(event) => onChange(track, { gainDb: Number(event.target.value) })} />
          <em>{formatDb(track.gainDb)}</em>
        </label>
        <label className="inspector-field">
          <span>Pan</span>
          <input type="range" min="-1" max="1" step="0.05" value={track.pan} onChange={(event) => onChange(track, { pan: Number(event.target.value) })} />
          <em>{track.pan.toFixed(2)}</em>
        </label>
        <label className="inspector-check">
          <input type="checkbox" checked={!!track.aiGenerated} onChange={(event) => onChange(track, { aiGenerated: event.target.checked })} />
          <span>AI generated stem</span>
        </label>
        <div className="inspector-actions">
          <button type="button" className="danger" onClick={() => onDelete(track)}>
            <Trash2 size={14} />
            <span>Delete track</span>
          </button>
        </div>
      </div>
      <div className="inspector-section">
        <div className="inspector-section-title">Audio</div>
        <dl className="inspector-stats">
          <div><dt>File</dt><dd>{source?.originalName ?? "No source"}</dd></div>
          <div><dt>Start</dt><dd>{formatTime(startSeconds)}</dd></div>
          <div><dt>Length</dt><dd>{formatTime(durationSeconds)}</dd></div>
          <div><dt>Peak</dt><dd>{source ? formatDb(source.analysis.peakDb) : "—"}</dd></div>
          <div><dt>LUFS</dt><dd>{source ? source.analysis.lufsEstimate.toFixed(1) : "—"}</dd></div>
        </dl>
      </div>
    </aside>
  );
}

function TrackRow({
  track,
  selected,
  armed,
  clips,
  selectedClipId,
  selectedRange,
  recording,
  livePeaks,
  playhead,
  duration,
  onSelect,
  onClipSelect,
  onRangeSelect,
  onRangeClear,
  onArm,
  onSeek,
  onChange,
}: {
  track: Track;
  selected: boolean;
  armed: boolean;
  clips: { id: string; name: string; startSeconds: number; sourceSeconds: number; peaks?: number[] }[];
  selectedClipId?: string;
  selectedRange?: { start: number; end: number };
  recording: boolean;
  livePeaks?: number[];
  playhead: number;
  duration: number;
  onSelect: () => void;
  onClipSelect: (clipId: string) => void;
  onRangeSelect: (start: number, end: number) => void;
  onRangeClear: () => void;
  onArm: () => void;
  onSeek: (seconds: number) => void;
  onChange: (patch: Partial<Track>) => void;
}) {
  const dragRef = useRef<{ start: number; moved: boolean } | null>(null);
  const secondsFromPointer = (event: React.PointerEvent<HTMLDivElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const fraction = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    return fraction * duration;
  };
  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    onSelect();
    const seconds = secondsFromPointer(event);
    dragRef.current = { start: seconds, moved: false };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const seconds = secondsFromPointer(event);
    if (Math.abs(seconds - drag.start) > 0.03) {
      drag.moved = true;
      onRangeSelect(drag.start, seconds);
    }
  };
  const handlePointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    event.stopPropagation();
    event.currentTarget.releasePointerCapture(event.pointerId);
    const seconds = secondsFromPointer(event);
    if (drag.moved) {
      onRangeSelect(drag.start, seconds);
      return;
    }
    const clipNode = (event.target as HTMLElement | null)?.closest<HTMLElement>(".wave-clip");
    const clipId = clipNode?.dataset.clipId;
    if (clipId) {
      onClipSelect(clipId);
    } else {
      onRangeClear();
    }
    onSeek(seconds);
  };
  const cursorPct = duration > 0 ? Math.max(0, Math.min(100, (playhead / duration) * 100)) : 0;
  const rangeStartPct = selectedRange && duration > 0 ? Math.max(0, Math.min(100, (selectedRange.start / duration) * 100)) : 0;
  const rangeEndPct = selectedRange && duration > 0 ? Math.max(0, Math.min(100, (selectedRange.end / duration) * 100)) : 0;
  return (
    <div
      className={`track ${selected ? "selected" : ""} ${armed ? "armed" : ""}`}
      onClick={(event) => {
        if ((event.target as HTMLElement | null)?.closest(".wave-wrap")) return;
        onSelect();
      }}
    >
      <div className="track-head" style={{ borderLeftColor: track.color }}>
        <div className="track-compact-name" title={track.name}>{track.name}</div>
        <div className="toggles">
          <button
            className={`record-arm ${armed ? "active" : ""}`}
            title={armed ? "Record enabled. Click to disarm." : "Record enable this track"}
            onClick={(event) => { event.stopPropagation(); onArm(); }}
            aria-pressed={armed}
          >R</button>
          <button className={track.muted ? "active" : ""} onClick={(event) => { event.stopPropagation(); onChange({ muted: !track.muted }); }}>M</button>
          <button className={track.solo ? "active" : ""} onClick={(event) => { event.stopPropagation(); onChange({ solo: !track.solo }); }}>S</button>
        </div>
      </div>
      <div
        className="wave-wrap"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        title="Click to select track and set playhead. Drag to select a time range."
      >
        {clips.map((clip) => {
          const clipLeftPct = duration > 0 ? (clip.startSeconds / duration) * 100 : 0;
          const clipWidthPct = duration > 0 ? (clip.sourceSeconds / duration) * 100 : 100;
          return (
            <div
              key={clip.id}
              data-clip-id={clip.id}
              className={`wave-clip ${selectedClipId === clip.id ? "selected" : ""}`}
              style={{ left: `${clipLeftPct}%`, width: `${clipWidthPct}%`, borderLeftColor: track.color }}
              title={clip.name}
            >
              <Waveform peaks={clip.peaks} color={track.color} />
              <span className="clip-label">{clip.name}</span>
            </div>
          );
        })}
        {selectedRange && Math.abs(rangeEndPct - rangeStartPct) > 0.1 ? (
          <div
            className="range-selection"
            style={{ left: `${Math.min(rangeStartPct, rangeEndPct)}%`, width: `${Math.abs(rangeEndPct - rangeStartPct)}%` }}
          />
        ) : null}
        {recording ? <LiveWaveform peaks={livePeaks ?? []} color={track.color} /> : null}
        {recording ? <div className="recording-overlay">Recording</div> : null}
        <div className="playhead" style={{ left: `${cursorPct}%` }} />
      </div>
    </div>
  );
}

function Waveform({ peaks, color }: { peaks?: number[]; color: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const draw = () => {
      const rect = canvas.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return;
      const scale = window.devicePixelRatio || 1;
      canvas.width = Math.max(1, Math.floor(rect.width * scale));
      canvas.height = Math.max(1, Math.floor(rect.height * scale));
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(scale, 0, 0, scale, 0, 0);
      ctx.clearRect(0, 0, rect.width, rect.height);
      ctx.fillStyle = "#161b20";
      ctx.fillRect(0, 0, rect.width, rect.height);
      if (!peaks?.length) return;
      const step = peaks.length / rect.width;
      ctx.strokeStyle = color;
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (let x = 0; x < rect.width; x++) {
        const sample = Math.min(1, Math.max(0, peaks[Math.min(peaks.length - 1, Math.floor(x * step))] ?? 0));
        if (sample <= 0.0001) continue;
        const y1 = ((1 - sample) * rect.height) / 2;
        const y2 = ((1 + sample) * rect.height) / 2;
        ctx.moveTo(x, y1);
        ctx.lineTo(x, y2);
      }
      ctx.stroke();
    };

    let frame = 0;
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => draw());
    };
    schedule();
    const observer = new ResizeObserver(schedule);
    observer.observe(canvas);
    if (canvas.parentElement) observer.observe(canvas.parentElement);
    window.addEventListener("resize", schedule);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", schedule);
      cancelAnimationFrame(frame);
    };
  }, [peaks, color]);
  return <canvas ref={canvasRef} />;
}

function LiveWaveform({ peaks, color }: { peaks: number[]; color: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const draw = () => {
      const rect = canvas.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return;
      const scale = window.devicePixelRatio || 1;
      canvas.width = Math.max(1, Math.floor(rect.width * scale));
      canvas.height = Math.max(1, Math.floor(rect.height * scale));
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(scale, 0, 0, scale, 0, 0);
      ctx.clearRect(0, 0, rect.width, rect.height);
      ctx.fillStyle = "rgba(168, 61, 61, 0.12)";
      ctx.fillRect(0, 0, rect.width, rect.height);

      const center = rect.height / 2;
      ctx.strokeStyle = "rgba(255, 184, 184, 0.34)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, center);
      ctx.lineTo(rect.width, center);
      ctx.stroke();

      const targetCount = Math.max(24, Math.floor(rect.width));
      const time = performance.now() / 1000;
      const visible = peaks.length
        ? peaks.slice(-targetCount)
        : Array.from({ length: targetCount }, (_, i) => {
            const a = Math.abs(Math.sin(i * 0.19 + time * 5.4));
            const b = Math.abs(Math.sin(i * 0.047 + time * 1.7));
            return 0.04 + a * b * 0.34;
          });
      const step = rect.width / Math.max(1, visible.length - 1);
      ctx.strokeStyle = color;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      visible.forEach((peak, i) => {
        const sample = Math.min(1, Math.max(0, peak));
        const x = i * step;
        const y1 = ((1 - sample) * rect.height) / 2;
        const y2 = ((1 + sample) * rect.height) / 2;
        ctx.moveTo(x, y1);
        ctx.lineTo(x, y2);
      });
      ctx.stroke();

      const sweepX = (time * 160) % rect.width;
      const gradient = ctx.createLinearGradient(sweepX - 24, 0, sweepX + 24, 0);
      gradient.addColorStop(0, "rgba(255, 184, 184, 0)");
      gradient.addColorStop(0.5, "rgba(255, 184, 184, 0.32)");
      gradient.addColorStop(1, "rgba(255, 184, 184, 0)");
      ctx.fillStyle = gradient;
      ctx.fillRect(Math.max(0, sweepX - 24), 0, 48, rect.height);
    };

    let frame = 0;
    const animate = () => {
      draw();
      frame = requestAnimationFrame(animate);
    };
    frame = requestAnimationFrame(animate);
    return () => {
      cancelAnimationFrame(frame);
    };
  }, [peaks, color]);
  return <canvas className="live-waveform" ref={canvasRef} />;
}

function MasterBar({ gainDb, onChange }: { gainDb: number; onChange: (gainDb: number) => void | Promise<void> }) {
  const [local, setLocal] = useState(gainDb);
  useEffect(() => { setLocal(gainDb); }, [gainDb]);
  return (
    <div className="master-bar">
      <strong>Master</strong>
      <input
        type="range"
        min={-24}
        max={12}
        step={0.5}
        value={local}
        onChange={(event) => setLocal(Number(event.target.value))}
        onPointerUp={() => void onChange(local)}
        onKeyUp={() => void onChange(local)}
      />
      <span className="master-value">{formatDb(local)}</span>
      <button type="button" className="master-reset" onClick={() => { setLocal(0); void onChange(0); }} title="Reset to 0 dB">
        0 dB
      </button>
    </div>
  );
}

const AUTO_MIX_STAGES = [
  { id: "raw_session_prep", label: "Raw session prep" },
  { id: "prep_intent", label: "Prep / intent" },
  { id: "static_balance", label: "Static balance" },
  { id: "cleanup_filters", label: "Cleanup HP/LP" },
  { id: "subtractive_eq", label: "Subtractive EQ" },
  { id: "dynamics", label: "Dynamics control" },
  { id: "tonal_enhancement", label: "Tonal enhancement" },
  { id: "depth_space", label: "Depth & space" },
  { id: "section_automation", label: "Section automation" },
  { id: "mix_bus_loudness", label: "Mix bus / loudness" },
];

function AutoMixView({
  stages,
  running,
  disabled,
  onStart
}: {
  stages: { stageId: string; displayName: string; status: string; actionCount: number; warnings: string[]; error?: string; tokens: number; elapsedMs: number; explanation?: string }[];
  running: boolean;
  disabled: boolean;
  onStart: (stageIds: string[]) => void;
}) {
  const [selected, setSelected] = useState<string[]>(AUTO_MIX_STAGES.map((s) => s.id));
  return (
    <div className="auto-mix-view">
      <div className="auto-mix-head">
        <strong>Autonomous mix</strong>
        <span>runs each stage in order; toggle stages off to skip them</span>
      </div>
      <div className="auto-mix-checklist">
        {AUTO_MIX_STAGES.map((s) => {
          const on = selected.includes(s.id);
          return (
            <button
              key={s.id}
              className={`auto-mix-stage-pick ${on ? "on" : "off"}`}
              disabled={running}
              onClick={() => setSelected((cur) => (cur.includes(s.id) ? cur.filter((x) => x !== s.id) : [...cur, s.id]))}
            >{s.label}</button>
          );
        })}
      </div>
      <div className="auto-mix-controls">
        <button
          className="auto-mix-start"
          disabled={disabled || running || selected.length === 0}
          onClick={() => onStart(AUTO_MIX_STAGES.filter((s) => selected.includes(s.id)).map((s) => s.id))}
        >
          {running ? "Running…" : "Start auto-mix"}
        </button>
      </div>
      <div className="auto-mix-stages">
        {stages.length === 0 ? (
          <div className="auto-mix-empty">No stages run yet.</div>
        ) : (
          stages.map((s) => (
            <div key={s.stageId} className={`auto-mix-stage status-${s.status}`}>
              <div className="auto-mix-stage-head">
                <span className="auto-mix-stage-name">{s.displayName}</span>
                <span className="auto-mix-stage-status">{s.status}</span>
                {s.tokens > 0 ? <span className="auto-mix-stage-meta">{s.tokens.toLocaleString()} tok · {(s.elapsedMs / 1000).toFixed(1)}s</span> : null}
              </div>
              {s.explanation ? <div className="auto-mix-stage-explanation">{s.explanation}</div> : null}
              {s.status !== "running" ? (
                <div className="auto-mix-stage-actions">
                  {s.actionCount} action{s.actionCount === 1 ? "" : "s"} applied
                </div>
              ) : null}
              {s.error ? <div className="auto-mix-stage-error">{s.error}</div> : null}
              {s.warnings.length > 0 ? (
                <ul className="auto-mix-stage-warnings">
                  {s.warnings.map((w, i) => <li key={i}>{w}</li>)}
                </ul>
              ) : null}
            </div>
          ))
        )}
      </div>
    </div>
  );
}

function stageLabel(stage: string): string {
  switch (stage) {
    case "starting": return "Starting analysis";
    case "rendering": return "Rendering mix";
    case "connecting": return "Connecting to audio service";
    case "loading_model": return "Loading model (first run)";
    case "analyzing": return "Analyzing structure";
    case "finalizing": return "Finalizing";
    case "done": return "Done";
    case "error": return "Error";
    default: return stage.replace(/_/g, " ");
  }
}

function formatTime(seconds: number) {
  const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  const min = Math.floor(safe / 60);
  const sec = Math.floor(safe % 60).toString().padStart(2, "0");
  return `${min}:${sec}`;
}

function buildAbJudgeFixPrompt(result: AbJudgeResponse) {
  const winner = result.winner === "after" ? "the current MIX" : result.winner === "before" ? "the ORIGINAL/bypass" : "a tie";
  const lines = [
    `Use the latest Gemini A/B judge result as your main brief. It compared ORIGINAL/bypass against the current MIX on ${formatTime(result.clipStart)}-${formatTime(result.clipStart + result.clipDuration)} and chose ${winner} with ${(result.confidence * 100).toFixed(0)}% confidence.`,
    `Summary: ${result.summary}`,
  ];
  if (result.improvements.length > 0) {
    lines.push(`Preserve these improvements: ${result.improvements.join("; ")}`);
  }
  if (result.regressions.length > 0) {
    lines.push(`Fix these regressions first: ${result.regressions.join("; ")}`);
  }
  if (result.mixIssuesAfter.length > 0) {
    lines.push(`Issues heard in the current MIX: ${result.mixIssuesAfter.map((issue) => `${issue.severity} ${issue.category}: ${issue.message}`).join("; ")}`);
  }
  if (result.recommendedNextMoves.length > 0) {
    lines.push(`Recommended next moves: ${result.recommendedNextMoves.join("; ")}`);
  }
  lines.push("Apply only small, reversible mix moves. Do not make the mix louder just to win the A/B. If the A/B result is a tie or the requested fix is not actionable from the available controls, make the smallest useful change or ask for clarification.");
  return lines.join("\n\n");
}

type AssistantTurnMessage = {
  role: "assistant-turn";
  explanation: string;
  skills: string[];
  actions: MixAction[];
  warnings: string[];
  rationale?: string;
  perActionNotes?: string[];
  forwardPatch: JsonPatch[];
  inversePatch: JsonPatch[];
  applied: boolean;
};

type CritiqueMessage = { role: "critique"; critique: MixCritique; skills: string[] };
type AbJudgeMessage = { role: "ab-judge"; result: AbJudgeResponse };

type ChatMessage =
  | { role: "user"; text: string }
  | { role: "assistant"; text: string }
  | { role: "system"; text: string }
  | CritiqueMessage
  | AbJudgeMessage
  | AssistantTurnMessage;

type ActionDescription = {
  target: string;
  kind: string;
  fields: { label: string; value: string }[];
};

type DiffRow = { label: string; before: string; after: string };

function AssistantTurn({
  message,
  session,
  toggleState,
  toggleDisabled,
  onToggle
}: {
  message: AssistantTurnMessage;
  session: MixSession;
  toggleState: "on" | "off" | "locked";
  toggleDisabled: boolean;
  onToggle: () => void;
}) {
  const [diffOpen, setDiffOpen] = useState(false);
  const [whyOpen, setWhyOpen] = useState(false);
  const items = useMemo(
    () =>
      message.actions
        .map((action, i) => ({ desc: describeAction(action, session), note: message.perActionNotes?.[i] }))
        .filter((item): item is { desc: ActionDescription; note: string | undefined } => item.desc !== null),
    [message.actions, message.perActionNotes, session]
  );
  const diffRows = useMemo(() => buildDiffRows(message.forwardPatch, message.inversePatch, session), [message.forwardPatch, message.inversePatch, session]);
  const hasRationale = !!message.rationale && message.rationale.trim().length > 0;

  return (
    <div className="message assistant-turn">
      {message.explanation ? <div className="turn-prose">{message.explanation}</div> : null}
      {message.warnings.length > 0 ? (
        <ul className="turn-warnings">
          {message.warnings.map((warning, i) => <li key={i}>{warning}</li>)}
        </ul>
      ) : null}
      {items.length > 0 ? (
        <ul className="turn-actions">
          {items.map(({ desc, note }, i) => (
            <li key={i}>
              <div className="turn-action-row">
                <span className="turn-action-target">{desc.target}</span>
                <span className="turn-action-kind">{desc.kind}</span>
                {desc.fields.length > 0 ? (
                  <span className="turn-action-fields">
                    {desc.fields.map((field, j) => (
                      <span className="turn-action-field" key={j}>
                        <span className="turn-action-label">{field.label}</span>
                        <span className="turn-action-value">{field.value}</span>
                      </span>
                    ))}
                  </span>
                ) : null}
              </div>
              {note ? <div className="turn-action-note">{note}</div> : null}
            </li>
          ))}
        </ul>
      ) : null}
      <div className="turn-meta">
        {message.skills.length > 0 ? <span className="turn-skills">{message.skills.join(" · ")}</span> : null}
        {hasRationale ? (
          <button type="button" className="turn-diff-toggle" onClick={() => setWhyOpen((open) => !open)}>
            {whyOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            <span>Why</span>
          </button>
        ) : null}
        {diffRows.length > 0 ? (
          <button type="button" className="turn-diff-toggle" onClick={() => setDiffOpen((open) => !open)}>
            {diffOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            <span>{diffRows.length} change{diffRows.length === 1 ? "" : "s"}</span>
          </button>
        ) : null}
        {toggleState !== "locked" ? (
          <button
            type="button"
            className={`turn-toggle ${toggleState}`}
            onClick={onToggle}
            disabled={toggleDisabled}
            title={toggleState === "on" ? "Bypass this turn" : "Re-enable this turn"}
            aria-pressed={toggleState === "on"}
          >
            <Power size={14} />
            <span>{toggleState === "on" ? "On" : "Off"}</span>
          </button>
        ) : null}
      </div>
      {whyOpen && hasRationale ? (
        <div className="turn-rationale">{message.rationale}</div>
      ) : null}
      {diffOpen && diffRows.length > 0 ? (
        <ul className="turn-diff">
          {diffRows.map((row, i) => (
            <li key={i}>
              <span className="turn-diff-label">{row.label}</span>
              <span className="turn-diff-values">
                <span className="turn-diff-before">{row.before}</span>
                <span className="turn-diff-arrow">→</span>
                <span className="turn-diff-after">{row.after}</span>
              </span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function findLatestCritique(messages: ChatMessage[]): MixCritique | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "critique") return m.critique;
  }
  return undefined;
}

function findLatestAbJudge(messages: ChatMessage[]): AbJudgeResponse | undefined {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "ab-judge") return m.result;
  }
  return undefined;
}

function AbJudgeCard({ result, onApply }: { result: AbJudgeResponse; onApply?: () => void }) {
  const winner = result.winner === "after" ? "MIX" : result.winner === "before" ? "ORIG" : "TIE";
  const winnerClass = result.winner === "after" ? "good" : result.winner === "before" ? "poor" : "ok";
  const issues = result.mixIssuesAfter;

  return (
    <div className="message critique ab-judge">
      <div className="crit-head">
        <div className={`crit-score ${winnerClass}`}>
          <span className="crit-score-value">{winner}</span>
          <span className="crit-score-label">{(result.confidence * 100).toFixed(0)}%</span>
        </div>
        <div className="crit-summary">
          <strong>A/B judge</strong>
          <p>{result.summary}</p>
        </div>
      </div>
      <div className="crit-meters">
        <span><span className="crit-meter-label">Model</span> {result.model}</span>
        <span><span className="crit-meter-label">Clip</span> {formatTime(result.clipStart)}-{formatTime(result.clipStart + result.clipDuration)}</span>
        {result.promptTokens || result.outputTokens ? (
          <span><span className="crit-meter-label">Tokens</span> {(result.promptTokens ?? 0).toLocaleString()} in · {(result.outputTokens ?? 0).toLocaleString()} out</span>
        ) : null}
      </div>
      {result.improvements.length > 0 ? (
        <div className="crit-section">
          <div className="crit-section-title">Improvements to preserve</div>
          <ul className="crit-strengths">
            {result.improvements.map((item, i) => <li key={i}>{item}</li>)}
          </ul>
        </div>
      ) : null}
      {result.regressions.length > 0 ? (
        <div className="crit-section">
          <div className="crit-section-title">Regressions to fix</div>
          <ul className="crit-issues">
            {result.regressions.map((item, i) => (
              <li key={i}>
                <span className="crit-sev crit-sev-medium">fix</span>
                <span className="crit-msg">{item}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {issues.length > 0 ? (
        <div className="crit-section">
          <div className="crit-section-title">Current mix issues</div>
          <ul className="crit-issues">
            {issues.map((issue, i) => (
              <li key={i}>
                <span className={`crit-sev crit-sev-${issue.severity}`}>{issue.severity}</span>
                <span className="crit-cat">{issue.category}</span>
                <span className="crit-msg">{issue.message}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {result.recommendedNextMoves.length > 0 ? (
        <div className="crit-section">
          <div className="crit-section-title">Recommended next moves</div>
          <ol className="crit-next">
            {result.recommendedNextMoves.map((step, i) => <li key={i}>{step}</li>)}
          </ol>
        </div>
      ) : null}
      <div className="crit-foot">
        <span className="crit-skills">{result.provider}</span>
        {onApply ? (
          <button type="button" className="crit-apply" onClick={onApply}>
            <Power size={14} />
            <span>Fix A/B</span>
          </button>
        ) : null}
      </div>
    </div>
  );
}

function CritiqueCard({ critique, skills, session, onApply }: { critique: MixCritique; skills: string[]; session: MixSession; onApply?: () => void }) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const ratingClass = (n: number) => (n >= 8 ? "good" : n >= 5 ? "ok" : "poor");
  const sevClass = (s: string) => `crit-sev crit-sev-${s}`;
  const trackName = (id: string) =>
    session.tracks.find((t) => t.id === id)?.name ?? id;

  return (
    <div className="message critique">
      <div className="crit-head">
        <div className={`crit-score ${ratingClass(critique.mixScore)}`}>
          <span className="crit-score-value">{critique.mixScore.toFixed(1)}</span>
          <span className="crit-score-label">/ 10</span>
        </div>
        <div className="crit-summary">
          <strong>Mix critique</strong>
          <p>{critique.summary}</p>
        </div>
      </div>
      <div className="crit-meters">
        <span><span className="crit-meter-label">Headroom</span> {critique.headroomDb.toFixed(1)} dB</span>
        <span><span className="crit-meter-label">Integrated</span> {critique.integratedLufsEstimate.toFixed(1)} LUFS</span>
        <span><span className="crit-meter-label">True peak</span> {critique.truePeakDbEstimate.toFixed(1)} dBTP</span>
      </div>
      {critique.mixIssues.length > 0 ? (
        <div className="crit-section">
          <div className="crit-section-title">Mix-level issues</div>
          <ul className="crit-issues">
            {critique.mixIssues.map((issue, i) => (
              <li key={i}>
                <span className={sevClass(issue.severity)}>{issue.severity}</span>
                <span className="crit-cat">{issue.category}</span>
                <span className="crit-msg">{issue.message}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {critique.perTrack.length > 0 ? (
        <div className="crit-section">
          <div className="crit-section-title">Per-track</div>
          <ul className="crit-tracks">
            {critique.perTrack.map((tc) => {
              const isOpen = expanded[tc.trackId] ?? false;
              const hasDetails = tc.issues.length > 0 || tc.strengths.length > 0;
              return (
                <li key={tc.trackId}>
                  <button
                    type="button"
                    className="crit-track-row"
                    onClick={() => hasDetails && setExpanded((s) => ({ ...s, [tc.trackId]: !isOpen }))}
                    disabled={!hasDetails}
                  >
                    {hasDetails ? (isOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />) : <span style={{ width: 14 }} />}
                    <span className="crit-track-name">{trackName(tc.trackId) || tc.trackName}</span>
                    <span className={`crit-track-rating ${ratingClass(tc.rating)}`}>{tc.rating.toFixed(1)}</span>
                  </button>
                  {isOpen && hasDetails ? (
                    <div className="crit-track-detail">
                      {tc.issues.length > 0 ? (
                        <ul className="crit-issues">
                          {tc.issues.map((issue, i) => (
                            <li key={i}>
                              <span className={sevClass(issue.severity)}>{issue.severity}</span>
                              <span className="crit-cat">{issue.category}</span>
                              <span className="crit-msg">{issue.message}</span>
                            </li>
                          ))}
                        </ul>
                      ) : null}
                      {tc.strengths.length > 0 ? (
                        <ul className="crit-strengths">
                          {tc.strengths.map((s, i) => <li key={i}>{s}</li>)}
                        </ul>
                      ) : null}
                    </div>
                  ) : null}
                </li>
              );
            })}
          </ul>
        </div>
      ) : null}
      {critique.recommendedNextSteps.length > 0 ? (
        <div className="crit-section">
          <div className="crit-section-title">Next steps</div>
          <ol className="crit-next">
            {critique.recommendedNextSteps.map((step, i) => <li key={i}>{step}</li>)}
          </ol>
        </div>
      ) : null}
      <div className="crit-foot">
        {skills.length > 0 ? <span className="crit-skills">{skills.join(" · ")}</span> : <span />}
        {onApply ? (
          <button type="button" className="crit-apply" onClick={onApply}>
            <Power size={14} />
            <span>Apply suggestions</span>
          </button>
        ) : null}
      </div>
    </div>
  );
}

function describeAction(action: MixAction, session: MixSession): ActionDescription | null {
  const trackName = (id: string) => session.tracks.find((track) => track.id === id)?.name ?? "track";
  switch (action.tool) {
    case "rename_track":
      return { target: trackName(action.trackId), kind: "Rename", fields: [{ label: "name", value: action.name }] };
    case "set_track_role":
      return { target: trackName(action.trackId), kind: "Role", fields: [{ label: "role", value: action.role ?? "none" }] };
    case "set_track_gain":
      return { target: trackName(action.trackId), kind: "Gain", fields: [{ label: "level", value: formatDb(action.gainDb) }] };
    case "adjust_track_gain":
      return { target: trackName(action.trackId), kind: "Gain", fields: [{ label: "delta", value: formatDelta(action.deltaDb) }] };
    case "set_track_pan":
      return { target: trackName(action.trackId), kind: "Pan", fields: [{ label: "position", value: formatPan(action.pan) }] };
    case "mute_track":
      return { target: trackName(action.trackId), kind: "Mute", fields: [{ label: "muted", value: action.muted ? "on" : "off" }] };
    case "solo_track":
      return { target: trackName(action.trackId), kind: "Solo", fields: [{ label: "solo", value: action.solo ? "on" : "off" }] };
    case "set_track_ai_generated":
      return { target: trackName(action.trackId), kind: "AI flag", fields: [{ label: "ai", value: action.aiGenerated ? "on" : "off" }] };
    case "set_high_pass":
      return {
        target: trackName(action.trackId),
        kind: "High-pass",
        fields: [
          { label: "freq", value: formatHz(action.frequencyHz) },
          { label: "slope", value: `${action.slopeDbOct} dB/oct` }
        ]
      };
    case "set_low_pass":
      return {
        target: trackName(action.trackId),
        kind: "Low-pass",
        fields: [
          { label: "freq", value: formatHz(action.frequencyHz) },
          { label: "slope", value: `${action.slopeDbOct} dB/oct` }
        ]
      };
    case "set_eq_band":
      return {
        target: trackName(action.trackId),
        kind: `EQ band ${action.band + 1}`,
        fields: [
          { label: "freq", value: formatHz(action.frequencyHz) },
          { label: "gain", value: formatDb(action.gainDb) },
          { label: "Q", value: action.q.toFixed(2) }
        ]
      };
    case "set_compressor":
      return {
        target: trackName(action.trackId),
        kind: "Compressor",
        fields: [
          { label: "threshold", value: formatDb(action.thresholdDb) },
          { label: "ratio", value: `${action.ratio.toFixed(1)}:1` },
          { label: "attack", value: `${action.attackMs.toFixed(0)} ms` },
          { label: "release", value: `${action.releaseMs.toFixed(0)} ms` },
          { label: "knee", value: formatDb(action.kneeDb) },
          { label: "makeup", value: formatDb(action.makeupDb) }
        ]
      };
    case "set_reverb_send":
      return { target: trackName(action.trackId), kind: "Reverb send", fields: [{ label: "level", value: formatDb(action.levelDb) }] };
    case "set_delay_send":
      return { target: trackName(action.trackId), kind: "Delay send", fields: [{ label: "level", value: formatDb(action.levelDb) }] };
    case "set_master_gain":
      return { target: "Master", kind: "Master gain", fields: [{ label: "level", value: formatDb(action.gainDb) }] };
    case "adjust_master_gain":
      return { target: "Master", kind: "Master gain", fields: [{ label: "delta", value: formatDelta(action.deltaDb) }] };
    case "set_processor_param":
      return {
        target: trackName(action.targetId),
        kind: action.processorId,
        fields: [{ label: action.paramId, value: formatNumber(action.value) }]
      };
    case "set_region_gain":
      return { target: trackName(action.trackId), kind: "Region gain", fields: [{ label: "gain", value: formatDb(action.gainDb) }] };
    case "apply_section_automation":
      return {
        target: trackName(action.trackId),
        kind: `Automation`,
        fields: [
          { label: "param", value: action.param },
          { label: "value", value: formatNumber(action.value) }
        ]
      };
    case "create_region":
      return { target: action.name, kind: "Create region", fields: [] };
    case "delete_track":
      return { target: trackName(action.trackId), kind: "Delete track", fields: [] };
    case "undo":
    case "redo":
    case "render_mix":
      return null;
  }
}

function buildDiffRows(forward: JsonPatch[], inverse: JsonPatch[], session: MixSession): DiffRow[] {
  const inverseByPath = new Map(inverse.map((op) => [op.path, op]));
  const rows: DiffRow[] = [];
  for (const op of forward) {
    if (op.op === "remove") {
      rows.push({ label: humanizePath(op.path, session), before: stringifyPatchValue(inverseByPath.get(op.path)?.value), after: "—" });
      continue;
    }
    if (op.op === "add") {
      rows.push({ label: humanizePath(op.path, session), before: "—", after: stringifyPatchValue(op.value) });
      continue;
    }
    rows.push({
      label: humanizePath(op.path, session),
      before: stringifyPatchValue(inverseByPath.get(op.path)?.value),
      after: stringifyPatchValue(op.value)
    });
  }
  return rows;
}

function humanizePath(path: string, session: MixSession): string {
  const parts = path.split("/").filter(Boolean);
  const segments: string[] = [];
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i];
    if (part === "tracks" && parts[i + 1] !== undefined) {
      const idx = Number(parts[i + 1]);
      const trackName = Number.isFinite(idx) ? session.tracks[idx]?.name : undefined;
      segments.push(trackName ?? `track ${idx + 1}`);
      i += 1;
    } else if (part === "chain" && parts[i + 1] === "highPass") {
      segments.push("high-pass");
      i += 1;
    } else if (part === "chain" && parts[i + 1] === "lowPass") {
      segments.push("low-pass");
      i += 1;
    } else if (part === "chain" && parts[i + 1] === "compressor") {
      segments.push("compressor");
      i += 1;
    } else if (part === "chain" && parts[i + 1] === "eq") {
      const bandIdx = Number(parts[i + 2]);
      segments.push(Number.isFinite(bandIdx) ? `EQ band ${bandIdx + 1}` : "EQ");
      i += Number.isFinite(bandIdx) ? 2 : 1;
    } else if (part === "sends" && parts[i + 1]) {
      segments.push(parts[i + 1] === "reverbDb" ? "reverb send" : parts[i + 1] === "delayDb" ? "delay send" : `sends.${parts[i + 1]}`);
      i += 1;
    } else if (part === "automation") {
      segments.push("automation");
    } else if (part === "clips") {
      segments.push("clip");
    } else {
      segments.push(camelToWords(part));
    }
  }
  return segments.join(" · ") || path;
}

function camelToWords(input: string) {
  return input.replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase();
}

function stringifyPatchValue(value: unknown): string {
  if (value === undefined || value === null) return "—";
  if (typeof value === "number") return formatNumber(value);
  if (typeof value === "boolean") return value ? "on" : "off";
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return `[${value.length}]`;
  if (typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const keys = Object.keys(obj);
    if (keys.length === 0) return "{}";
    return keys.slice(0, 3).map((key) => `${key}: ${stringifyPatchValue(obj[key])}`).join(", ") + (keys.length > 3 ? ", …" : "");
  }
  return String(value);
}

function formatNumber(value: number): string {
  if (!Number.isFinite(value)) return "—";
  if (Math.abs(value) >= 100) return value.toFixed(0);
  if (Math.abs(value) >= 10) return value.toFixed(1);
  return value.toFixed(2);
}

function formatDb(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return `${value > 0 ? "+" : ""}${value.toFixed(1)} dB`;
}

function formatDelta(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return `${value >= 0 ? "+" : ""}${value.toFixed(1)} dB`;
}

function formatHz(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return value >= 1000 ? `${(value / 1000).toFixed(1)} kHz` : `${Math.round(value)} Hz`;
}

function formatPan(value: number): string {
  if (!Number.isFinite(value)) return "—";
  if (Math.abs(value) < 0.005) return "C";
  return value < 0 ? `L${Math.round(-value * 100)}` : `R${Math.round(value * 100)}`;
}
