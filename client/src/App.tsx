import { memo, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { AlertCircle, Aperture, Camera, Check, CheckCircle2, ChevronDown, ChevronRight, Circle, Download, FilePlus2, Focus, FolderOpen, GitCompareArrows, Info, Keyboard, Maximize2, MessageSquare, Mic, Palette, Pause, Pencil, Play, Plus, Power, RefreshCw, RotateCcw, RotateCw, Save, Scissors, Settings, Share2, SkipBack, SlidersHorizontal, Sun, Square, Trash2, Upload, Video, X } from "lucide-react";
import type { AbJudgeResponse, AgentColorGrade, AgentVideoEffects, AgentVideoScriptEntry, AssistantResponse, ClipRegion, JsonPatch, MixAction, MixCritique, MixerProfile, MixProject, MixSession, ProfilePreset, Track, VideoCanvas, VideoClipRegion, VideoFilterPreset, VideoLayout } from "../../shared/types";
import { open, save } from "@tauri-apps/plugin-dialog";
import { convertFileSrc } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { WebviewWindow, getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { getVersion } from "@tauri-apps/api/app";
import { api, type ExportAspect, type ExportQuality } from "./api";
import { cssAdjustFilter, vignetteStyle, grainStyle, whiteBalanceStyle } from "./videoAdjust";

const DEFAULT_OLLAMA_URL = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL = "gpt-oss:20b";
const DEFAULT_AGENT_VIDEO_MODEL = "qwen2.5vl:latest";

const IS_MAC = typeof navigator !== "undefined" && /mac/i.test(navigator.platform || navigator.userAgent);
const MOD_KEY = IS_MAC ? "⌘" : "Ctrl";
const ALT_KEY = IS_MAC ? "⌥" : "Alt";
const SHIFT_KEY = IS_MAC ? "⇧" : "Shift";

type Toast = { id: number; kind: "success" | "error" | "info"; text: string };

type ConfirmRequest = {
  title: string;
  message: string;
  confirmLabel: string;
  resolve: (accepted: boolean) => void;
};

type CameraPreviewTrack = {
  id: string;
  name: string;
  color: string;
  deviceId: string;
  deviceLabel: string;
  armed: boolean;
  recording: boolean;
  transportPlaying: boolean;
  activeClip?: CameraPreviewClip;
  defaultLayout: VideoLayout;
};

type CameraPreviewClip = {
  id: string;
  name: string;
  src: string;
  startSeconds: number;
  endSeconds: number;
  localTime: number;
  layout: VideoLayout;
};

type CameraPreviewLayoutEvent = {
  trackId: string;
  clipId: string;
  layout: VideoLayout;
};

type CameraPreviewCanvasEvent = {
  canvas: VideoCanvas;
};

type CameraPreviewPayload = {
  tracks: CameraPreviewTrack[];
  canvas: VideoCanvas;
};

type VideoEditorWindowPayload = {
  sessionId: string;
  trackIds: string[];
  range?: { start: number; end: number };
  playhead: number;
};

type MixerWindowPayload = {
  sessionId: string;
};

type CameraCanvasLayerModel = {
  id: string;
  track: CameraPreviewTrack;
  clip?: CameraPreviewClip;
  layout: VideoLayout;
  live: boolean;
};

type VideoEditHistoryItem = {
  id: string;
  createdAt: string;
  outputPath: string;
  visionModel: string;
  editModel: string;
  intervalSeconds: number;
  instructions: string;
  script: AgentVideoScriptEntry[];
};

type VideoChatMessage = {
  role: "user" | "agent" | "system";
  text: string;
  createdAt: string;
};

type MainVideoEdit = {
  script: AgentVideoScriptEntry[];
  outputPath?: string;
  createdAt?: string;
  rangeStartSeconds?: number;
};

export function App() {
  const initialOllamaUrlRef = useRef(localStorage.getItem("autoMixer.ollamaUrl"));
  const initialOllamaModelRef = useRef(localStorage.getItem("autoMixer.ollamaModel"));
  const initialAgentVideoModelRef = useRef(localStorage.getItem("autoMixer.agentVideoModel"));
  const initialAgentVideoEditModelRef = useRef(localStorage.getItem("autoMixer.agentVideoEditModel"));
  const initialAgentVideoInstructionsRef = useRef(localStorage.getItem("autoMixer.agentVideoInstructions"));
  const initialGeminiKeyRef = useRef(localStorage.getItem("autoMixer.geminiApiKey"));
  const playStartedAtRef = useRef(0);
  const pausedAtRef = useRef(0);
  // When recording was started with a ruler region active, this is the region's right
  // edge (seconds) — playback tick stops the recording automatically on reaching it.
  const recordingPunchOutRef = useRef<number | undefined>(undefined);
  const playbackAnchorRef = useRef<number | undefined>(undefined);
  const togglePlayRef = useRef<() => void | Promise<void>>(() => undefined);
  const videoRecordersRef = useRef<Record<string, { recorder: MediaRecorder; stream: MediaStream; previewElement: HTMLVideoElement; chunks: Blob[]; startSample: number; startedAt: number; mimeType: string; createAudioTrack: boolean; transportOffsetMs: number }>>({});
  const cameraPreviewWindowRef = useRef<WebviewWindow | null>(null);
  const videoEditorWindowRef = useRef<WebviewWindow | null>(null);
  const mixerWindowRef = useRef<WebviewWindow | null>(null);
  const trackLanesRef = useRef<HTMLDivElement>(null);
  const [project, setProject] = useState<MixProject>();
  const [selectedTrackIds, setSelectedTrackIds] = useState<string[]>([]);
  // Inspector "focus" — single track selected by clicking its lane. Decoupled from the
  // group selection (selectedTrackIds), which is for batch operations and S-clicks.
  const [focusedTrackId, setFocusedTrackId] = useState<string | undefined>();
  const [selectedRegionIds, setSelectedRegionIds] = useState<string[]>([]);
  const [selectedClip, setSelectedClip] = useState<{ trackId: string; clipId: string } | undefined>();
  const [selectedClips, setSelectedClips] = useState<{ trackId: string; clipId: string }[]>([]);
  const [clipMenu, setClipMenu] = useState<{ x: number; y: number } | null>(null);
  const [selectedRange, setSelectedRange] = useState<{ trackId?: string; start: number; end: number } | undefined>();
  const [alignmentGuideSeconds, setAlignmentGuideSeconds] = useState<number | undefined>();
  const [draggingTrackIds, setDraggingTrackIds] = useState<string[]>([]);
  const [trackDropTarget, setTrackDropTarget] = useState<{ trackId: string; position: "before" | "after" } | null>(null);
  const [chatText, setChatText] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [startupError, setStartupError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [recording, setRecording] = useState(false);
  const [recordingStarting, setRecordingStarting] = useState(false);
  const [recordingTrackId, setRecordingTrackId] = useState<string | undefined>();
  const [recordingStartSeconds, setRecordingStartSeconds] = useState<number | undefined>();
  // Audio tracks currently armed (R-on). All show the R indicator; the first one
  // is the actual target when transport+record fires (engine records one stream).
  const [armedAudioTrackIds, setArmedAudioTrackIds] = useState<string[]>([]);
  const armedTrackId = armedAudioTrackIds[0];
  const setArmedTrackId = (next: string | undefined | ((current: string | undefined) => string | undefined)) => {
    setArmedAudioTrackIds((current) => {
      const resolved = typeof next === "function" ? (next as (c: string | undefined) => string | undefined)(current[0]) : next;
      return resolved ? [resolved] : [];
    });
  };
  const [inputMonitoring, setInputMonitoring] = useState(false);
  const [inputMonitorStarting, setInputMonitorStarting] = useState(false);
  const [inputMonitorPeaks, setInputMonitorPeaks] = useState<number[]>([]);
  // Latest per-channel peaks from the input monitor / live recording — drives the
  // L/R meters in the inspector so the user can verify the right channel is hot.
  const [inputChannelLevels, setInputChannelLevels] = useState<number[]>([]);
  const [cameraDevices, setCameraDevices] = useState<MediaDeviceInfo[]>([]);
  const [trackCameraDevices, setTrackCameraDevices] = useState<Record<string, string>>({});
  const [trackCameraAudio, setTrackCameraAudio] = useState<Record<string, boolean>>({});
  const [armedVideoTrackIds, setArmedVideoTrackIds] = useState<string[]>([]);
  const [videoRecordingTrackIds, setVideoRecordingTrackIds] = useState<string[]>([]);
  const [videoRecordingStartSeconds, setVideoRecordingStartSeconds] = useState<number | undefined>();
  const [playhead, setPlayhead] = useState(0);
  const [bypass, setBypass] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sessionMenuOpen, setSessionMenuOpen] = useState(false);
  const [addTrackMenuOpen, setAddTrackMenuOpen] = useState(false);
  // When true, clicking on a clip splits it at the click position instead of selecting.
  const [cutToolActive, setCutToolActive] = useState(false);
  // Live cursor position (seconds) shown as a vertical guide while the cut tool is on.
  const [cutCursorSeconds, setCutCursorSeconds] = useState<number | undefined>();
  useEffect(() => { if (!cutToolActive) setCutCursorSeconds(undefined); }, [cutToolActive]);
  const [sessionList, setSessionList] = useState<MixSession[]>([]);
  const [renameDraft, setRenameDraft] = useState<string | null>(null);
  const [newDraft, setNewDraft] = useState<string | null>(null);
  const [analysisProgress, setAnalysisProgress] = useState<{ stage: string; message: string; elapsedSeconds: number } | null>(null);
  const [videoEditorOpen, setVideoEditorOpen] = useState(false);
  const [agentIntervalSeconds, setAgentIntervalSeconds] = useState("2");
  const [agentVideoModel, setAgentVideoModel] = useState(() => initialAgentVideoModelRef.current ?? DEFAULT_AGENT_VIDEO_MODEL);
  const [agentVideoEditModel, setAgentVideoEditModel] = useState(() => initialAgentVideoEditModelRef.current ?? initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL);
  const [agentVideoInstructions, setAgentVideoInstructions] = useState(() => initialAgentVideoInstructionsRef.current ?? "");
  const [agentEditStatus, setAgentEditStatus] = useState<string | null>(null);
  const [agentEditProgress, setAgentEditProgress] = useState<{ stage: string; message: string; current: number; total: number; elapsedSeconds: number } | null>(null);
  const [agentEditScript, setAgentEditScript] = useState<AgentVideoScriptEntry[]>([]);
  const [agentEditContext, setAgentEditContext] = useState<{ trackId: string; clipId: string; sourceTrackIds: string[] } | null>(null);
  const [agentEditLook, setAgentEditLook] = useState<VideoFilterPreset>("none");
  // Custom color grade the agent emits from user instructions ("epic cinematic teal-and-orange",
  // "dreamy faded film"...). Takes priority over the Look chip during renders. Cleared when
  // the user clicks a Look chip so the chip override is honored.
  const [agentColorGrade, setAgentColorGrade] = useState<AgentColorGrade | null>(null);
  // Whole-edit video effects (fade in/out, speed). Persists across Process/re-render so
  // a fade you asked for via Send is still honored when you tweak the Look chip.
  const [agentVideoEffects, setAgentVideoEffects] = useState<AgentVideoEffects | null>(null);
  // Final-export aspect ratio. "original" copies bytes / keeps current canvas size;
  // "square" letterboxes into 1:1; "portrait916" into 9:16 (phone). Used by both
  // Export MP4 buttons (main + editor). Black bars fill the padding.
  const [exportAspect, setExportAspect] = useState<ExportAspect>("original");
  // Export encoder quality. "high" re-renders from camera sources with
  // -preset slow -crf 17 -b:a 320k (visually lossless, much slower). "fast" reuses
  // the preview cache (-preset veryfast). Default high so quality is the default.
  const [exportQuality, setExportQuality] = useState<ExportQuality>("high");
  // The plan the agent produced from the last Send. The user reviews/edits it then
  // clicks Process to actually render. Null = no plan pending.
  const [agentPlan, setAgentPlan] = useState<AgentVideoScriptEntry[] | null>(null);
  const [agentPlanContext, setAgentPlanContext] = useState<{ sourceTrackIds: string[]; startSample?: number; endSample?: number; intervalSeconds: number } | null>(null);
  const [, setMainVideoEdit] = useState<MainVideoEdit>({ script: [] });
  const [videoEditHistory, setVideoEditHistory] = useState<VideoEditHistoryItem[]>([]);
  const [videoChatMessages, setVideoChatMessages] = useState<VideoChatMessage[]>([]);
  const [videoChatDraft, setVideoChatDraft] = useState("");
  const [videoHistoryLoadedSessionId, setVideoHistoryLoadedSessionId] = useState<string | null>(null);
  const [scopedSection, setScopedSection] = useState<{ index: number; start: number; end: number; label: string } | null>(null);
  const [loopSection, setLoopSection] = useState<{ start: number; end: number } | null>(null);
  const [profilePresets, setProfilePresets] = useState<ProfilePreset[]>([]);
  const [reasoning, setReasoning] = useState<{ phase: string; text: string; tokens: { prompt: number; response: number; elapsedMs: number } | null }[]>([]);
  const [streamingTurn, setStreamingTurn] = useState<{ phase: string; text: string } | null>(null);
  // Estimated agent token usage + how full the conversation is before the next
  // auto-compaction (turnsSinceCompaction / compactAfter). Resets on compaction.
  const [agentUsage, setAgentUsage] = useState<{ output: number; thought: number; turns: number; compactAfter: number } | null>(null);
  const [mode, setMode] = useState<"interactive" | "auto" | "video">("interactive");
  const [autoMixStages, setAutoMixStages] = useState<{ stageId: string; displayName: string; status: string; actionCount: number; warnings: string[]; error?: string; tokens: number; elapsedMs: number; explanation?: string }[]>([]);
  const [autoMixRunning, setAutoMixRunning] = useState(false);
  const [ollamaUrl, setOllamaUrl] = useState(() => initialOllamaUrlRef.current ?? DEFAULT_OLLAMA_URL);
  const [ollamaModel, setOllamaModel] = useState(() => initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL);
  const [geminiApiKey, setGeminiApiKey] = useState(() => initialGeminiKeyRef.current ?? "");
  const [modelOptions, setModelOptions] = useState<string[]>(() => [initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL]);
  const [modelStatus, setModelStatus] = useState("Not checked");
  const [modelsLoading, setModelsLoading] = useState(false);
  // Embedded Hermes agent orchestration model (any OpenAI-compatible endpoint).
  const [agentUrl, setAgentUrl] = useState("");
  const [agentModel, setAgentModel] = useState("");
  const [agentStatus, setAgentStatus] = useState("");
  // Video/vision VLM endpoint the video-edit skill calls (e.g. Qwen3-VL on the Spark).
  const [videoUrl, setVideoUrl] = useState("");
  const [videoModelName, setVideoModelName] = useState("");
  const [videoStatus, setVideoStatus] = useState("");
  const [inputDevices, setInputDevices] = useState<string[]>([]);
  const [trackInputDevices, setTrackInputDevices] = useState<Record<string, string>>({});
  // Per-track input gain in dB (applied to the recorded signal before it hits disk).
  const [trackInputGains, setTrackInputGains] = useState<Record<string, number>>({});
  // Per-track input channel indices (0-based) — which channel(s) of the interface to record.
  // For mono tracks: [chIdx]. For stereo: [leftIdx, rightIdx]. Empty/undefined = default 0/0+1.
  const [trackInputChannels, setTrackInputChannels] = useState<Record<string, number[]>>({});
  const [liveRecordingPeaks, setLiveRecordingPeaks] = useState<number[]>([]);
  // Per-track playback levels from the engine (indexed by track slot), used to
  // light up a small meter on each track lane while the transport runs.
  const [trackPeaks, setTrackPeaks] = useState<number[]>([]);
  // Transient notifications (errors, action confirmations). Rendered in a fixed
  // stack so feedback is visible even when the chat log is scrolled away.
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastIdRef = useRef(0);
  const toastTimersRef = useRef<Record<number, number>>({});
  // Pending in-app confirmation dialog (replaces window.confirm).
  const [confirmRequest, setConfirmRequest] = useState<ConfirmRequest | null>(null);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [mixerWindowOpen, setMixerWindowOpen] = useState(false);
  const [videoMonitorOpen, setVideoMonitorOpen] = useState(false);
  const videoMonitorRef = useRef<WebviewWindow | null>(null);
  const busyRef = useRef(false);
  useEffect(() => { busyRef.current = busy; }, [busy]);

  const session = project?.session;
  const chatLogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const node = chatLogRef.current;
    if (!node) return;
    node.scrollTop = node.scrollHeight;
  }, [messages.length, busy]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || !selectedClip) return;
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, button")) return;
      event.preventDefault();
      const step = event.shiftKey ? 0.001 : event.altKey ? 0.1 : 0.01;
      void moveSelectedClip((event.key === "ArrowRight" ? 1 : -1) * step);
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [selectedClip, session]);

  // Global shortcuts: Space (play/pause), Cmd/Ctrl+Z (undo), Shift+Cmd/Ctrl+Z or
  // Cmd/Ctrl+Y (redo), ? (shortcut help), Escape (close overlays).
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const inEditable = !!target?.closest("input, textarea, select, [contenteditable=\"true\"]");
      const mod = event.metaKey || event.ctrlKey;
      if (event.key === "Escape") {
        setShortcutsOpen(false);
        setAddTrackMenuOpen(false);
        setSessionMenuOpen(false);
        return;
      }
      if (inEditable) return;
      if (mod && !event.altKey && (event.key === "z" || event.key === "Z")) {
        event.preventDefault();
        if (busyRef.current) return;
        if (event.shiftKey) void doRedo(); else void doUndo();
        return;
      }
      if (mod && !event.altKey && !event.shiftKey && (event.key === "y" || event.key === "Y")) {
        event.preventDefault();
        if (busyRef.current) return;
        void doRedo();
        return;
      }
      if (event.key === " " && !mod && !event.altKey && !event.shiftKey) {
        if (target?.closest("button, a, [role=\"menu\"]")) return;
        event.preventDefault();
        void togglePlayRef.current();
        return;
      }
      if (event.key === "?" && !mod) {
        event.preventDefault();
        setShortcutsOpen((open) => !open);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [session?.id]);

  // Keyboard handling for the confirmation dialog: Escape cancels, Enter confirms.
  // Capture phase so timeline/selection Escape handlers do not also fire.
  useEffect(() => {
    if (!confirmRequest) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        confirmRequest.resolve(false);
        setConfirmRequest(null);
      } else if (event.key === "Enter") {
        event.preventDefault();
        event.stopPropagation();
        confirmRequest.resolve(true);
        setConfirmRequest(null);
      }
    };
    window.addEventListener("keydown", handleKeyDown, true);
    return () => window.removeEventListener("keydown", handleKeyDown, true);
  }, [confirmRequest]);

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
    let cancelled = false;
    void api.stopRecording(session.id).then((updated) => {
      if (cancelled) return;
      setProject(updated);
      setRecording(false);
      setRecordingStarting(false);
      setRecordingTrackId(undefined);
      setRecordingStartSeconds(undefined);
      pushSystem("Recovered and imported an active recording that was left open.");
    }).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      if (!message.includes("No recording is active")) pushSystem(error);
    });
    return () => {
      cancelled = true;
      void api.stopRecording(session.id).catch(() => undefined);
    };
  }, [session?.id]);

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
    let unlisten: (() => void) | undefined;
    void listen<{ sessionId: string }>("mixer:session-updated", (event) => {
      if (!session || event.payload.sessionId !== session.id) return;
      void api.getSession(session.id).then(setProject).catch(pushSystem);
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [session?.id]);

  useEffect(() => {
    if (!session || !mixerWindowOpen) return;
    void mixerWindowRef.current?.emit("mixer:update", { sessionId: session.id } satisfies MixerWindowPayload).catch(() => undefined);
  }, [session?.id, project, mixerWindowOpen]);

  // Keep the backend (for the video-edit skill) and the monitor window in sync with
  // the user's track selection — so the agent edits, and the monitor shows, only
  // what's selected.
  useEffect(() => {
    if (!session) return;
    void api.setVideoSelection(session.id, selectedTrackIds).catch(() => undefined);
    void videoMonitorRef.current?.emit("video-monitor:selection", { trackIds: selectedTrackIds }).catch(() => undefined);
  }, [selectedTrackIds, session?.id]);

  useEffect(() => {
    void api.listMixerProfiles().then(setProfilePresets).catch(() => undefined);
    void api.inputDevices().then((result) => setInputDevices(result.devices)).catch(() => undefined);
  }, []);

  // Keep the OS window title in sync with the open session, like any
  // document-based app ("thunder — AutoMixer").
  useEffect(() => {
    if (!session?.name) return;
    try {
      void getCurrentWebviewWindow().setTitle(`${session.name} — AutoMixer`).catch(() => undefined);
    } catch {
      // Not running inside Tauri (plain-browser dev) — skip.
    }
  }, [session?.name]);

  const [appVersion, setAppVersion] = useState("");
  useEffect(() => {
    try {
      void getVersion().then(setAppVersion).catch(() => undefined);
    } catch {
      // Not running inside Tauri — leave the version blank.
    }
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
    let unlisten: (() => void) | undefined;
    void api.onAgentVideoProgress((event) => {
      setAgentEditProgress(event);
      setAgentEditStatus(event.message);
      if (event.stage === "done" || event.stage === "error") {
        setTimeout(() => setAgentEditProgress(null), 6000);
      }
    }).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
    // Tick the displayed elapsed every second so the timer keeps moving between the
    // per-window backend events (and during the final encode, which emits none).
    const tick = setInterval(() => {
      setAgentEditProgress((current) =>
        current && current.stage !== "done" && current.stage !== "error"
          ? { ...current, elapsedSeconds: current.elapsedSeconds + 1 }
          : current,
      );
    }, 1000);
    return () => { cancelled = true; unlisten?.(); clearInterval(tick); };
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
    // An external agent (the Hermes control surface) mutated a session. Refresh the
    // project only if it's the one currently open — compared via the functional
    // updater so this empty-deps effect never reads a stale session id.
    reg(api.onSessionExternallyUpdated((event) => {
      setProject((prev) => (prev && prev.session.id === event.sessionId ? event.project : prev));
    }));
    // A video render finished: drop a result chip in the chat (click to open the
    // monitor). We don't auto-open — the monitor is a user-toggled window now.
    reg(api.onVideoRendered((event) => {
      setMessages((items) => [...items, { role: "video", path: event.path, cuts: event.cuts, lookPreset: event.lookPreset }]);
    }));
    // Spacebar pressed in a secondary window (mixer / monitor / video editor) — the
    // main window owns the transport, so toggle play here.
    reg(listen("transport:toggle", () => { void togglePlayRef.current(); }));
    // Live token/context usage from the agent (cumulative for the current session).
    reg(listen<{ outputTokens: number; thoughtTokens: number; turnsSinceCompaction: number; compactAfter: number }>("agent:usage", (event) => {
      setAgentUsage({
        output: event.payload.outputTokens,
        thought: event.payload.thoughtTokens,
        turns: event.payload.turnsSinceCompaction,
        compactAfter: event.payload.compactAfter,
      });
    }));
    // Clicking a tile in the video monitor selects that track here.
    reg(listen<{ trackId: string }>("video-monitor:select", (event) => {
      setSelectedTrackIds([event.payload.trackId]);
    }));
    // The agent set the selection (select_tracks tool) — mirror it in the UI.
    reg(listen<{ sessionId: string; trackIds: string[] }>("selection:set", (event) => {
      setSelectedTrackIds(event.payload.trackIds ?? []);
    }));
    reg(api.onLlmTurnStart(() => {
      setReasoning([]);
      setStreamingTurn(null);
    }));
    reg(api.onLlmTurnEnd(() => setStreamingTurn(null)));
    // The background video-edit job failed — clear the "rendering…" overlay and tell
    // the user (the job runs independently of the chat turn, so this is its closure).
    reg(listen<{ sessionId: string; error: string }>("video:edit-failed", (event) => {
      setAgentEditProgress(null);
      pushSystem(`Video edit failed: ${event.payload.error}`);
    }));
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

  // Live reasoning (the agent's "thinking") and the tool calls it has made this
  // turn — surfaced inline in the chat so long operations never look frozen.
  const liveReasoning = useMemo(
    () => reasoning.filter((r) => r.phase === "think").map((r) => r.text).join("").trim(),
    [reasoning],
  );
  const liveTools = useMemo(
    () => reasoning.filter((r) => r.phase === "tool").map((r) => r.text.trim()).filter(Boolean),
    [reasoning],
  );
  const liveTokens = useMemo(
    () => reasoning.reduce(
      (acc, r) => ({
        prompt: acc.prompt + (r.tokens?.prompt ?? 0),
        response: acc.response + (r.tokens?.response ?? 0),
      }),
      { prompt: 0, response: 0 },
    ),
    [reasoning],
  );

  // Mirror the live reasoning into a ref so handleAssistantResponse (which runs
  // after the async turn resolves, when its closure's `reasoning` is stale) can
  // snapshot the full thinking + tool track and persist it onto the turn card.
  const reasoningRef = useRef(reasoning);
  useEffect(() => { reasoningRef.current = reasoning; }, [reasoning]);

  useEffect(() => {
    localStorage.setItem("autoMixer.ollamaUrl", ollamaUrl);
  }, [ollamaUrl]);

  useEffect(() => {
    localStorage.setItem("autoMixer.ollamaModel", ollamaModel);
    setModelOptions((items) => items.includes(ollamaModel) ? items : [...items, ollamaModel]);
  }, [ollamaModel]);

  useEffect(() => {
    localStorage.setItem("autoMixer.agentVideoModel", agentVideoModel);
    setModelOptions((items) => items.includes(agentVideoModel) ? items : [...items, agentVideoModel]);
  }, [agentVideoModel]);

  useEffect(() => {
    localStorage.setItem("autoMixer.agentVideoEditModel", agentVideoEditModel);
    setModelOptions((items) => items.includes(agentVideoEditModel) ? items : [...items, agentVideoEditModel]);
  }, [agentVideoEditModel]);

  useEffect(() => {
    localStorage.setItem("autoMixer.agentVideoInstructions", agentVideoInstructions);
  }, [agentVideoInstructions]);

  useEffect(() => {
    if (!session) return;
    setVideoHistoryLoadedSessionId(null);
    const raw = localStorage.getItem(`autoMixer.videoEditHistory.${session.id}`);
    if (!raw) {
      setVideoEditHistory([]);
      setVideoChatMessages([]);
      setMainVideoEdit({ script: [] });
      setAgentEditScript([]);
      setAgentEditContext(null);
      setVideoHistoryLoadedSessionId(session.id);
      return;
    }
    try {
      const parsed = JSON.parse(raw) as { history?: VideoEditHistoryItem[]; chat?: VideoChatMessage[]; context?: typeof agentEditContext } | VideoEditHistoryItem[];
      if (Array.isArray(parsed)) {
        setVideoEditHistory(parsed);
        setVideoChatMessages([]);
        setMainVideoEdit(parsed[0] ? { script: parsed[0].script, outputPath: parsed[0].outputPath, createdAt: parsed[0].createdAt } : { script: [] });
        setAgentEditScript(parsed[0]?.script ?? []);
        setAgentEditContext(null);
      } else {
        const history = Array.isArray(parsed.history) ? parsed.history : [];
        setVideoEditHistory(history);
        setVideoChatMessages(Array.isArray(parsed.chat) ? parsed.chat : []);
        setMainVideoEdit(history[0] ? { script: history[0].script, outputPath: history[0].outputPath, createdAt: history[0].createdAt } : { script: [] });
        setAgentEditScript(history[0]?.script ?? []);
        setAgentEditContext(parsed.context ?? null);
      }
    } catch {
      setVideoEditHistory([]);
      setVideoChatMessages([]);
      setMainVideoEdit({ script: [] });
      setAgentEditScript([]);
      setAgentEditContext(null);
    }
    setVideoHistoryLoadedSessionId(session.id);
  }, [session?.id]);

  useEffect(() => {
    if (!session || videoHistoryLoadedSessionId !== session.id) return;
    localStorage.setItem(
      `autoMixer.videoEditHistory.${session.id}`,
      JSON.stringify({ history: videoEditHistory.slice(0, 20), chat: videoChatMessages.slice(-80), context: agentEditContext })
    );
  }, [session?.id, videoHistoryLoadedSessionId, videoEditHistory, videoChatMessages, agentEditContext]);

  useEffect(() => {
    localStorage.setItem("autoMixer.geminiApiKey", geminiApiKey);
  }, [geminiApiKey]);

  useEffect(() => {
    if (!session) return;
    const raw = localStorage.getItem(`autoMixer.trackInputDevices.${session.id}`);
    if (!raw) {
      setTrackInputDevices({});
      setTrackInputGains({});
      setTrackInputChannels({});
      return;
    }
    try {
      const parsed = JSON.parse(raw);
      setTrackInputDevices(parsed.devices ?? parsed);
      setTrackInputGains(parsed.gains ?? {});
      setTrackInputChannels(parsed.channels ?? {});
    } catch {
      setTrackInputDevices({});
      setTrackInputGains({});
      setTrackInputChannels({});
    }
  }, [session?.id]);

  useEffect(() => {
    if (!session) return;
    localStorage.setItem(
      `autoMixer.trackInputDevices.${session.id}`,
      JSON.stringify({ devices: trackInputDevices, gains: trackInputGains, channels: trackInputChannels })
    );
  }, [trackInputDevices, trackInputGains, trackInputChannels, session?.id]);

  useEffect(() => {
    if (!recording) {
      setLiveRecordingPeaks([]);
      setRecordingStarting(false);
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const result = await api.recordingMeters();
        if (!cancelled && result.peaks.length > 0) {
          setRecordingStarting(false);
          setLiveRecordingPeaks((items) => [...items, ...result.peaks].slice(-512));
        }
        if (!cancelled && result.channelPeaks?.length) {
          setInputChannelLevels(result.channelPeaks);
        }
      } catch (error) {
        if (!cancelled) {
          setRecording(false);
          setRecordingStarting(false);
          setRecordingTrackId(undefined);
          setRecordingStartSeconds(undefined);
          setLiveRecordingPeaks([]);
          pushSystem(error);
        }
      }
      if (!cancelled) window.setTimeout(tick, 33);
    };
    void tick();
    return () => { cancelled = true; };
  }, [recording]);

  useEffect(() => {
    let cancelled = false;
    const inputDevice = armedTrackId ? trackInputDevices[armedTrackId] : undefined;

    if (!armedTrackId || recording) {
      setInputMonitoring(false);
      setInputMonitorStarting(false);
      setInputMonitorPeaks([]);
      void api.stopInputMonitor().catch(() => undefined);
      return () => { cancelled = true; };
    }

    setInputMonitoring(true);
    setInputMonitorStarting(true);
    setInputMonitorPeaks([]);

    void api.startInputMonitor(inputDevice).catch((error) => {
      if (!cancelled) {
        setInputMonitoring(false);
        setInputMonitorStarting(false);
        pushSystem(error);
      }
    });

    const tick = async () => {
      try {
        const result = await api.inputMonitorMeters();
        if (!cancelled && result.peaks.length > 0) {
          setInputMonitorStarting(false);
          setInputMonitorPeaks((items) => [...items, ...result.peaks].slice(-512));
        }
        if (!cancelled && result.channelPeaks?.length) {
          setInputChannelLevels(result.channelPeaks);
        }
      } catch (error) {
        if (!cancelled) {
          setInputMonitoring(false);
          setInputMonitorStarting(false);
          setInputMonitorPeaks([]);
          pushSystem(error);
        }
      }
      if (!cancelled) window.setTimeout(tick, 33);
    };

    void tick();
    return () => {
      cancelled = true;
      void api.stopInputMonitor().catch(() => undefined);
    };
  }, [armedTrackId, recording, trackInputDevices[armedTrackId ?? ""]]);

  useEffect(() => {
    void refreshCameraDevices();
    const mediaDevices = navigator.mediaDevices;
    const handleDeviceChange = () => void refreshCameraDevices();
    if (mediaDevices) {
      mediaDevices.addEventListener?.("devicechange", handleDeviceChange);
    }
    return () => {
      mediaDevices?.removeEventListener?.("devicechange", handleDeviceChange);
      Object.values(videoRecordersRef.current).forEach((active) => {
        active.stream.getTracks().forEach((track) => track.stop());
        active.previewElement.remove();
        if (active.recorder.state !== "inactive") active.recorder.stop();
      });
    };
  }, []);

  useEffect(() => {
    if (!session) return;
    const videoTrackIds = new Set(session.tracks.filter((track) => track.kind === "video").map((track) => track.id));
    setArmedVideoTrackIds((ids) => ids.filter((id) => videoTrackIds.has(id)));
  }, [session]);

  useEffect(() => {
    if (!session) return;
    void updateCameraPreviewWindow(buildCameraPreviewTracks());
  }, [session, selectedTrackIds.join("|"), JSON.stringify(trackCameraDevices), cameraDevices.length, armedVideoTrackIds.join("|"), videoRecordingTrackIds.join("|"), playing, Math.round(playhead * 5)]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<CameraPreviewLayoutEvent>("camera-preview:clip-layout", (event) => {
      void updateVideoClipLayout(event.payload);
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [session]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<CameraPreviewCanvasEvent>("camera-preview:canvas", (event) => {
      void updateVideoCanvas(event.payload.canvas);
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [session]);

  useEffect(() => {
    if (!session || !armedTrackId) return;
    if (!session.tracks.some((track) => track.id === armedTrackId)) {
      setArmedTrackId(undefined);
    }
  }, [session, armedTrackId]);

  useEffect(() => {
    if (!session || !selectedClip) return;
    const track = session.tracks.find((item) => item.id === selectedClip.trackId);
    const hasClip = track?.kind === "video"
      ? track.videoClips?.some((clip) => clip.id === selectedClip.clipId)
      : track?.clips.some((clip) => clip.id === selectedClip.clipId);
    if (!track || !hasClip) {
      setSelectedClip(undefined);
    }
  }, [session, selectedClip]);

  useEffect(() => {
    if (!session || !selectedRange || !selectedRange.trackId) return;
    if (!session.tracks.some((track) => track.id === selectedRange.trackId)) {
      setSelectedRange(undefined);
    }
  }, [session, selectedRange]);

  // Drop any multi-selected clips that no longer exist (session switch, deletes, etc.).
  useEffect(() => {
    if (!session) {
      setSelectedClips((prev) => (prev.length === 0 ? prev : []));
      return;
    }
    setSelectedClips((prev) => {
      const next = prev.filter((ref) => {
        const track = session.tracks.find((item) => item.id === ref.trackId);
        if (!track) return false;
        if (ref.clipId === `legacy-${track.id}`) return track.clips.length === 0;
        const clips = track.kind === "video" ? (track.videoClips ?? []) : track.clips;
        return clips.some((clip) => clip.id === ref.clipId);
      });
      return next.length === prev.length ? prev : next;
    });
  }, [session]);

  // Close the clip context menu on Escape.
  useEffect(() => {
    if (!clipMenu) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setClipMenu(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [clipMenu]);

  const duration = useMemo(() => {
    if (!session) return 0;
    const sources = new Map(session.sourceFiles.map((source) => [source.id, source]));
    return Math.max(0, ...session.tracks.map((track) => {
      if (track.kind === "video") {
        return Math.max(0, ...(track.videoClips ?? []).map((clip) => clip.endSample / session.sampleRate));
      }
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
        // Region-bounded recording: stop the moment we hit the region's right edge.
        if (
          recordingPunchOutRef.current !== undefined
          && (recording || videoRecordingTrackIds.length > 0 || Object.keys(videoRecordersRef.current).length > 0)
          && elapsed >= recordingPunchOutRef.current
        ) {
          recordingPunchOutRef.current = undefined;
          void stop();
        }
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
  }, [playing, duration, loopSection, session?.sampleRate, recording, videoRecordingTrackIds]);

  // Live per-track playback meters (30 Hz from the engine). When the transport
  // is stopped we let the bars fall back to zero so nothing stays lit.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.onMeters((event) => { if (!cancelled) setTrackPeaks(event.trackPeaks); })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch(() => undefined);
    return () => { cancelled = true; unlisten?.(); };
  }, []);
  useEffect(() => {
    if (!playing) setTrackPeaks([]);
  }, [playing]);

  useEffect(() => {
    togglePlayRef.current = togglePlay;
  });

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.code !== "Space") return;
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return;
      event.preventDefault();
      void togglePlayRef.current();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const inField = target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable);
      if (event.key === "Escape" && !inField && selectedRange) {
        event.preventDefault();
        setSelectedRange(undefined);
        return;
      }
      // M and R: toggle Mute / Record-arm on the S-group (or the focused track if no group).
      // First hit selects (mutes/arms), second hit deselects (unmutes/disarms).
      if ((event.key === "m" || event.key === "M" || event.key === "r" || event.key === "R") && !inField && session && !busy) {
        const targetIds = selectedTrackIds.length > 0 ? selectedTrackIds : (focusedTrackId ? [focusedTrackId] : []);
        if (targetIds.length === 0) return;
        const targetTracks = session.tracks.filter((t) => targetIds.includes(t.id));
        if (event.key === "m" || event.key === "M") {
          event.preventDefault();
          const allMuted = targetTracks.every((t) => t.muted);
          const nextMuted = !allMuted;
          for (const t of targetTracks) if (t.muted !== nextMuted) void updateTrack(t, { muted: nextMuted });
          return;
        }
        if (event.key === "r" || event.key === "R") {
          event.preventDefault();
          const videoTargets = targetTracks.filter((t) => t.kind === "video").map((t) => t.id);
          if (videoTargets.length > 0) {
            const allArmed = videoTargets.every((id) => armedVideoTrackIds.includes(id));
            setArmedVideoTrackIds((ids) => allArmed
              ? ids.filter((id) => !videoTargets.includes(id))
              : Array.from(new Set([...ids, ...videoTargets])));
          }
          const audioTargets = targetTracks.filter((t) => t.kind !== "video").map((t) => t.id);
          if (audioTargets.length > 0) {
            const allArmed = audioTargets.every((id) => armedAudioTrackIds.includes(id));
            setArmedAudioTrackIds((ids) => allArmed
              ? ids.filter((id) => !audioTargets.includes(id))
              : Array.from(new Set([...ids, ...audioTargets])));
          }
          return;
        }
      }
      if (event.key !== "Delete" && event.key !== "Backspace") return;
      if (inField) return;
      if (!session || busy) return;
      // Prefer deleting a selected clip — a global ruler region is just a marker,
      // not a target for deletion. Only fall through to range-delete when it is a
      // per-track range (has a trackId) AND no clip is selected.
      if (selectedClip) {
        event.preventDefault();
        void deleteClip(selectedClip.trackId, selectedClip.clipId);
        return;
      }
      if (selectedRange && selectedRange.trackId) {
        event.preventDefault();
        void deleteClipRange(selectedRange);
        return;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [session, selectedClip, selectedRange, selectedTrackIds, focusedTrackId, armedTrackId, armedVideoTrackIds, busy, recording, recordingTrackId]);

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
      playbackAnchorRef.current = undefined;
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
      playbackAnchorRef.current = undefined;
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
    const confirmed = await confirmAction({
      title: "Delete session",
      message: `Delete session "${session.name}"? This cannot be undone.`,
      confirmLabel: "Delete session",
    });
    if (!confirmed) return;
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
      playbackAnchorRef.current = undefined;
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
      pushToast("success", `Saved project bundle to ${folder}`);
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
      pushToast("success", `Loaded project bundle "${loaded.session.name}"`);
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
      // Reflect the embedded agent's current orchestration model in the settings.
      void api.getHermesModel().then((m) => {
        setAgentUrl(m.baseUrl);
        setAgentModel(m.model);
      }).catch(() => undefined);
      void api.getVideoModel().then((m) => {
        setVideoUrl(m.baseUrl);
        setVideoModelName(m.model);
      }).catch(() => undefined);
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
      setModelOptions(Array.from(new Set([...models, ollamaModel, agentVideoModel, agentVideoEditModel])));
      setModelStatus(`${models.length} model${models.length === 1 ? "" : "s"} · ${result.provider}`);
    } catch (error) {
      setModelStatus(error instanceof Error ? error.message : "Could not reach the model server");
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

  async function addRecordingTrack(channels: 1 | 2 = 1) {
    if (!session) return;
    try {
      const updated = await api.createRecordingTrack(session.id, channels);
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

  async function addVideoTrack() {
    if (!session) return;
    try {
      const updated = await api.createVideoTrack(session.id);
      setProject(updated);
      const added = updated.session.tracks[updated.session.tracks.length - 1];
      if (added) {
        setSelectedTrackIds([added.id]);
        setArmedTrackId(undefined);
        setArmedVideoTrackIds((ids) => [...ids.filter((id) => id !== added.id), added.id]);
      }
      void refreshCameraDevices();
    } catch (error) {
      pushSystem(error);
    }
  }

  async function refreshCameraDevices(requestPermission = false) {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    try {
      if (requestPermission && navigator.mediaDevices.getUserMedia) {
        try {
          const stream = await navigator.mediaDevices.getUserMedia({ video: true, audio: false });
          stream.getTracks().forEach((track) => track.stop());
        } catch {
          // Keep showing whatever enumerateDevices can return if permission/device warmup fails.
        }
      }
      const devices = await navigator.mediaDevices.enumerateDevices();
      setCameraDevices(devices.filter((device) => device.kind === "videoinput"));
    } catch (error) {
      pushSystem(error);
    }
  }

  function buildCameraPreviewTracks() {
    if (!session) return [];
    const videoSourceById = new Map((session.videoSourceFiles ?? []).map((source) => [source.id, source]));
    return session.tracks
      .filter((track) => track.kind === "video" && selectedTrackIds.includes(track.id))
      .map((track, trackIndex) => {
        const deviceId = trackCameraDevices[track.id] ?? track.cameraDeviceId ?? "";
        const device = cameraDevices.find((item) => item.deviceId === deviceId);
        const clips = (track.videoClips ?? []).flatMap((clip) => {
          const source = videoSourceById.get(clip.videoSourceFileId);
          if (!source) return [];
          return [{
            id: clip.id,
            name: clip.name ?? source.originalName,
            src: source.path,
            startSeconds: clip.startSample / session.sampleRate,
            endSeconds: clip.endSample / session.sampleRate,
            localTime: Math.max(0, (clip.sourceOffsetMs ?? 0) / 1000 + playhead - (clip.startSample / session.sampleRate)),
            layout: normalizeVideoLayout(clip.layout, trackIndex),
          }];
        });
        const activeClip = clips.find((clip) => playhead >= clip.startSeconds && playhead <= clip.endSeconds);
        return {
          id: track.id,
          name: track.name,
          color: track.color,
          deviceId,
          deviceLabel: device?.label || "Default camera",
          armed: armedVideoTrackIds.includes(track.id),
          recording: videoRecordingTrackIds.includes(track.id),
          transportPlaying: playing,
          activeClip,
          defaultLayout: defaultVideoLayout(trackIndex),
        };
      });
  }

  async function openCameraPreviewWindow(tracks: CameraPreviewTrack[], force = true) {
    try {
      let preview = await WebviewWindow.getByLabel("camera-preview");
      if (!preview) {
        preview = new WebviewWindow("camera-preview", {
          url: "/?cameraPreview=1",
          title: "AutoMixer Camera Preview",
          width: 980,
          height: 640,
          minWidth: 520,
          minHeight: 360,
          resizable: true,
          center: false,
        });
        cameraPreviewWindowRef.current = preview;
        preview.once("tauri://created", () => {
          void updateCameraPreviewWindow(tracks);
          window.setTimeout(() => void updateCameraPreviewWindow(tracks), 150);
        });
        preview.once("tauri://error", (event) => {
          pushSystem(`Could not open camera preview window: ${String(event.payload)}`);
        });
      } else {
        cameraPreviewWindowRef.current = preview;
        await updateCameraPreviewWindow(tracks);
      }
      if (force) {
        await preview.show().catch(() => undefined);
        await preview.setFocus().catch(() => undefined);
      }
    } catch (error) {
      pushSystem(error);
    }
  }

  async function updateCameraPreviewWindow(tracks: CameraPreviewTrack[]) {
    const preview = await WebviewWindow.getByLabel("camera-preview");
    if (!preview) return;
    cameraPreviewWindowRef.current = preview;
    await preview.emit("camera-preview:update", {
      tracks,
      canvas: normalizeVideoCanvas(session?.videoCanvas),
    } satisfies CameraPreviewPayload).catch(() => undefined);
  }

  function videoEditorPayload(): VideoEditorWindowPayload | undefined {
    if (!session) return undefined;
    return {
      sessionId: session.id,
      trackIds: selectedTrackIds.filter((id) => session.tracks.some((track) => track.id === id && track.kind === "video")),
      range: selectedRange ? {
        start: Math.max(0, Math.min(selectedRange.start, selectedRange.end)),
        end: Math.max(0, Math.max(selectedRange.start, selectedRange.end)),
      } : undefined,
      playhead,
    };
  }

  async function openVideoEditorWindow() {
    const payload = videoEditorPayload();
    if (!payload) return;
    try {
      const query = new URLSearchParams({ videoEditor: "1", sessionId: payload.sessionId });
      if (payload.trackIds.length > 0) query.set("trackIds", payload.trackIds.join(","));
      if (payload.range) {
        query.set("start", String(payload.range.start));
        query.set("end", String(payload.range.end));
      }
      let editor = await WebviewWindow.getByLabel("video-editor");
      if (!editor) {
        editor = new WebviewWindow("video-editor", {
          url: `/?${query.toString()}`,
          title: "AutoMixer Video Editor",
          width: 1280,
          height: 820,
          minWidth: 900,
          minHeight: 560,
          resizable: true,
          center: true,
        });
        videoEditorWindowRef.current = editor;
        const createdEditor = editor;
        createdEditor.once("tauri://created", () => {
          void createdEditor.emit("video-editor:update", payload);
        });
        createdEditor.once("tauri://error", (event) => {
          pushSystem(`Could not open video editor window: ${String(event.payload)}`);
        });
      } else {
        videoEditorWindowRef.current = editor;
        await editor.emit("video-editor:update", payload).catch(() => undefined);
      }
      await editor.show().catch(() => undefined);
      await editor.setFocus().catch(() => undefined);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function toggleMixerWindow() {
    if (!session) return;
    const payload: MixerWindowPayload = { sessionId: session.id };
    try {
      const query = new URLSearchParams({ mixer: "1", sessionId: session.id });
      let mixer = await WebviewWindow.getByLabel("mixer");
      // If the mixer is already open, a second click hides it (toggle).
      if (mixer && mixerWindowOpen) {
        await mixer.hide().catch(() => undefined);
        setMixerWindowOpen(false);
        return;
      }
      if (!mixer) {
        mixer = new WebviewWindow("mixer", {
          url: `/?${query.toString()}`,
          title: `${session.name} Mixer`,
          width: 1180,
          height: 430,
          minWidth: 760,
          minHeight: 320,
          resizable: true,
          center: true,
        });
        mixerWindowRef.current = mixer;
        setMixerWindowOpen(true);
        const createdMixer = mixer;
        createdMixer.once("tauri://created", () => {
          void createdMixer.emit("mixer:update", payload);
        });
        createdMixer.once("tauri://destroyed", () => {
          if (mixerWindowRef.current === createdMixer) mixerWindowRef.current = null;
          setMixerWindowOpen(false);
        });
        createdMixer.once("tauri://error", (event) => {
          setMixerWindowOpen(false);
          pushSystem(`Could not open mixer window: ${String(event.payload)}`);
        });
      } else {
        mixerWindowRef.current = mixer;
        setMixerWindowOpen(true);
        await mixer.emit("mixer:update", payload).catch(() => undefined);
      }
      await mixer.show().catch(() => undefined);
      await mixer.setTitle(`${session.name} Mixer`).catch(() => undefined);
      await mixer.setFocus().catch(() => undefined);
    } catch (error) {
      pushSystem(error);
    }
  }

  // Toggle the floating multicam Video Monitor: a synced grid of every video clip
  // in the session (camera angles + agent renders), driven by the transport.
  async function toggleVideoMonitor(forceShow = false) {
    if (!session) return;
    try {
      const query = new URLSearchParams({ videoMonitor: "1", sessionId: session.id });
      let monitor = await WebviewWindow.getByLabel("video-monitor");
      if (monitor && videoMonitorOpen && !forceShow) {
        await monitor.hide().catch(() => undefined);
        setVideoMonitorOpen(false);
        return;
      }
      if (!monitor) {
        monitor = new WebviewWindow("video-monitor", {
          url: `/?${query.toString()}`,
          title: "Video Monitor",
          width: 880,
          height: 560,
          minWidth: 360,
          minHeight: 240,
          resizable: true,
        });
        videoMonitorRef.current = monitor;
        setVideoMonitorOpen(true);
        const created = monitor;
        created.once("tauri://created", () => {
          void created.emit("video-monitor:session", { sessionId: session.id });
          void created.emit("video-monitor:selection", { trackIds: selectedTrackIds });
        });
        created.once("tauri://destroyed", () => { if (videoMonitorRef.current === created) videoMonitorRef.current = null; setVideoMonitorOpen(false); });
        created.once("tauri://error", (event) => { setVideoMonitorOpen(false); pushSystem(`Could not open video monitor: ${String(event.payload)}`); });
      } else {
        videoMonitorRef.current = monitor;
        setVideoMonitorOpen(true);
        await monitor.emit("video-monitor:session", { sessionId: session.id }).catch(() => undefined);
        await monitor.emit("video-monitor:selection", { trackIds: selectedTrackIds }).catch(() => undefined);
      }
      await monitor.show().catch(() => undefined);
      await monitor.setFocus().catch(() => undefined);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function startVideoRecordingsAt(startSeconds: number) {
    if (!session) return true;
    if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
      pushSystem("Camera recording is not available in this webview.");
      return false;
    }
    const tracks = armedVideoTrackIds
      .map((trackId) => session.tracks.find((track) => track.id === trackId))
      .filter((track): track is Track => !!track && track.kind === "video");
    if (tracks.length === 0) return true;
    const startSample = Math.round(Math.max(0, startSeconds) * session.sampleRate);
    const startedIds: string[] = [];
    try {
      await updateCameraPreviewWindow([]);
      for (const track of tracks) {
        const deviceId = trackCameraDevices[track.id] ?? track.cameraDeviceId ?? "";
        const includeAudio = !armedTrackId && (trackCameraAudio[track.id] ?? !!track.recordCameraAudio);
        const stream = await navigator.mediaDevices.getUserMedia({
          video: cameraVideoConstraints(deviceId),
          audio: includeAudio,
        });
        const actualDevice = stream.getVideoTracks()[0]?.getSettings().deviceId ?? "";
        if (deviceId && actualDevice && actualDevice !== deviceId) {
          throw new Error(`Camera mismatch for ${track.name}. Selected ${deviceId}, got ${actualDevice}.`);
        }
        const previewElement = await prepareVideoRecordingStream(stream);
        const mimeType = pickVideoMimeType();
        const recorderOptions: MediaRecorderOptions = {
          videoBitsPerSecond: 8_000_000,
          audioBitsPerSecond: 192_000,
        };
        const recorder = mimeType
          ? new MediaRecorder(stream, { ...recorderOptions, mimeType })
          : new MediaRecorder(stream, recorderOptions);
        const chunks: Blob[] = [];
        recorder.ondataavailable = (event) => {
          if (event.data.size > 0) chunks.push(event.data);
        };
        videoRecordersRef.current[track.id] = {
          recorder,
          stream,
          previewElement,
          chunks,
          startSample,
          startedAt: performance.now(),
          mimeType: recorder.mimeType || mimeType || "video/webm",
          createAudioTrack: includeAudio,
          transportOffsetMs: 0,
        };
        recorder.start(250);
        startedIds.push(track.id);
      }
      setVideoRecordingTrackIds(startedIds);
      setVideoRecordingStartSeconds(Math.max(0, startSeconds));
      return true;
    } catch (error) {
      await stopVideoRecordings();
      pushSystem(error);
      return false;
    }
  }

  async function stopVideoRecordings() {
    if (!session) return;
    const entries = Object.entries(videoRecordersRef.current);
    if (entries.length === 0) {
      setVideoRecordingTrackIds([]);
      setVideoRecordingStartSeconds(undefined);
      return;
    }
    videoRecordersRef.current = {};
    for (const [trackId, active] of entries) {
      const blob = await stopMediaRecorder(active.recorder, active.stream, active.chunks, active.mimeType);
      active.previewElement.remove();
      if (blob.size === 0) continue;
      const dataBase64 = await blobToDataUrl(blob);
      const durationMs = Math.max(1, Math.round(performance.now() - active.startedAt));
      const extension = active.mimeType.includes("mp4") ? "mp4" : "webm";
      const updated = await api.saveVideoRecording(
        session.id,
        trackId,
        `video-${Date.now()}.${extension}`,
        active.mimeType,
        active.startSample,
        durationMs,
        dataBase64,
        active.createAudioTrack,
        active.transportOffsetMs
      );
      setProject(updated);
    }
    setVideoRecordingTrackIds([]);
    setVideoRecordingStartSeconds(undefined);
  }

  function markVideoTransportStart() {
    const now = performance.now();
    Object.values(videoRecordersRef.current).forEach((active) => {
      active.transportOffsetMs = Math.max(0, Math.round(now - active.startedAt));
    });
  }

  // Persist a track's input-latency offset (ms). Direct JSON patch — there's no mix
  // action for this field since it only affects placement of *future* recordings.
  async function setTrackInputLatency(track: Track, latencyMs: number) {
    if (!session) return;
    const trackIndex = session.tracks.findIndex((t) => t.id === track.id);
    if (trackIndex < 0) return;
    const value = Math.round(Math.max(-200, Math.min(500, latencyMs)));
    if ((track.inputLatencyMs ?? 0) === value) return;
    try {
      const updated = await api.applyPatch(
        session.id,
        [{ op: "replace", path: `/tracks/${trackIndex}/inputLatencyMs`, value }],
        [{ op: "replace", path: `/tracks/${trackIndex}/inputLatencyMs`, value: track.inputLatencyMs ?? 0 }],
        `Set input latency on ${track.name}`,
      );
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function updateTrack(track: Track, patch: Partial<Track>) {
    if (!session) return;
    // Input latency offset has no mix action — persist it through a direct JSON patch.
    if (patch.inputLatencyMs !== undefined) {
      await setTrackInputLatency(track, patch.inputLatencyMs);
    }
    const actions = [];
    if (patch.name !== undefined && patch.name.trim() && patch.name.trim() !== track.name) {
      actions.push({ tool: "rename_track" as const, trackId: track.id, name: patch.name.trim() });
    }
    if (patch.role !== undefined && patch.role !== track.role) {
      actions.push({ tool: "set_track_role" as const, trackId: track.id, role: patch.role?.trim() || undefined });
    }
    if (patch.gainDb !== undefined) actions.push({ tool: "set_track_gain" as const, trackId: track.id, gainDb: patch.gainDb });
    if (patch.pan !== undefined) actions.push({ tool: "set_track_pan" as const, trackId: track.id, pan: patch.pan });
    if (patch.muted !== undefined) actions.push({ tool: "mute_track" as const, trackId: track.id, muted: patch.muted });
    if (patch.solo !== undefined) actions.push({ tool: "solo_track" as const, trackId: track.id, solo: patch.solo });
    if (patch.aiGenerated !== undefined) actions.push({ tool: "set_track_ai_generated" as const, trackId: track.id, aiGenerated: patch.aiGenerated });
    if (!actions.length) return;
    try {
      const updated = await api.applyActions(session.id, actions, "Manual control change");
      setProject(updated);
    } catch (error) {
      // Surface the failure instead of letting the control silently snap back.
      pushSystem(error instanceof Error ? error.message : String(error));
    }
  }

  function toggleTrackSelection(trackId: string) {
    setSelectedClip(undefined);
    // Keep a global ruler region — only per-track ranges (with trackId) get cleared.
    setSelectedRange((current) => (current && !current.trackId ? current : undefined));
    setSelectedTrackIds((current) => {
      return current.includes(trackId)
        ? current.filter((id) => id !== trackId)
        : [...current, trackId];
    });
  }

  function beginTrackDrag(trackId: string, event: React.DragEvent) {
    if (!session) return;
    const selectedIds = selectedTrackIds.includes(trackId) ? selectedTrackIds : [trackId];
    setSelectedTrackIds(selectedIds);
    setSelectedClip(undefined);
    setSelectedRange((current) => (current && !current.trackId ? current : undefined));
    setDraggingTrackIds(selectedIds);
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", selectedIds.join(","));
  }

  function updateTrackDropTarget(trackId: string, event: React.DragEvent) {
    if (!draggingTrackIds.length || draggingTrackIds.includes(trackId)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const position = event.clientY < rect.top + rect.height / 2 ? "before" : "after";
    setTrackDropTarget({ trackId, position });
  }

  async function dropTracks(trackId: string, event: React.DragEvent) {
    event.preventDefault();
    if (!session || !draggingTrackIds.length) return;
    const position = trackDropTarget?.trackId === trackId ? trackDropTarget.position : "before";
    const movingSet = new Set(draggingTrackIds);
    if (movingSet.has(trackId)) {
      setDraggingTrackIds([]);
      setTrackDropTarget(null);
      return;
    }

    const moving = session.tracks.filter((track) => movingSet.has(track.id));
    const remaining = session.tracks.filter((track) => !movingSet.has(track.id));
    const targetIndex = remaining.findIndex((track) => track.id === trackId);
    if (targetIndex === -1 || moving.length === 0) return;
    const insertIndex = targetIndex + (position === "after" ? 1 : 0);
    const nextTracks = [
      ...remaining.slice(0, insertIndex),
      ...moving,
      ...remaining.slice(insertIndex),
    ];
    if (nextTracks.map((track) => track.id).join("|") === session.tracks.map((track) => track.id).join("|")) {
      setDraggingTrackIds([]);
      setTrackDropTarget(null);
      return;
    }

    setBusy(true);
    try {
      const updated = await api.applyPatch(
        session.id,
        [{ op: "replace", path: "/tracks", value: nextTracks }],
        [{ op: "replace", path: "/tracks", value: session.tracks }],
        moving.length > 1 ? `Reordered ${moving.length} tracks` : `Reordered ${moving[0].name}`
      );
      setProject(updated);
      setSelectedTrackIds(nextTracks.filter((track) => movingSet.has(track.id)).map((track) => track.id));
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
      setDraggingTrackIds([]);
      setTrackDropTarget(null);
    }
  }

  async function deleteSelectedTracks() {
    if (!session || selectedTrackIds.length === 0) return;
    if (recording && recordingTrackId && selectedTrackIds.includes(recordingTrackId)) {
      pushSystem("Stop recording before deleting the recording track.");
      return;
    }
    const tracks = session.tracks.filter((track) => selectedTrackIds.includes(track.id));
    if (tracks.length === 0) return;
    const confirmed = await confirmAction({
      title: tracks.length === 1 ? "Delete track" : "Delete tracks",
      message: tracks.length === 1
        ? `Delete track "${tracks[0].name}"?`
        : `Delete ${tracks.length} selected tracks?`,
    });
    if (!confirmed) return;
    setBusy(true);
    try {
      const updated = await api.applyActions(
        session.id,
        tracks.map((track) => ({ tool: "delete_track" as const, trackId: track.id })),
        tracks.length === 1 ? `Deleted ${tracks[0].name}` : `Deleted ${tracks.length} tracks`
      );
      setProject(updated);
      setSelectedTrackIds([]);
      setSelectedClip(undefined);
      setSelectedRange((current) => (current && !current.trackId ? current : undefined));
      setArmedTrackId((id) => id && tracks.some((track) => track.id === id) ? undefined : id);
      setArmedVideoTrackIds((ids) => ids.filter((id) => !tracks.some((track) => track.id === id)));
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  function handleTrackLaneKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (!session || session.tracks.length === 0) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select, button")) return;
    if (selectedClip && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
      event.preventDefault();
      const step = event.shiftKey ? 0.001 : event.altKey ? 0.1 : 0.01;
      void moveSelectedClip((event.key === "ArrowRight" ? 1 : -1) * step);
      return;
    }
    const lastSelectedId = selectedTrackIds[selectedTrackIds.length - 1];
    const currentIndex = Math.max(0, session.tracks.findIndex((track) => track.id === lastSelectedId));
    const selectIndex = (index: number, extend: boolean) => {
      const bounded = Math.max(0, Math.min(session.tracks.length - 1, index));
      const nextId = session.tracks[bounded].id;
      if (extend && selectedTrackIds.length > 0) {
        const anchorIndex = Math.max(0, session.tracks.findIndex((track) => track.id === selectedTrackIds[0]));
        const [start, end] = [anchorIndex, bounded].sort((a, b) => a - b);
        setSelectedTrackIds(session.tracks.slice(start, end + 1).map((track) => track.id));
      } else {
        setSelectedTrackIds([nextId]);
      }
      setSelectedClip(undefined);
      setSelectedRange((current) => (current && !current.trackId ? current : undefined));
    };

    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      selectIndex(currentIndex + (event.key === "ArrowDown" ? 1 : -1), event.shiftKey);
    } else if (event.key === "Home") {
      event.preventDefault();
      selectIndex(0, event.shiftKey);
    } else if (event.key === "End") {
      event.preventDefault();
      selectIndex(session.tracks.length - 1, event.shiftKey);
    } else if (event.key === "Escape") {
      setSelectedTrackIds([]);
      setSelectedClip(undefined);
      setSelectedRange(undefined);
    } else if (event.key === "Delete" || event.key === "Backspace") {
      event.preventDefault();
      if (selectedRange) {
        void deleteClipRange(selectedRange);
      } else if (selectedClip) {
        void deleteClip(selectedClip.trackId, selectedClip.clipId);
      } else if (event.shiftKey) {
        void deleteSelectedTracks();
      }
    }
  }

  async function moveSelectedClip(deltaSeconds: number) {
    if (!selectedClip) return;
    await moveClip(selectedClip.trackId, selectedClip.clipId, deltaSeconds);
  }

  // Split an audio or video clip at a timeline position into two clips. The right half
  // gets a new id; both halves still reference the same source. Routed through applyPatch
  // for a single undoable step. Handles "legacy" whole-track playback (where the track
  // has no clips yet, just a single source file) by materialising real clips first.
  async function splitClip(trackId: string, clipId: string, splitSeconds: number) {
    if (!session) return;
    const trackIndex = session.tracks.findIndex((t) => t.id === trackId);
    if (trackIndex < 0) return;
    const track = session.tracks[trackIndex];
    const splitSample = Math.round(Math.max(0, splitSeconds) * session.sampleRate);
    const newId = (): string => (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function")
      ? crypto.randomUUID()
      : `clip-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
    if (track.kind === "video") {
      const before = track.videoClips ?? [];
      const clip = before.find((item) => item.id === clipId);
      if (!clip || splitSample <= clip.startSample + 1 || splitSample >= clip.endSample - 1) return;
      const offsetMs = (clip.sourceOffsetMs ?? 0) + Math.round((splitSample - clip.startSample) / session.sampleRate * 1000);
      const next = before.flatMap((item) => item.id === clipId
        ? [
            { ...item, endSample: splitSample },
            { ...item, id: newId(), startSample: splitSample, sourceOffsetMs: offsetMs },
          ]
        : [item]);
      try {
        const updated = await api.applyPatch(
          session.id,
          [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: next }],
          [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: before }],
          `Split ${clip.name ?? "video clip"}`
        );
        setProject(updated);
      } catch (error) {
        pushSystem(error);
      }
      return;
    }

    // Audio split — including the synthetic `legacy-<trackId>` clip used to render a
    // single imported source as one big clip. For legacy, materialise it as a real
    // ClipRegion first, then split that.
    const before = track.clips;
    if (clipId === `legacy-${track.id}`) {
      const source = session.sourceFiles.find((src) => src.id === track.sourceFileId);
      if (!source) { pushSystem("Track has no source file to split."); return; }
      const trackStart = track.startSample;
      const trackEnd = trackStart + (source.durationSamples ?? 0);
      if (splitSample <= trackStart + 1 || splitSample >= trackEnd - 1) return;
      const nextClips = [
        { id: newId(), sourceFileId: source.id, name: track.name, startSample: trackStart, endSample: splitSample, sourceOffsetSample: 0, gainDb: 0 },
        { id: newId(), sourceFileId: source.id, name: track.name, startSample: splitSample, endSample: trackEnd, sourceOffsetSample: splitSample - trackStart, gainDb: 0 },
      ];
      try {
        const updated = await api.applyPatch(
          session.id,
          [{ op: "replace", path: `/tracks/${trackIndex}/clips`, value: nextClips }],
          [{ op: "replace", path: `/tracks/${trackIndex}/clips`, value: before }],
          `Split ${track.name}`
        );
        setProject(updated);
      } catch (error) {
        pushSystem(error);
      }
      return;
    }

    const clip = before.find((item) => item.id === clipId);
    if (!clip || splitSample <= clip.startSample + 1 || splitSample >= clip.endSample - 1) return;
    const sourceOffset = (clip.sourceOffsetSample ?? 0) + (splitSample - clip.startSample);
    const next = before.flatMap((item) => item.id === clipId
      ? [
          { ...item, endSample: splitSample },
          { ...item, id: newId(), startSample: splitSample, sourceOffsetSample: sourceOffset },
        ]
      : [item]);
    try {
      const updated = await api.applyPatch(
        session.id,
        [{ op: "replace", path: `/tracks/${trackIndex}/clips`, value: next }],
        [{ op: "replace", path: `/tracks/${trackIndex}/clips`, value: before }],
        `Split ${clip.name ?? "audio clip"}`
      );
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function moveClip(trackId: string, clipId: string, deltaSeconds: number) {
    if (!session || Math.abs(deltaSeconds) < 0.0005) return;
    const trackIndex = session.tracks.findIndex((track) => track.id === trackId);
    if (trackIndex < 0) return;
    const track = session.tracks[trackIndex];
    const deltaSamples = Math.round(deltaSeconds * session.sampleRate);
    if (deltaSamples === 0) return;
    if (track.kind === "video") {
      const before = track.videoClips ?? [];
      const clip = before.find((item) => item.id === clipId);
      if (!clip) return;
      const durationSamples = clip.endSample - clip.startSample;
      const nextStart = Math.max(0, clip.startSample + deltaSamples);
      const next = before.map((item) => item.id === clipId
        ? { ...item, startSample: nextStart, endSample: nextStart + durationSamples }
        : item
      );
      const updated = await api.applyPatch(
        session.id,
        [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: next }],
        [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: before }],
        `Moved ${clip.name ?? "video clip"}`
      );
      setProject(updated);
      setSelectedClip({ trackId, clipId });
      return;
    }
    const before = track.clips;
    const clip = before.find((item) => item.id === clipId);
    if (!clip) return;
    const durationSamples = clip.endSample - clip.startSample;
    const nextStart = Math.max(0, clip.startSample + deltaSamples);
    const next = before.map((item) => item.id === clipId
      ? { ...item, startSample: nextStart, endSample: nextStart + durationSamples }
      : item
    );
    const updated = await api.applyPatch(
      session.id,
      [{ op: "replace", path: `/tracks/${trackIndex}/clips`, value: next }],
      [{ op: "replace", path: `/tracks/${trackIndex}/clips`, value: before }],
      `Moved ${clip.name ?? "audio clip"}`
    );
    setProject(updated);
    setSelectedClip({ trackId, clipId });
  }

  // Start sample of a clip (or the track itself for the legacy whole-track clip).
  function clipStartSample(ref: { trackId: string; clipId: string }): number | undefined {
    if (!session) return undefined;
    const track = session.tracks.find((item) => item.id === ref.trackId);
    if (!track) return undefined;
    if (ref.clipId === `legacy-${track.id}`) return track.startSample;
    const clips = track.kind === "video" ? (track.videoClips ?? []) : track.clips;
    return clips.find((clip) => clip.id === ref.clipId)?.startSample;
  }

  // Shift every selected clip so its left edge matches the reference clip's start, as one undo step.
  async function alignClipsLeft(refTrackId: string, refClipId: string) {
    if (!session) return;
    const refStart = clipStartSample({ trackId: refTrackId, clipId: refClipId });
    if (refStart === undefined) return;
    const forward: JsonPatch[] = [];
    const inverse: JsonPatch[] = [];
    const arrayEdits = new Map<number, { field: "clips" | "videoClips"; before: (ClipRegion | VideoClipRegion)[]; next: (ClipRegion | VideoClipRegion)[] }>();
    for (const ref of selectedClips) {
      if (ref.trackId === refTrackId && ref.clipId === refClipId) continue;
      const trackIndex = session.tracks.findIndex((item) => item.id === ref.trackId);
      if (trackIndex < 0) continue;
      const track = session.tracks[trackIndex];
      if (ref.clipId === `legacy-${track.id}`) {
        if (track.startSample === refStart) continue;
        forward.push({ op: "replace", path: `/tracks/${trackIndex}/startSample`, value: refStart });
        inverse.push({ op: "replace", path: `/tracks/${trackIndex}/startSample`, value: track.startSample });
        continue;
      }
      const field = track.kind === "video" ? "videoClips" : "clips";
      const before = (track.kind === "video" ? track.videoClips : track.clips) ?? [];
      let edit = arrayEdits.get(trackIndex);
      if (!edit) {
        edit = { field, before, next: [...before] };
        arrayEdits.set(trackIndex, edit);
      }
      edit.next = edit.next.map((clip) => clip.id === ref.clipId
        ? { ...clip, startSample: refStart, endSample: refStart + (clip.endSample - clip.startSample) }
        : clip
      );
    }
    for (const [trackIndex, edit] of arrayEdits) {
      forward.push({ op: "replace", path: `/tracks/${trackIndex}/${edit.field}`, value: edit.next });
      inverse.push({ op: "replace", path: `/tracks/${trackIndex}/${edit.field}`, value: edit.before });
    }
    if (forward.length === 0) return;
    try {
      const updated = await api.applyPatch(session.id, forward, inverse, "Left-aligned clips");
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function updateVideoClipLayout(event: CameraPreviewLayoutEvent) {
    if (!session) return;
    const trackIndex = session.tracks.findIndex((track) => track.id === event.trackId);
    if (trackIndex < 0) return;
    const track = session.tracks[trackIndex];
    if (track.kind !== "video") return;
    const before = track.videoClips ?? [];
    if (!before.some((clip) => clip.id === event.clipId)) return;
    const next = before.map((clip) => clip.id === event.clipId
      ? { ...clip, layout: normalizeVideoLayout(event.layout) }
      : clip
    );
    try {
      const updated = await api.applyPatch(
        session.id,
        [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: next }],
        [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: before }],
        "Updated video canvas"
      );
      setProject(updated);
      setSelectedClip({ trackId: event.trackId, clipId: event.clipId });
      setSelectedTrackIds((ids) => ids.includes(event.trackId) ? ids : [...ids, event.trackId]);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function updateVideoCanvas(canvas: VideoCanvas) {
    if (!session) return;
    const next = normalizeVideoCanvas(canvas);
    const before = normalizeVideoCanvas(session.videoCanvas);
    if (before.width === next.width && before.height === next.height && before.background === next.background) return;
    try {
      const updated = await api.applyPatch(
        session.id,
        [{ op: "replace", path: "/videoCanvas", value: next }],
        [{ op: "replace", path: "/videoCanvas", value: before }],
        "Updated video canvas format"
      );
      setProject(updated);
    } catch (error) {
      pushSystem(error);
    }
  }

  async function deleteTrack(track: Track) {
    if (!session) return;
    if (recording && recordingTrackId === track.id) {
      pushSystem("Stop recording before deleting the recording track.");
      return;
    }
    const confirmed = await confirmAction({ title: "Delete track", message: `Delete track "${track.name}"?` });
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
    const clip = track?.kind === "video"
      ? track.videoClips?.find((item) => item.id === clipId)
      : track?.clips.find((item) => item.id === clipId);
    if (!track || !clip) return;
    if ((recording && recordingTrackId === trackId) || videoRecordingTrackIds.includes(trackId)) {
      pushSystem("Stop recording before deleting clips on the recording track.");
      return;
    }
    const confirmed = await confirmAction({ title: "Delete clip", message: `Delete recording "${clip.name ?? track.name}"?` });
    if (!confirmed) return;
    setBusy(true);
    try {
      let updated: MixProject;
      if (track.kind === "video") {
        const trackIndex = session.tracks.findIndex((item) => item.id === trackId);
        const before = track.videoClips ?? [];
        const after = before.filter((item) => item.id !== clipId);
        updated = await api.applyPatch(
          session.id,
          [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: after }],
          [{ op: "replace", path: `/tracks/${trackIndex}/videoClips`, value: before }],
          "Deleted video clip"
        );
      } else {
        updated = await api.deleteClip(session.id, trackId, clipId);
      }
      setProject(updated);
      setSelectedClip(undefined);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function deleteClipRange(range: { trackId?: string; start: number; end: number }) {
    if (!session || !range.trackId) return;
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
      setSelectedRange(undefined);
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
      // Snapshot this turn's full track — the agent's thinking and the tools it
      // called — so the activity feedback persists in the chat instead of
      // vanishing when the turn ends.
      const snap = reasoningRef.current;
      const turnReasoning = snap.filter((r) => r.phase === "think").map((r) => r.text).join("").trim();
      const turnTools = snap.filter((r) => r.phase === "tool").map((r) => r.text.trim()).filter(Boolean);
      const turnTokens = snap.reduce(
        (acc, r) => ({
          prompt: acc.prompt + (r.tokens?.prompt ?? 0),
          response: acc.response + (r.tokens?.response ?? 0),
          elapsedMs: acc.elapsedMs + (r.tokens?.elapsedMs ?? 0),
        }),
        { prompt: 0, response: 0, elapsedMs: 0 },
      );
      setMessages((items) => [
        ...items,
        {
          role: "assistant-turn",
          explanation: response.explanation,
          reasoning: turnReasoning,
          tools: turnTools,
          tokens: turnTokens,
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

  // Cancel the in-flight agent run (chat turn, auto-mix pipeline, or video agent
  // edit). The backend aborts between model calls; the pending request returns with
  // "Stopped by user." which pushSystem reports as a neutral notice, not an error.
  async function stopAgentRun() {
    try {
      await api.cancelAgent();
      pushToast("info", "Stopping the agent run…");
    } catch (error) {
      pushSystem(error);
    }
  }

  // Clear the chat: wipe the on-screen transcript AND tell the agent to forget the
  // conversation, so a fresh request doesn't inherit stale context (e.g. an earlier
  // "cinema" look the user never asked for again).
  async function clearChat() {
    if (!session) return;
    try {
      await api.clearChat(session.id);
    } catch (error) {
      pushSystem(error);
    }
    setMessages([]);
    setReasoning([]);
    setStreamingTurn(null);
    setAgentUsage(null);
    pushToast("info", "Chat cleared — the agent starts fresh.");
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

  function selectedPlaybackStartSeconds() {
    if (!session) return undefined;
    // Only the ruler region wins, and only when its loop is active (the band is bright).
    // Otherwise play resumes from the current playhead (pausedAtRef in togglePlay).
    if (loopSection && selectedRange
      && Math.abs(loopSection.start - Math.min(selectedRange.start, selectedRange.end)) < 0.01
      && Math.abs(loopSection.end - Math.max(selectedRange.start, selectedRange.end)) < 0.01) {
      return loopSection.start;
    }
    return undefined;
  }

  async function togglePlay() {
    if (!session) return;
    if (playing || recording || Object.keys(videoRecordersRef.current).length > 0) {
      if (recording || videoRecordingTrackIds.length > 0 || Object.keys(videoRecordersRef.current).length > 0) {
        await stop();
        return;
      }
      await api.pause();
      pausedAtRef.current = Math.max(0, (performance.now() - playStartedAtRef.current) / 1000);
      setPlaying(false);
    } else {
      // Region-bounded recording: when armed AND a ruler region exists, start at the
      // region's left edge and remember the right edge so we can auto-stop there.
      const armed = armedVideoTrackIds.length > 0 || !!armedTrackId;
      const regionLo = selectedRange ? Math.min(selectedRange.start, selectedRange.end) : undefined;
      const regionHi = selectedRange ? Math.max(selectedRange.start, selectedRange.end) : undefined;
      const useRegionForRecording = armed && regionLo !== undefined && regionHi !== undefined && regionHi - regionLo > 0.02;
      const start = useRegionForRecording
        ? regionLo!
        : (selectedPlaybackStartSeconds() ?? pausedAtRef.current);
      recordingPunchOutRef.current = useRegionForRecording ? regionHi! : undefined;
      pausedAtRef.current = Math.max(0, Math.min(duration, start));
      setPlayhead(pausedAtRef.current);
      await api.seek(Math.round(pausedAtRef.current * session.sampleRate)).catch(() => undefined);
      if (armedVideoTrackIds.length > 0 && videoRecordingTrackIds.length === 0) {
        const started = await startVideoRecordingsAt(pausedAtRef.current);
        if (!started) return;
      }
      if (armedTrackId && !recording) {
        const started = await startRecordingAt(armedTrackId, pausedAtRef.current);
        if (!started) {
          await stopVideoRecordings();
          return;
        }
      }
      markVideoTransportStart();
      await api.play(session.id);
      playStartedAtRef.current = performance.now() - pausedAtRef.current * 1000;
      setPlaying(true);
    }
  }

  async function startRecordingAt(targetTrackId: string, startSeconds: number) {
    if (!session) return false;
    if (!session.tracks.some((track) => track.id === targetTrackId)) {
      pushSystem("The armed track is no longer available. Arm a track again.");
      setArmedTrackId(undefined);
      return false;
    }
    try {
      const startSample = Math.round(Math.max(0, startSeconds) * session.sampleRate);
      const inputDevice = trackInputDevices[targetTrackId];
      await api.stopRecording(session.id).then(setProject).catch((error) => {
        const message = error instanceof Error ? error.message : String(error);
        if (!message.includes("No recording is active")) throw error;
      });
      await api.stopInputMonitor().catch(() => undefined);
      setInputMonitoring(false);
      setInputMonitorStarting(false);
      setInputMonitorPeaks([]);
      setLiveRecordingPeaks([]);
      setRecording(true);
      setRecordingStarting(true);
      setRecordingTrackId(targetTrackId);
      setRecordingStartSeconds(Math.max(0, startSeconds));
      await api.startRecording(session.id, startSample, targetTrackId, inputDevice, trackInputGains[targetTrackId] ?? 0, trackInputChannels[targetTrackId]);
      return true;
    } catch (error) {
      setRecording(false);
      setRecordingStarting(false);
      setRecordingTrackId(undefined);
      setRecordingStartSeconds(undefined);
      setLiveRecordingPeaks([]);
      pushSystem(error);
      return false;
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
        setRecordingStarting(false);
        setRecordingTrackId(undefined);
        setRecordingStartSeconds(undefined);
      } catch (error) {
        pushSystem(error);
      }
      return;
    }
    try {
      let targetTrackId = armedTrackId;
      if (!targetTrackId && selectedTrackIds.length === 1) {
        targetTrackId = selectedTrackIds[0];
        setArmedTrackId(targetTrackId);
      }
      if (!targetTrackId) {
        pushSystem("Arm one track with R before recording.");
        return;
      }
      if (!session.tracks.some((track) => track.id === targetTrackId)) {
        pushSystem("The armed track is no longer available. Arm a track again.");
        setArmedTrackId(undefined);
        return;
      }
      const startSeconds = selectedPlaybackStartSeconds() ?? playhead;
      await seekTo(Math.max(0, startSeconds));
      await startRecordingAt(targetTrackId, startSeconds);
    } catch (error) {
      setRecording(false);
      setRecordingStarting(false);
      setRecordingTrackId(undefined);
      setLiveRecordingPeaks([]);
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

  function seekTo(seconds: number, options?: { updateAnchor?: boolean }) {
    if (!session) return;
    const safe = Math.max(0, Math.min(duration, seconds));
    if (options?.updateAnchor !== false) {
      playbackAnchorRef.current = safe;
    }
    pausedAtRef.current = safe;
    if (playing) {
      playStartedAtRef.current = performance.now() - safe * 1000;
    }
    setPlayhead(safe);
    void api.seek(Math.round(safe * session.sampleRate)).catch(() => undefined);
  }

  async function stop() {
    if (videoRecordingTrackIds.length > 0 || Object.keys(videoRecordersRef.current).length > 0) {
      try {
        await stopVideoRecordings();
      } catch (error) {
        pushSystem(error);
      }
    }
    if (session) {
      try {
        const updated = await api.stopRecording(session.id);
        setProject(updated);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (!message.includes("No recording is active")) pushSystem(error);
      }
    }
    setRecording(false);
    setRecordingStarting(false);
    setRecordingTrackId(undefined);
    setRecordingStartSeconds(undefined);
    await api.stop();
    const start = selectedPlaybackStartSeconds() ?? 0;
    pausedAtRef.current = Math.max(0, Math.min(duration, start));
    setPlayhead(pausedAtRef.current);
    setPlaying(false);
  }

  // Transport "return to zero" — stop playback and park the playhead at 0:00.
  async function goToStart() {
    await stop();
    pausedAtRef.current = 0;
    setPlayhead(0);
  }

  async function resetSession() {
    if (!session) return;
    const confirmed = await confirmAction({
      title: "Reset session",
      message: "Clear all tracks, history, and chat to start a fresh session?",
      confirmLabel: "Clear session",
    });
    if (!confirmed) return;
    setBusy(true);
    try {
      const updated = await api.resetSession(session.id);
      setProject(updated);
      setSelectedTrackIds([]);
      setSelectedRegionIds([]);
      playbackAnchorRef.current = undefined;
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
      pushToast("success", `Exported WAV to ${outputPath}`);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function renderCurrentVideo() {
    if (!session) return;
    const selectedVideoTrackIds = selectedTrackIds.filter((id) => session.tracks.some((track) => track.id === id && track.kind === "video"));
    if (selectedVideoTrackIds.length === 0) {
      pushSystem("Select one or more video tracks in the canvas before exporting MP4.");
      return;
    }
    const range = selectedRange
      ? {
          startSample: Math.round(Math.max(0, Math.min(selectedRange.start, selectedRange.end)) * session.sampleRate),
          endSample: Math.round(Math.max(0, Math.max(selectedRange.start, selectedRange.end)) * session.sampleRate),
        }
      : undefined;
    const outputPath = await save({
      defaultPath: `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "automix"}.mp4`,
      filters: [{ name: "MP4", extensions: ["mp4"] }]
    });
    if (!outputPath) return;
    setBusy(true);
    try {
      await api.renderVideoMix(session.id, outputPath, range?.startSample, range?.endSample, selectedVideoTrackIds, exportAspect, exportQuality);
      const rangeText = range ? ` (${formatTime(range.startSample / session.sampleRate)}-${formatTime(range.endSample / session.sampleRate)})` : "";
      setMessages((items) => [...items, { role: "system", text: `Rendered ${selectedVideoTrackIds.length} selected video track${selectedVideoTrackIds.length === 1 ? "" : "s"}${rangeText} ${outputPath}` }]);
      pushToast("success", `Exported MP4 to ${outputPath}`);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function renderAutoVideoEdit() {
    if (!session) return;
    const selectedVideoTrackIds = selectedTrackIds.filter((id) => session.tracks.some((track) => track.id === id && track.kind === "video"));
    if (selectedVideoTrackIds.length === 0) {
      pushSystem("Select one or more video tracks before running Auto Video Edit.");
      return;
    }
    const sampleIntervalSeconds = Number(agentIntervalSeconds);
    if (!Number.isFinite(sampleIntervalSeconds) || sampleIntervalSeconds <= 0) {
      pushSystem("Set a positive cut interval (seconds) in the video panel before running Quick Edit.");
      return;
    }
    const range = selectedRange
      ? {
          startSample: Math.round(Math.max(0, Math.min(selectedRange.start, selectedRange.end)) * session.sampleRate),
          endSample: Math.round(Math.max(0, Math.max(selectedRange.start, selectedRange.end)) * session.sampleRate),
        }
      : undefined;
    const outputPath = await save({
      defaultPath: `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "automix"}_auto_edit.mp4`,
      filters: [{ name: "MP4", extensions: ["mp4"] }]
    });
    if (!outputPath) return;
    setBusy(true);
    try {
      await api.renderAutoVideoEdit(session.id, outputPath, range?.startSample, range?.endSample, selectedVideoTrackIds, sampleIntervalSeconds);
      const rangeText = range ? ` (${formatTime(range.startSample / session.sampleRate)}-${formatTime(range.endSample / session.sampleRate)})` : "";
      setMessages((items) => [...items, { role: "system", text: `Auto Video Edit rendered ${selectedVideoTrackIds.length} selected track${selectedVideoTrackIds.length === 1 ? "" : "s"} every ${sampleIntervalSeconds}s${rangeText} ${outputPath}` }]);
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  // Send produces a PLAN (the script of cuts) for the user to review/edit. Nothing
  // is rendered yet — the user clicks Process to render from the (possibly edited) plan.
  async function renderAgentVideoEdit(sampleIntervalSeconds: number, instructionsOverride?: string) {
    if (!session) return;
    const selectedVideoTrackIds = selectedTrackIds.filter((id) => session.tracks.some((track) => track.id === id && track.kind === "video"));
    if (selectedVideoTrackIds.length === 0) {
      pushSystem("Select one or more video tracks before running Agent Video Edit.");
      setAgentEditStatus("Select one or more video tracks first.");
      return;
    }
    if (!Number.isFinite(sampleIntervalSeconds) || sampleIntervalSeconds <= 0) {
      pushSystem("Use a positive number for the Agent Video Edit interval.");
      setAgentEditStatus("Use a positive interval.");
      return;
    }
    const visionModel = agentVideoModel.trim() || DEFAULT_AGENT_VIDEO_MODEL;
    const editModel = agentVideoEditModel.trim() || ollamaModel.trim() || DEFAULT_OLLAMA_MODEL;
    const instructions = (instructionsOverride ?? agentVideoInstructions).trim();
    const range = selectedRange
      ? {
          startSample: Math.round(Math.max(0, Math.min(selectedRange.start, selectedRange.end)) * session.sampleRate),
          endSample: Math.round(Math.max(0, Math.max(selectedRange.start, selectedRange.end)) * session.sampleRate),
        }
      : undefined;
    setBusy(true);
    setAgentEditScript([]);
    setAgentPlan(null);
    setAgentPlanContext(null);
    setAgentEditProgress({ stage: "starting", message: "Planning agent video edit...", current: 0, total: 1, elapsedSeconds: 0 });
    setAgentEditStatus(`Running ${visionModel} + ${editModel} to plan cuts...`);
    try {
      // planOnly = true: backend runs vision + edit models and returns the script,
      // skipping the ffmpeg render step. The user reviews/edits the plan, then clicks
      // Process to render.
      const result = await api.renderAgentVideoEdit(session.id, undefined, range?.startSample, range?.endSample, selectedVideoTrackIds, sampleIntervalSeconds, ollamaUrl, visionModel, editModel, instructions, true);
      const rangeText = range ? ` (${formatTime(range.startSample / session.sampleRate)}-${formatTime(range.endSample / session.sampleRate)})` : "";
      setAgentEditScript(result.script);
      setAgentPlan(result.script);
      setAgentPlanContext({
        sourceTrackIds: selectedVideoTrackIds,
        startSample: range?.startSample,
        endSample: range?.endSample,
        intervalSeconds: sampleIntervalSeconds,
      });
      // Agent inferred a color look from instructions ("cinematic", "moody", etc.).
      // Sync the chip so the Process render applies it, and tell the user about it.
      if (result.lookPreset && result.lookPreset !== "none") {
        setAgentEditLook(result.lookPreset);
      }
      // Custom free-form grade (preferred over the preset on the render side).
      setAgentColorGrade(result.colorGrade ?? null);
      setAgentVideoEffects(result.videoEffects ?? null);
      const planSummary = summarizeAgentPlan(result.script, result.lookPreset, result.colorGrade, result.videoEffects);
      setVideoChatMessages((items) => [...items, {
        role: "agent",
        text: `Planned ${result.script.length} shots${rangeText}.\n\n${planSummary}\n\nReview the plan and click Process to render.`,
        createdAt: new Date().toISOString(),
      }]);
      setAgentEditStatus(`Plan ready — ${result.script.length} shots. Review and click Process.`);
    } catch (error) {
      pushSystem(error);
      setAgentEditStatus(`Error: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  // Change which camera/track a single shot (script window) uses while reviewing the plan.
  function setPlanShot(windowIndex: number, trackIndex: number) {
    setAgentPlan((plan) => plan?.map((entry) => entry.windowIndex === windowIndex
      ? { ...entry, chosenTrackIndex: trackIndex, chosenTrackName: entry.candidates.find((c) => c.trackIndex === trackIndex)?.trackName ?? entry.chosenTrackName }
      : entry) ?? null);
  }

  // Render the approved plan into an mp4 and attach it as the agent-edit track
  // (replacing the current one if it exists). No LLM calls — pure ffmpeg.
  async function processPlan() {
    if (!session || !agentPlan || !agentPlanContext) {
      pushSystem("No plan to process. Send a prompt first.");
      return;
    }
    setBusy(true);
    setAgentEditProgress({ stage: "rendering", message: "Rendering the approved plan...", current: 0, total: 1, elapsedSeconds: 0 });
    setAgentEditStatus("Rendering plan…");
    try {
      const result = await api.renderVideoFromScript(
        session.id,
        agentPlanContext.sourceTrackIds,
        agentPlanContext.startSample,
        agentPlanContext.endSample,
        agentPlan,
        agentEditLook && agentEditLook !== "none" ? agentEditLook : undefined,
        agentColorGrade ?? undefined,
        agentVideoEffects ?? undefined,
      );
      const existingTrack = agentEditContext
        ? session.tracks.find((track) => track.id === agentEditContext.trackId)
        : undefined;
      if (agentEditContext && existingTrack) {
        const withTrack = await api.replaceRenderedVideoTrack(session.id, agentEditContext.trackId, agentEditContext.clipId, result.path, result.durationMs);
        setProject(withTrack);
        setVideoChatMessages((items) => [...items, {
          role: "agent",
          text: `Updated "${existingTrack.name}" with the new edit.`,
          createdAt: new Date().toISOString(),
        }]);
      } else {
        const startSample = agentPlanContext.startSample ?? 0;
        const existingAgentTracks = session.tracks.filter((track) => (track.name ?? "").startsWith("Agent Edit")).length;
        const trackName = `Agent Edit ${existingAgentTracks + 1}`;
        const withTrack = await api.addRenderedVideoTrack(session.id, result.path, trackName, startSample, result.durationMs);
        setProject(withTrack);
        const newTrack = withTrack.session.tracks[withTrack.session.tracks.length - 1];
        if (newTrack) setSelectedTrackIds((ids) => ids.includes(newTrack.id) ? ids : [...ids, newTrack.id]);
        const newClip = newTrack?.videoClips?.[0];
        if (newTrack && newClip) {
          setAgentEditContext({ trackId: newTrack.id, clipId: newClip.id, sourceTrackIds: agentPlanContext.sourceTrackIds });
        }
        setVideoChatMessages((items) => [...items, {
          role: "agent",
          text: `Added "${trackName}" as a new video track.`,
          createdAt: new Date().toISOString(),
        }]);
      }
      setAgentEditStatus("Rendered.");
      setAgentPlan(null);
      setAgentPlanContext(null);
    } catch (error) {
      pushSystem(error);
      setAgentEditStatus(`Error: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  // Re-render the agent edit from the current (edited) script — no vision/edit LLM run.
  // Picks up the current canvas, layouts and selected range, and updates the agent-edit track in place.
  // Optional `lookOverride` applies a global color/grade look to every cut.
  async function rerenderEdit(lookOverride?: VideoFilterPreset) {
    if (!session || !agentEditContext || agentEditScript.length === 0) return;
    const range = selectedRange
      ? {
          startSample: Math.round(Math.max(0, Math.min(selectedRange.start, selectedRange.end)) * session.sampleRate),
          endSample: Math.round(Math.max(0, Math.max(selectedRange.start, selectedRange.end)) * session.sampleRate),
        }
      : undefined;
    const look = lookOverride ?? agentEditLook;
    setBusy(true);
    setAgentEditStatus(`Re-rendering${look && look !== "none" ? ` with ${look} look` : ""} (no agent)...`);
    try {
      const updated = await api.rerenderAgentEdit(
        session.id,
        agentEditContext.trackId,
        agentEditContext.clipId,
        agentEditContext.sourceTrackIds,
        range?.startSample,
        range?.endSample,
        agentEditScript,
        look && look !== "none" ? look : undefined,
        // Clicking a Look chip clears the agent's custom grade (handled at the chip
        // onClick), so we don't pass colorGrade here. Effects persist across chip
        // changes — a requested fade-in should still apply when the user tries Cinema.
        undefined,
        agentVideoEffects ?? undefined,
      );
      setProject(updated);
      setAgentEditStatus("Re-rendered the edit.");
      setVideoChatMessages((items) => [...items, {
        role: "agent",
        text: "Re-rendered the edit from the current script (no agent run).",
        createdAt: new Date().toISOString(),
      }]);
    } catch (error) {
      pushSystem(error);
      setAgentEditStatus(`Error: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  // Save the current agent-edit video to a file the user picks. Just copies the existing
  // rendered mp4 — no re-encode.
  async function downloadAgentEdit() {
    if (!session || !agentEditContext) {
      pushSystem("No agent edit to download. Run the agent first.");
      return;
    }
    const track = session.tracks.find((item) => item.id === agentEditContext.trackId);
    const clip = track?.videoClips?.find((item) => item.id === agentEditContext.clipId);
    const sourceId = clip?.videoSourceFileId;
    const sourcePath = sourceId ? session.videoSourceFiles?.find((item) => item.id === sourceId)?.path : undefined;
    if (!sourcePath) {
      pushSystem("Could not find the rendered file for the current agent edit.");
      return;
    }
    const outputPath = await save({
      defaultPath: `${(track?.name ?? "agent_edit").replace(/[^a-z0-9-]+/gi, "_")}.mp4`,
      filters: [{ name: "MP4", extensions: ["mp4"] }],
    });
    if (!outputPath) return;
    try {
      // High-quality path: if we still have the agent edit's script + context, re-render
      // from the original camera sources with -preset slow -crf 17 -b:a 320k, then route
      // the new high-q file through the aspect transcoder. Otherwise fall back to copying
      // the preview cache (can't make a lossy cache lossless after the fact).
      const canHQRender = exportQuality === "high"
        && agentEditContext
        && agentEditScript.length > 0
        && agentPlanContext;
      let intermediateSource = sourcePath;
      if (canHQRender && agentPlanContext) {
        const hq = await api.renderVideoFromScript(
          session.id,
          agentPlanContext.sourceTrackIds,
          agentPlanContext.startSample,
          agentPlanContext.endSample,
          agentEditScript,
          agentEditLook && agentEditLook !== "none" ? agentEditLook : undefined,
          agentColorGrade ?? undefined,
          agentVideoEffects ?? undefined,
          "high",
        );
        intermediateSource = hq.path;
      }
      const result = await api.exportRenderedVideo(intermediateSource, outputPath, exportAspect, exportQuality);
      pushSystem(`Saved ${result.path}${canHQRender ? " (high quality, re-rendered from sources)" : exportQuality === "high" ? " (high quality, transcoded)" : " (fast copy)"}`);
    } catch (error) {
      pushSystem(error);
    }
  }

  // Clear manual processing (crop, color, PiP position/size) from every video clip but
  // KEEP rotation so an upside-down camera stays right-side-up. Undoable via Cmd+Z.
  async function resetVideoProcessing() {
    if (!session) return;
    const forward: JsonPatch[] = [];
    const inverse: JsonPatch[] = [];
    session.tracks.forEach((track, index) => {
      if (track.kind !== "video") return;
      const before = track.videoClips ?? [];
      if (!before.some((clip) => clip.layout)) return;
      const next = before.map((clip) => {
        if (!clip.layout) return clip;
        const rotation = clip.layout.rotation ?? 0;
        if (!rotation) {
          // Nothing worth preserving — drop the layout entirely.
          const { layout: _drop, ...rest } = clip;
          return rest;
        }
        // Default full-frame layout, but carry over the rotation correction.
        return {
          ...clip,
          layout: {
            x: 0, y: 0, width: 100, height: 100,
            cropTop: 0, cropRight: 0, cropBottom: 0, cropLeft: 0,
            opacity: 1, rotation, zIndex: 0,
            brightness: 1, contrast: 1, saturation: 1, blur: 0,
            preset: "none" as const,
          },
        };
      });
      forward.push({ op: "replace", path: `/tracks/${index}/videoClips`, value: next });
      inverse.push({ op: "replace", path: `/tracks/${index}/videoClips`, value: before });
    });
    if (forward.length === 0) {
      pushSystem("No manual video processing to reset.");
      return;
    }
    setBusy(true);
    try {
      const updated = await api.applyPatch(session.id, forward, inverse, "Reset video processing");
      setProject(updated);
      pushSystem("Reset manual video processing (crop, color, PiP position). Rotation kept. Undo to restore.");
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  function sendVideoEditorChat() {
    const text = videoChatDraft.trim();
    if (!text) return;
    const now = new Date().toISOString();
    setVideoChatMessages((items) => [...items, { role: "user", text, createdAt: now }]);
    setAgentVideoInstructions((current) => current.trim() ? `${current.trim()}\n${text}` : text);
    setVideoChatDraft("");
  }

  // Pull the focused video clip out of selectedClip — only when it points at a video
  // track. Audio clips and ruler-range selections don't qualify; in those cases the
  // chat falls through to the multicam agent flow.
  const focusedVideoClip = (() => {
    if (!session || !selectedClip) return null;
    const track = session.tracks.find((t) => t.id === selectedClip.trackId);
    if (!track || track.kind !== "video") return null;
    const clip = (track.videoClips ?? []).find((c) => c.id === selectedClip.clipId);
    if (!clip) return null;
    return { trackId: track.id, clipId: clip.id, name: clip.name ?? track.name };
  })();

  async function runAgentVideoEditFromDraft() {
    const draft = videoChatDraft.trim();
    const currentInstructions = agentVideoInstructions.trim();
    const instructions = draft ? (currentInstructions ? `${currentInstructions}\n${draft}` : draft) : currentInstructions;
    if (draft) {
      setAgentVideoInstructions(instructions);
      setVideoChatMessages((items) => [...items, { role: "user", text: draft, createdAt: new Date().toISOString() }]);
      setVideoChatDraft("");
    }
    // Focused-clip mode: a single video clip is selected → apply look/effects ONLY to
    // that clip's source range. Bypasses the multicam agent (no cuts, no frame
    // selection). One vision call + keyword fallback. Renders in seconds.
    if (focusedVideoClip) {
      await applyEffectsToFocusedClip(draft || instructions);
      return;
    }
    await renderAgentVideoEdit(Number(agentIntervalSeconds), instructions);
  }

  // Restore the focused clip to the pristine recording stored on the first effects
  // render. No LLM, no ffmpeg — just swaps the clip's source-id/offset back.
  async function revertFocusedClip() {
    if (!session || !focusedVideoClip) return;
    setBusy(true);
    setAgentEditStatus(`Reverting "${focusedVideoClip.name}" to original…`);
    try {
      const updated = await api.revertClipVideo(session.id, focusedVideoClip.trackId, focusedVideoClip.clipId);
      setProject(updated);
      setAgentColorGrade(null);
      setAgentVideoEffects(null);
      setVideoChatMessages((items) => [...items, {
        role: "agent",
        text: `Reverted "${focusedVideoClip.name}" to the original recording.`,
        createdAt: new Date().toISOString(),
      }]);
      setAgentEditStatus(`Reverted "${focusedVideoClip.name}".`);
    } catch (error) {
      pushSystem(error);
      setAgentEditStatus(`Error: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  async function applyEffectsToFocusedClip(text: string) {
    if (!session || !focusedVideoClip) return;
    if (!text.trim()) {
      pushSystem("Type what you want for the focused clip (e.g. \"cinematic, fade in 1s\").");
      return;
    }
    setBusy(true);
    setAgentEditStatus(`Editing "${focusedVideoClip.name}"…`);
    try {
      const ollamaUrl = localStorage.getItem("autoMixer.ollamaUrl") ?? undefined;
      const visionModel = localStorage.getItem("autoMixer.agentVideoModel") ?? undefined;
      const result = await api.applyClipEffects(
        session.id,
        focusedVideoClip.trackId,
        focusedVideoClip.clipId,
        text,
        ollamaUrl ?? undefined,
        visionModel ?? undefined,
      );
      setProject(result.project);
      if (result.lookPreset && result.lookPreset !== "none") setAgentEditLook(result.lookPreset);
      setAgentColorGrade(result.colorGrade ?? null);
      setAgentVideoEffects(result.videoEffects ?? null);
      const summary = summarizeClipEditResult(focusedVideoClip.name, result.lookPreset, result.colorGrade, result.videoEffects, result.sourceSummary);
      setVideoChatMessages((items) => [...items, {
        role: "agent",
        text: summary,
        createdAt: new Date().toISOString(),
      }]);
      setAgentEditStatus(`Updated "${focusedVideoClip.name}".`);
    } catch (error) {
      pushSystem(error);
      setAgentEditStatus(`Error: ${error instanceof Error ? error.message : String(error)}`);
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
    // A user-initiated stop is expected, not an error.
    if (/stopped by user/i.test(text)) pushToast("info", "Agent run stopped.");
    else pushToast("error", text);
  }

  function pushToast(kind: Toast["kind"], text: string) {
    const id = ++toastIdRef.current;
    // Keep the stack short — older toasts roll off instead of piling up.
    setToasts((items) => [...items.slice(-3), { id, kind, text }]);
    toastTimersRef.current[id] = window.setTimeout(() => dismissToast(id), kind === "error" ? 8000 : 4500);
  }

  function dismissToast(id: number) {
    window.clearTimeout(toastTimersRef.current[id]);
    delete toastTimersRef.current[id];
    setToasts((items) => items.filter((item) => item.id !== id));
  }

  // In-app replacement for window.confirm — resolves true when the user confirms.
  function confirmAction(options: { title: string; message: string; confirmLabel?: string }): Promise<boolean> {
    return new Promise((resolve) => {
      setConfirmRequest({
        title: options.title,
        message: options.message,
        confirmLabel: options.confirmLabel ?? "Delete",
        resolve,
      });
    });
  }

  function resolveConfirm(accepted: boolean) {
    confirmRequest?.resolve(accepted);
    setConfirmRequest(null);
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

  const selectedVideoTrackIdsForEditor = selectedTrackIds.filter((id) => session.tracks.some((track) => track.id === id && track.kind === "video"));
  const selectedVideoTracksForEditor = session.tracks.filter((track) => track.kind === "video" && selectedVideoTrackIdsForEditor.includes(track.id));
  const audioTrackCountForEditor = session.tracks.filter((track) => track.kind !== "video").length;
  const videoEditorScript = agentEditScript.length > 0 ? agentEditScript : videoEditHistory[0]?.script ?? [];
  const rangeStartSeconds = selectedRange ? Math.max(0, Math.min(selectedRange.start, selectedRange.end)) : 0;
  const explicitRangeEndSeconds = selectedRange ? Math.max(0, Math.max(selectedRange.start, selectedRange.end)) : 0;
  const scriptEndSeconds = Math.max(0, ...videoEditorScript.map((entry) => entry.endSeconds));
  const clipEndSeconds = Math.max(0, ...selectedVideoTracksForEditor.flatMap((track) => (track.videoClips ?? []).map((clip) => clip.endSample / session.sampleRate)));
  const videoEditorStartSeconds = selectedRange ? rangeStartSeconds : 0;
  const videoEditorEndSeconds = Math.max(selectedRange ? explicitRangeEndSeconds : duration, scriptEndSeconds, clipEndSeconds, 1);
  const videoEditorSpanSeconds = Math.max(1, videoEditorEndSeconds - videoEditorStartSeconds);
  const videoCanvas = normalizeVideoCanvas(session.videoCanvas);
  const editorVideoSourceById = new Map((session.videoSourceFiles ?? []).map((source) => [source.id, source]));
  const assistantVideoPreviewClips = selectedVideoTracksForEditor.flatMap((track) => (track.videoClips ?? []).flatMap((clip) => {
    const source = editorVideoSourceById.get(clip.videoSourceFileId);
    if (!source) return [];
    const startSeconds = clip.startSample / session.sampleRate;
    const endSeconds = clip.endSample / session.sampleRate;
    const sourceOffsetSeconds = Math.max(0, (clip.sourceOffsetMs ?? 0) / 1000);
    return [{
      id: clip.id,
      name: clip.name ?? source.originalName ?? track.name,
      trackName: track.name,
      color: track.color,
      src: source.path,
      startSeconds,
      endSeconds,
      localTime: Math.max(0, sourceOffsetSeconds + (playhead >= startSeconds && playhead <= endSeconds ? playhead - startSeconds : 0)),
    }];
  }));
  const assistantVideoPreviewClip = assistantVideoPreviewClips.find((clip) => playhead >= clip.startSeconds && playhead <= clip.endSeconds) ?? assistantVideoPreviewClips[0];

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
          </div>
          <div className="transport-center">
            <div className="topbar-group" role="group" aria-label="Transport">
              <button onClick={() => void goToStart()} title="Jump to start"><SkipBack size={15} /></button>
              <button
                className={`transport-play ${playing ? "playing" : ""}`}
                onClick={() => void togglePlay()}
                title={playing ? "Pause (Space)" : "Play (Space)"}
              >
                {playing ? <Pause size={18} /> : <Play size={18} />}
              </button>
              <button onClick={() => void stop()} title="Stop"><Square size={15} /></button>
            </div>
            <div className="lcd">
              <div className="lcd-main">
                <span className="lcd-time">{formatLcdTime(playhead)}</span>
                <span className="lcd-total">/ {formatTime(duration)}</span>
                {session.bpm ? <span className="lcd-bars">{formatBars(playhead, session.bpm)}</span> : null}
              </div>
              <div className="lcd-sub">
                <span>{session.tracks.length} TRK</span>
                <span>{Math.round(session.sampleRate / 100) / 10} kHz</span>
                {session.bpm ? <span>{Math.round(session.bpm)} BPM</span> : null}
                <TransportMeter />
              </div>
            </div>
          </div>
          <div className="transport">
            <div className="topbar-group" role="group" aria-label="Editing tools">
              <button
                onClick={() => setAddTrackMenuOpen(true)}
                disabled={busy}
                title="Add a track"
                aria-haspopup="dialog"
              >
                <Plus size={18} />
              </button>
              <button
                className={cutToolActive ? "active" : ""}
                onClick={() => setCutToolActive((on) => !on)}
                disabled={busy}
                title={cutToolActive ? "Cut tool active. Click again to disable." : "Cut tool — click a clip to split it"}
                aria-pressed={cutToolActive}
              >
                <Scissors size={18} />
              </button>
              <button onClick={doUndo} disabled={busy} title={`Undo (${MOD_KEY}Z)`}><RotateCcw size={18} /></button>
              <button onClick={doRedo} disabled={busy} title={`Redo (${SHIFT_KEY}${MOD_KEY}Z)`}><RotateCw size={18} /></button>
            </div>
            <span className="topbar-sep" aria-hidden="true" />
            <div className="topbar-group" role="group" aria-label="Monitoring">
              <button
                className={`bypass-toggle ${bypass ? "bypass-active" : ""}`}
                onClick={() => void toggleBypass()}
                title={bypass ? "Hearing ORIGINAL (no processing). Click for the mix." : "Hearing the MIX (with all processing). Click for the original."}
                aria-pressed={bypass}
              >
                <GitCompareArrows size={16} />
                <span className="bypass-label">{bypass ? "ORIG" : "MIX"}</span>
              </button>
              <button
                className={`ai-bulk ${session.tracks.length > 0 && session.tracks.every((t) => t.aiGenerated) ? "active" : ""}`}
                onClick={() => void toggleAllAi()}
                disabled={busy || session.tracks.length === 0}
                title="Toggle AI-generated flag on all tracks (Suno, demucs, etc.). The agent uses gentler EQ/compression and lower reverb on AI stems."
              >
                All AI
              </button>
            </div>
            <span className="topbar-sep" aria-hidden="true" />
            <div className="topbar-group" role="group" aria-label="File and help">
              <button
                className={mixerWindowOpen ? "active" : ""}
                onClick={() => void toggleMixerWindow()}
                title={mixerWindowOpen ? "Hide mixer window" : "Show mixer window"}
                aria-pressed={mixerWindowOpen}
              >
                <SlidersHorizontal size={18} />
              </button>
              <button
                className={videoMonitorOpen ? "active" : ""}
                onClick={() => void toggleVideoMonitor()}
                title={videoMonitorOpen ? "Hide video monitor" : "Show video monitor"}
                aria-pressed={videoMonitorOpen}
              >
                <Video size={18} />
              </button>
              <button onClick={() => void renderCurrentMix()} title="Export WAV"><Download size={18} /></button>
              <button className="upload" onClick={() => void importFiles()} title="Import audio">
                <Upload size={18} />
              </button>
              <button onClick={() => setShortcutsOpen(true)} title="Keyboard shortcuts (?)" aria-haspopup="dialog">
                <Keyboard size={18} />
              </button>
            </div>
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

        {videoEditorOpen ? (
          <div className="video-editor-shell">
            <div className="video-editor-head">
              <div>
                <strong>Video Editor</strong>
                <span>
                  {selectedVideoTracksForEditor.length} video track{selectedVideoTracksForEditor.length === 1 ? "" : "s"} selected
                  {selectedRange ? ` · ${formatTime(videoEditorStartSeconds)}-${formatTime(videoEditorEndSeconds)}` : " · full timeline"}
                </span>
              </div>
              <div className="video-editor-actions">
                <button type="button" onClick={() => void openCameraPreviewWindow(buildCameraPreviewTracks(), true)} disabled={selectedVideoTracksForEditor.length === 0}>
                  Show Canvas Preview
                </button>
                <select
                  className="aspect-select"
                  value={exportAspect}
                  onChange={(event) => setExportAspect(event.target.value as ExportAspect)}
                  title="Output aspect ratio for export (black bars added; source not cropped)"
                  disabled={busy}
                >
                  <option value="original">Original</option>
                  <option value="square">Square 1:1</option>
                  <option value="portrait916">Portrait 9:16</option>
                </select>
                <select
                  className="aspect-select"
                  value={exportQuality}
                  onChange={(event) => setExportQuality(event.target.value as ExportQuality)}
                  title="Encoder quality. Hi-Q = -preset slow -crf 17 -b:a 320k (visually lossless). Fast = -preset veryfast (~3-5x faster, smaller file)."
                  disabled={busy}
                >
                  <option value="high">Hi-Q (slow)</option>
                  <option value="fast">Fast</option>
                </select>
                <button type="button" onClick={() => void renderCurrentVideo()} disabled={busy || selectedVideoTracksForEditor.length === 0}>
                  <Download size={15} /> Export MP4
                </button>
                <button
                  type="button"
                  onClick={() => void renderAutoVideoEdit()}
                  disabled={busy || selectedVideoTracksForEditor.length === 0}
                  title={`Auto-cut between the selected cameras every ${agentIntervalSeconds || "2"}s (no AI) and export the result`}
                >
                  Quick Edit
                </button>
                <button type="button" className="primary" onClick={() => void runAgentVideoEditFromDraft()} disabled={busy || selectedVideoTracksForEditor.length === 0}>
                  Run Agent Edit
                </button>
                <button type="button" className="video-editor-close" onClick={() => setVideoEditorOpen(false)}>×</button>
              </div>
            </div>

            <div className="video-editor-body">
              <section className="video-editor-main">
                <div className="video-editor-canvas">
                  <div className="video-editor-canvas-frame" style={{ aspectRatio: `${videoCanvas.width} / ${videoCanvas.height}`, background: videoCanvas.background }}>
                    {selectedVideoTracksForEditor.length === 0 ? (
                      <span>Select video tracks to edit.</span>
                    ) : videoEditorScript.find((entry) => entry.chosenTrackName) ? (
                      <div className="video-editor-program">
                        <Video size={38} />
                        <strong>{videoEditorScript.find((entry) => entry.chosenTrackName)?.chosenTrackName}</strong>
                        <span>{videoEditorScript.length} agent decisions on the timeline</span>
                      </div>
                    ) : (
                      <div className="video-editor-program">
                        <Video size={38} />
                        <strong>{videoCanvas.width}×{videoCanvas.height}</strong>
                        <span>Run the agent to build the edit script.</span>
                      </div>
                    )}
                  </div>
                </div>

                <div className="video-editor-timeline">
                  <div className="video-editor-ruler">
                    <span>{formatTime(videoEditorStartSeconds)}</span>
                    <span>{formatTime(videoEditorStartSeconds + videoEditorSpanSeconds / 2)}</span>
                    <span>{formatTime(videoEditorEndSeconds)}</span>
                  </div>
                  <div className="video-editor-row agent-row">
                    <div className="video-editor-row-label">Agent cuts</div>
                    <div className="video-editor-row-lane">
                      {videoEditorScript.length === 0 ? <span className="video-editor-empty">No agent decisions yet</span> : null}
                      {videoEditorScript.map((entry) => {
                        const startPct = Math.max(0, Math.min(100, ((entry.startSeconds - videoEditorStartSeconds) / videoEditorSpanSeconds) * 100));
                        const endPct = Math.max(startPct + 0.4, Math.min(100, ((entry.endSeconds - videoEditorStartSeconds) / videoEditorSpanSeconds) * 100));
                        return (
                          <button
                            type="button"
                            className={`video-editor-event ${entry.chosenTrackName ? "" : "black"}`}
                            key={`event-${entry.windowIndex}-${entry.startSeconds}`}
                            style={{ left: `${startPct}%`, width: `${Math.max(0.8, endPct - startPct)}%` }}
                            title={`${formatTime(entry.startSeconds)}-${formatTime(entry.endSeconds)} ${entry.chosenTrackName ?? "black"}`}
                            onClick={() => seekTo(entry.startSeconds)}
                          >
                            {entry.chosenTrackName ?? "black"}
                          </button>
                        );
                      })}
                    </div>
                  </div>
                  <div className="video-editor-row audio-row">
                    <div className="video-editor-row-label">Audio mix</div>
                    <div className="video-editor-row-lane">
                      <div className="video-editor-audio-bed">
                        <span>{audioTrackCountForEditor} audio track{audioTrackCountForEditor === 1 ? "" : "s"} in export mix</span>
                      </div>
                    </div>
                  </div>
                  {selectedVideoTracksForEditor.map((track) => (
                    <div className="video-editor-row" key={`editor-track-${track.id}`}>
                      <div className="video-editor-row-label">{track.name}</div>
                      <div className="video-editor-row-lane">
                        {(track.videoClips ?? []).map((clip) => {
                          const clipStart = clip.startSample / session.sampleRate;
                          const clipEnd = clip.endSample / session.sampleRate;
                          if (clipEnd < videoEditorStartSeconds || clipStart > videoEditorEndSeconds) return null;
                          const startPct = Math.max(0, Math.min(100, ((clipStart - videoEditorStartSeconds) / videoEditorSpanSeconds) * 100));
                          const endPct = Math.max(startPct + 0.6, Math.min(100, ((clipEnd - videoEditorStartSeconds) / videoEditorSpanSeconds) * 100));
                          return (
                            <button
                              type="button"
                              className="video-editor-clip"
                              key={clip.id}
                              style={{ left: `${startPct}%`, width: `${Math.max(1, endPct - startPct)}%`, borderColor: track.color }}
                              onClick={() => {
                                setSelectedClip({ trackId: track.id, clipId: clip.id });
                                seekTo(clipStart);
                              }}
                            >
                              {clip.name ?? track.name}
                            </button>
                          );
                        })}
                      </div>
                    </div>
                  ))}
                </div>

                {agentEditProgress ? (
                  <div className={`agent-editor-progress stage-${agentEditProgress.stage}`}>
                    <div className="agent-editor-progress-row">
                      <strong>{agentEditProgress.stage}</strong>
                      <span>{agentEditProgress.current}/{agentEditProgress.total}</span>
                      <em>{Math.round(agentEditProgress.elapsedSeconds)}s</em>
                    </div>
                    <div className="agent-editor-progress-bar">
                      <span style={{ width: `${Math.max(3, Math.min(100, (agentEditProgress.current / Math.max(1, agentEditProgress.total)) * 100))}%` }} />
                    </div>
                    <div className="agent-editor-status">{agentEditProgress.message}</div>
                  </div>
                ) : agentEditStatus ? <div className="agent-editor-status">{agentEditStatus}</div> : null}

                {videoEditorScript.length > 0 ? (
                  <div className="agent-editor-script">
                    <div className="agent-editor-script-head">
                      <strong>Edit script</strong>
                      <span>{videoEditorScript.length} decisions</span>
                    </div>
                    <div className="agent-editor-script-list">
                      {videoEditorScript.map((entry) => (
                        <div className="agent-editor-script-item" key={`${entry.windowIndex}-${entry.startSeconds}-${entry.endSeconds}`}>
                          <div className="agent-editor-script-meta">
                            <strong>{formatTime(entry.startSeconds)}-{formatTime(entry.endSeconds)}</strong>
                            <span>
                              {entry.decision ? `${entry.decision} · ` : ""}
                              {entry.chosenTrackName ? `${entry.chosenTrackName} · track ${(entry.chosenTrackIndex ?? 0) + 1}` : "Black / no active video"}
                              {entry.varietyOverride ? " · variation override" : ""}
                            </span>
                          </div>
                          <p>{entry.reason}</p>
                          {entry.candidates.length > 0 ? (
                            <div className="agent-editor-script-candidates">
                              {entry.candidates.map((candidate) => (
                                <span key={`${entry.windowIndex}-${candidate.imageNumber}`}>
                                  Frame {candidate.imageNumber}: {candidate.angleLabel ? `${candidate.angleLabel} · ` : ""}{candidate.trackName} @ {formatTime(candidate.timelineSeconds)}
                                  {candidate.note ? ` - ${candidate.note}` : ""}
                                </span>
                              ))}
                            </div>
                          ) : null}
                          {entry.dataProvided?.length > 0 ? (
                            <div className="agent-editor-script-data">
                              <strong>Data provided</strong>
                              {entry.dataProvided.map((item, index) => (
                                <span key={`${entry.windowIndex}-data-${index}`}>{item}</span>
                              ))}
                            </div>
                          ) : null}
                        </div>
                      ))}
                    </div>
                  </div>
                ) : null}
              </section>

              <aside className="video-editor-side">
                <div className="video-editor-card video-editor-chat">
                  <strong>Video agent</strong>
                  <label className="video-editor-command">
                    Instructions
                    <textarea
                      value={videoChatDraft}
                      onChange={(event) => setVideoChatDraft(event.target.value)}
                      onKeyDown={(event) => {
                        if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
                          event.preventDefault();
                          void runAgentVideoEditFromDraft();
                        }
                      }}
                      placeholder="Example: use the overhead view during dense guitar parts, hold closeups through phrases, and avoid sudden cuts unless the music changes."
                    />
                  </label>
                  <div className="video-editor-command-actions">
                    <button type="button" onClick={sendVideoEditorChat} disabled={!videoChatDraft.trim()}>
                      Add instruction
                    </button>
                    <button type="button" className="primary" onClick={() => void runAgentVideoEditFromDraft()} disabled={busy || selectedVideoTracksForEditor.length === 0}>
                      Run Agent Edit
                    </button>
                  </div>
                  <div className="video-editor-chat-log">
                    {videoChatMessages.length === 0 ? (
                      <span className="video-editor-empty">Select tracks, choose a range, then describe the edit in plain language.</span>
                    ) : videoChatMessages.map((message, index) => (
                      <div className={`video-editor-message ${message.role}`} key={`${message.createdAt}-${index}`}>
                        <span>{message.role}</span>
                        <p>{message.text}</p>
                      </div>
                    ))}
                  </div>
                </div>

                <div className="video-editor-card video-editor-settings">
                  <strong>Agent settings</strong>
                  <label>
                    Interval
                    <input value={agentIntervalSeconds} onChange={(event) => setAgentIntervalSeconds(event.target.value)} inputMode="decimal" />
                  </label>
                  <label>
                    Vision model
                    <select value={agentVideoModel} onChange={(event) => setAgentVideoModel(event.target.value)}>
                      {modelOptions.map((model) => <option key={model} value={model}>{model}</option>)}
                    </select>
                  </label>
                  <label>
                    Edit model
                    <select value={agentVideoEditModel} onChange={(event) => setAgentVideoEditModel(event.target.value)}>
                      {modelOptions.map((model) => <option key={model} value={model}>{model}</option>)}
                    </select>
                  </label>
                </div>

                <div className="video-editor-card video-editor-history">
                  <strong>History</strong>
                  {videoEditHistory.length === 0 ? (
                    <span className="video-editor-empty">No saved video edit runs yet.</span>
                  ) : videoEditHistory.map((item) => (
                    <button
                      type="button"
                      key={item.id}
                      onClick={() => {
                        setAgentEditScript(item.script);
                        setAgentVideoInstructions(item.instructions);
                        setAgentVideoModel(item.visionModel);
                        setAgentVideoEditModel(item.editModel);
                      }}
                    >
                      <span>{new Date(item.createdAt).toLocaleString()}</span>
                      <strong>{item.script.length} decisions</strong>
                      <em>{item.outputPath}</em>
                    </button>
                  ))}
                </div>
              </aside>
            </div>
          </div>
        ) : null}

        <div className="timeline">
          <div className="daw-workspace">
            <TrackInspector
              track={focusedTrackId ? session.tracks.find((track) => track.id === focusedTrackId) : undefined}
              source={focusedTrackId ? session.sourceFiles.find((source) => source.id === session.tracks.find((track) => track.id === focusedTrackId)?.sourceFileId) : undefined}
              sampleRate={session.sampleRate}
              inputDevices={inputDevices}
              inputDevice={focusedTrackId ? trackInputDevices[focusedTrackId] ?? "" : ""}
              inputGainDb={focusedTrackId ? trackInputGains[focusedTrackId] ?? 0 : 0}
              inputChannels={focusedTrackId ? trackInputChannels[focusedTrackId] ?? [] : []}
              inputChannelLevels={inputChannelLevels}
              cameraDevices={cameraDevices}
              cameraDevice={focusedTrackId ? trackCameraDevices[focusedTrackId] ?? session.tracks.find((track) => track.id === focusedTrackId)?.cameraDeviceId ?? "" : ""}
              cameraAudio={focusedTrackId ? trackCameraAudio[focusedTrackId] ?? !!session.tracks.find((track) => track.id === focusedTrackId)?.recordCameraAudio : false}
              selectionCount={focusedTrackId ? 1 : 0}
              onChange={(track, patch) => void updateTrack(track, patch)}
              onInputDeviceChange={(trackId, device) => setTrackInputDevices((current) => ({ ...current, [trackId]: device }))}
              onInputGainChange={(trackId, gainDb) => setTrackInputGains((current) => ({ ...current, [trackId]: gainDb }))}
              onInputChannelsChange={(trackId, channels) => setTrackInputChannels((current) => ({ ...current, [trackId]: channels }))}
              onRefreshInputDevices={() => void api.inputDevices().then((result) => setInputDevices(result.devices)).catch(pushSystem)}
              onCameraDeviceChange={(trackId, device) => {
                setTrackCameraDevices((current) => ({ ...current, [trackId]: device }));
                const trackIndex = session.tracks.findIndex((track) => track.id === trackId);
                if (trackIndex >= 0) {
                  const previousTrack = session.tracks[trackIndex];
                  const nextTrack: Track = { ...previousTrack };
                  if (device) nextTrack.cameraDeviceId = device;
                  else delete nextTrack.cameraDeviceId;
                  void api.applyPatch(
                    session.id,
                    [{ op: "replace", path: `/tracks/${trackIndex}`, value: nextTrack }],
                    [{ op: "replace", path: `/tracks/${trackIndex}`, value: previousTrack }],
                    "Set video camera"
                  ).then(setProject).catch(pushSystem);
                }
              }}
              onCameraAudioChange={(trackId, enabled) => {
                setTrackCameraAudio((current) => ({ ...current, [trackId]: enabled }));
                const trackIndex = session.tracks.findIndex((track) => track.id === trackId);
                if (trackIndex >= 0) {
                  const previousTrack = session.tracks[trackIndex];
                  const nextTrack: Track = { ...previousTrack, recordCameraAudio: enabled };
                  void api.applyPatch(
                    session.id,
                    [{ op: "replace", path: `/tracks/${trackIndex}`, value: nextTrack }],
                    [{ op: "replace", path: `/tracks/${trackIndex}`, value: previousTrack }],
                    enabled ? "Enabled camera audio" : "Disabled camera audio"
                  ).then(setProject).catch(pushSystem);
                }
              }}
              onRefreshCameraDevices={() => void refreshCameraDevices(true)}
              onDelete={(track) => void deleteTrack(track)}
            />
            <div
              ref={trackLanesRef}
              className="track-lanes"
              tabIndex={0}
              role="listbox"
              aria-label="Tracks"
              aria-multiselectable="true"
              onKeyDown={handleTrackLaneKeyDown}
            >
              <TimeRuler
                duration={duration}
                playhead={playhead}
                bpm={session.bpm}
                selection={selectedRange}
                loopActive={!!(loopSection && selectedRange &&
                  Math.abs(loopSection.start - Math.min(selectedRange.start, selectedRange.end)) < 0.01 &&
                  Math.abs(loopSection.end - Math.max(selectedRange.start, selectedRange.end)) < 0.01)}
                onSeek={(seconds) => seekTo(seconds)}
                onSelect={(start, end) => {
                  trackLanesRef.current?.focus({ preventScroll: true });
                  playbackAnchorRef.current = Math.max(0, Math.min(start, end));
                  setSelectedClip(undefined);
                  setSelectedClips([]);
                  setSelectedRange({ start: Math.min(start, end), end: Math.max(start, end) });
                  // A fresh region replaces any previous loop — re-click the band to activate.
                  setLoopSection(null);
                }}
                onClear={() => { setSelectedRange(undefined); setLoopSection(null); }}
                onToggleLoop={() => {
                  if (!selectedRange) return;
                  const lo = Math.min(selectedRange.start, selectedRange.end);
                  const hi = Math.max(selectedRange.start, selectedRange.end);
                  if (hi - lo < 0.02) return;
                  setLoopSection((current) => current
                    && Math.abs(current.start - lo) < 0.01
                    && Math.abs(current.end - hi) < 0.01
                    ? null
                    : { start: lo, end: hi });
                }}
              />
              {selectedRange && !selectedRange.trackId && duration > 0 && Math.abs(selectedRange.end - selectedRange.start) > 0.02 ? (
                <div className="lane-region-overlay" aria-hidden="true">
                  <div
                    className={`lane-region-band ${loopSection
                      && Math.abs(loopSection.start - Math.min(selectedRange.start, selectedRange.end)) < 0.01
                      && Math.abs(loopSection.end - Math.max(selectedRange.start, selectedRange.end)) < 0.01
                      ? "active" : ""}`}
                    style={{
                      left: `${(Math.min(selectedRange.start, selectedRange.end) / duration) * 100}%`,
                      width: `${(Math.abs(selectedRange.end - selectedRange.start) / duration) * 100}%`,
                    }}
                  />
                </div>
              ) : null}
              {cutToolActive && cutCursorSeconds !== undefined && duration > 0 ? (
                <div className="lane-region-overlay" aria-hidden="true">
                  <div
                    className="lane-cut-cursor"
                    style={{ left: `${Math.max(0, Math.min(100, (cutCursorSeconds / duration) * 100))}%` }}
                  />
                </div>
              ) : null}
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
                  <span>This session is empty.</span>
                  <div className="empty-actions">
                    <button type="button" onClick={() => void importFiles()} disabled={busy}>
                      <Upload size={15} /> Import stems
                    </button>
                    <button type="button" onClick={() => setAddTrackMenuOpen(true)} disabled={busy}>
                      <Plus size={15} /> Add a track
                    </button>
                  </div>
                  <small>Import audio stems, or add a recording / video track to capture takes.</small>
                </div>
              ) : (() => {
                const alignmentCandidates = session.tracks.flatMap((candidateTrack) => {
                  if (candidateTrack.kind === "video") {
                    return (candidateTrack.videoClips ?? []).map((clip) => clip.startSample / session.sampleRate);
                  }
                  return candidateTrack.clips.length > 0
                    ? candidateTrack.clips.map((clip) => clip.startSample / session.sampleRate)
                    : [candidateTrack.startSample / session.sampleRate];
                });
                return session.tracks.map((track, trackIndex) => {
                  const sourceById = new Map(session.sourceFiles.map((item) => [item.id, item]));
                  const videoSourceById = new Map((session.videoSourceFiles ?? []).map((item) => [item.id, item]));
                  const source = sourceById.get(track.sourceFileId);
                  const isVideo = track.kind === "video";
                  const clips = isVideo
                    ? (track.videoClips ?? []).map((clip) => {
                        const videoSource = videoSourceById.get(clip.videoSourceFileId);
                        return {
                          id: clip.id,
                          kind: "video" as const,
                          name: clip.name ?? videoSource?.originalName ?? "Video",
                          src: videoSource?.path,
                          startSeconds: clip.startSample / session.sampleRate,
                          sourceSeconds: Math.max(0, (clip.endSample - clip.startSample) / session.sampleRate),
                        };
                      })
                    : track.clips.length > 0
                    ? track.clips.map((clip) => ({
                        id: clip.id,
                        kind: "audio" as const,
                        name: clip.name ?? "Recording",
                        startSeconds: clip.startSample / session.sampleRate,
                        sourceSeconds: Math.max(0, (clip.endSample - clip.startSample) / session.sampleRate),
                        peaks: sourceById.get(clip.sourceFileId ?? track.sourceFileId)?.peakPreview,
                      }))
                    : [{
                        id: `legacy-${track.id}`,
                        kind: "audio" as const,
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
                      focused={focusedTrackId === track.id}
                      armed={isVideo ? armedVideoTrackIds.includes(track.id) : armedAudioTrackIds.includes(track.id)}
                      peak={playing ? (trackPeaks[trackIndex] ?? 0) : 0}
                      playhead={playhead}
                      transportPlaying={playing}
                      duration={duration}
                      alignmentCandidates={alignmentCandidates}
                      alignmentGuideSeconds={alignmentGuideSeconds}
                      clips={clips}
                      selectedClipId={selectedClip?.trackId === track.id ? selectedClip.clipId : undefined}
                      selectedClipIds={selectedClips.filter((ref) => ref.trackId === track.id).map((ref) => ref.clipId)}
                      selectedRange={selectedRange?.trackId === track.id ? selectedRange : undefined}
                      recording={isVideo ? videoRecordingTrackIds.includes(track.id) : recordingTrackId === track.id}
                      recordingStarting={recordingTrackId === track.id && recordingStarting}
                      recordingStartSeconds={isVideo && videoRecordingTrackIds.includes(track.id) ? videoRecordingStartSeconds : recordingTrackId === track.id ? recordingStartSeconds : undefined}
                      monitoring={isVideo ? armedVideoTrackIds.includes(track.id) && !videoRecordingTrackIds.includes(track.id) : armedTrackId === track.id && inputMonitoring && !recording}
                      monitorStarting={armedTrackId === track.id && inputMonitorStarting && !recording}
                      livePeaks={
                        recordingTrackId === track.id
                          ? liveRecordingPeaks
                          : armedTrackId === track.id && inputMonitoring && !recording
                            ? inputMonitorPeaks
                            : undefined
                      }
                      onToggleSelect={() => toggleTrackSelection(track.id)}
                      onSelectTrack={(additive) => {
                        // Cmd/Ctrl-click also toggles group membership; plain click only
                        // focuses the track for the inspector and leaves the group alone.
                        if (additive) {
                          setSelectedTrackIds((ids) => ids.includes(track.id) ? ids.filter((id) => id !== track.id) : [...ids, track.id]);
                        }
                        setFocusedTrackId(track.id);
                      }}
                      onClipSelect={(clipId, additive) => {
                        trackLanesRef.current?.focus({ preventScroll: true });
                        // Selecting a clip never moves the playhead or the playback anchor —
                        // playback always continues / resumes from wherever it currently is.
                        // Preserve a global ruler range; only drop any per-track range.
                        setSelectedRange((current) => (current && !current.trackId ? current : undefined));
                        // Focus the clip's track so the left inspector switches to it —
                        // covers every click path (plain, Cmd, drag), unlike onSelectTrack.
                        setFocusedTrackId(track.id);
                        setSelectedClip({ trackId: track.id, clipId });
                        if (additive) {
                          setSelectedClips((prev) => prev.some((ref) => ref.trackId === track.id && ref.clipId === clipId)
                            ? prev.filter((ref) => !(ref.trackId === track.id && ref.clipId === clipId))
                            : [...prev, { trackId: track.id, clipId }]);
                        } else {
                          setSelectedClips([{ trackId: track.id, clipId }]);
                        }
                      }}
                      onClipContextMenu={(clipId, event) => {
                        event.preventDefault();
                        setFocusedTrackId(track.id);
                        setSelectedClips((prev) => prev.some((ref) => ref.trackId === track.id && ref.clipId === clipId)
                          ? prev
                          : [{ trackId: track.id, clipId }]);
                        setSelectedClip({ trackId: track.id, clipId });
                        setClipMenu({ x: event.clientX, y: event.clientY });
                      }}
                      onClipMove={(clipId, deltaSeconds) => void moveClip(track.id, clipId, deltaSeconds)}
                      cutToolActive={cutToolActive}
                      onClipCut={(clipId, atSeconds) => void splitClip(track.id, clipId, atSeconds)}
                      onCutHover={(seconds) => setCutCursorSeconds(seconds)}
                      onAlignmentGuideChange={setAlignmentGuideSeconds}
                      onRangeSelect={(start, end) => {
                        trackLanesRef.current?.focus({ preventScroll: true });
                        playbackAnchorRef.current = Math.max(0, Math.min(start, end));
                        setSelectedClip(undefined);
                        setSelectedClips([]);
                        setSelectedRange({ trackId: track.id, start: Math.min(start, end), end: Math.max(start, end) });
                      }}
                      onRangeClear={() => {
                        // A click on a track's wave area only clears the selection if it's
                        // a per-track one. Global ruler selections (no trackId) stick around.
                        setSelectedRange((current) => (current && !current.trackId ? current : undefined));
                        setSelectedClip(undefined);
                        setSelectedClips([]);
                      }}
                      onArm={() => {
                        // Group-aware arm: if this track is in the S-group with others, apply
                        // to every track in the group; otherwise just this track.
                        const inGroup = selectedTrackIds.includes(track.id) && selectedTrackIds.length > 1;
                        const targets = inGroup ? selectedTrackIds : [track.id];
                        const targetVideos = targets.filter((id) => session.tracks.some((t) => t.id === id && t.kind === "video"));
                        if (targetVideos.length > 0) {
                          const allArmed = targetVideos.every((id) => armedVideoTrackIds.includes(id));
                          setArmedVideoTrackIds((ids) => allArmed
                            ? ids.filter((id) => !targetVideos.includes(id))
                            : Array.from(new Set([...ids, ...targetVideos])));
                        }
                        const targetAudios = targets.filter((id) => session.tracks.some((t) => t.id === id && t.kind !== "video"));
                        if (targetAudios.length > 0) {
                          const allArmed = targetAudios.every((id) => armedAudioTrackIds.includes(id));
                          setArmedAudioTrackIds((ids) => allArmed
                            ? ids.filter((id) => !targetAudios.includes(id))
                            : Array.from(new Set([...ids, ...targetAudios])));
                        }
                      }}
                      onMute={() => {
                        // Group-aware mute: if track is in the S-group with others, mute/unmute
                        // every group member together based on whether all are currently muted.
                        const inGroup = selectedTrackIds.includes(track.id) && selectedTrackIds.length > 1;
                        const targetIds = inGroup ? selectedTrackIds : [track.id];
                        const targetTracks = session.tracks.filter((t) => targetIds.includes(t.id));
                        const allMuted = targetTracks.every((t) => t.muted);
                        const nextMuted = !allMuted;
                        for (const t of targetTracks) {
                          if (t.muted !== nextMuted) void updateTrack(t, { muted: nextMuted });
                        }
                      }}
                      onSeek={(seconds) => seekTo(seconds)}
                      onChange={(patch) => void updateTrack(track, patch)}
                      onDragOver={(event) => updateTrackDropTarget(track.id, event)}
                      onDrop={(event) => void dropTracks(track.id, event)}
                    />
                  );
                });
              })()}
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

      <aside className="assistant">
        <div className="assistant-head">
          <div className="assistant-title">
            <MessageSquare size={14} />
            <span>Assistant</span>
          </div>
          <div className="assistant-head-actions">
            {agentUsage ? (
              <span
                className="agent-ctx-badge"
                title={`This conversation: ~${(agentUsage.output + agentUsage.thought).toLocaleString()} tokens generated (${agentUsage.output.toLocaleString()} reply + ${agentUsage.thought.toLocaleString()} reasoning). Context is ${agentUsage.turns}/${agentUsage.compactAfter} turns full — it auto-compacts (resets) at ${agentUsage.compactAfter}.`}
              >
                {agentUsage.turns}/{agentUsage.compactAfter} ctx · {(agentUsage.output + agentUsage.thought).toLocaleString()} tok
              </span>
            ) : null}
            {busy || autoMixRunning ? (
              <button
                className="chat-stop"
                onClick={() => void stopAgentRun()}
                title="Stop the current agent run"
              >
                <Square size={14} />
                <span>Stop</span>
              </button>
            ) : null}
            <button
              className="icon-btn"
              onClick={() => void clearChat()}
              disabled={busy || autoMixRunning}
              title="Clear chat — start a fresh conversation so the agent forgets earlier context"
              aria-label="Clear chat"
            >
              <Trash2 size={14} />
            </button>
            <button className="icon-btn" onClick={() => setSettingsOpen(true)} title="Settings" aria-label="Settings">
              <Settings size={14} />
            </button>
          </div>
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
            onStop={() => void stopAgentRun()}
          />
        ) : mode === "video" ? (
          <div className="assistant-video-panel">
            <div className="assistant-video-preview-card">
              <div className="assistant-video-preview">
                {assistantVideoPreviewClip ? (
                  <TimelineVideo
                    src={assistantVideoPreviewClip.src}
                    color={assistantVideoPreviewClip.color}
                    active={playhead >= assistantVideoPreviewClip.startSeconds && playhead <= assistantVideoPreviewClip.endSeconds}
                    localTime={assistantVideoPreviewClip.localTime}
                    playing={playing}
                  />
                ) : (
                  <div className="assistant-video-preview-empty">
                    <Video size={24} />
                    <span>Select video tracks to preview.</span>
                  </div>
                )}
              </div>
              {/* Look picker: click a preset to re-render the agent edit with that color grade.
                  No-op when there's no edit yet. Active preset has the "active" class. */}
              <div className="look-picker">
                <span className="look-picker-label">Look</span>
                {(["none","warm","cool","mono","punch","dream","cinema","noir","moody","vintage","golden","cold"] as VideoFilterPreset[]).map((preset) => (
                  <button
                    key={preset}
                    type="button"
                    className={`look-chip${agentEditLook === preset ? " active" : ""}`}
                    onClick={() => {
                      setAgentEditLook(preset);
                      // Clicking a chip = "use this preset, not the agent's custom grade".
                      setAgentColorGrade(null);
                      // Focused-clip mode: apply (or revert) to JUST that clip — never
                      // touch the agent edit track. "Original" → revert the clip to its
                      // pristine recording (no LLM, no ffmpeg).
                      if (focusedVideoClip) {
                        if (preset === "none") {
                          void revertFocusedClip();
                        } else {
                          void applyEffectsToFocusedClip(preset);
                        }
                        return;
                      }
                      // No focused clip — re-render the multicam agent edit if one exists.
                      if (agentEditContext && agentEditScript.length > 0) void rerenderEdit(preset);
                    }}
                    disabled={busy}
                    title={focusedVideoClip
                      ? (preset === "none"
                          ? `Revert "${focusedVideoClip.name}" to the original recording`
                          : `Apply ${preset} to "${focusedVideoClip.name}"`)
                      : (agentEditContext ? `Re-render the current edit with the ${preset} look` : "Run the agent or select a clip first")}
                  >
                    {preset === "none" ? "Original" : preset[0].toUpperCase() + preset.slice(1)}
                  </button>
                ))}
              </div>
              <div className="assistant-video-preview-meta">
                <span>
                  {assistantVideoPreviewClip
                    ? `${assistantVideoPreviewClip.trackName} · ${formatTime(assistantVideoPreviewClip.startSeconds)}-${formatTime(assistantVideoPreviewClip.endSeconds)}`
                    : selectedRange
                      ? `${formatTime(videoEditorStartSeconds)}-${formatTime(videoEditorEndSeconds)}`
                      : "Full timeline"}
                </span>
                <select
                  className="aspect-select"
                  value={exportAspect}
                  onChange={(event) => setExportAspect(event.target.value as ExportAspect)}
                  title="Output aspect ratio (black bars added; source not cropped)"
                  disabled={busy}
                >
                  <option value="original">Original</option>
                  <option value="square">Square 1:1</option>
                  <option value="portrait916">Portrait 9:16</option>
                </select>
                <select
                  className="aspect-select"
                  value={exportQuality}
                  onChange={(event) => setExportQuality(event.target.value as ExportQuality)}
                  title="Encoder quality. Hi-Q re-renders from camera sources at -preset slow -crf 17 -b:a 320k (slower). Fast copies the preview cache."
                  disabled={busy}
                >
                  <option value="high">Hi-Q (slow)</option>
                  <option value="fast">Fast (copy)</option>
                </select>
                <button
                  type="button"
                  onClick={() => void downloadAgentEdit()}
                  disabled={busy || !agentEditContext}
                  title={agentEditContext ? "Save the current agent edit to a file" : "Run the agent once first"}
                >
                  Download
                </button>
                <button
                  type="button"
                  className="icon-only"
                  onClick={() => void openCameraPreviewWindow(buildCameraPreviewTracks(), true)}
                  disabled={selectedVideoTracksForEditor.length === 0}
                  title={selectedVideoTracksForEditor.length === 0 ? "Select video tracks first" : "Open the live multi-camera view"}
                  aria-label="Open camera view"
                >
                  <Camera size={16} />
                </button>
              </div>
            </div>

            {agentEditProgress ? (
              <div className={`agent-editor-progress compact stage-${agentEditProgress.stage}`}>
                <div className="agent-editor-progress-row">
                  <strong>{agentEditProgress.stage}</strong>
                  <span>{agentEditProgress.current}/{agentEditProgress.total}</span>
                  <em>{Math.round(agentEditProgress.elapsedSeconds)}s</em>
                </div>
                <div className="agent-editor-progress-bar">
                  <span style={{ width: `${Math.max(3, Math.min(100, (agentEditProgress.current / Math.max(1, agentEditProgress.total)) * 100))}%` }} />
                </div>
                <div className="agent-editor-status">{agentEditProgress.message}</div>
              </div>
            ) : agentEditStatus ? <div className="agent-editor-status compact">{agentEditStatus}</div> : null}

            {focusedVideoClip ? (
              <div className="focused-clip-badge">
                <span>Editing clip: <strong>{focusedVideoClip.name}</strong></span>
                <button type="button" onClick={() => setSelectedClip(undefined)} title="Defocus and go back to agent mode">
                  ×
                </button>
              </div>
            ) : null}
            <div className="video-history">
              {videoChatMessages.length === 0 ? (
                <span className="video-history-empty">Prompts you send to the video agent appear here.</span>
              ) : (
                videoChatMessages.map((message, index) => (
                  <div className={`video-history-msg ${message.role}`} key={`${message.createdAt}-${index}`}>
                    <span className="video-history-role">{message.role === "user" ? "You" : message.role === "agent" ? "Agent" : "System"}</span>
                    <p>{message.text}</p>
                  </div>
                ))
              )}
            </div>

            {agentPlan && agentPlan.length > 0 ? (
              <div className="plan-view">
                <div className="plan-head">
                  <strong>Plan · {agentPlan.length} shots</strong>
                  <button
                    type="button"
                    className="primary"
                    onClick={() => void processPlan()}
                    disabled={busy}
                    title="Render the (possibly edited) plan into a video and attach it to the session"
                  >
                    Process
                  </button>
                </div>
                <p className="plan-hint">Adjust any shot's camera, then click Process to render. No agent run.</p>
                <div className="plan-list">
                  {agentPlan.map((entry) => {
                    const candidates = Array.from(new Map(entry.candidates.map((c) => [c.trackIndex, c])).values());
                    return (
                      <div className="plan-row" key={entry.windowIndex}>
                        <span className="plan-time">{formatTime(entry.startSeconds)}–{formatTime(entry.endSeconds)}</span>
                        {candidates.length > 0 ? (
                          <select
                            value={entry.chosenTrackIndex ?? ""}
                            onChange={(event) => setPlanShot(entry.windowIndex, Number(event.target.value))}
                          >
                            {entry.chosenTrackIndex == null ? <option value="">(none)</option> : null}
                            {candidates.map((candidate) => (
                              <option key={candidate.trackIndex} value={candidate.trackIndex}>
                                {candidate.trackName}{candidate.angleLabel ? ` · ${candidate.angleLabel}` : ""}
                              </option>
                            ))}
                          </select>
                        ) : (
                          <span className="plan-fixed">{entry.chosenTrackName ?? "—"}</span>
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>
            ) : null}

            <form
              className="assistant-video-command"
              onSubmit={(event) => {
                event.preventDefault();
                void runAgentVideoEditFromDraft();
              }}
            >
              <textarea
                aria-label="Tell the video agent what to do"
                value={videoChatDraft}
                onChange={(event) => setVideoChatDraft(event.target.value)}
                onKeyDown={(event) => {
                  if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
                    event.preventDefault();
                    void runAgentVideoEditFromDraft();
                  }
                }}
                placeholder={focusedVideoClip
                  ? `Edit "${focusedVideoClip.name}" — e.g. "cinematic, fade in 1s"`
                  : "Tell the video agent what to do..."}
              />
              <button type="submit" className="primary" disabled={busy || (!focusedVideoClip && selectedVideoTracksForEditor.length === 0)}>
                {focusedVideoClip ? "Apply to clip" : "Send"}
              </button>
            </form>

          </div>
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
                  if (message.role === "video") {
                    return (
                      <button key={index} type="button" className="chat-video-chip" onClick={() => void toggleVideoMonitor(true)} title="Open in the video monitor">
                        <span className="chip-play"><Play size={14} /></span>
                        <span className="chip-text">
                          <strong>Video edit ready</strong>
                          <span className="chip-meta">{message.cuts} cuts{message.lookPreset ? ` · ${message.lookPreset}` : ""}</span>
                        </span>
                        <Maximize2 size={14} className="chip-open" />
                      </button>
                    );
                  }
                  return <div key={index} className={`message ${message.role}`}>{message.text}</div>;
                })
              )}
              {busy ? (
                <div className="message activity">
                  <div className="activity-head">
                    <span className="activity-dot" />
                    <span>{autoMixRunning ? "Auto-mixing…" : agentEditProgress ? "Editing video…" : "Working…"}</span>
                    {agentUsage ? (
                      <span className="activity-tokens" title="Estimated tokens generated this session (reply · reasoning).">
                        {agentUsage.output.toLocaleString()} out · {agentUsage.thought.toLocaleString()} reasoning
                      </span>
                    ) : liveTokens.prompt > 0 || liveTokens.response > 0 ? (
                      <span className="activity-tokens">{liveTokens.prompt.toLocaleString()} → {liveTokens.response.toLocaleString()} tok</span>
                    ) : null}
                  </div>

                  {liveReasoning ? (
                    <details className="activity-reasoning" open>
                      <summary>Reasoning</summary>
                      <pre>{liveReasoning}</pre>
                    </details>
                  ) : null}

                  {liveTools.length > 0 ? (
                    <div className="activity-tools">
                      {liveTools.slice(-6).map((tool, i) => (
                        <span key={i} className="activity-tool">{tool}</span>
                      ))}
                    </div>
                  ) : null}

                  {streamingTurn?.text ? <pre className="streaming-text">{streamingTurn.text}</pre> : null}

                  {autoMixRunning && autoMixStages.length > 0 ? (
                    <div className="activity-stages">
                      {autoMixStages.map((s) => (
                        <div key={s.stageId} className={`activity-stage ${s.status}`}>
                          <span className="stage-icon">{s.status === "running" ? "▸" : s.status === "complete" ? "✓" : s.status === "error" ? "✗" : "·"}</span>
                          <span className="stage-name">{s.displayName}</span>
                          {s.actionCount ? <span className="stage-count">{s.actionCount}</span> : null}
                        </div>
                      ))}
                    </div>
                  ) : null}

                </div>
              ) : null}
            </div>
            {/* Standalone video-render progress — shows while the background edit runs,
                independent of the chat turn (which ends immediately now). */}
            {agentEditProgress && agentEditProgress.stage !== "done" && agentEditProgress.stage !== "error" ? (
              <div className={`video-render-progress stage-${agentEditProgress.stage}`}>
                <div className="vrp-head">
                  <span className="vrp-spinner" />
                  <span className="vrp-title">Rendering video edit</span>
                  <span className="vrp-meta">
                    {agentEditProgress.total > 1 ? `window ${agentEditProgress.current}/${agentEditProgress.total} · ` : ""}
                    {Math.round(agentEditProgress.elapsedSeconds)}s
                  </span>
                </div>
                <div className="vrp-bar">
                  <div
                    className="vrp-fill"
                    style={{
                      width: agentEditProgress.total > 1
                        ? `${Math.min(100, (agentEditProgress.current / agentEditProgress.total) * 100)}%`
                        : "20%",
                    }}
                  />
                </div>
                <div className="vrp-msg">{agentEditProgress.message || agentEditProgress.stage}</div>
              </div>
            ) : null}
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
            {agentUsage ? (
              <div className="chat-usage" title="Context fills as the conversation grows, then auto-compacts (resets) to keep it light. Tokens are an estimate of what the agent generated this session (reply + reasoning).">
                <div className="chat-usage-row">
                  <span className="chat-usage-label">context</span>
                  <div className="chat-usage-bar">
                    <div
                      className="chat-usage-fill"
                      style={{ width: `${Math.min(100, (agentUsage.turns / Math.max(1, agentUsage.compactAfter)) * 100)}%` }}
                    />
                  </div>
                  <span className="chat-usage-meta">{agentUsage.turns}/{agentUsage.compactAfter} turns</span>
                </div>
                <div className="chat-usage-tokens">
                  ~{(agentUsage.output + agentUsage.thought).toLocaleString()} tok generated · {agentUsage.output.toLocaleString()} reply · {agentUsage.thought.toLocaleString()} reasoning
                </div>
              </div>
            ) : null}
            <form
              className="chat-input"
              onSubmit={(event) => {
                event.preventDefault();
                if (!busy && chatText.trim()) void sendChat();
              }}
            >
              <textarea
                value={chatText}
                onChange={(event) => setChatText(event.target.value)}
                onKeyDown={(event) => {
                  // Enter sends; Shift+Enter inserts a newline. Ignore Enter while
                  // an IME composition is active so CJK input commits cleanly.
                  if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                    event.preventDefault();
                    if (!busy && chatText.trim()) void sendChat();
                  }
                }}
                placeholder={busy ? "Working…" : "Make a change or ask anything…"}
                disabled={busy}
                rows={1}
              />
            </form>
          </>
        )}
      </aside>
      {settingsOpen ? (
        <div className="settings-backdrop" onPointerDown={() => setSettingsOpen(false)}>
          <div className="settings-modal" role="dialog" aria-modal="true" aria-label="Settings" onPointerDown={(e) => e.stopPropagation()}>
            <header className="settings-modal-head">
              <h2>Settings</h2>
              <button className="icon-btn" onClick={() => setSettingsOpen(false)} aria-label="Close settings"><X size={18} /></button>
            </header>
            <div className="settings-modal-body">
              <section className="settings-group">
                <div className="settings-group-title">Agent model</div>
                <p className="settings-group-desc">The model that powers the chat — and the auto-mix. Any OpenAI-compatible endpoint (vLLM / llama.cpp / Ollama).</p>
                <label className="settings-field"><span>Endpoint URL</span>
                  <input value={agentUrl} onChange={(e) => setAgentUrl(e.target.value)} placeholder="http://127.0.0.1:2256/v1" />
                </label>
                <label className="settings-field"><span>Model</span>
                  <input value={agentModel} onChange={(e) => setAgentModel(e.target.value)} placeholder="qwen3.6-35b-a3b" />
                </label>
                <div className="settings-field-actions">
                  <button className="primary" onClick={async () => {
                    setAgentStatus("Applying…");
                    try { await api.setHermesModel(agentUrl.trim(), agentModel.trim()); setAgentStatus("Agent restarted on new model"); }
                    catch (error) { setAgentStatus(error instanceof Error ? error.message : String(error)); }
                  }}>Apply</button>
                  <span className="settings-status">{agentStatus}</span>
                </div>
              </section>

              <section className="settings-group">
                <div className="settings-group-title">Video model</div>
                <p className="settings-group-desc">The vision model the video-edit skill uses to "see" frames (e.g. Qwen3-VL on the Spark).</p>
                <label className="settings-field"><span>Endpoint URL</span>
                  <input value={videoUrl} onChange={(e) => setVideoUrl(e.target.value)} placeholder="http://127.0.0.1:11435" />
                </label>
                <label className="settings-field"><span>Model</span>
                  <input value={videoModelName} onChange={(e) => setVideoModelName(e.target.value)} placeholder="qwen3-vl:30b-a3b-instruct-q4_K_M" />
                </label>
                <div className="settings-field-actions">
                  <button className="primary" onClick={async () => {
                    setVideoStatus("Saving…");
                    try { await api.setVideoModel(videoUrl.trim(), videoModelName.trim()); setOllamaUrl(videoUrl.trim()); setAgentVideoModel(videoModelName.trim()); setVideoStatus("Video model saved"); }
                    catch (error) { setVideoStatus(error instanceof Error ? error.message : String(error)); }
                  }}>Apply</button>
                  <span className="settings-status">{videoStatus}</span>
                </div>
              </section>

              <section className="settings-group">
                <div className="settings-group-title">Mix profile</div>
                <label className="settings-field"><span>Preset</span>
                  <select value={session.mixerProfile?.presetId ?? "balanced"} onChange={(e) => { const preset = profilePresets.find((p) => p.id === e.target.value); if (preset) void applyProfilePreset(preset); }}>
                    {profilePresets.map((preset) => <option key={preset.id} value={preset.id}>{preset.displayName}</option>)}
                  </select>
                </label>
                {(() => { const current = profilePresets.find((p) => p.id === (session.mixerProfile?.presetId ?? "balanced")); return current ? <p className="settings-group-desc">{current.summary}</p> : null; })()}
              </section>

              <section className="settings-group">
                <div className="settings-group-title">A/B judge</div>
                <label className="settings-field"><span>Gemini API key</span>
                  <input value={geminiApiKey} onChange={(e) => setGeminiApiKey(e.target.value)} type="password" placeholder="Used for the A/B audio judge" />
                </label>
              </section>
            </div>
            {appVersion ? <footer className="settings-modal-foot">AutoMixer v{appVersion}</footer> : null}
          </div>
        </div>
      ) : null}
      {addTrackMenuOpen ? (
        <div className="add-track-modal-backdrop" onPointerDown={() => setAddTrackMenuOpen(false)}>
          <div
            className="add-track-modal"
            role="dialog"
            aria-label="Add a track"
            onPointerDown={(event) => event.stopPropagation()}
          >
            <div className="add-track-modal-head">
              <strong>Add a track</strong>
              <button type="button" className="add-track-modal-close" onClick={() => setAddTrackMenuOpen(false)} aria-label="Close">×</button>
            </div>
            <div className="add-track-modal-grid">
              <button
                type="button"
                className="add-track-card"
                onClick={() => { setAddTrackMenuOpen(false); void addRecordingTrack(1); }}
              >
                <Mic size={28} />
                <strong>Audio · Mono</strong>
                <span>1-channel recording track</span>
              </button>
              <button
                type="button"
                className="add-track-card"
                onClick={() => { setAddTrackMenuOpen(false); void addRecordingTrack(2); }}
              >
                <Mic size={28} />
                <strong>Audio · Stereo</strong>
                <span>2-channel recording track</span>
              </button>
              <button
                type="button"
                className="add-track-card"
                onClick={() => { setAddTrackMenuOpen(false); void addVideoTrack(); }}
              >
                <Camera size={28} />
                <strong>Video</strong>
                <span>Camera / video clips</span>
              </button>
            </div>
          </div>
        </div>
      ) : null}
      {clipMenu ? (
        <>
          <div
            className="context-menu-backdrop"
            onPointerDown={() => setClipMenu(null)}
            onContextMenu={(event) => { event.preventDefault(); setClipMenu(null); }}
          />
          <div className="clip-context-menu" style={{ left: clipMenu.x, top: clipMenu.y }}>
            {selectedClips.length >= 2 ? (
              <>
                <div className="context-menu-header">Left align to</div>
                {selectedClips.map((ref) => {
                  const refTrack = session.tracks.find((item) => item.id === ref.trackId);
                  if (!refTrack) return null;
                  const clip = ref.clipId === `legacy-${refTrack.id}`
                    ? undefined
                    : (refTrack.kind === "video" ? (refTrack.videoClips ?? []) : refTrack.clips).find((item) => item.id === ref.clipId);
                  const clipName = clip?.name;
                  return (
                    <button
                      key={`${ref.trackId}:${ref.clipId}`}
                      className="context-menu-item"
                      onClick={() => { void alignClipsLeft(ref.trackId, ref.clipId); setClipMenu(null); }}
                    >
                      <span className="context-menu-track">{refTrack.name}</span>
                      {clipName && clipName !== refTrack.name ? <span className="context-menu-sub">{clipName}</span> : null}
                    </button>
                  );
                })}
              </>
            ) : (
              <div className="context-menu-empty">⌘-click clips on 2+ tracks to align them</div>
            )}
          </div>
        </>
      ) : null}
      {confirmRequest ? (
        <div className="confirm-backdrop" onPointerDown={() => resolveConfirm(false)}>
          <div
            className="confirm-dialog"
            role="alertdialog"
            aria-modal="true"
            aria-label={confirmRequest.title}
            onPointerDown={(event) => event.stopPropagation()}
          >
            <strong>{confirmRequest.title}</strong>
            <p>{confirmRequest.message}</p>
            <div className="confirm-actions">
              <button type="button" autoFocus onClick={() => resolveConfirm(false)}>Cancel</button>
              <button type="button" className="confirm-danger" onClick={() => resolveConfirm(true)}>{confirmRequest.confirmLabel}</button>
            </div>
          </div>
        </div>
      ) : null}
      {shortcutsOpen ? (
        <div className="confirm-backdrop" onPointerDown={() => setShortcutsOpen(false)}>
          <div
            className="shortcuts-dialog"
            role="dialog"
            aria-modal="true"
            aria-label="Keyboard shortcuts"
            onPointerDown={(event) => event.stopPropagation()}
          >
            <div className="shortcuts-head">
              <strong>Keyboard shortcuts</strong>
              <button type="button" className="add-track-modal-close" onClick={() => setShortcutsOpen(false)} aria-label="Close">×</button>
            </div>
            <div className="shortcuts-grid">
              {[
                ["Space", "Play / pause"],
                [`${MOD_KEY}Z`, "Undo"],
                [`${SHIFT_KEY}${MOD_KEY}Z`, "Redo"],
                ["← →", `Nudge selected clip 10 ms (${ALT_KEY} = 100 ms, ${SHIFT_KEY} = 1 ms)`],
                ["↑ ↓", `Select previous / next track (${SHIFT_KEY} extends)`],
                ["Delete", "Delete selected clip, range, or selection"],
                [`${SHIFT_KEY} Delete`, "Delete selected tracks"],
                ["Esc", "Clear selection / close dialogs"],
                [`${MOD_KEY}Enter`, "Send chat or video instruction"],
                [`${MOD_KEY} click`, "Multi-select tracks or clips"],
                [`${SHIFT_KEY} click section`, "Scope the chat to that section"],
                [`${ALT_KEY} click section`, "Loop that section"],
                ["?", "Show this help"],
              ].map(([keys, label]) => (
                <div className="shortcuts-row" key={keys}>
                  <kbd>{keys}</kbd>
                  <span>{label}</span>
                </div>
              ))}
            </div>
          </div>
        </div>
      ) : null}
      <div className="toast-stack" role="status" aria-live="polite">
        {toasts.map((toast) => (
          <div key={toast.id} className={`toast toast-${toast.kind}`}>
            {toast.kind === "success" ? <CheckCircle2 size={15} /> : toast.kind === "error" ? <AlertCircle size={15} /> : <Info size={15} />}
            <span className="toast-text">{toast.text}</span>
            <button type="button" onClick={() => dismissToast(toast.id)} aria-label="Dismiss notification">
              <X size={13} />
            </button>
          </div>
        ))}
      </div>
    </main>
  );
}

// Build a human-readable summary of the agent's edit plan from its script entries.
// Collapses consecutive windows that picked the same camera into one "shot" line so the
// user sees the visual structure (which cameras, in what order, for how long) rather
// than per-window noise. Returns markdown-ish text suitable for the chat bubble.
function summarizeAgentPlan(
  script: AgentVideoScriptEntry[],
  lookPreset?: VideoFilterPreset,
  colorGrade?: AgentColorGrade | null,
  videoEffects?: AgentVideoEffects | null,
): string {
  if (!script.length) return "No shots planned.";
  type Shot = { trackName: string; startSeconds: number; endSeconds: number; windowCount: number; sampleReason?: string; sampleIntent?: string };
  const shots: Shot[] = [];
  for (const entry of script) {
    const name = entry.chosenTrackName ?? (entry.decision === "black" ? "(no camera — black)" : "(unknown)");
    const last = shots[shots.length - 1];
    if (last && last.trackName === name) {
      last.endSeconds = entry.endSeconds;
      last.windowCount += 1;
    } else {
      // Pull a one-shot rationale: prefer a short fragment after "for " in the reason,
      // or the model's edit-intent line from dataProvided. Best-effort, no failure mode.
      const intentLine = entry.dataProvided.find((line) => line.startsWith("Edit intent:"))?.replace(/^Edit intent:\s*/, "");
      shots.push({
        trackName: name,
        startSeconds: entry.startSeconds,
        endSeconds: entry.endSeconds,
        windowCount: 1,
        sampleReason: entry.reason || undefined,
        sampleIntent: intentLine,
      });
    }
  }
  // Show the agent's custom grade when present (richer than the preset name); otherwise
  // fall back to the named preset; otherwise note that no look was applied.
  let lookLine: string;
  if (colorGrade) {
    const gradeBits: string[] = [];
    if (colorGrade.contrast != null) gradeBits.push(`contrast ${colorGrade.contrast.toFixed(2)}`);
    if (colorGrade.saturation != null) gradeBits.push(`saturation ${colorGrade.saturation.toFixed(2)}`);
    if (colorGrade.brightness != null && colorGrade.brightness !== 0) gradeBits.push(`brightness ${colorGrade.brightness.toFixed(2)}`);
    if (colorGrade.gamma != null && colorGrade.gamma !== 1) gradeBits.push(`gamma ${colorGrade.gamma.toFixed(2)}`);
    if (colorGrade.rgbMix) {
      const { rr, gg, bb } = colorGrade.rgbMix;
      const mix = [rr, gg, bb].filter((v) => v != null).map((v) => v!.toFixed(2)).join(" / ");
      if (mix) gradeBits.push(`RGB ${mix}`);
    }
    if (colorGrade.hueShift != null && Math.abs(colorGrade.hueShift) >= 1) gradeBits.push(`hue ${colorGrade.hueShift.toFixed(0)}°`);
    if (colorGrade.vignette != null && colorGrade.vignette > 0.05) gradeBits.push(`vignette ${colorGrade.vignette.toFixed(2)}`);
    if (colorGrade.blur != null && colorGrade.blur > 0.5) gradeBits.push(`blur ${colorGrade.blur.toFixed(1)}`);
    if (colorGrade.sharpen != null && colorGrade.sharpen > 0.05) gradeBits.push(`sharpen ${colorGrade.sharpen.toFixed(2)}`);
    if (colorGrade.grain != null && colorGrade.grain > 0.5) gradeBits.push(`grain ${colorGrade.grain.toFixed(1)}`);
    const gradeName = colorGrade.name?.trim() || lookPreset || "custom";
    const params = gradeBits.length ? gradeBits.join(", ") : "neutral parameters";
    const rationale = colorGrade.reason?.trim();
    lookLine = rationale
      ? `Look: ${gradeName} (custom grade) — ${params}.\nWhy: ${rationale}`
      : `Look: ${gradeName} (custom grade) — ${params}.`;
  } else if (lookPreset && lookPreset !== "none") {
    lookLine = `Look: ${lookPreset} — applied as a global color grade on the final render.`;
  } else {
    lookLine = `Look: none — no color grade applied (override with a Look chip if you want one).`;
  }
  const shotLines = shots.map((shot, index) => {
    const duration = Math.max(0, shot.endSeconds - shot.startSeconds);
    const detail = shot.sampleIntent ? ` (${shot.sampleIntent})` : "";
    return `${index + 1}. ${shot.trackName} ${formatTime(shot.startSeconds)}–${formatTime(shot.endSeconds)} (${duration.toFixed(1)}s)${detail}`;
  });
  // Effects line: fade in/out + speed, with the agent's rationale when present.
  let effectsLine: string | undefined;
  if (videoEffects) {
    const bits: string[] = [];
    if (videoEffects.fadeInSeconds != null && videoEffects.fadeInSeconds > 0) bits.push(`fade in ${videoEffects.fadeInSeconds.toFixed(1)}s`);
    if (videoEffects.fadeOutSeconds != null && videoEffects.fadeOutSeconds > 0) bits.push(`fade out ${videoEffects.fadeOutSeconds.toFixed(1)}s`);
    if (videoEffects.speedFactor != null && Math.abs(videoEffects.speedFactor - 1) >= 0.005) bits.push(`speed ${videoEffects.speedFactor.toFixed(2)}x`);
    if (bits.length) {
      const rationale = videoEffects.reason?.trim();
      effectsLine = rationale
        ? `Effects: ${bits.join(", ")}.\nWhy: ${rationale}`
        : `Effects: ${bits.join(", ")}.`;
    }
  }
  const head = effectsLine
    ? `${lookLine}\n\n${effectsLine}\n\nVisual plan — ${shots.length} shot${shots.length === 1 ? "" : "s"}:`
    : `${lookLine}\n\nVisual plan — ${shots.length} shot${shots.length === 1 ? "" : "s"}:`;
  return [head, ...shotLines].join("\n");
}

// Chat summary for the single-clip direct-edit path. No shot list (it's one clip);
// shows the look name, the grade params if present, the effects, and a tag noting
// whether each came from the vision model or the keyword detector.
function summarizeClipEditResult(
  clipName: string,
  lookPreset?: VideoFilterPreset,
  colorGrade?: AgentColorGrade | null,
  videoEffects?: AgentVideoEffects | null,
  sourceSummary?: string,
): string {
  const lines: string[] = [`Updated "${clipName}" in place.`];
  if (colorGrade) {
    const bits: string[] = [];
    if (colorGrade.contrast != null) bits.push(`contrast ${colorGrade.contrast.toFixed(2)}`);
    if (colorGrade.saturation != null) bits.push(`saturation ${colorGrade.saturation.toFixed(2)}`);
    if (colorGrade.brightness != null && colorGrade.brightness !== 0) bits.push(`brightness ${colorGrade.brightness.toFixed(2)}`);
    if (colorGrade.gamma != null && colorGrade.gamma !== 1) bits.push(`gamma ${colorGrade.gamma.toFixed(2)}`);
    if (colorGrade.rgbMix) {
      const { rr, gg, bb } = colorGrade.rgbMix;
      const mix = [rr, gg, bb].filter((v) => v != null).map((v) => v!.toFixed(2)).join(" / ");
      if (mix) bits.push(`RGB ${mix}`);
    }
    if (colorGrade.vignette != null && colorGrade.vignette > 0.05) bits.push(`vignette ${colorGrade.vignette.toFixed(2)}`);
    if (colorGrade.sharpen != null && colorGrade.sharpen > 0.05) bits.push(`sharpen ${colorGrade.sharpen.toFixed(2)}`);
    if (colorGrade.grain != null && colorGrade.grain > 0.5) bits.push(`grain ${colorGrade.grain.toFixed(1)}`);
    const gradeName = colorGrade.name?.trim() || lookPreset || "custom";
    lines.push(`Look: ${gradeName} — ${bits.length ? bits.join(", ") : "neutral"}.`);
    if (colorGrade.reason?.trim()) lines.push(`Why: ${colorGrade.reason.trim()}`);
  } else if (lookPreset && lookPreset !== "none") {
    lines.push(`Look: ${lookPreset} preset.`);
  }
  if (videoEffects) {
    const bits: string[] = [];
    if (videoEffects.fadeInSeconds != null && videoEffects.fadeInSeconds > 0) bits.push(`fade in ${videoEffects.fadeInSeconds.toFixed(1)}s`);
    if (videoEffects.fadeOutSeconds != null && videoEffects.fadeOutSeconds > 0) bits.push(`fade out ${videoEffects.fadeOutSeconds.toFixed(1)}s`);
    if (videoEffects.speedFactor != null && Math.abs(videoEffects.speedFactor - 1) >= 0.005) bits.push(`speed ${videoEffects.speedFactor.toFixed(2)}x`);
    if (bits.length) {
      lines.push(`Effects: ${bits.join(", ")}.`);
      if (videoEffects.reason?.trim()) lines.push(`Why: ${videoEffects.reason.trim()}`);
    }
  }
  if (sourceSummary) lines.push(`(${sourceSummary})`);
  return lines.join("\n");
}

// Pick "nice" tick marks (1/2/5/10/15/30/60... seconds) so the ruler shows ~6-10 labels.
function buildRulerTicks(duration: number): number[] {
  if (!(duration > 0)) return [];
  const steps = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600];
  const target = duration / 8;
  const step = steps.find((value) => value >= target) ?? Math.ceil(target / 60) * 60;
  const ticks: number[] = [];
  for (let t = 0; t <= duration + 0.001; t += step) ticks.push(Math.round(t * 1000) / 1000);
  return ticks;
}

// DAW-style time ruler at the top of the timeline. Drag to select a region, click to seek.
function TimeRuler({
  duration,
  playhead,
  selection,
  loopActive,
  bpm,
  onSeek,
  onSelect,
  onClear,
  onToggleLoop,
}: {
  duration: number;
  playhead: number;
  selection?: { start: number; end: number };
  loopActive: boolean;
  bpm?: number;
  onSeek: (seconds: number) => void;
  onSelect: (start: number, end: number) => void;
  onClear: () => void;
  onToggleLoop: () => void;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ start: number; moved: boolean } | null>(null);
  const resizeRef = useRef<{ anchor: number } | null>(null);
  const bandRef = useRef<{ start: number; moved: boolean } | null>(null);
  const secondsFromClientX = (clientX: number) => {
    const rect = wrapRef.current?.getBoundingClientRect();
    if (!rect || rect.width <= 0) return 0;
    return Math.max(0, Math.min(duration, ((clientX - rect.left) / rect.width) * duration));
  };
  // Drag a selection edge: keep the opposite edge fixed and move this one to the pointer.
  const handleEdgeDown = (edge: "start" | "end", event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || !selection) return;
    event.stopPropagation();
    resizeRef.current = { anchor: edge === "start" ? Math.max(selection.start, selection.end) : Math.min(selection.start, selection.end) };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const handleEdgeMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const resize = resizeRef.current;
    if (!resize) return;
    event.stopPropagation();
    onSelect(resize.anchor, secondsFromClientX(event.clientX));
  };
  const handleEdgeUp = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!resizeRef.current) return;
    resizeRef.current = null;
    event.stopPropagation();
    event.currentTarget.releasePointerCapture(event.pointerId);
  };
  // Click on the selection band toggles the loop; a drag inside it redraws the region.
  const handleBandDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    bandRef.current = { start: secondsFromClientX(event.clientX), moved: false };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const handleBandMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const band = bandRef.current;
    if (!band) return;
    event.stopPropagation();
    const seconds = secondsFromClientX(event.clientX);
    if (Math.abs(seconds - band.start) > 0.02) {
      band.moved = true;
      onSelect(band.start, seconds);
    }
  };
  const handleBandUp = (event: React.PointerEvent<HTMLDivElement>) => {
    const band = bandRef.current;
    if (!band) return;
    bandRef.current = null;
    event.stopPropagation();
    event.currentTarget.releasePointerCapture(event.pointerId);
    if (!band.moved) onToggleLoop();
  };
  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    dragRef.current = { start: secondsFromClientX(event.clientX), moved: false };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const seconds = secondsFromClientX(event.clientX);
    if (Math.abs(seconds - drag.start) > 0.02) {
      drag.moved = true;
      onSelect(drag.start, seconds);
    }
  };
  const handlePointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
    const seconds = secondsFromClientX(event.clientX);
    if (drag.moved) {
      onSelect(drag.start, seconds);
    } else {
      // Click without drag = just move the playhead. The selection persists until the
      // user explicitly drags a new one (or presses Escape).
      onSeek(seconds);
    }
  };
  const ticks = buildRulerTicks(duration);
  // Bar marks when the session has a tempo. Memoized: the ruler re-renders at
  // playhead rate, but the bar grid only changes with bpm/duration.
  const barMarks = useMemo(() => {
    if (!bpm || bpm <= 0 || duration <= 0) return [];
    const barSeconds = 240 / bpm;
    const totalBars = Math.floor(duration / barSeconds) + 1;
    const labelEvery = totalBars > 96 ? 8 : totalBars > 48 ? 4 : totalBars > 24 ? 2 : 1;
    const renderEvery = totalBars > 240 ? labelEvery : 1;
    const marks: { bar: number; left: number; labeled: boolean }[] = [];
    for (let bar = 1; bar <= totalBars; bar++) {
      if ((bar - 1) % renderEvery !== 0) continue;
      marks.push({
        bar,
        left: (((bar - 1) * barSeconds) / duration) * 100,
        labeled: (bar - 1) % labelEvery === 0,
      });
    }
    return marks;
  }, [bpm, duration]);
  const cursorPct = duration > 0 ? Math.max(0, Math.min(100, (playhead / duration) * 100)) : 0;
  const selStart = selection ? Math.min(selection.start, selection.end) : 0;
  const selEnd = selection ? Math.max(selection.start, selection.end) : 0;
  const selLeftPct = duration > 0 ? (selStart / duration) * 100 : 0;
  const selWidthPct = duration > 0 ? ((selEnd - selStart) / duration) * 100 : 0;
  return (
    <div className="time-ruler">
      <div className="time-ruler-spacer" />
      <div
        ref={wrapRef}
        className="time-ruler-track"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        title="Drag to select a region. Click to set the playhead."
      >
        {ticks.map((tick) => (
          <div key={tick} className="time-ruler-tick" style={{ left: `${duration > 0 ? (tick / duration) * 100 : 0}%` }}>
            <span>{formatTime(tick)}</span>
          </div>
        ))}
        {barMarks.map((mark) => (
          <div
            key={`bar-${mark.bar}`}
            className={`time-ruler-bar ${mark.labeled ? "labeled" : ""}`}
            style={{ left: `${mark.left}%` }}
          >
            {mark.labeled ? <span>{mark.bar}</span> : null}
          </div>
        ))}
        {selection && selWidthPct > 0.05 ? (
          <div
            className={`time-ruler-selection ${loopActive ? "active" : ""}`}
            style={{ left: `${selLeftPct}%`, width: `${selWidthPct}%` }}
            onPointerDown={handleBandDown}
            onPointerMove={handleBandMove}
            onPointerUp={handleBandUp}
            title={loopActive ? "Loop active. Click to disable." : "Click to loop this region."}
          >
            <div
              className="time-ruler-handle start"
              onPointerDown={(event) => handleEdgeDown("start", event)}
              onPointerMove={handleEdgeMove}
              onPointerUp={handleEdgeUp}
              title="Drag to move the region start"
            />
            <div
              className="time-ruler-handle end"
              onPointerDown={(event) => handleEdgeDown("end", event)}
              onPointerMove={handleEdgeMove}
              onPointerUp={handleEdgeUp}
              title="Drag to move the region end"
            />
          </div>
        ) : null}
        <div className="time-ruler-playhead" style={{ left: `${cursorPct}%` }} />
      </div>
    </div>
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

function defaultVideoLayout(index = 0): VideoLayout {
  if (index === 0) {
    return {
      x: 0,
      y: 0,
      width: 100,
      height: 100,
      cropTop: 0,
      cropRight: 0,
      cropBottom: 0,
      cropLeft: 0,
      opacity: 1,
      rotation: 0,
      zIndex: 0,
      brightness: 1,
      contrast: 1,
      saturation: 1,
      blur: 0,
      preset: "none",
      exposure: 0,
      highlights: 0,
      shadows: 0,
      temperature: 0,
      tint: 0,
      gamma: 1,
      vignette: 0,
      sharpen: 0,
      grain: 0,
    };
  }
  const slot = (index - 1) % 4;
  const x = slot === 0 || slot === 2 ? 64 : 4;
  const y = slot < 2 ? 5 : 55;
  return { ...defaultVideoLayout(0), x, y, width: 32, height: 40, zIndex: index };
}

function normalizeVideoLayout(layout?: Partial<VideoLayout>, index = 0): VideoLayout {
  const base = defaultVideoLayout(index);
  const clamp = (value: unknown, min: number, max: number, fallback: number) => {
    const number = typeof value === "number" && Number.isFinite(value) ? value : fallback;
    return Math.max(min, Math.min(max, number));
  };
  const width = clamp(layout?.width, 1, 300, base.width);
  const height = clamp(layout?.height, 1, 300, base.height);
  return {
    x: clamp(layout?.x, -300, 300, base.x),
    y: clamp(layout?.y, -300, 300, base.y),
    width,
    height,
    cropTop: clamp(layout?.cropTop, 0, 45, base.cropTop),
    cropRight: clamp(layout?.cropRight, 0, 45, base.cropRight),
    cropBottom: clamp(layout?.cropBottom, 0, 45, base.cropBottom),
    cropLeft: clamp(layout?.cropLeft, 0, 45, base.cropLeft),
    opacity: clamp(layout?.opacity, 0, 1, base.opacity),
    rotation: clamp(layout?.rotation, -180, 180, base.rotation),
    zIndex: Math.round(clamp(layout?.zIndex, -20, 20, base.zIndex)),
    brightness: clamp(layout?.brightness, 0.2, 2, base.brightness),
    contrast: clamp(layout?.contrast, 0.2, 2, base.contrast),
    saturation: clamp(layout?.saturation, 0, 2, base.saturation),
    blur: clamp(layout?.blur, 0, 10, base.blur),
    preset: layout?.preset ?? base.preset,
    exposure: clamp(layout?.exposure, -1, 1, base.exposure ?? 0),
    highlights: clamp(layout?.highlights, -1, 1, base.highlights ?? 0),
    shadows: clamp(layout?.shadows, -1, 1, base.shadows ?? 0),
    temperature: clamp(layout?.temperature, -1, 1, base.temperature ?? 0),
    tint: clamp(layout?.tint, -1, 1, base.tint ?? 0),
    gamma: clamp(layout?.gamma, 0.5, 1.8, base.gamma ?? 1),
    vignette: clamp(layout?.vignette, 0, 1, base.vignette ?? 0),
    sharpen: clamp(layout?.sharpen, 0, 2, base.sharpen ?? 0),
    grain: clamp(layout?.grain, 0, 1, base.grain ?? 0),
  };
}

function defaultVideoCanvas(): VideoCanvas {
  return { width: 1280, height: 720, background: "#000000" };
}

function normalizeVideoCanvas(canvas?: Partial<VideoCanvas>): VideoCanvas {
  const width = typeof canvas?.width === "number" && Number.isFinite(canvas.width) ? canvas.width : 1280;
  const height = typeof canvas?.height === "number" && Number.isFinite(canvas.height) ? canvas.height : 720;
  const background = typeof canvas?.background === "string" && /^#[0-9a-f]{6}$/i.test(canvas.background)
    ? canvas.background
    : "#000000";
  return {
    width: Math.max(240, Math.min(3840, Math.round(width))),
    height: Math.max(240, Math.min(3840, Math.round(height))),
    background,
  };
}

function emitCameraPreviewLayout(payload: CameraPreviewLayoutEvent) {
  return emit("camera-preview:clip-layout", payload).catch(() => undefined);
}

function emitCameraPreviewCanvas(canvas: VideoCanvas) {
  return emit("camera-preview:canvas", { canvas: normalizeVideoCanvas(canvas) } satisfies CameraPreviewCanvasEvent).catch(() => undefined);
}

function readVideoEditorPayloadFromUrl(): VideoEditorWindowPayload | undefined {
  const params = new URLSearchParams(window.location.search);
  const sessionId = params.get("sessionId") ?? "";
  if (!sessionId) return undefined;
  const trackIds = (params.get("trackIds") ?? "").split(",").map((item) => item.trim()).filter(Boolean);
  const start = Number(params.get("start"));
  const end = Number(params.get("end"));
  return {
    sessionId,
    trackIds,
    range: Number.isFinite(start) && Number.isFinite(end) ? { start: Math.min(start, end), end: Math.max(start, end) } : undefined,
    playhead: 0,
  };
}

function readMixerPayloadFromUrl(): MixerWindowPayload | undefined {
  const params = new URLSearchParams(window.location.search);
  const sessionId = params.get("sessionId") ?? "";
  return sessionId ? { sessionId } : undefined;
}

/** Floating video monitor window — plays the latest agent render. Receives the
 *  path via the URL on first open and via `video-monitor:load` events after. */
/** Secondary windows (mixer / monitor / video editor) don't own the transport, so a
 *  Space press there is forwarded to the main window via `transport:toggle`. Ignores
 *  Space while typing or on a focused control, matching the main window's behavior. */
function useSpaceToggleTransport() {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== " " || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest('input, textarea, select, [contenteditable="true"], button, a, [role="menu"]')) return;
      event.preventDefault();
      void emit("transport:toggle").catch(() => undefined);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

/** Floating multicam Video Monitor — a synced grid of every video clip in the
 *  session (camera angles + agent renders), driven by the engine transport. */
// macOS Photos–style pill slider: label left, value right, a lighter fill that
// grows from the center (bipolar) or left (unipolar). The whole pill is the
// drag area (a transparent range input overlays it); double-click resets.
type AdjustSliderDef = { key: string; label: string; min: number; max: number; step: number; def: number };

function PhotoSlider({ def, value, onChange }: { def: AdjustSliderDef; value: number; onChange: (v: number) => void }) {
  const { label, min, max, step, def: dflt } = def;
  const span = max - min || 1;
  const frac = Math.min(1, Math.max(0, (value - min) / span));
  const bipolar = min < 0 && max > 0;
  const origin = bipolar ? (0 - min) / span : 0;
  const a = Math.min(origin, frac);
  const b = Math.max(origin, frac);
  const r = Math.round(value * 100) / 100;
  const display = bipolar ? `${r > 0 ? "+" : ""}${r.toFixed(2)}` : r.toFixed(2);
  return (
    <div className="ph-slider" onDoubleClick={() => onChange(dflt)}>
      <div className="ph-slider-fill" style={{ left: `${a * 100}%`, width: `${(b - a) * 100}%` }} />
      <div className="ph-slider-tick" style={{ left: `${frac * 100}%` }} />
      <span className="ph-slider-label">{label}</span>
      <span className="ph-slider-value">{display}</span>
      <input
        className="ph-slider-input"
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
      />
    </div>
  );
}

type AdjustSectionDef = { id: string; title: string; icon: ReactNode; auto?: Record<string, number>; sliders: AdjustSliderDef[] };

const ADJUST_SECTIONS: AdjustSectionDef[] = [
  {
    id: "light", title: "Light", icon: <Sun size={14} />, auto: { contrast: 1.12, brightness: 1.06, shadows: 0.12 },
    sliders: [
      { key: "exposure", label: "Exposure", min: -1, max: 1, step: 0.01, def: 0 },
      { key: "highlights", label: "Highlights", min: -1, max: 1, step: 0.01, def: 0 },
      { key: "shadows", label: "Shadows", min: -1, max: 1, step: 0.01, def: 0 },
      { key: "brightness", label: "Brightness", min: 0.2, max: 2, step: 0.01, def: 1 },
      { key: "contrast", label: "Contrast", min: 0.2, max: 2, step: 0.01, def: 1 },
      { key: "gamma", label: "Black Point", min: 0.5, max: 1.8, step: 0.01, def: 1 },
    ],
  },
  {
    id: "color", title: "Color", icon: <Palette size={14} />, auto: { saturation: 1.18 },
    sliders: [
      { key: "saturation", label: "Saturation", min: 0, max: 2, step: 0.01, def: 1 },
      { key: "temperature", label: "Temperature", min: -1, max: 1, step: 0.01, def: 0 },
      { key: "tint", label: "Tint", min: -1, max: 1, step: 0.01, def: 0 },
    ],
  },
  { id: "sharpen", title: "Sharpen", icon: <Focus size={14} />, sliders: [{ key: "sharpen", label: "Intensity", min: 0, max: 2, step: 0.01, def: 0 }] },
  { id: "noise", title: "Noise Reduction", icon: <Aperture size={14} />, sliders: [{ key: "grain", label: "Amount", min: 0, max: 1, step: 0.01, def: 0 }] },
  {
    id: "vignette", title: "Vignette", icon: <Circle size={14} />,
    sliders: [
      { key: "vignette", label: "Strength", min: 0, max: 1, step: 0.01, def: 0 },
      { key: "blur", label: "Blur", min: 0, max: 10, step: 0.1, def: 0 },
    ],
  },
];

function AdjustSection({ section, grade, open, onToggleOpen, onAdjust }: {
  section: AdjustSectionDef; grade: VideoLayout; open: boolean; onToggleOpen: () => void; onAdjust: (patch: Record<string, number>) => void;
}) {
  const val = (k: string) => (grade as unknown as Record<string, number>)[k];
  const isActive = section.sliders.some((s) => Math.abs((val(s.key) ?? s.def) - s.def) > 1e-4);
  const resetSection = () => {
    const p: Record<string, number> = {};
    section.sliders.forEach((s) => { p[s.key] = s.def; });
    onAdjust(p);
  };
  const applyAuto = () => { if (section.auto) onAdjust(section.auto); };
  return (
    <div className={`ph-section ${open ? "open" : ""}`}>
      <div className="ph-section-head">
        <button type="button" className="ph-chevron" onClick={onToggleOpen} aria-label="Expand section">
          {open ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        </button>
        <span className="ph-section-icon">{section.icon}</span>
        <button type="button" className="ph-section-title" onClick={onToggleOpen}>{section.title}</button>
        <button type="button" className="ph-icon-btn" title="Reset section" onClick={resetSection}><RotateCcw size={11} /></button>
        {section.auto ? <button type="button" className="ph-auto" title="Auto" onClick={applyAuto}>AUTO</button> : null}
        <button
          type="button"
          className={`ph-check ${isActive ? "on" : ""}`}
          title={isActive ? "Reset this section" : "Apply auto"}
          onClick={() => (isActive ? resetSection() : applyAuto())}
        >
          {isActive ? <Check size={10} strokeWidth={3} /> : null}
        </button>
      </div>
      {open ? (
        <div className="ph-section-body">
          {section.sliders.map((s) => (
            <PhotoSlider key={s.key} def={s} value={val(s.key) ?? s.def} onChange={(v) => onAdjust({ [s.key]: v })} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

// A monitor grid tile. Geometry (crop-zoom/rotate/opacity) stays in CSS; color
// is CSS + SVG filters applied directly to the <video> (works on cross-origin
// media, unlike WebGL). Look preset goes on the surrounding box.
function MonitorTile({ clip, grade, selected, registerVideo, onClick }: {
  clip: { id: string; trackName: string; path: string; cl: number; cr: number; ct: number; cb: number; rot: number; op: number };
  grade: VideoLayout;
  selected: boolean;
  registerVideo: (id: string, el: HTMLVideoElement | null) => void;
  onClick: () => void;
}) {
  const sx = 1 / Math.max(0.1, 1 - clip.cl - clip.cr);
  const sy = 1 / Math.max(0.1, 1 - clip.ct - clip.cb);
  const ox = ((clip.cl + (1 - clip.cr)) / 2) * 100;
  const oy = ((clip.ct + (1 - clip.cb)) / 2) * 100;
  const cropped = clip.cl > 0 || clip.cr > 0 || clip.ct > 0 || clip.cb > 0 || clip.rot !== 0;
  return (
    <div
      className={`monitor-tile ${selected ? "selected" : ""}`}
      onClick={onClick}
      title={`Click to adjust ${clip.trackName}`}
      style={{ filter: presetCss(grade) }}
    >
      <video
        ref={(el) => registerVideo(clip.id, el)}
        src={convertFileSrc(clip.path)}
        muted
        playsInline
        preload="auto"
        style={{
          objectFit: cropped ? "cover" : "contain",
          transform: `rotate(${clip.rot}deg) scale(${sx}, ${sy})`,
          transformOrigin: `${ox}% ${oy}%`,
          opacity: clip.op,
          filter: cssAdjustFilter(grade),
        }}
      />
      {(() => { const wb = whiteBalanceStyle(grade); return wb ? <div style={wb} /> : null; })()}
      {(grade.vignette ?? 0) > 0 ? <div style={vignetteStyle(grade)} /> : null}
      {(grade.grain ?? 0) > 0 ? <div style={grainStyle(grade)} /> : null}
      <span className="monitor-tile-label">{clip.trackName}</span>
    </div>
  );
}

export function VideoMonitorApp() {
  useSpaceToggleTransport();
  const initialSid = new URLSearchParams(window.location.search).get("sessionId") ?? "";
  const [sessionId, setSessionId] = useState(initialSid);
  const [project, setProject] = useState<MixProject>();
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const playRef = useRef<{ sample: number; running: boolean }>({ sample: 0, running: false });
  const videoEls = useRef<Map<string, HTMLVideoElement>>(new Map());
  // Export controls (shape + resolution + fit/fill).
  const [exportAspect, setExportAspect] = useState("original");
  const [exportRes, setExportRes] = useState<number | "source">("source");
  const [exportMode, setExportMode] = useState<"fit" | "fill">("fit");
  const [exporting, setExporting] = useState(false);
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const [shareOpen, setShareOpen] = useState(false);
  // Photos-style adjust: the clip being edited and its live (un-persisted) grade.
  const [focusedClipId, setFocusedClipId] = useState<string | null>(null);
  const [draftLayout, setDraftLayout] = useState<VideoLayout | null>(null);
  const [openSections, setOpenSections] = useState<Record<string, boolean>>({ light: true, color: true });
  const persistTimer = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    void api.getSession(sessionId).then((p) => { if (!cancelled) setProject(p); }).catch(() => undefined);
    void api.getVideoSelection(sessionId).then((ids) => { if (!cancelled) setSelectedIds(ids); }).catch(() => undefined);
    return () => { cancelled = true; };
  }, [sessionId]);

  useEffect(() => {
    const unsubs: (() => void)[] = [];
    void listen<{ sessionId: string }>("video-monitor:session", (e) => setSessionId(e.payload.sessionId)).then((fn) => unsubs.push(fn));
    void listen<{ trackIds: string[] }>("video-monitor:selection", (e) => setSelectedIds(e.payload.trackIds ?? [])).then((fn) => unsubs.push(fn));
    void api.onSessionExternallyUpdated((e) => setProject((prev) => (prev && prev.session.id === e.sessionId ? e.project : prev))).then((fn) => unsubs.push(fn));
    void api.onPlayhead((e) => { playRef.current = { sample: e.sample, running: e.running }; }).then((fn) => unsubs.push(fn));
    return () => unsubs.forEach((f) => f());
  }, []);

  const sr = project?.session.sampleRate ?? 48000;
  type MonitorClip = { id: string; trackId: string; trackName: string; path: string; start: number; end: number; offset: number; cl: number; cr: number; ct: number; cb: number; rot: number; op: number; layout: VideoLayout; ti: number; ci: number };
  const allClips = useMemo(() => {
    const s = project?.session;
    if (!s) return [] as MonitorClip[];
    const byId = new Map((s.videoSourceFiles ?? []).map((v) => [v.id, v]));
    const out: MonitorClip[] = [];
    s.tracks.forEach((t, ti) => {
      if (t.kind !== "video") return;
      (t.videoClips ?? []).forEach((c, ci) => {
        const src = byId.get(c.videoSourceFileId);
        if (!src?.path) return;
        out.push({
          id: c.id, trackId: t.id, trackName: t.name, path: src.path,
          start: c.startSample / sr, end: c.endSample / sr, offset: (c.sourceOffsetMs ?? 0) / 1000,
          cl: (c.layout?.cropLeft ?? 0) / 100, cr: (c.layout?.cropRight ?? 0) / 100,
          ct: (c.layout?.cropTop ?? 0) / 100, cb: (c.layout?.cropBottom ?? 0) / 100,
          rot: c.layout?.rotation ?? 0, op: c.layout?.opacity ?? 1,
          layout: normalizeVideoLayout(c.layout), ti, ci,
        });
      });
    });
    return out;
  }, [project, sr]);

  // Follow the main window's track selection: aim the Adjust panel at the
  // selected track's clip so sliders target what the user picked there. Keep a
  // manual in-monitor focus if it already belongs to a selected track (so
  // clicking a specific tile on a multi-clip track isn't overridden).
  useEffect(() => {
    if (selectedIds.length === 0) return;
    const current = allClips.find((c) => c.id === focusedClipId);
    if (current && selectedIds.includes(current.trackId)) return;
    const target = allClips.find((c) => selectedIds.includes(c.trackId));
    if (target) {
      setFocusedClipId(target.id);
      setDraftLayout(target.layout);
    }
  }, [selectedIds, allClips, focusedClipId]);

  // Show only the selected video tracks when a selection exists; otherwise all.
  const clips = useMemo(() => {
    const selected = allClips.filter((c) => selectedIds.includes(c.trackId));
    return selected.length > 0 ? selected : allClips;
  }, [allClips, selectedIds]);

  // Keep every tile's playback locked to the engine playhead.
  useEffect(() => {
    let raf = 0;
    const tick = () => {
      const { sample, running } = playRef.current;
      const pos = sample / sr;
      for (const c of clips) {
        const el = videoEls.current.get(c.id);
        if (!el) continue;
        const active = pos >= c.start && pos <= c.end;
        const target = Math.max(0, pos - c.start + c.offset);
        if (active && running) {
          if (Math.abs(el.currentTime - target) > 0.3) el.currentTime = target;
          if (el.paused) void el.play().catch(() => undefined);
        } else {
          if (!el.paused) el.pause();
          if (active && Math.abs(el.currentTime - target) > 0.05) el.currentTime = target;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [clips, sr]);

  const cols = Math.max(1, Math.ceil(Math.sqrt(clips.length || 1)));

  // The clip to export: prefer the agent's rendered output, else the focused/only clip.
  const exportClip = useMemo(() => {
    const agent = allClips.find((c) => c.trackName === "Agent video edit" || c.trackName.startsWith("Agent Edit"));
    if (agent) return agent;
    if (clips.length === 1) return clips[0];
    return allClips.find((c) => selectedIds.includes(c.trackId)) ?? null;
  }, [allClips, clips, selectedIds]);

  const ASPECTS: { id: string; label: string }[] = [
    { id: "original", label: "Original" },
    { id: "16:9", label: "16:9 Landscape" },
    { id: "9:16", label: "9:16 Vertical" },
    { id: "1:1", label: "1:1 Square" },
    { id: "4:5", label: "4:5 Portrait" },
    { id: "4:3", label: "4:3 Classic" },
    { id: "21:9", label: "21:9 Cinema" },
  ];
  const RESOLUTIONS: { id: number | "source"; label: string }[] = [
    { id: "source", label: "Source" },
    { id: 3840, label: "4K" },
    { id: 2560, label: "1440p" },
    { id: 1920, label: "1080p" },
    { id: 1280, label: "720p" },
    { id: 854, label: "480p" },
  ];
  // Preview the output dimensions for the chosen shape + resolution (when not Source).
  const exportDims = useMemo(() => {
    if (exportRes === "source") return null;
    const long = exportRes;
    let aw = 16, ah = 9;
    if (exportAspect !== "original") {
      const [a, b] = exportAspect.split(":").map(Number);
      if (a && b) { aw = a; ah = b; }
    } else { aw = 16; ah = 9; }
    const r = aw / ah;
    const w = r >= 1 ? long : Math.round(long * r);
    const h = r >= 1 ? Math.round(long / r) : long;
    const even = (n: number) => n - (n % 2);
    return exportAspect === "original" ? `≤${long}px` : `${even(w)}×${even(h)}`;
  }, [exportAspect, exportRes]);

  async function doExport() {
    if (!exportClip) { setExportStatus("Nothing to export — render an Agent Edit first."); return; }
    const suffix = exportAspect === "original" ? "" : `-${exportAspect.replace(":", "x")}`;
    const outputPath = await save({
      title: "Export video",
      defaultPath: `automixer-export${suffix}.mp4`,
      filters: [{ name: "MP4 Video", extensions: ["mp4"] }],
    });
    if (!outputPath) return;
    setExporting(true);
    setExportStatus("Exporting…");
    try {
      const res = await api.exportVideo(
        exportClip.path,
        outputPath,
        exportAspect,
        exportRes === "source" ? undefined : exportRes,
        exportMode,
      );
      setExportStatus(`Exported to ${res.path}`);
    } catch (error) {
      setExportStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setExporting(false);
    }
  }

  const focusedClip = useMemo(() => allClips.find((c) => c.id === focusedClipId) ?? null, [allClips, focusedClipId]);
  const focusGrade: VideoLayout | null = focusedClip ? (draftLayout ?? focusedClip.layout) : null;

  function focusClip(c: MonitorClip) {
    setFocusedClipId(c.id);
    setDraftLayout(c.layout);
    void emit("video-monitor:select", { trackId: c.trackId }).catch(() => undefined);
  }

  // Update the focused clip's grade: instant local preview + debounced persist
  // (via applyPatch) so the change survives reloads and downstream renders use it.
  function adjust(patch: Partial<VideoLayout>) {
    if (!focusedClip) return;
    const next = normalizeVideoLayout({ ...(draftLayout ?? focusedClip.layout), ...patch });
    setDraftLayout(next);
    window.clearTimeout(persistTimer.current);
    persistTimer.current = window.setTimeout(() => {
      const s = project?.session;
      const track = s?.tracks[focusedClip.ti];
      const before = track?.videoClips ?? [];
      if (!s || !before.some((c) => c.id === focusedClip.id)) return;
      const arr = before.map((c) => (c.id === focusedClip.id ? { ...c, layout: next } : c));
      void api.applyPatch(
        s.id,
        [{ op: "replace", path: `/tracks/${focusedClip.ti}/videoClips`, value: arr }],
        [{ op: "replace", path: `/tracks/${focusedClip.ti}/videoClips`, value: before }],
        "Adjust clip",
      ).then(setProject).catch(() => undefined);
    }, 300) as unknown as number;
  }

  return (
    <div className="video-monitor-root">
    <div className="video-monitor-body">
    <main className="video-monitor-grid" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}>
      {clips.length === 0 ? (
        <div className="video-monitor-empty">No video in this session yet — record or import video, or ask the assistant to edit a clip.</div>
      ) : (
        clips.map((c) => (
          <MonitorTile
            key={c.id}
            clip={c}
            grade={c.id === focusedClipId && draftLayout ? draftLayout : c.layout}
            selected={c.id === focusedClipId || selectedIds.includes(c.trackId)}
            registerVideo={(id, el) => { if (el) videoEls.current.set(id, el); else videoEls.current.delete(id); }}
            onClick={() => focusClip(c)}
          />
        ))
      )}
    </main>
    {focusGrade && focusedClip ? (
      <aside className="video-monitor-adjust photos">
        <div className="ph-head">
          <span>ADJUST</span>
          <button type="button" className="ph-share" onClick={() => setShareOpen(true)} title="Share / Export">
            <Share2 size={11} />
            Share
          </button>
        </div>
        <div className="ph-sections">
          {ADJUST_SECTIONS.map((sec) => (
            <AdjustSection
              key={sec.id}
              section={sec}
              grade={focusGrade}
              open={!!openSections[sec.id]}
              onToggleOpen={() => setOpenSections((o) => ({ ...o, [sec.id]: !o[sec.id] }))}
              onAdjust={(p) => adjust(p as Partial<VideoLayout>)}
            />
          ))}
        </div>
        <button
          type="button"
          className="ph-reset-all"
          onClick={() => adjust({ exposure: 0, highlights: 0, shadows: 0, gamma: 1, brightness: 1, contrast: 1, saturation: 1, temperature: 0, tint: 0, sharpen: 0, grain: 0, vignette: 0, blur: 0, preset: "none" })}
        >
          Reset Adjustments
        </button>
      </aside>
    ) : (
      <aside className="video-monitor-adjust photos empty">
        <SlidersHorizontal size={26} />
        <span>Select a clip to adjust</span>
      </aside>
    )}
    </div>
    {shareOpen ? (
      <div className="share-backdrop" onClick={() => { if (!exporting) setShareOpen(false); }}>
        <div className="share-modal" onClick={(e) => e.stopPropagation()}>
          <div className="share-modal-head">
            <Share2 size={16} />
            <span>Export Video</span>
            <button type="button" className="share-modal-close" onClick={() => setShareOpen(false)} disabled={exporting}><X size={16} /></button>
          </div>
          <div className="share-modal-body">
            <label className="share-field">
              <span>Shape</span>
              <select value={exportAspect} onChange={(e) => setExportAspect(e.target.value)} disabled={exporting}>
                {ASPECTS.map((a) => <option key={a.id} value={a.id}>{a.label}</option>)}
              </select>
            </label>
            <label className="share-field">
              <span>Resolution</span>
              <select value={String(exportRes)} onChange={(e) => setExportRes(e.target.value === "source" ? "source" : Number(e.target.value))} disabled={exporting}>
                {RESOLUTIONS.map((r) => <option key={String(r.id)} value={String(r.id)}>{r.label}</option>)}
              </select>
            </label>
            <label className="share-field">
              <span>Fit</span>
              <select value={exportMode} onChange={(e) => setExportMode(e.target.value as "fit" | "fill")} disabled={exporting} title="Fit = letterbox (show everything). Fill = crop to fill the frame.">
                <option value="fit">Fit (pad)</option>
                <option value="fill">Fill (crop)</option>
              </select>
            </label>
            {exportDims ? <div className="share-dims">Output · {exportDims}</div> : null}
            {exportStatus ? <div className="share-status">{exportStatus}</div> : null}
          </div>
          <div className="share-modal-foot">
            <button type="button" className="share-cancel" onClick={() => setShareOpen(false)} disabled={exporting}>Cancel</button>
            <button type="button" className="share-export" onClick={() => void doExport()} disabled={exporting || !exportClip}>
              {exporting ? "Exporting…" : "Export MP4"}
            </button>
          </div>
        </div>
      </div>
    ) : null}
    </div>
  );
}

export function MixerWindowApp() {
  useSpaceToggleTransport();
  const initialPayloadRef = useRef<MixerWindowPayload | undefined>(readMixerPayloadFromUrl());
  const [project, setProject] = useState<MixProject>();
  const [focusedTrackId, setFocusedTrackId] = useState<string | undefined>();
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState<string | null>(null);
  // True once the first session has loaded. Subsequent refreshes (every fader
  // tweak round-trips through the main window) update the project in place
  // without flipping back to the loading screen — that unmount/remount was
  // resetting the strip scroll and focus back to the first track.
  const loadedRef = useRef(false);
  // Timestamp of the mixer's last self-initiated edit. When the mixer changes a
  // fader it applies the authoritative result locally (publishUpdate) and pings
  // the main window, which echoes a "mixer:update" straight back. Reloading on
  // that echo re-fetches the session and resets the strip-rack scroll/focus, so
  // we ignore any echo that lands right after one of our own edits.
  const lastSelfEditRef = useRef(0);
  const session = project?.session;

  useEffect(() => {
    void loadMixerSession(initialPayloadRef.current?.sessionId);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<MixerWindowPayload>("mixer:update", (event) => {
      if (performance.now() - lastSelfEditRef.current < 1500) return;
      void loadMixerSession(event.payload.sessionId);
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  async function loadMixerSession(sessionId?: string) {
    if (!sessionId) {
      setStatus("Open the mixer from the main AutoMixer window.");
      setLoading(false);
      return;
    }
    // Only show the full-screen loading state on the very first load. Refreshes
    // update in place so the MixerDock stays mounted (scroll + focus preserved).
    if (!loadedRef.current) setLoading(true);
    try {
      const loaded = await api.getSession(sessionId);
      setProject(loaded);
      setStatus(null);
      loadedRef.current = true;
      void getCurrentWebviewWindow().setTitle(`${loaded.session.name} Mixer`).catch(() => undefined);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }

  async function publishUpdate(updated: MixProject) {
    lastSelfEditRef.current = performance.now();
    setProject(updated);
    await emit("mixer:session-updated", { sessionId: updated.session.id }).catch(() => undefined);
  }

  async function updateMixerTrack(track: Track, patch: Partial<Track>) {
    if (!session) return;
    const actions: MixAction[] = [];
    if (patch.gainDb !== undefined) actions.push({ tool: "set_track_gain" as const, trackId: track.id, gainDb: patch.gainDb });
    if (patch.pan !== undefined) actions.push({ tool: "set_track_pan" as const, trackId: track.id, pan: patch.pan });
    if (patch.muted !== undefined) actions.push({ tool: "mute_track" as const, trackId: track.id, muted: patch.muted });
    if (patch.solo !== undefined) actions.push({ tool: "solo_track" as const, trackId: track.id, solo: patch.solo });
    if (actions.length === 0) return;
    try {
      await publishUpdate(await api.applyActions(session.id, actions, "Mixer window control change"));
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function setMixerMasterGain(gainDb: number) {
    if (!session) return;
    try {
      await publishUpdate(await api.setMasterGain(session.id, gainDb));
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  if (loading) {
    return <main className="mixer-window loading">Loading mixer...</main>;
  }

  if (!session) {
    return (
      <main className="mixer-window mixer-window-empty">
        <strong>Mixer unavailable</strong>
        <span>{status ?? "No session loaded."}</span>
      </main>
    );
  }

  return (
    <main className="mixer-window">
      <div className="mixer-window-head">
        <strong>{session.name}</strong>
        <span>{session.tracks.length} tracks</span>
        {status ? <em>{status}</em> : null}
      </div>
      <MixerDock
        session={session}
        focusedTrackId={focusedTrackId}
        onFocusTrack={setFocusedTrackId}
        onChange={(track, patch) => void updateMixerTrack(track, patch)}
        masterGainDb={session.master.gainDb}
        onMasterGain={(gainDb) => void setMixerMasterGain(gainDb)}
      />
    </main>
  );
}

export function VideoEditorWindowApp() {
  useSpaceToggleTransport();
  const initialOllamaUrlRef = useRef(localStorage.getItem("autoMixer.ollamaUrl"));
  const initialOllamaModelRef = useRef(localStorage.getItem("autoMixer.ollamaModel"));
  const initialAgentVideoModelRef = useRef(localStorage.getItem("autoMixer.agentVideoModel"));
  const initialAgentVideoEditModelRef = useRef(localStorage.getItem("autoMixer.agentVideoEditModel"));
  const initialAgentVideoInstructionsRef = useRef(localStorage.getItem("autoMixer.agentVideoInstructions"));
  const initialPayloadRef = useRef<VideoEditorWindowPayload | undefined>(readVideoEditorPayloadFromUrl());
  const rangeDragRef = useRef<{ start: number; pointerId: number } | null>(null);
  const [project, setProject] = useState<MixProject>();
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [selectedTrackIds, setSelectedTrackIds] = useState<string[]>(initialPayloadRef.current?.trackIds ?? []);
  const [selectedRange, setSelectedRange] = useState<{ start: number; end: number } | undefined>(initialPayloadRef.current?.range);
  const [playhead, setPlayhead] = useState(initialPayloadRef.current?.playhead ?? 0);
  const [cameraDevices, setCameraDevices] = useState<MediaDeviceInfo[]>([]);
  const [agentIntervalSeconds, setAgentIntervalSeconds] = useState("2");
  const [agentVideoModel, setAgentVideoModel] = useState(() => initialAgentVideoModelRef.current ?? DEFAULT_AGENT_VIDEO_MODEL);
  const [agentVideoEditModel, setAgentVideoEditModel] = useState(() => initialAgentVideoEditModelRef.current ?? initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL);
  const [agentVideoInstructions, setAgentVideoInstructions] = useState(() => initialAgentVideoInstructionsRef.current ?? "");
  const [agentEditProgress, setAgentEditProgress] = useState<{ stage: string; message: string; current: number; total: number; elapsedSeconds: number } | null>(null);
  const [agentEditScript, setAgentEditScript] = useState<AgentVideoScriptEntry[]>([]);
  const [mainVideoEdit, setMainVideoEdit] = useState<MainVideoEdit>({ script: [] });
  // Export aspect ratio for the editor window (separate state from the main App).
  // "original" copies bytes; "square"/"portrait916" reencode with letterbox/pillarbox.
  const [exportAspect, setExportAspect] = useState<ExportAspect>("original");
  const [programPlaying, setProgramPlaying] = useState(false);
  const [programPreviewSize, setProgramPreviewSize] = useState<"small" | "medium" | "large">("medium");
  const [videoEditHistory, setVideoEditHistory] = useState<VideoEditHistoryItem[]>([]);
  const [videoChatMessages, setVideoChatMessages] = useState<VideoChatMessage[]>([]);
  const [videoChatDraft, setVideoChatDraft] = useState("");
  const [videoHistoryLoadedSessionId, setVideoHistoryLoadedSessionId] = useState<string | null>(null);
  const [ollamaUrl, setOllamaUrl] = useState(() => initialOllamaUrlRef.current ?? DEFAULT_OLLAMA_URL);
  const [ollamaModel, setOllamaModel] = useState(() => initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL);
  const [modelOptions, setModelOptions] = useState<string[]>(() => [initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL]);
  const session = project?.session;

  useEffect(() => {
    void bootstrapVideoEditor(initialPayloadRef.current);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<VideoEditorWindowPayload>("video-editor:update", (event) => {
      const payload = event.payload;
      setSelectedTrackIds(payload.trackIds);
      setSelectedRange(payload.range);
      setPlayhead(payload.playhead);
      void bootstrapVideoEditor(payload);
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void api.onAgentVideoProgress((event) => {
      setAgentEditProgress(event);
      setStatus(event.message);
      if (event.stage === "done" || event.stage === "error") {
        setTimeout(() => setAgentEditProgress(null), 6000);
      }
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  useEffect(() => {
    localStorage.setItem("autoMixer.ollamaUrl", ollamaUrl);
  }, [ollamaUrl]);

  useEffect(() => {
    localStorage.setItem("autoMixer.agentVideoModel", agentVideoModel);
    setModelOptions((items) => items.includes(agentVideoModel) ? items : [...items, agentVideoModel]);
  }, [agentVideoModel]);

  useEffect(() => {
    localStorage.setItem("autoMixer.agentVideoEditModel", agentVideoEditModel);
    setModelOptions((items) => items.includes(agentVideoEditModel) ? items : [...items, agentVideoEditModel]);
  }, [agentVideoEditModel]);

  useEffect(() => {
    localStorage.setItem("autoMixer.agentVideoInstructions", agentVideoInstructions);
  }, [agentVideoInstructions]);

  useEffect(() => {
    if (!session) return;
    setVideoHistoryLoadedSessionId(null);
    const raw = localStorage.getItem(`autoMixer.videoEditHistory.${session.id}`);
    if (!raw) {
      setVideoEditHistory([]);
      setVideoChatMessages([]);
      setVideoHistoryLoadedSessionId(session.id);
      return;
    }
    try {
      const parsed = JSON.parse(raw) as { history?: VideoEditHistoryItem[]; chat?: VideoChatMessage[] } | VideoEditHistoryItem[];
      if (Array.isArray(parsed)) {
        setVideoEditHistory(parsed);
        setVideoChatMessages([]);
      } else {
        setVideoEditHistory(Array.isArray(parsed.history) ? parsed.history : []);
        setVideoChatMessages(Array.isArray(parsed.chat) ? parsed.chat : []);
      }
    } catch {
      setVideoEditHistory([]);
      setVideoChatMessages([]);
    }
    setVideoHistoryLoadedSessionId(session.id);
  }, [session?.id]);

  useEffect(() => {
    if (!session || videoHistoryLoadedSessionId !== session.id) return;
    localStorage.setItem(
      `autoMixer.videoEditHistory.${session.id}`,
      JSON.stringify({ history: videoEditHistory.slice(0, 20), chat: videoChatMessages.slice(-80) })
    );
  }, [session?.id, videoHistoryLoadedSessionId, videoEditHistory, videoChatMessages]);

  useEffect(() => {
    void refreshCameraDevices();
  }, []);

  useEffect(() => {
    if (mainVideoEdit.script.length > 0 || !videoEditHistory[0]) return;
    const latest = videoEditHistory[0];
    const rangeStartSeconds = latest.script.length > 0 ? Math.min(...latest.script.map((entry) => entry.startSeconds)) : 0;
    setMainVideoEdit({ script: latest.script, outputPath: latest.outputPath, createdAt: latest.createdAt, rangeStartSeconds });
  }, [videoEditHistory, mainVideoEdit.script.length]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.code !== "Space") return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, button")) return;
      event.preventDefault();
      toggleProgramPlayback();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [selectedRange?.start, selectedRange?.end, playhead, mainVideoEdit.script]);

  async function bootstrapVideoEditor(payload?: VideoEditorWindowPayload) {
    const sessionId = payload?.sessionId;
    if (!sessionId) {
      setStatus("Open the video editor from the main AutoMixer window.");
      setLoading(false);
      return;
    }
    setLoading(true);
    try {
      const [config, loaded] = await Promise.all([api.config().catch(() => undefined), api.getSession(sessionId)]);
      if (config) {
        if (!initialOllamaUrlRef.current) setOllamaUrl(config.ollamaBaseUrl);
        if (!initialOllamaModelRef.current) {
          setOllamaModel(config.ollamaModel);
          setModelOptions((items) => Array.from(new Set([...items, config.ollamaModel])));
        }
      }
      setProject(loaded);
      const videoIds = loaded.session.tracks.filter((track) => track.kind === "video").map((track) => track.id);
      const nextTrackIds = payload?.trackIds?.filter((id) => videoIds.includes(id)) ?? [];
      setSelectedTrackIds(nextTrackIds.length > 0 ? nextTrackIds : videoIds);
      setSelectedRange(payload?.range);
      setPlayhead(payload?.playhead ?? 0);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }

  async function refreshCameraDevices() {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      setCameraDevices(devices.filter((device) => device.kind === "videoinput"));
    } catch {
      setCameraDevices([]);
    }
  }

  function selectedVideoTracks() {
    if (!session) return [];
    return session.tracks.filter((track) => track.kind === "video" && selectedTrackIds.includes(track.id));
  }

  function allVideoTracks() {
    if (!session) return [];
    return session.tracks.filter((track) => track.kind === "video");
  }

  function selectedVideoTrackIds() {
    return selectedVideoTracks().map((track) => track.id);
  }

  function buildPreviewTracks() {
    if (!session) return [];
    const videoSourceById = new Map((session.videoSourceFiles ?? []).map((source) => [source.id, source]));
    return selectedVideoTracks().map((track, trackIndex) => {
      const deviceId = track.cameraDeviceId ?? "";
      const device = cameraDevices.find((item) => item.deviceId === deviceId);
      const clips = (track.videoClips ?? []).flatMap((clip) => {
        const source = videoSourceById.get(clip.videoSourceFileId);
        if (!source) return [];
        return [{
          id: clip.id,
          name: clip.name ?? source.originalName,
          src: source.path,
          startSeconds: clip.startSample / session.sampleRate,
          endSeconds: clip.endSample / session.sampleRate,
          localTime: Math.max(0, (clip.sourceOffsetMs ?? 0) / 1000 + playhead - (clip.startSample / session.sampleRate)),
          layout: normalizeVideoLayout(clip.layout, trackIndex),
        }];
      });
      const activeClip = clips.find((clip) => playhead >= clip.startSeconds && playhead <= clip.endSeconds);
      return {
        id: track.id,
        name: track.name,
        color: track.color,
        deviceId,
        deviceLabel: device?.label || "Default camera",
        armed: false,
        recording: false,
        transportPlaying: false,
        activeClip,
        defaultLayout: defaultVideoLayout(trackIndex),
      };
    });
  }

  async function openCameraPreview() {
    if (!session) return;
    const tracks = buildPreviewTracks();
    try {
      let preview = await WebviewWindow.getByLabel("camera-preview");
      if (!preview) {
        preview = new WebviewWindow("camera-preview", {
          url: "/?cameraPreview=1",
          title: "AutoMixer Camera Preview",
          width: 980,
          height: 640,
          minWidth: 520,
          minHeight: 360,
          resizable: true,
          center: false,
        });
        const createdPreview = preview;
        createdPreview.once("tauri://created", () => {
          void createdPreview.emit("camera-preview:update", { tracks, canvas: normalizeVideoCanvas(session.videoCanvas) } satisfies CameraPreviewPayload);
        });
      } else {
        await preview.emit("camera-preview:update", { tracks, canvas: normalizeVideoCanvas(session.videoCanvas) } satisfies CameraPreviewPayload).catch(() => undefined);
      }
      await preview.show().catch(() => undefined);
      await preview.setFocus().catch(() => undefined);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function renderRangeSamples() {
    if (!session || !selectedRange) return undefined;
    return {
      startSample: Math.round(Math.max(0, selectedRange.start) * session.sampleRate),
      endSample: Math.round(Math.max(0, selectedRange.end) * session.sampleRate),
    };
  }

  function clampRangeSeconds(value: number, maxSeconds: number) {
    if (!Number.isFinite(value)) return 0;
    return Math.max(0, Math.min(Math.max(0, maxSeconds), value));
  }

  function setEditorRange(start: number, end: number, maxSeconds: number) {
    const a = clampRangeSeconds(start, maxSeconds);
    const b = clampRangeSeconds(end, maxSeconds);
    setSelectedRange({ start: Math.min(a, b), end: Math.max(a, b) });
    setPlayhead(Math.min(a, b));
  }

  function editorSecondsFromPointer(event: ReactPointerEvent<HTMLElement>, maxSeconds: number) {
    const rect = event.currentTarget.getBoundingClientRect();
    const ratio = rect.width > 0 ? (event.clientX - rect.left) / rect.width : 0;
    return clampRangeSeconds(ratio * maxSeconds, maxSeconds);
  }

  function handleSelectionPointerDown(event: ReactPointerEvent<HTMLDivElement>, maxSeconds: number) {
    if (event.button !== 0) return;
    const seconds = editorSecondsFromPointer(event, maxSeconds);
    rangeDragRef.current = { start: seconds, pointerId: event.pointerId };
    event.currentTarget.setPointerCapture(event.pointerId);
    setEditorRange(seconds, seconds, maxSeconds);
  }

  function handleSelectionPointerMove(event: ReactPointerEvent<HTMLDivElement>, maxSeconds: number) {
    const drag = rangeDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    setEditorRange(drag.start, editorSecondsFromPointer(event, maxSeconds), maxSeconds);
  }

  function handleSelectionPointerUp(event: ReactPointerEvent<HTMLDivElement>, maxSeconds: number) {
    const drag = rangeDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    setEditorRange(drag.start, editorSecondsFromPointer(event, maxSeconds), maxSeconds);
    rangeDragRef.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
  }

  function editorPlaybackEnd() {
    if (!session) return 1;
    const selectedTracks = selectedVideoTracks();
    const clipEnd = Math.max(0, ...selectedTracks.flatMap((track) => (track.videoClips ?? []).map((clip) => clip.endSample / session.sampleRate)));
    const scriptEnd = Math.max(0, ...mainVideoEdit.script.map((entry) => entry.endSeconds));
    const rangeEnd = selectedRange?.end ?? 0;
    return Math.max(clipEnd, scriptEnd, rangeEnd, 1);
  }

  function toggleProgramPlayback() {
    if (mainVideoEdit.script.length === 0) {
      setStatus("Run Agent Edit first to create the Main video lane.");
      return;
    }
    const start = selectedRange?.start ?? 0;
    const end = selectedRange?.end ?? editorPlaybackEnd();
    if (!programPlaying && (playhead < start || playhead >= end)) {
      setPlayhead(start);
    }
    setProgramPlaying((playing) => !playing);
  }

  async function renderCurrentVideo() {
    if (!session) return;
    const outputPath = await save({
      defaultPath: `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "automix"}.mp4`,
      filters: [{ name: "MP4", extensions: ["mp4"] }]
    });
    if (!outputPath) return;
    setBusy(true);
    try {
      if (mainVideoEdit.outputPath) {
        const result = await api.exportRenderedVideo(mainVideoEdit.outputPath, outputPath, exportAspect);
        setStatus(`Exported Main video ${result.path}`);
      } else {
        const trackIds = selectedVideoTrackIds();
        if (trackIds.length === 0) {
          setStatus("Select one or more video tracks, or run Agent Edit to create the Main video.");
          return;
        }
        const range = renderRangeSamples();
        await api.renderVideoMix(session.id, outputPath, range?.startSample, range?.endSample, trackIds, exportAspect);
        setStatus(`Rendered raw selected tracks ${outputPath}`);
      }
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  async function renderAutoVideoEdit() {
    if (!session) return;
    const trackIds = selectedVideoTrackIds();
    if (trackIds.length === 0) {
      setStatus("Select one or more video tracks in the main window, then open the editor again.");
      return;
    }
    const sampleIntervalSeconds = Number(agentIntervalSeconds);
    if (!Number.isFinite(sampleIntervalSeconds) || sampleIntervalSeconds <= 0) {
      setStatus("Use a positive interval.");
      return;
    }
    const range = renderRangeSamples();
    const outputPath = await save({
      defaultPath: `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "automix"}_auto_edit.mp4`,
      filters: [{ name: "MP4", extensions: ["mp4"] }]
    });
    if (!outputPath) return;
    setBusy(true);
    try {
      await api.renderAutoVideoEdit(session.id, outputPath, range?.startSample, range?.endSample, trackIds, sampleIntervalSeconds);
      setStatus(`Quick edit rendered ${outputPath}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  async function renderAgentVideoEdit(sampleIntervalSeconds: number, instructionsOverride?: string) {
    if (!session) return;
    const trackIds = selectedVideoTrackIds();
    if (trackIds.length === 0) {
      setStatus("Select one or more video tracks in the main window, then open the editor again.");
      return;
    }
    if (!Number.isFinite(sampleIntervalSeconds) || sampleIntervalSeconds <= 0) {
      setStatus("Use a positive interval.");
      return;
    }
    const visionModel = agentVideoModel.trim() || DEFAULT_AGENT_VIDEO_MODEL;
    const editModel = agentVideoEditModel.trim() || ollamaModel.trim() || DEFAULT_OLLAMA_MODEL;
    const instructions = (instructionsOverride ?? agentVideoInstructions).trim();
    const range = renderRangeSamples();
    const outputPath = await save({
      defaultPath: `${session.name.replace(/[^a-z0-9-]+/gi, "_") || "automix"}_agent_edit.mp4`,
      filters: [{ name: "MP4", extensions: ["mp4"] }]
    });
    if (!outputPath) return;
    setBusy(true);
    setAgentEditScript([]);
    setAgentEditProgress({ stage: "starting", message: "Starting Agent Video Edit...", current: 0, total: 1, elapsedSeconds: 0 });
    setStatus(`Running ${visionModel} for vision and ${editModel} for edit decisions...`);
    try {
      const result = await api.renderAgentVideoEdit(session.id, outputPath, range?.startSample, range?.endSample, trackIds, sampleIntervalSeconds, ollamaUrl, visionModel, editModel, instructions);
      const createdAt = new Date().toISOString();
      const rangeStartSeconds = range ? range.startSample / session.sampleRate : 0;
      setAgentEditScript(result.script);
      setMainVideoEdit({ script: result.script, outputPath, createdAt, rangeStartSeconds });
      setVideoEditHistory((items) => [{
        id: `${Date.now()}`,
        createdAt,
        outputPath,
        visionModel,
        editModel,
        intervalSeconds: sampleIntervalSeconds,
        instructions,
        script: result.script,
      }, ...items].slice(0, 20));
      setVideoChatMessages((items) => [...items, {
        role: "agent",
        text: `Rendered ${result.script.length} decisions into the Main video lane with ${visionModel} + ${editModel}.`,
        createdAt,
      }]);
      setStatus(`Done: ${result.path}`);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  function sendVideoEditorChat() {
    const text = videoChatDraft.trim();
    if (!text) return;
    const now = new Date().toISOString();
    setVideoChatMessages((items) => [...items, { role: "user", text, createdAt: now }]);
    setAgentVideoInstructions((current) => current.trim() ? `${current.trim()}\n${text}` : text);
    setVideoChatDraft("");
  }

  async function runAgentVideoEditFromDraft() {
    const draft = videoChatDraft.trim();
    const currentInstructions = agentVideoInstructions.trim();
    const instructions = draft ? (currentInstructions ? `${currentInstructions}\n${draft}` : draft) : currentInstructions;
    if (draft) {
      setAgentVideoInstructions(instructions);
      setVideoChatMessages((items) => [...items, { role: "user", text: draft, createdAt: new Date().toISOString() }]);
      setVideoChatDraft("");
    }
    await renderAgentVideoEdit(Number(agentIntervalSeconds), instructions);
  }

  function cycleProgramPreviewSize() {
    setProgramPreviewSize((size) => size === "small" ? "medium" : size === "medium" ? "large" : "small");
  }

  if (loading) return <main className="loading">Loading Video Editor...</main>;
  if (!session) {
    return (
      <main className="loading">
        <div>
          <h1>Video Editor</h1>
          <p>{status ?? "No session was loaded."}</p>
        </div>
      </main>
    );
  }

  const selectedTracks = selectedVideoTracks();
  const availableVideoTracks = allVideoTracks();
  const audioTrackCount = session.tracks.filter((track) => track.kind !== "video").length;
  const videoEditorScript = agentEditScript.length > 0 ? agentEditScript : mainVideoEdit.script.length > 0 ? mainVideoEdit.script : videoEditHistory[0]?.script ?? [];
  const rangeStartSeconds = selectedRange ? Math.max(0, selectedRange.start) : 0;
  const explicitRangeEndSeconds = selectedRange ? Math.max(0, selectedRange.end) : 0;
  const scriptEndSeconds = Math.max(0, ...videoEditorScript.map((entry) => entry.endSeconds), ...mainVideoEdit.script.map((entry) => entry.endSeconds));
  const clipEndSeconds = Math.max(0, ...selectedTracks.flatMap((track) => (track.videoClips ?? []).map((clip) => clip.endSample / session.sampleRate)));
  const duration = Math.max(0, ...session.tracks.flatMap((track) => track.kind === "video" ? (track.videoClips ?? []).map((clip) => clip.endSample / session.sampleRate) : track.clips.map((clip) => clip.endSample / session.sampleRate)));
  const editorStartSeconds = 0;
  const editorEndSeconds = Math.max(duration, scriptEndSeconds, clipEndSeconds, explicitRangeEndSeconds, 1);
  const editorSpanSeconds = Math.max(1, editorEndSeconds - editorStartSeconds);
  const selectedRangeStartPct = selectedRange ? Math.max(0, Math.min(100, (selectedRange.start / editorSpanSeconds) * 100)) : 0;
  const selectedRangeEndPct = selectedRange ? Math.max(selectedRangeStartPct, Math.min(100, (selectedRange.end / editorSpanSeconds) * 100)) : 0;
  const videoCanvas = normalizeVideoCanvas(session.videoCanvas);
  const activeProgramEntry = mainVideoEdit.script.find((entry) => entry.chosenTrackName && playhead >= entry.startSeconds && playhead < entry.endSeconds);
  const renderedProgramStartSeconds = mainVideoEdit.rangeStartSeconds ?? (mainVideoEdit.script.length > 0 ? Math.min(...mainVideoEdit.script.map((entry) => entry.startSeconds)) : 0);
  const renderedProgramLocalTime = Math.max(0, playhead - renderedProgramStartSeconds);

  return (
    <main className="video-editor-window">
      <div className="video-editor-shell windowed">
        <div className="video-editor-head">
          <div>
            <strong>Video Editor</strong>
            <span>
              {selectedTracks.length} video track{selectedTracks.length === 1 ? "" : "s"} selected
              {selectedRange ? ` · edit range ${formatTime(selectedRange.start)}-${formatTime(selectedRange.end)}` : " · full timeline"}
            </span>
          </div>
          <div className="video-editor-actions">
            <button type="button" onClick={toggleProgramPlayback} disabled={mainVideoEdit.script.length === 0}>
              {programPlaying ? <Pause size={15} /> : <Play size={15} />} {programPlaying ? "Pause" : "Play"}
            </button>
            <button type="button" onClick={() => void bootstrapVideoEditor({ sessionId: session.id, trackIds: selectedTrackIds, range: selectedRange, playhead })}>
              Refresh
            </button>
            <button type="button" onClick={() => void openCameraPreview()} disabled={selectedTracks.length === 0}>
              Show Canvas Preview
            </button>
            <select
              className="aspect-select"
              value={exportAspect}
              onChange={(event) => setExportAspect(event.target.value as ExportAspect)}
              title="Output aspect ratio for export (black bars added; source not cropped)"
              disabled={busy}
            >
              <option value="original">Original</option>
              <option value="square">Square 1:1</option>
              <option value="portrait916">Portrait 9:16</option>
            </select>
            <button type="button" onClick={() => void renderCurrentVideo()} disabled={busy || selectedTracks.length === 0}>
              <Download size={15} /> Export MP4
            </button>
            <button type="button" onClick={() => void renderAutoVideoEdit()} disabled={busy || selectedTracks.length === 0}>
              Quick Edit
            </button>
            <button type="button" className="primary" onClick={() => void runAgentVideoEditFromDraft()} disabled={busy || selectedTracks.length === 0}>
              Run Agent Edit
            </button>
          </div>
        </div>

        <div className="video-editor-body">
          <section className="video-editor-main">
            <div className="video-editor-canvas">
              <div className="video-editor-canvas-frame" style={{ aspectRatio: mainVideoEdit.outputPath ? undefined : `${videoCanvas.width} / ${videoCanvas.height}`, background: videoCanvas.background }}>
                {selectedTracks.length === 0 ? (
                  <span>Select video tracks in the main window, then open the editor.</span>
                ) : mainVideoEdit.outputPath ? (
                  <div className="program-preview-stage">
                    <ProgramPreviewVideo
                      src={mainVideoEdit.outputPath}
                      localTime={renderedProgramLocalTime}
                      playing={programPlaying}
                      muted={false}
                      onTime={(seconds) => setPlayhead(renderedProgramStartSeconds + seconds)}
                      onPause={() => setProgramPlaying(false)}
                      size={programPreviewSize}
                      onResize={cycleProgramPreviewSize}
                    />
                    <div className="video-editor-program-overlay">
                      <strong>{activeProgramEntry?.chosenTrackName ?? "Main video"}</strong>
                      <span>{formatTime(playhead)} · rendered agent video</span>
                    </div>
                  </div>
                ) : videoEditorScript.find((entry) => entry.chosenTrackName) ? (
                  <div className="video-editor-program">
                    <Video size={38} />
                    <strong>Render the Main video</strong>
                    <span>Run Agent Edit to create the MP4, then the canvas will play that rendered video.</span>
                  </div>
                ) : (
                  <div className="video-editor-program">
                    <Video size={38} />
                    <strong>{videoCanvas.width}x{videoCanvas.height}</strong>
                    <span>Run the agent to build the edit script.</span>
                  </div>
                )}
              </div>
            </div>

            <div className="video-editor-rangebar">
              <label>
                Start
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  value={selectedRange ? selectedRange.start.toFixed(2) : ""}
                  placeholder={formatTime(0)}
                  onChange={(event) => {
                    const value = Number(event.currentTarget.value);
                    if (Number.isFinite(value)) setEditorRange(value, selectedRange?.end ?? editorEndSeconds, editorEndSeconds);
                  }}
                />
              </label>
              <label>
                End
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  value={selectedRange ? selectedRange.end.toFixed(2) : ""}
                  placeholder={formatTime(editorEndSeconds)}
                  onChange={(event) => {
                    const value = Number(event.currentTarget.value);
                    if (Number.isFinite(value)) setEditorRange(selectedRange?.start ?? 0, value, editorEndSeconds);
                  }}
                />
              </label>
              <button type="button" onClick={() => setEditorRange(0, editorEndSeconds, editorEndSeconds)}>Whole Timeline</button>
              <button type="button" onClick={() => setSelectedRange(undefined)}>Clear Range</button>
              <span>{selectedRange ? `Edits use ${formatTime(selectedRange.start)}-${formatTime(selectedRange.end)}` : "Edits use the full timeline"}</span>
            </div>

            <div className="video-editor-timeline">
              <div className="video-editor-ruler">
                <span>{formatTime(editorStartSeconds)}</span>
                <span>{formatTime(editorStartSeconds + editorSpanSeconds / 2)}</span>
                <span>{formatTime(editorEndSeconds)}</span>
              </div>
              <div className="video-editor-row selection-row">
                <div className="video-editor-row-label">Edit range</div>
                <div
                  className="video-editor-row-lane video-editor-selection-lane"
                  onPointerDown={(event) => handleSelectionPointerDown(event, editorEndSeconds)}
                  onPointerMove={(event) => handleSelectionPointerMove(event, editorEndSeconds)}
                  onPointerUp={(event) => handleSelectionPointerUp(event, editorEndSeconds)}
                  onPointerCancel={() => { rangeDragRef.current = null; }}
                >
                  <span className="video-editor-selection-help">Drag here to choose the section for Agent Edit and export</span>
                  {selectedRange ? (
                    <div
                      className="video-editor-selection"
                      style={{ left: `${selectedRangeStartPct}%`, width: `${Math.max(0.6, selectedRangeEndPct - selectedRangeStartPct)}%` }}
                    >
                      {formatTime(selectedRange.start)}-{formatTime(selectedRange.end)}
                    </div>
                  ) : null}
                </div>
              </div>
              <div className="video-editor-row main-video-row">
                <div className="video-editor-row-label">Main video</div>
                <div className="video-editor-row-lane">
                  {mainVideoEdit.script.length === 0 ? <span className="video-editor-empty">Run Agent Edit to write the selected shots here</span> : null}
                  {mainVideoEdit.script.map((entry) => {
                    const startPct = Math.max(0, Math.min(100, ((entry.startSeconds - editorStartSeconds) / editorSpanSeconds) * 100));
                    const endPct = Math.max(startPct + 0.4, Math.min(100, ((entry.endSeconds - editorStartSeconds) / editorSpanSeconds) * 100));
                    const active = playhead >= entry.startSeconds && playhead < entry.endSeconds;
                    return (
                      <button
                        type="button"
                        className={`video-editor-event main ${entry.chosenTrackName ? "" : "black"} ${active ? "active" : ""}`}
                        key={`main-${entry.windowIndex}-${entry.startSeconds}`}
                        style={{ left: `${startPct}%`, width: `${Math.max(0.8, endPct - startPct)}%` }}
                        title={`${formatTime(entry.startSeconds)}-${formatTime(entry.endSeconds)} ${entry.chosenTrackName ?? "black"}`}
                        onClick={() => setPlayhead(entry.startSeconds)}
                      >
                        {entry.chosenTrackName ?? "black"}
                      </button>
                    );
                  })}
                  <div className="video-editor-program-playhead" style={{ left: `${Math.max(0, Math.min(100, (playhead / editorSpanSeconds) * 100))}%` }} />
                </div>
              </div>
              <div className="video-editor-row agent-row">
                <div className="video-editor-row-label">Agent cuts</div>
                <div className="video-editor-row-lane">
                  {videoEditorScript.length === 0 ? <span className="video-editor-empty">No agent decisions yet</span> : null}
                  {videoEditorScript.map((entry) => {
                    const startPct = Math.max(0, Math.min(100, ((entry.startSeconds - editorStartSeconds) / editorSpanSeconds) * 100));
                    const endPct = Math.max(startPct + 0.4, Math.min(100, ((entry.endSeconds - editorStartSeconds) / editorSpanSeconds) * 100));
                    return (
                      <button
                        type="button"
                        className={`video-editor-event ${entry.chosenTrackName ? "" : "black"}`}
                        key={`event-${entry.windowIndex}-${entry.startSeconds}`}
                        style={{ left: `${startPct}%`, width: `${Math.max(0.8, endPct - startPct)}%` }}
                        title={`${formatTime(entry.startSeconds)}-${formatTime(entry.endSeconds)} ${entry.chosenTrackName ?? "black"}`}
                        onClick={() => setPlayhead(entry.startSeconds)}
                      >
                        {entry.chosenTrackName ?? "black"}
                      </button>
                    );
                  })}
                </div>
              </div>
              <div className="video-editor-row audio-row">
                <div className="video-editor-row-label">Audio mix</div>
                <div className="video-editor-row-lane">
                  <div className="video-editor-audio-bed">
                    <span>{audioTrackCount} audio track{audioTrackCount === 1 ? "" : "s"} in export mix</span>
                  </div>
                </div>
              </div>
              {selectedTracks.map((track) => (
                <div className="video-editor-row" key={`editor-track-${track.id}`}>
                  <div className="video-editor-row-label">{track.name}</div>
                  <div className="video-editor-row-lane">
                    {(track.videoClips ?? []).map((clip) => {
                      const clipStart = clip.startSample / session.sampleRate;
                      const clipEnd = clip.endSample / session.sampleRate;
                      if (clipEnd < editorStartSeconds || clipStart > editorEndSeconds) return null;
                      const startPct = Math.max(0, Math.min(100, ((clipStart - editorStartSeconds) / editorSpanSeconds) * 100));
                      const endPct = Math.max(startPct + 0.6, Math.min(100, ((clipEnd - editorStartSeconds) / editorSpanSeconds) * 100));
                      return (
                        <button
                          type="button"
                          className="video-editor-clip"
                          key={clip.id}
                          style={{ left: `${startPct}%`, width: `${Math.max(1, endPct - startPct)}%`, borderColor: track.color }}
                          onClick={() => setPlayhead(clipStart)}
                        >
                          {clip.name ?? track.name}
                        </button>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>

            {agentEditProgress ? (
              <div className={`agent-editor-progress stage-${agentEditProgress.stage}`}>
                <div className="agent-editor-progress-row">
                  <strong>{agentEditProgress.stage}</strong>
                  <span>{agentEditProgress.current}/{agentEditProgress.total}</span>
                  <em>{Math.round(agentEditProgress.elapsedSeconds)}s</em>
                </div>
                <div className="agent-editor-progress-bar">
                  <span style={{ width: `${Math.max(3, Math.min(100, (agentEditProgress.current / Math.max(1, agentEditProgress.total)) * 100))}%` }} />
                </div>
                <div className="agent-editor-status">{agentEditProgress.message}</div>
              </div>
            ) : status ? <div className="agent-editor-status">{status}</div> : null}

            {videoEditorScript.length > 0 ? (
              <div className="agent-editor-script">
                <div className="agent-editor-script-head">
                  <strong>Edit script</strong>
                  <span>{videoEditorScript.length} decisions</span>
                </div>
                <div className="agent-editor-script-list">
                  {videoEditorScript.map((entry) => (
                    <div className="agent-editor-script-item" key={`${entry.windowIndex}-${entry.startSeconds}-${entry.endSeconds}`}>
                      <div className="agent-editor-script-meta">
                        <strong>{formatTime(entry.startSeconds)}-{formatTime(entry.endSeconds)}</strong>
                        <span>
                          {entry.decision ? `${entry.decision} · ` : ""}
                          {entry.chosenTrackName ? `${entry.chosenTrackName} · track ${(entry.chosenTrackIndex ?? 0) + 1}` : "Black / no active video"}
                          {entry.varietyOverride ? " · variation override" : ""}
                        </span>
                      </div>
                      <p>{entry.reason}</p>
                      {entry.dataProvided?.length > 0 ? (
                        <div className="agent-editor-script-data">
                          <strong>Data provided</strong>
                          {entry.dataProvided.map((item, index) => (
                            <span key={`${entry.windowIndex}-data-${index}`}>{item}</span>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  ))}
                </div>
              </div>
            ) : null}
          </section>

          <aside className="video-editor-side">
            <div className="video-editor-card video-editor-track-picker">
              <strong>Video tracks</strong>
              {availableVideoTracks.length === 0 ? (
                <span className="video-editor-empty">No video tracks in this session.</span>
              ) : availableVideoTracks.map((track) => (
                <label key={`video-editor-select-${track.id}`} className="video-editor-track-option">
                  <input
                    type="checkbox"
                    checked={selectedTrackIds.includes(track.id)}
                    onChange={(event) => {
                      const checked = event.currentTarget.checked;
                      setSelectedTrackIds((items) => {
                        if (checked) return [...items.filter((id) => id !== track.id), track.id];
                        if (items.length <= 1 && items.includes(track.id)) {
                          setStatus("Keep at least one video track selected for the editor.");
                          return items;
                        }
                        return items.filter((id) => id !== track.id);
                      });
                    }}
                  />
                  <span style={{ borderColor: track.color }} />
                  {track.name}
                </label>
              ))}
            </div>

            <div className="video-editor-card video-editor-chat">
              <strong>Video agent</strong>
              <label className="video-editor-command">
                Instructions
                <textarea
                  value={videoChatDraft}
                  onChange={(event) => setVideoChatDraft(event.target.value)}
                  onKeyDown={(event) => {
                    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
                      event.preventDefault();
                      void runAgentVideoEditFromDraft();
                    }
                  }}
                  placeholder="Example: use the overhead view during dense guitar parts, hold closeups through phrases, and avoid sudden cuts unless the music changes."
                />
              </label>
              <div className="video-editor-command-actions">
                <button type="button" onClick={sendVideoEditorChat} disabled={!videoChatDraft.trim()}>
                  Add instruction
                </button>
                <button type="button" className="primary" onClick={() => void runAgentVideoEditFromDraft()} disabled={busy || selectedTracks.length === 0}>
                  Run Agent Edit
                </button>
              </div>
              <div className="video-editor-chat-log">
                {videoChatMessages.length === 0 ? (
                  <span className="video-editor-empty">Select tracks, choose a range, then describe the edit in plain language.</span>
                ) : videoChatMessages.map((message, index) => (
                  <div className={`video-editor-message ${message.role}`} key={`${message.createdAt}-${index}`}>
                    <span>{message.role}</span>
                    <p>{message.text}</p>
                  </div>
                ))}
              </div>
            </div>

            <div className="video-editor-card video-editor-settings">
              <strong>Agent settings</strong>
              <label>
                Interval
                <input value={agentIntervalSeconds} onChange={(event) => setAgentIntervalSeconds(event.target.value)} inputMode="decimal" />
              </label>
              <label>
                Vision model
                <select value={agentVideoModel} onChange={(event) => setAgentVideoModel(event.target.value)}>
                  {modelOptions.map((model) => <option key={model} value={model}>{model}</option>)}
                </select>
              </label>
              <label>
                Edit model
                <select value={agentVideoEditModel} onChange={(event) => setAgentVideoEditModel(event.target.value)}>
                  {modelOptions.map((model) => <option key={model} value={model}>{model}</option>)}
                </select>
              </label>
            </div>

            <div className="video-editor-card video-editor-history">
              <strong>History</strong>
              {videoEditHistory.length === 0 ? (
                <span className="video-editor-empty">No saved video edit runs yet.</span>
              ) : videoEditHistory.map((item) => (
                <button
                  type="button"
                  key={item.id}
                  onClick={() => {
                    const rangeStartSeconds = item.script.length > 0 ? Math.min(...item.script.map((entry) => entry.startSeconds)) : 0;
                    setAgentEditScript(item.script);
                    setMainVideoEdit({ script: item.script, outputPath: item.outputPath, createdAt: item.createdAt, rangeStartSeconds });
                    setAgentVideoInstructions(item.instructions);
                    setAgentVideoModel(item.visionModel);
                    setAgentVideoEditModel(item.editModel);
                  }}
                >
                  <span>{new Date(item.createdAt).toLocaleString()}</span>
                  <strong>{item.script.length} decisions</strong>
                  <em>{item.outputPath}</em>
                </button>
              ))}
            </div>
          </aside>
        </div>
      </div>
    </main>
  );
}

function cameraVideoConstraints(deviceId?: string): MediaTrackConstraints {
  return {
    ...(deviceId ? { deviceId: { exact: deviceId } } : {}),
    width: { ideal: 1920 },
    height: { ideal: 1080 },
    frameRate: { ideal: 30, max: 60 },
  };
}

export function CameraPreviewApp() {
  const [tracks, setTracks] = useState<CameraPreviewTrack[]>([]);
  const [canvas, setCanvas] = useState<VideoCanvas>(defaultVideoCanvas());
  const [selectedLayerId, setSelectedLayerId] = useState<string | undefined>();
  const [layoutDrafts, setLayoutDrafts] = useState<Record<string, VideoLayout>>({});
  const layoutSaveTimersRef = useRef<Record<string, number>>({});
  const canvasSaveTimerRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<CameraPreviewPayload | CameraPreviewTrack[]>("camera-preview:update", (event) => {
      const payload = event.payload;
      if (Array.isArray(payload)) {
        setTracks(payload);
        return;
      }
      setTracks(payload?.tracks ?? []);
      setCanvas(normalizeVideoCanvas(payload?.canvas));
    }).then((fn) => { unlisten = fn; });
    return () => {
      unlisten?.();
      Object.values(layoutSaveTimersRef.current).forEach((timer) => window.clearTimeout(timer));
      layoutSaveTimersRef.current = {};
      if (canvasSaveTimerRef.current) window.clearTimeout(canvasSaveTimerRef.current);
    };
  }, []);

  useEffect(() => {
    const clipIds = new Set(tracks.flatMap((track) => track.activeClip ? [track.activeClip.id] : []));
    setLayoutDrafts((drafts) => Object.fromEntries(Object.entries(drafts).filter(([clipId]) => clipIds.has(clipId))));
    if (selectedLayerId && !tracks.some((track) => (track.activeClip?.id ?? `live-${track.id}`) === selectedLayerId)) {
      setSelectedLayerId(undefined);
    }
  }, [tracks, selectedLayerId]);

  const layers = tracks
    .map((track, index) => ({
      track,
      clip: track.activeClip,
      layout: track.activeClip ? (layoutDrafts[track.activeClip.id] ?? track.activeClip.layout) : (track.defaultLayout ?? defaultVideoLayout(index)),
      live: !track.activeClip,
      id: track.activeClip?.id ?? `live-${track.id}`,
    }))
    .sort((a, b) => a.layout.zIndex - b.layout.zIndex);
  const selectedLayer = layers.find((layer) => layer.id === selectedLayerId) ?? layers.find((layer) => layer.clip) ?? layers[0];
  const updateLayerLayout = (trackId: string, clipId: string, layout: VideoLayout, delayMs = 350) => {
    const normalized = normalizeVideoLayout(layout);
    setLayoutDrafts((drafts) => ({ ...drafts, [clipId]: normalized }));
    const timers = layoutSaveTimersRef.current;
    if (timers[clipId]) window.clearTimeout(timers[clipId]);
    timers[clipId] = window.setTimeout(() => {
      delete timers[clipId];
      void emitCameraPreviewLayout({ trackId, clipId, layout: normalized });
    }, delayMs);
  };
  const updateCanvas = (nextCanvas: VideoCanvas) => {
    const normalized = normalizeVideoCanvas(nextCanvas);
    setCanvas(normalized);
    if (canvasSaveTimerRef.current) window.clearTimeout(canvasSaveTimerRef.current);
    canvasSaveTimerRef.current = window.setTimeout(() => {
      canvasSaveTimerRef.current = undefined;
      void emitCameraPreviewCanvas(normalized);
    }, 350);
  };
  const canvasRatio = canvas.width / Math.max(1, canvas.height);

  // Avoid 'unused' warnings on the canvas-design helpers we no longer use in this
  // simplified view. They stay because they're still referenced by other window types.
  void canvasRatio;
  void selectedLayer;
  void selectedLayerId;
  void setSelectedLayerId;
  void updateCanvas;

  return (
    <main className="camera-preview-window">
      <header className="camera-preview-header">
        <strong>Cameras</strong>
        <span>{tracks.length ? `${tracks.length} ${tracks.length === 1 ? "camera" : "cameras"}` : "No selected video tracks"}</span>
      </header>
      {tracks.length === 0 ? (
        <div className="camera-preview-empty">Select one or more video tracks in AutoMixer.</div>
      ) : (
        <div className="camera-stack">
          {layers.map((layer) => (
            <div className="camera-stack-panel" key={layer.id}>
              <div className="camera-stack-head">
                <span className="camera-stack-color" style={{ background: layer.track.color }} aria-hidden="true" />
                <strong>{layer.track.name}</strong>
                {layer.live ? <span className={`camera-stack-state ${layer.track.recording ? "rec" : layer.track.armed ? "arm" : "live"}`}>
                  {layer.track.recording ? "REC" : layer.track.armed ? "ARM" : "LIVE"}
                </span> : null}
              </div>
              <div className="camera-stack-video">
                {layer.clip
                  ? <RecordedVideoFeed clip={layer.clip} playing={layer.track.transportPlaying} />
                  : <CameraLiveFeed track={layer.track} />}
                {layer.clip ? (
                  <CropEditor
                    layout={layer.layout}
                    onChange={(next) => updateLayerLayout(layer.track.id, layer.clip!.id, next, 250)}
                  />
                ) : null}
              </div>
            </div>
          ))}
        </div>
      )}
    </main>
  );
}

// Lightweight free-form crop editor for a single video panel in the camera-preview
// window. Four draggable edge handles set cropTop/Right/Bottom/Left on the clip's
// layout; the dimmed regions show what will be cropped out.
function CropEditor({ layout, onChange }: { layout: VideoLayout; onChange: (next: VideoLayout) => void }) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ edge: "top" | "right" | "bottom" | "left"; startX: number; startY: number; original: number } | null>(null);
  const edgeKey: Record<"top" | "right" | "bottom" | "left", "cropTop" | "cropRight" | "cropBottom" | "cropLeft"> = {
    top: "cropTop", right: "cropRight", bottom: "cropBottom", left: "cropLeft",
  };
  const handleDown = (edge: "top" | "right" | "bottom" | "left", event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    dragRef.current = {
      edge,
      startX: event.clientX,
      startY: event.clientY,
      original: layout[edgeKey[edge]],
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const handleMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    const rect = wrapRef.current?.getBoundingClientRect();
    if (!drag || !rect || rect.width <= 0 || rect.height <= 0) return;
    event.preventDefault();
    let deltaPct = 0;
    if (drag.edge === "top" || drag.edge === "bottom") {
      const deltaY = event.clientY - drag.startY;
      deltaPct = (deltaY / rect.height) * 100;
      if (drag.edge === "bottom") deltaPct = -deltaPct;
    } else {
      const deltaX = event.clientX - drag.startX;
      deltaPct = (deltaX / rect.width) * 100;
      if (drag.edge === "right") deltaPct = -deltaPct;
    }
    const nextVal = Math.max(0, Math.min(45, drag.original + deltaPct));
    onChange({ ...layout, [edgeKey[drag.edge]]: nextVal });
  };
  const handleUp = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    event.currentTarget.releasePointerCapture(event.pointerId);
    dragRef.current = null;
  };
  const tBar = `${layout.cropTop}%`;
  const bBar = `${layout.cropBottom}%`;
  const lBar = `${layout.cropLeft}%`;
  const rBar = `${layout.cropRight}%`;
  return (
    <div ref={wrapRef} className="crop-overlay">
      <div className="crop-dim" style={{ top: 0, left: 0, right: 0, height: tBar }} />
      <div className="crop-dim" style={{ bottom: 0, left: 0, right: 0, height: bBar }} />
      <div className="crop-dim" style={{ top: tBar, bottom: bBar, left: 0, width: lBar }} />
      <div className="crop-dim" style={{ top: tBar, bottom: bBar, right: 0, width: rBar }} />
      <div
        className="crop-handle horizontal"
        style={{ top: tBar, left: lBar, right: rBar }}
        onPointerDown={(e) => handleDown("top", e)}
        onPointerMove={handleMove}
        onPointerUp={handleUp}
      />
      <div
        className="crop-handle horizontal"
        style={{ bottom: bBar, left: lBar, right: rBar }}
        onPointerDown={(e) => handleDown("bottom", e)}
        onPointerMove={handleMove}
        onPointerUp={handleUp}
      />
      <div
        className="crop-handle vertical"
        style={{ left: lBar, top: tBar, bottom: bBar }}
        onPointerDown={(e) => handleDown("left", e)}
        onPointerMove={handleMove}
        onPointerUp={handleUp}
      />
      <div
        className="crop-handle vertical"
        style={{ right: rBar, top: tBar, bottom: bBar }}
        onPointerDown={(e) => handleDown("right", e)}
        onPointerMove={handleMove}
        onPointerUp={handleUp}
      />
    </div>
  );
}

function CameraCanvasLayer({
  layer,
  selected,
  onSelect,
  onCommit,
}: {
  layer: CameraCanvasLayerModel;
  selected: boolean;
  onSelect: () => void;
  onCommit: (layout: VideoLayout) => void;
}) {
  const [draft, setDraft] = useState<VideoLayout>(layer.layout);
  const dragRef = useRef<{ mode: "move" | "resize"; x: number; y: number; layout: VideoLayout } | null>(null);
  useEffect(() => setDraft(layer.layout), [layer.id, JSON.stringify(layer.layout)]);
  const layout = draft;
  const cropX = Math.min(90, layout.cropLeft + layout.cropRight);
  const cropY = Math.min(90, layout.cropTop + layout.cropBottom);
  const innerStyle = {
    left: `${-(layout.cropLeft / Math.max(1, 100 - cropX)) * 100}%`,
    top: `${-(layout.cropTop / Math.max(1, 100 - cropY)) * 100}%`,
    width: `${(100 / Math.max(1, 100 - cropX)) * 100}%`,
    height: `${(100 / Math.max(1, 100 - cropY)) * 100}%`,
  };
  const beginDrag = (mode: "move" | "resize", event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    onSelect();
    dragRef.current = { mode, x: event.clientX, y: event.clientY, layout };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const moveDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    const canvas = event.currentTarget.closest(".video-canvas") as HTMLElement | null;
    const rect = canvas?.getBoundingClientRect();
    if (!drag || !rect) return;
    event.stopPropagation();
    const dx = ((event.clientX - drag.x) / rect.width) * 100;
    const dy = ((event.clientY - drag.y) / rect.height) * 100;
    if (drag.mode === "move") {
      const next = normalizeVideoLayout({ ...drag.layout, x: drag.layout.x + dx, y: drag.layout.y + dy });
      setDraft(next);
    } else {
      const next = normalizeVideoLayout({ ...drag.layout, width: drag.layout.width + dx, height: drag.layout.height + dy });
      setDraft(next);
    }
  };
  const endDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    event.stopPropagation();
    event.currentTarget.releasePointerCapture(event.pointerId);
    if (layer.clip) onCommit(draft);
  };
  return (
    <div
      className={`video-canvas-layer ${selected ? "selected" : ""} ${layer.live ? "live" : ""}`}
      style={{
        left: `${layout.x}%`,
        top: `${layout.y}%`,
        width: `${layout.width}%`,
        height: `${layout.height}%`,
        opacity: layout.opacity,
        transform: `rotate(${layout.rotation}deg)`,
        zIndex: 20 + layout.zIndex,
        ["--track-color" as string]: layer.track.color,
      }}
      onPointerDown={(event) => beginDrag("move", event)}
      onPointerMove={moveDrag}
      onPointerUp={endDrag}
    >
      <div className="video-canvas-layer-media" style={{ filter: presetCss(layout) }}>
        <div className="video-canvas-layer-inner" style={innerStyle}>
          {layer.clip
            ? <RecordedVideoFeed clip={layer.clip} playing={layer.track.transportPlaying} grade={layout} />
            : <CameraLiveFeed track={layer.track} />}
        </div>
      </div>
      <span className="video-canvas-label">{layer.track.name}</span>
      {layer.live ? <span className="video-canvas-live">{layer.track.recording ? "REC" : layer.track.armed ? "ARM" : "LIVE"}</span> : null}
      {selected ? <div className="video-canvas-resize" onPointerDown={(event) => beginDrag("resize", event)} /> : null}
    </div>
  );
}

function VideoCanvasInspector({
  layer,
  layers,
  canvas,
  onSelect,
  onCanvasChange,
  onChange,
}: {
  layer?: CameraCanvasLayerModel;
  layers: CameraCanvasLayerModel[];
  canvas: VideoCanvas;
  onSelect: (id: string) => void;
  onCanvasChange: (canvas: VideoCanvas) => void;
  onChange: (layout: VideoLayout) => void;
}) {
  const layout = layer?.layout;
  const [widthDraft, setWidthDraft] = useState(String(canvas.width));
  const [heightDraft, setHeightDraft] = useState(String(canvas.height));
  useEffect(() => {
    setWidthDraft(String(canvas.width));
    setHeightDraft(String(canvas.height));
  }, [canvas.width, canvas.height]);
  const update = (patch: Partial<VideoLayout>) => {
    if (!layout || !layer?.clip) return;
    onChange(normalizeVideoLayout({ ...layout, ...patch }));
  };
  const commitCanvasNumber = (key: "width" | "height", value: string) => {
    const number = Number(value);
    if (!Number.isFinite(number)) {
      if (key === "width") setWidthDraft(String(canvas.width));
      else setHeightDraft(String(canvas.height));
      return;
    }
    const next = normalizeVideoCanvas({ ...canvas, [key]: number });
    if (key === "width") setWidthDraft(String(next.width));
    else setHeightDraft(String(next.height));
    onCanvasChange(next);
  };
  return (
    <aside className="video-canvas-inspector">
      <div className="video-canvas-panel-title">Canvas</div>
      <div className="video-canvas-formats">
        {[
          { label: "16:9", width: 1280, height: 720 },
          { label: "9:16", width: 1080, height: 1920 },
          { label: "1:1", width: 1080, height: 1080 },
          { label: "4:5", width: 1080, height: 1350 },
        ].map((preset) => (
          <button
            key={preset.label}
            type="button"
            className={canvas.width === preset.width && canvas.height === preset.height ? "active" : ""}
            onClick={() => onCanvasChange({ ...canvas, width: preset.width, height: preset.height })}
          >
            {preset.label}
          </button>
        ))}
      </div>
      <div className="video-canvas-size-row">
        <label>
          W
          <input
            type="number"
            min={240}
            max={3840}
            value={widthDraft}
            onChange={(event) => setWidthDraft(event.target.value)}
            onBlur={() => commitCanvasNumber("width", widthDraft)}
            onKeyDown={(event) => { if (event.key === "Enter") commitCanvasNumber("width", widthDraft); }}
          />
        </label>
        <label>
          H
          <input
            type="number"
            min={240}
            max={3840}
            value={heightDraft}
            onChange={(event) => setHeightDraft(event.target.value)}
            onBlur={() => commitCanvasNumber("height", heightDraft)}
            onKeyDown={(event) => { if (event.key === "Enter") commitCanvasNumber("height", heightDraft); }}
          />
        </label>
        <label>
          BG
          <input type="color" value={canvas.background} onChange={(event) => onCanvasChange({ ...canvas, background: event.target.value })} />
        </label>
      </div>
      <div className="video-canvas-panel-title">Layers</div>
      <div className="video-layer-list">
        {layers.map((item) => (
          <button
            key={item.id}
            type="button"
            className={item.id === layer?.id ? "active" : ""}
            onClick={() => onSelect(item.id)}
          >
            <span style={{ background: item.track.color }} />
            <strong>{item.track.name}</strong>
            <em>{item.clip ? "clip" : "live"}</em>
          </button>
        ))}
      </div>
      {!layer ? null : !layer.clip || !layout ? (
        <div className="video-canvas-note">Live camera layers are preview only. Move the recorded clip at the playhead to save a canvas layout.</div>
      ) : (
        <>
          <div className="video-canvas-panel-title">Layout</div>
          <VideoSlider label="X" value={layout.x} min={-300} max={300} step={0.5} onChange={(x) => update({ x })} />
          <VideoSlider label="Y" value={layout.y} min={-300} max={300} step={0.5} onChange={(y) => update({ y })} />
          <VideoSlider label="W" value={layout.width} min={1} max={300} step={0.5} onChange={(width) => update({ width })} />
          <VideoSlider label="H" value={layout.height} min={1} max={300} step={0.5} onChange={(height) => update({ height })} />
          <VideoSlider label="Layer" value={layout.zIndex} min={-10} max={10} step={1} onChange={(zIndex) => update({ zIndex })} />
          <VideoSlider label="Opacity" value={layout.opacity} min={0} max={1} step={0.01} onChange={(opacity) => update({ opacity })} />
          <VideoSlider label="Rotate" value={layout.rotation} min={-180} max={180} step={1} onChange={(rotation) => update({ rotation })} />
          <div className="video-canvas-panel-title">Crop</div>
          <VideoSlider label="Top" value={layout.cropTop} min={0} max={45} step={0.5} onChange={(cropTop) => update({ cropTop })} />
          <VideoSlider label="Right" value={layout.cropRight} min={0} max={45} step={0.5} onChange={(cropRight) => update({ cropRight })} />
          <VideoSlider label="Bottom" value={layout.cropBottom} min={0} max={45} step={0.5} onChange={(cropBottom) => update({ cropBottom })} />
          <VideoSlider label="Left" value={layout.cropLeft} min={0} max={45} step={0.5} onChange={(cropLeft) => update({ cropLeft })} />
          <div className="video-canvas-panel-title">Filters</div>
          <div className="video-filter-presets">
            {(["none", "warm", "cool", "mono", "punch", "dream"] as const).map((preset) => (
              <button key={preset} type="button" className={layout.preset === preset ? "active" : ""} onClick={() => update({ preset })}>{preset}</button>
            ))}
          </div>
          <div className="video-canvas-panel-title">Light</div>
          <VideoSlider label="Exposure" value={layout.exposure ?? 0} min={-1} max={1} step={0.01} onChange={(exposure) => update({ exposure })} />
          <VideoSlider label="Bright" value={layout.brightness} min={0.2} max={2} step={0.01} onChange={(brightness) => update({ brightness })} />
          <VideoSlider label="Contrast" value={layout.contrast} min={0.2} max={2} step={0.01} onChange={(contrast) => update({ contrast })} />
          <VideoSlider label="Highlights" value={layout.highlights ?? 0} min={-1} max={1} step={0.01} onChange={(highlights) => update({ highlights })} />
          <VideoSlider label="Shadows" value={layout.shadows ?? 0} min={-1} max={1} step={0.01} onChange={(shadows) => update({ shadows })} />
          <VideoSlider label="Gamma" value={layout.gamma ?? 1} min={0.5} max={1.8} step={0.01} onChange={(gamma) => update({ gamma })} />
          <div className="video-canvas-panel-title">Color</div>
          <VideoSlider label="Saturate" value={layout.saturation} min={0} max={2} step={0.01} onChange={(saturation) => update({ saturation })} />
          <VideoSlider label="Temp" value={layout.temperature ?? 0} min={-1} max={1} step={0.01} onChange={(temperature) => update({ temperature })} />
          <VideoSlider label="Tint" value={layout.tint ?? 0} min={-1} max={1} step={0.01} onChange={(tint) => update({ tint })} />
          <div className="video-canvas-panel-title">Detail</div>
          <VideoSlider label="Sharpen" value={layout.sharpen ?? 0} min={0} max={2} step={0.01} onChange={(sharpen) => update({ sharpen })} />
          <VideoSlider label="Noise" value={layout.grain ?? 0} min={0} max={1} step={0.01} onChange={(grain) => update({ grain })} />
          <VideoSlider label="Vignette" value={layout.vignette ?? 0} min={0} max={1} step={0.01} onChange={(vignette) => update({ vignette })} />
          <VideoSlider label="Blur" value={layout.blur} min={0} max={10} step={0.1} onChange={(blur) => update({ blur })} />
          <button type="button" className="video-reset-button" onClick={() => update(defaultVideoLayout())}>Reset layer</button>
        </>
      )}
    </aside>
  );
}

function VideoSlider({ label, value, min, max, step, onChange }: { label: string; value: number; min: number; max: number; step: number; onChange: (value: number) => void }) {
  return (
    <label className="video-slider">
      <span>{label}</span>
      <input type="range" min={min} max={max} step={step} value={value} onChange={(event) => onChange(Number(event.target.value))} />
      <em>{Math.abs(value) < 10 ? value.toFixed(2).replace(/\.00$/, "") : value.toFixed(0)}</em>
    </label>
  );
}

// CSS for the named look preset only (sepia/hue/grayscale combos CSS does well).
// Applied on the surrounding box so it composes over either the GL canvas or the
// plain <video> fallback.
function presetCss(layout: VideoLayout) {
  return {
    none: "",
    warm: "sepia(0.18) hue-rotate(-8deg)",
    cool: "hue-rotate(12deg) saturate(0.92)",
    mono: "grayscale(1)",
    punch: "contrast(1.16) saturate(1.18)",
    dream: "sepia(0.12) saturate(0.82) brightness(1.08)",
    cinema: "contrast(1.10) saturate(1.05) sepia(0.10) hue-rotate(-4deg)",
    noir: "grayscale(1) contrast(1.28)",
    moody: "brightness(0.92) contrast(1.18) saturate(0.85) hue-rotate(8deg)",
    vintage: "sepia(0.35) saturate(0.72) contrast(0.94)",
    golden: "sepia(0.18) saturate(1.12) brightness(1.04) hue-rotate(-6deg)",
    cold: "hue-rotate(18deg) saturate(0.92) contrast(1.05)",
  }[layout.preset] || "";
}

// CSS for the subset of numeric adjustments CSS can express (used as the
// fallback when WebGL is unavailable; the GL path handles the full set).
function numericCss(layout: VideoLayout) {
  return `brightness(${layout.brightness}) contrast(${layout.contrast}) saturate(${layout.saturation}) blur(${layout.blur}px)`;
}

function videoFilterCss(layout: VideoLayout) {
  return `${numericCss(layout)} ${presetCss(layout)}`.trim();
}

function RecordedVideoFeed({ clip, playing, grade }: { clip: CameraPreviewClip; playing: boolean; grade?: VideoLayout }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [error, setError] = useState<string | undefined>();
  const url = convertFileSrc(clip.src);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    if (Number.isFinite(clip.localTime) && Math.abs(video.currentTime - clip.localTime) > 0.22) {
      try {
        video.currentTime = clip.localTime;
      } catch {}
    }
    if (playing) {
      void video.play().catch(() => undefined);
    } else {
      video.pause();
    }
  }, [clip.id, clip.localTime, playing, url]);

  return (
    <>
      <video
        ref={videoRef}
        src={url}
        muted
        playsInline
        preload="auto"
        style={grade ? { filter: cssAdjustFilter(grade) } : undefined}
        onLoadedData={() => setError(undefined)}
        onError={() => setError(`Could not load recorded video: ${url}`)}
      />
      {(() => { const wb = grade ? whiteBalanceStyle(grade) : null; return wb ? <div style={wb} /> : null; })()}
      {grade && (grade.vignette ?? 0) > 0 ? <div style={vignetteStyle(grade)} /> : null}
      {grade && (grade.grain ?? 0) > 0 ? <div style={grainStyle(grade)} /> : null}
      <div className="camera-preview-source">{clip.name}</div>
      {error ? <div className="camera-preview-error">{error}</div> : null}
    </>
  );
}

function CameraLiveFeed({ track }: { track: CameraPreviewTrack }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [error, setError] = useState<string | undefined>();

  useEffect(() => {
    let stopped = false;
    let stream: MediaStream | undefined;
    setError(undefined);
    const start = async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          video: cameraVideoConstraints(track.deviceId),
          audio: false,
        });
        if (stopped) {
          stream.getTracks().forEach((item) => item.stop());
          return;
        }
        if (videoRef.current) {
          videoRef.current.srcObject = stream;
          await videoRef.current.play().catch(() => undefined);
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    };
    void start();
    return () => {
      stopped = true;
      stream?.getTracks().forEach((item) => item.stop());
    };
  }, [track.id, track.deviceId]);

  return (
    <>
      <video ref={videoRef} autoPlay muted playsInline />
      {error ? <div className="camera-preview-error">{error}</div> : null}
    </>
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

// A range input that holds its own value while the user drags, so a parent
// re-render (e.g. the 30 Hz playback meters) can't snap the thumb back to a
// stale prop. Commits on release like the mixer faders, not per input event.
function InspectorSlider({
  label,
  title,
  value,
  min,
  max,
  step,
  format,
  resetValue,
  onCommit,
}: {
  label: string;
  title: string;
  value: number;
  min: number;
  max: number;
  step: number;
  format: (v: number) => string;
  resetValue: number;
  onCommit: (v: number) => void;
}) {
  const [local, setLocal] = useState(value);
  const draggingRef = useRef(false);
  useEffect(() => { if (!draggingRef.current) setLocal(value); }, [value]);
  const commit = (v: number) => { draggingRef.current = false; onCommit(v); };
  return (
    <label className="inspector-field" title={title}>
      <span>{label}</span>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={local}
        onPointerDown={() => { draggingRef.current = true; }}
        onChange={(event) => setLocal(Number(event.target.value))}
        onPointerUp={(event) => commit(Number((event.currentTarget as HTMLInputElement).value))}
        onKeyUp={(event) => commit(Number((event.currentTarget as HTMLInputElement).value))}
        onBlur={() => { draggingRef.current = false; }}
        onDoubleClick={() => { draggingRef.current = false; setLocal(resetValue); onCommit(resetValue); }}
      />
      <em>{format(local)}</em>
    </label>
  );
}

function TrackInspector({
  track,
  source,
  sampleRate,
  inputDevices,
  inputDevice,
  inputGainDb,
  inputChannels,
  inputChannelLevels,
  cameraDevices,
  cameraDevice,
  cameraAudio,
  selectionCount,
  onChange,
  onInputDeviceChange,
  onInputGainChange,
  onInputChannelsChange,
  onRefreshInputDevices,
  onCameraDeviceChange,
  onCameraAudioChange,
  onRefreshCameraDevices,
  onDelete
}: {
  track?: Track;
  source?: MixSession["sourceFiles"][number];
  sampleRate: number;
  inputDevices: string[];
  inputDevice: string;
  inputGainDb: number;
  inputChannels: number[];
  inputChannelLevels: number[];
  cameraDevices: MediaDeviceInfo[];
  cameraDevice: string;
  cameraAudio: boolean;
  selectionCount: number;
  onChange: (track: Track, patch: Partial<Track>) => void;
  onInputDeviceChange: (trackId: string, device: string) => void;
  onInputGainChange: (trackId: string, gainDb: number) => void;
  onInputChannelsChange: (trackId: string, channels: number[]) => void;
  onRefreshInputDevices: () => void;
  onCameraDeviceChange: (trackId: string, device: string) => void;
  onCameraAudioChange: (trackId: string, enabled: boolean) => void;
  onRefreshCameraDevices: () => void;
  onDelete: (track: Track) => void;
}) {
  const [nameDraft, setNameDraft] = useState(track?.name ?? "");
  const [roleDraft, setRoleDraft] = useState(track?.role ?? "");
  // Number of input channels exposed by the currently-selected device. Probed when
  // the user opens or changes the input device.
  const [deviceChannelCount, setDeviceChannelCount] = useState<number>(2);

  useEffect(() => {
    setNameDraft(track?.name ?? "");
    setRoleDraft(track?.role ?? "");
  }, [track?.id, track?.name, track?.role]);

  useEffect(() => {
    if (!track || track.kind === "video") return;
    let cancelled = false;
    void api.inputDeviceChannelCount(inputDevice || undefined)
      .then((count) => { if (!cancelled) setDeviceChannelCount(Math.max(1, Math.min(64, count))); })
      .catch(() => undefined);
    return () => { cancelled = true; };
  }, [track?.id, track?.kind, inputDevice]);

  if (!track) {
    return (
      <aside className="track-inspector">
        <div className="inspector-empty">
          <strong>{selectionCount > 1 ? "Multiple Tracks Selected" : "No Track Selected"}</strong>
          <span>{selectionCount > 1 ? "Select one track to edit recording input and mix details." : "Click a track to show its details."}</span>
        </div>
      </aside>
    );
  }

  const isVideo = track.kind === "video";
  const durationSeconds = (source?.durationSamples ?? 0) / sampleRate;
  const startSeconds = track.startSample / sampleRate;
  const commitIdentity = () => {
    const nextName = nameDraft.trim();
    const nextRole = roleDraft.trim();
    const patch: Partial<Track> = {};
    if (nextName && nextName !== track.name) patch.name = nextName;
    if ((nextRole || undefined) !== track.role) patch.role = nextRole || undefined;
    if (Object.keys(patch).length > 0) onChange(track, patch);
  };
  return (
    <aside className="track-inspector">
      <div className="inspector-track-title" style={{ borderLeftColor: track.color }}>
        <input
          className="inspector-name-input"
          value={nameDraft}
          onChange={(event) => setNameDraft(event.target.value)}
          onBlur={commitIdentity}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
            if (event.key === "Escape") {
              setNameDraft(track.name);
              event.currentTarget.blur();
            }
          }}
          aria-label="Track name"
        />
        <input
          className="inspector-role-input"
          value={roleDraft}
          onChange={(event) => setRoleDraft(event.target.value)}
          onBlur={commitIdentity}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
            if (event.key === "Escape") {
              setRoleDraft(track.role ?? "");
              event.currentTarget.blur();
            }
          }}
          placeholder="track role"
          aria-label="Track role"
        />
      </div>
      <div className="inspector-section">
        <div className="inspector-section-title">{isVideo ? "Camera input" : "Record input"}</div>
        {isVideo ? (
          <>
            <label className="inspector-field">
              <span><Camera size={12} /> Camera</span>
              <select value={cameraDevice} onChange={(event) => onCameraDeviceChange(track.id, event.target.value)} onFocus={onRefreshCameraDevices}>
                <option value="">Default camera</option>
                {cameraDevices.map((device, index) => (
                  <option key={device.deviceId || index} value={device.deviceId}>{device.label || `Camera ${index + 1}`}</option>
                ))}
              </select>
            </label>
            <label className="inspector-check">
              <input type="checkbox" checked={cameraAudio} onChange={(event) => onCameraAudioChange(track.id, event.target.checked)} />
              <span>Create audio track</span>
            </label>
          </>
        ) : (
          <>
            <label className="inspector-field">
              <span><Mic size={12} /> Input</span>
              <select value={inputDevice} onChange={(event) => onInputDeviceChange(track.id, event.target.value)} onFocus={onRefreshInputDevices}>
                <option value="">Default input</option>
                {inputDevices.map((device) => <option key={device} value={device}>{device}</option>)}
              </select>
            </label>
            {(() => {
              const trackStereo = (source?.channels ?? 1) >= 2;
              // Build labels: for a 2-ch interface, label 0 = "L", 1 = "R"; otherwise "Ch N".
              const labelFor = (idx: number) => deviceChannelCount === 2
                ? (idx === 0 ? "L" : "R")
                : `Ch ${idx + 1}`;
              const opts: number[] = Array.from({ length: deviceChannelCount }, (_, i) => i);
              const levelAt = (idx: number) => Math.max(0, Math.min(1, inputChannelLevels[idx] ?? 0));
              if (!trackStereo) {
                const current = inputChannels[0] ?? 0;
                return (
                  <label className="inspector-field" title="Which physical input channel of the interface to record. The bar shows live level.">
                    <span>Channel</span>
                    <select value={current} onChange={(event) => onInputChannelsChange(track.id, [Number(event.target.value)])}>
                      {opts.map((idx) => <option key={idx} value={idx}>{labelFor(idx)}</option>)}
                    </select>
                    <div className="input-meter" aria-label={`Input level on ${labelFor(current)}`}>
                      <span style={{ width: `${levelAt(current) * 100}%` }} />
                    </div>
                  </label>
                );
              }
              const left = inputChannels[0] ?? 0;
              const right = inputChannels[1] ?? (deviceChannelCount > 1 ? 1 : 0);
              return (
                <>
                  <label className="inspector-field" title="Left input channel. The bar shows live level.">
                    <span>L</span>
                    <select value={left} onChange={(event) => onInputChannelsChange(track.id, [Number(event.target.value), right])}>
                      {opts.map((idx) => <option key={idx} value={idx}>{labelFor(idx)}</option>)}
                    </select>
                    <div className="input-meter">
                      <span style={{ width: `${levelAt(left) * 100}%` }} />
                    </div>
                  </label>
                  <label className="inspector-field" title="Right input channel. The bar shows live level.">
                    <span>R</span>
                    <select value={right} onChange={(event) => onInputChannelsChange(track.id, [left, Number(event.target.value)])}>
                      {opts.map((idx) => <option key={idx} value={idx}>{labelFor(idx)}</option>)}
                    </select>
                    <div className="input-meter">
                      <span style={{ width: `${levelAt(right) * 100}%` }} />
                    </div>
                  </label>
                </>
              );
            })()}
            <label className="inspector-field" title="Input gain. Double-click the slider to reset to 0 dB.">
              <span>In</span>
              <input
                type="range"
                min="-24"
                max="24"
                step="0.5"
                value={inputGainDb}
                onChange={(event) => onInputGainChange(track.id, Number(event.target.value))}
                onDoubleClick={() => onInputGainChange(track.id, 0)}
              />
              <em>{formatDb(inputGainDb)}</em>
            </label>
            <InspectorSlider
              label="Latency"
              title="Compensates the recording's placement on the timeline to align with what was actually playing. Positive ms = shift earlier. Double-click the slider to reset."
              value={track.inputLatencyMs ?? 0}
              min={-200}
              max={500}
              step={1}
              format={(v) => `${Math.round(v)} ms`}
              resetValue={0}
              onCommit={(v) => onChange(track, { inputLatencyMs: v })}
            />
          </>
        )}
      </div>
      <div className="inspector-section">
        <div className="inspector-section-title">Channel</div>
        <InspectorSlider
          label="Vol"
          title="Track volume. Double-click the slider to reset to 0 dB."
          value={track.gainDb}
          min={-24}
          max={12}
          step={0.5}
          format={formatDb}
          resetValue={0}
          onCommit={(v) => onChange(track, { gainDb: v })}
        />
        <InspectorSlider
          label="Pan"
          title="Stereo pan. Double-click the slider to recenter."
          value={track.pan}
          min={-1}
          max={1}
          step={0.05}
          format={formatPan}
          resetValue={0}
          onCommit={(v) => onChange(track, { pan: v })}
        />
        <label className="inspector-check">
          <input type="checkbox" checked={!!track.aiGenerated} onChange={(event) => onChange(track, { aiGenerated: event.target.checked })} />
          <span>AI generated stem</span>
        </label>
        <label className="inspector-check">
          <input type="checkbox" checked={track.solo} onChange={(event) => onChange(track, { solo: event.target.checked })} />
          <span>Solo</span>
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
  focused,
  armed,
  clips,
  selectedClipId,
  selectedClipIds,
  selectedRange,
  recording,
  recordingStarting,
  recordingStartSeconds,
  monitoring,
  monitorStarting,
  livePeaks,
  peak,
  playhead,
  transportPlaying,
  duration,
  alignmentCandidates,
  alignmentGuideSeconds,
  onToggleSelect,
  onClipSelect,
  onClipContextMenu,
  onClipMove,
  cutToolActive,
  onClipCut,
  onCutHover,
  onAlignmentGuideChange,
  onRangeSelect,
  onRangeClear,
  onArm,
  onMute,
  onSeek,
  onSelectTrack,
  onChange,
  onDragOver,
  onDrop,
}: {
  track: Track;
  selected: boolean;
  focused: boolean;
  armed: boolean;
  clips: { id: string; kind?: "audio" | "video"; name: string; startSeconds: number; sourceSeconds: number; peaks?: number[]; src?: string }[];
  selectedClipId?: string;
  selectedClipIds: string[];
  selectedRange?: { start: number; end: number };
  recording: boolean;
  recordingStarting: boolean;
  recordingStartSeconds?: number;
  monitoring: boolean;
  monitorStarting: boolean;
  livePeaks?: number[];
  peak: number;
  playhead: number;
  transportPlaying: boolean;
  duration: number;
  alignmentCandidates: number[];
  alignmentGuideSeconds?: number;
  onToggleSelect: () => void;
  onClipSelect: (clipId: string, additive?: boolean) => void;
  onClipContextMenu: (clipId: string, event: React.MouseEvent) => void;
  onClipMove: (clipId: string, deltaSeconds: number) => void;
  cutToolActive: boolean;
  onClipCut: (clipId: string, atSeconds: number) => void;
  onCutHover: (seconds: number | undefined) => void;
  onAlignmentGuideChange: (seconds: number | undefined) => void;
  onRangeSelect: (start: number, end: number) => void;
  onRangeClear: () => void;
  onArm: () => void;
  onMute: () => void;
  onSeek: (seconds: number) => void;
  onSelectTrack: (additive: boolean) => void;
  onChange: (patch: Partial<Track>) => void;
  onDragOver: (event: React.DragEvent) => void;
  onDrop: (event: React.DragEvent) => void;
}) {
  const dragRef = useRef<{ start: number; moved: boolean; clipId?: string; clipStart?: number; previewStart?: number } | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const [movePreview, setMovePreview] = useState<{ clipId: string; startSeconds: number; aligned: boolean } | undefined>();
  const secondsFromClientX = (clientX: number) => {
    const rect = wrapRef.current?.getBoundingClientRect();
    if (!rect || rect.width <= 0) return 0;
    const fraction = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
    return fraction * duration;
  };
  const secondsFromPointer = (event: React.PointerEvent<HTMLElement>) => {
    const rect = wrapRef.current?.getBoundingClientRect() ?? event.currentTarget.getBoundingClientRect();
    const fraction = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    return fraction * duration;
  };
  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    const seconds = secondsFromPointer(event);
    dragRef.current = { start: seconds, moved: false };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const handleClipPointerDown = (clipId: string, event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    // Cut tool: click splits the clip at the pointer position; no select, no drag.
    if (cutToolActive) {
      onClipCut(clipId, secondsFromClientX(event.clientX));
      return;
    }
    // Cmd/Ctrl-click toggles the clip into the multi-selection without starting a drag.
    if (event.metaKey || event.ctrlKey) {
      onClipSelect(clipId, true);
      return;
    }
    onClipSelect(clipId, false);
    // Only start a drag-move when Shift is held — plain clicks just select so the
    // user can't move a clip by accident while picking it.
    if (!event.shiftKey) return;
    const clipStart = clips.find((item) => item.id === clipId)?.startSeconds ?? 0;
    dragRef.current = { start: secondsFromClientX(event.clientX), moved: false, clipId, clipStart, previewStart: clipStart };
    setMovePreview({ clipId, startSeconds: clipStart, aligned: false });
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const handleClipPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag?.clipId) return;
    event.stopPropagation();
    const seconds = secondsFromClientX(event.clientX);
    if (Math.abs(seconds - drag.start) > 0.03) {
      drag.moved = true;
      const rawStart = Math.max(0, (drag.clipStart ?? 0) + seconds - drag.start);
      const nearest = nearestAlignment(rawStart, alignmentCandidates, drag.clipStart ?? rawStart);
      const startSeconds = nearest ?? rawStart;
      drag.previewStart = startSeconds;
      setMovePreview({ clipId: drag.clipId, startSeconds, aligned: nearest !== undefined });
      onAlignmentGuideChange(nearest);
    }
  };
  const handleClipPointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag?.clipId) return;
    dragRef.current = null;
    setMovePreview(undefined);
    onAlignmentGuideChange(undefined);
    event.stopPropagation();
    event.currentTarget.releasePointerCapture(event.pointerId);
    const seconds = secondsFromClientX(event.clientX);
    if (drag.moved) {
      const targetStart = drag.previewStart ?? Math.max(0, (drag.clipStart ?? 0) + seconds - drag.start);
      onClipMove(drag.clipId, targetStart - (drag.clipStart ?? targetStart));
    } else {
      // Click on a clip selects the clip (and ensures the parent track is selected).
      // The playhead only moves from the top ruler now.
      onClipSelect(drag.clipId, false);
      onSelectTrack(false);
      void seconds;
    }
  };
  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const seconds = secondsFromPointer(event);
    if (Math.abs(seconds - drag.start) > 0.03) {
      drag.moved = true;
      // Range selection lives on the top time ruler only — drag on a track lane is a no-op.
    }
  };
  const handlePointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    event.stopPropagation();
    event.currentTarget.releasePointerCapture(event.pointerId);
    // Whether the pointer moved or not, a press on the track lane just selects this
    // track (Cmd/Ctrl-click adds to the multi-selection). Range selection is exclusive
    // to the top ruler now.
    onRangeClear();
    onSelectTrack(event.metaKey || event.ctrlKey);
  };
  const cursorPct = duration > 0 ? Math.max(0, Math.min(100, (playhead / duration) * 100)) : 0;
  const rangeStartPct = selectedRange && duration > 0 ? Math.max(0, Math.min(100, (selectedRange.start / duration) * 100)) : 0;
  const rangeEndPct = selectedRange && duration > 0 ? Math.max(0, Math.min(100, (selectedRange.end / duration) * 100)) : 0;
  const alignmentGuidePct = alignmentGuideSeconds !== undefined && duration > 0 ? Math.max(0, Math.min(100, (alignmentGuideSeconds / duration) * 100)) : undefined;
  const liveLevel = livePeaks?.length ? livePeaks[livePeaks.length - 1] : 0;
  const isVideo = track.kind === "video";
  const recordingLeftPct = recordingStartSeconds !== undefined && duration > 0 ? Math.max(0, Math.min(100, (recordingStartSeconds / duration) * 100)) : 0;
  const recordingWidthPct = recordingStartSeconds !== undefined && duration > 0
    ? Math.max(0.6, Math.min(100 - recordingLeftPct, ((Math.max(playhead, recordingStartSeconds) - recordingStartSeconds) / duration) * 100))
    : 0;
  return (
    <div
      className={`track ${selected ? "selected" : ""} ${focused ? "focused" : ""} ${armed ? "armed" : ""} ${recording ? "recording" : ""} ${track.muted ? "muted" : ""}`}
      onDragOver={onDragOver}
      onDrop={onDrop}
      role="option"
      aria-selected={selected}
    >
      <div className="track-head" style={{ ['--track-color' as string]: track.color }}>
        <button
          className={`record-arm ${armed ? "active" : ""}`}
          title={armed ? "Record enabled. Click to disarm." : "Record enable this track (or group)"}
          onClick={(event) => { event.stopPropagation(); onArm(); }}
          aria-pressed={armed}
        >R</button>
        <button
          className={`mute-btn ${track.muted ? "active" : ""}`}
          title={track.muted ? "Muted. Click to unmute." : "Mute this track (or group)"}
          onClick={(event) => { event.stopPropagation(); onMute(); }}
          aria-pressed={track.muted}
        >M</button>
        <button
          className={`solo-btn ${selected ? "active" : ""}`}
          title={selected ? "In group. Click to remove from group." : "Add to group"}
          onClick={(event) => { event.stopPropagation(); onToggleSelect(); }}
          aria-pressed={selected}
        >S</button>
        {!isVideo && (recording || monitoring) ? (
          <div className={`track-record-meter ${recording ? "recording" : "monitoring"}`} title="Live input level">
            <span style={{ width: `${Math.max(2, Math.min(100, liveLevel * 100))}%` }} />
          </div>
        ) : null}
        {!isVideo && !recording && !monitoring ? (
          <div className="track-vu" title="Playback level" aria-hidden="true">
            <span style={{ height: `${Math.max(0, Math.min(100, peak * 100))}%`, background: track.color }} />
          </div>
        ) : null}
      </div>
      <div
        ref={wrapRef}
        className={`wave-wrap ${cutToolActive ? "cut-mode" : ""}`}
        style={{ ['--track-color' as string]: track.color }}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onMouseMove={cutToolActive ? (event) => onCutHover(secondsFromClientX(event.clientX)) : undefined}
        onMouseLeave={cutToolActive ? () => onCutHover(undefined) : undefined}
        title={cutToolActive ? "Click on a clip to split it at the cursor." : "Click to select this track."}
      >
        {clips.map((clip) => {
          const preview = movePreview?.clipId === clip.id ? movePreview : undefined;
          const visualStart = preview?.startSeconds ?? clip.startSeconds;
          const clipLeftPct = duration > 0 ? (visualStart / duration) * 100 : 0;
          const clipWidthPct = duration > 0 ? (clip.sourceSeconds / duration) * 100 : 100;
          return (
            <div
              key={clip.id}
              data-clip-id={clip.id}
              className={`wave-clip ${clip.kind === "video" ? "video-clip" : ""} ${selectedClipId === clip.id || selectedClipIds.includes(clip.id) ? "selected" : ""} ${preview ? "moving" : ""} ${preview?.aligned ? "aligned" : ""}`}
              style={{ left: `${clipLeftPct}%`, width: `${clipWidthPct}%`, borderLeftColor: track.color }}
              title={clip.name}
              onPointerDown={(event) => handleClipPointerDown(clip.id, event)}
              onPointerMove={handleClipPointerMove}
              onPointerUp={handleClipPointerUp}
              onContextMenu={(event) => onClipContextMenu(clip.id, event)}
            >
              {clip.kind === "video"
                ? <VideoStrip color={track.color} />
                : <Waveform peaks={clip.peaks} color={track.color} />}
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
        {alignmentGuidePct !== undefined ? <div className="alignment-guide" style={{ left: `${alignmentGuidePct}%` }} /> : null}
        {recording && recordingStartSeconds !== undefined ? (
          <div
            className={`wave-clip recording-live ${isVideo ? "video-clip" : ""}`}
            style={{ left: `${recordingLeftPct}%`, width: `${recordingWidthPct}%`, borderLeftColor: track.color }}
            title="Recording"
          >
            {isVideo ? <VideoStrip color={track.color} /> : <Waveform peaks={livePeaks ?? []} color={track.color} />}
            <span className="clip-label">{recordingStarting ? "Opening input" : "Recording"}</span>
          </div>
        ) : null}
        {/* Audio armed-monitoring: show live input waveform + a small "Input" badge.
            For video tracks we used to paint a full-lane VideoStrip with a "Camera"
            label — too much; just light the R button instead. */}
        {monitoring && !recording && !isVideo ? <LiveWaveform peaks={livePeaks ?? []} color={track.color} /> : null}
        {monitoring && !isVideo ? <div className="recording-overlay monitor">{monitorStarting ? "Opening input" : "Input"}</div> : null}
        <div className="playhead" style={{ left: `${cursorPct}%` }} />
      </div>
    </div>
  );
}

const VideoStrip = memo(function VideoStrip({ color }: { color: string }) {
  return (
    <div className="video-strip" style={{ backgroundColor: color }}>
      <Video size={18} />
    </div>
  );
});

function nearestAlignment(seconds: number, candidates: number[], originalSeconds: number) {
  let best: number | undefined;
  let bestDistance = 0.035;
  for (const candidate of candidates) {
    if (!Number.isFinite(candidate) || Math.abs(candidate - originalSeconds) < 0.0005) continue;
    const distance = Math.abs(candidate - seconds);
    if (distance <= bestDistance) {
      best = candidate;
      bestDistance = distance;
    }
  }
  return best;
}

function TimelineVideo({ src, color, active, localTime, playing }: { src?: string; color: string; active: boolean; localTime: number; playing: boolean }) {
  const ref = useRef<HTMLVideoElement>(null);
  const [failed, setFailed] = useState(false);
  const url = src ? convertFileSrc(src) : undefined;
  useEffect(() => {
    const video = ref.current;
    if (!video || !url) return;
    // Only re-sync when the playhead is clearly out of sync (>0.6s). At play time the
    // browser's media clock advances on its own; constant re-seeks on small drift make
    // playback stutter and feel slow.
    if (Number.isFinite(localTime) && Math.abs(video.currentTime - localTime) > 0.6) {
      video.currentTime = localTime;
    }
    if (active && playing) {
      void video.play().catch(() => undefined);
    } else {
      video.pause();
    }
  }, [active, localTime, playing, url]);
  if (!url || failed) return <VideoStrip color={color} />;
  return <video className="video-preview" ref={ref} src={url} muted playsInline preload="auto" onLoadedData={() => setFailed(false)} onError={() => setFailed(true)} />;
}

function ProgramPreviewVideo({
  src,
  localTime,
  playing,
  muted,
  size,
  onTime,
  onPause,
  onResize,
}: {
  src?: string;
  localTime: number;
  playing: boolean;
  muted: boolean;
  size: "small" | "medium" | "large";
  onTime: (seconds: number) => void;
  onPause: () => void;
  onResize: () => void;
}) {
  const ref = useRef<HTMLVideoElement>(null);
  const wasPlayingRef = useRef(false);
  const [failed, setFailed] = useState(false);
  const [portraitDisplay, setPortraitDisplay] = useState(false);
  const url = src ? convertFileSrc(src) : undefined;
  useEffect(() => {
    const video = ref.current;
    if (!video || !url) return;
    if ((!playing || !wasPlayingRef.current) && Number.isFinite(localTime) && Math.abs(video.currentTime - localTime) > 0.12) {
      video.currentTime = localTime;
    }
    if (playing) {
      void video.play().catch(() => undefined);
    } else {
      video.pause();
    }
    wasPlayingRef.current = playing;
  }, [localTime, playing, url]);
  if (!url || failed) {
    return (
      <div className="program-preview-fallback">
        <Video size={38} />
        <span>Video preview unavailable</span>
      </div>
    );
  }
  return (
    <div className={`program-preview-fit ${portraitDisplay ? "portrait" : ""} size-${size}`}>
      <video
        className="program-preview-video"
        ref={ref}
        src={url}
        muted={muted}
        playsInline
        preload="auto"
        controls
        onLoadedMetadata={(event) => {
          const video = event.currentTarget;
          setPortraitDisplay(video.videoWidth > video.videoHeight);
        }}
        onLoadedData={() => setFailed(false)}
        onError={() => setFailed(true)}
        onTimeUpdate={(event) => onTime(event.currentTarget.currentTime)}
        onPause={onPause}
        onEnded={onPause}
        onClick={(event) => {
          if (event.detail === 1) onResize();
        }}
        title="Click to resize preview"
      />
    </div>
  );
}

// Memoized: peaks arrays are stable references on the project object, so the
// canvas neither re-renders nor redraws during playhead/meter updates.
const Waveform = memo(function Waveform({ peaks, color }: { peaks?: number[]; color: string }) {
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
});

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
      const visible = peaks.slice(-targetCount);
      if (!visible.length) {
        return;
      }
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
        onDoubleClick={() => { setLocal(0); void onChange(0); }}
        title="Master output gain. Double-click to reset to 0 dB."
      />
      <span className={`master-value ${local !== 0 ? "nonzero" : ""}`}>{formatDb(local)}</span>
      <button type="button" className="master-reset" onClick={() => { setLocal(0); void onChange(0); }} title="Reset to 0 dB">
        0 dB
      </button>
    </div>
  );
}

// ---------------- Mixer console ----------------

// Bottom-dock mixer: one channel strip per track plus a master strip.
// Subscribes to engine meters itself so the 30 Hz updates stay local.
function MixerDock({
  session,
  focusedTrackId,
  onFocusTrack,
  onChange,
  masterGainDb,
  onMasterGain,
}: {
  session: MixSession;
  focusedTrackId?: string;
  onFocusTrack: (trackId: string) => void;
  onChange: (track: Track, patch: Partial<Track>) => void;
  masterGainDb: number;
  onMasterGain: (gainDb: number) => void | Promise<void>;
}) {
  const [trackPeaks, setTrackPeaks] = useState<number[]>([]);
  const [masterPeak, setMasterPeak] = useState(0);
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.onMeters((event) => {
      setTrackPeaks(event.trackPeaks);
      setMasterPeak(event.masterPeak);
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch(() => undefined);
    return () => { cancelled = true; unlisten?.(); };
  }, []);
  return (
    <div className="mixer-dock">
      <div className="mixer-strips">
        {session.tracks.map((track, index) => (
          <ChannelStrip
            key={track.id}
            track={track}
            peak={trackPeaks[index] ?? 0}
            focused={focusedTrackId === track.id}
            onFocus={() => onFocusTrack(track.id)}
            onChange={onChange}
          />
        ))}
        <MasterStrip gainDb={masterGainDb} peak={masterPeak} onChange={onMasterGain} />
      </div>
    </div>
  );
}

function ChannelStrip({
  track,
  peak,
  focused,
  onFocus,
  onChange,
}: {
  track: Track;
  peak: number;
  focused: boolean;
  onFocus: () => void;
  onChange: (track: Track, patch: Partial<Track>) => void;
}) {
  const [gain, setGain] = useState(track.gainDb);
  useEffect(() => { setGain(track.gainDb); }, [track.gainDb]);
  const [pan, setPan] = useState(track.pan);
  useEffect(() => { setPan(track.pan); }, [track.pan]);
  return (
    <div
      className={`mixer-strip ${focused ? "focused" : ""}`}
      style={{ ["--track-color" as string]: track.color }}
      onPointerDown={onFocus}
    >
      <div className="strip-pan" title="Pan. Double-click to recenter.">
        <input
          type="range"
          min={-1}
          max={1}
          step={0.05}
          value={pan}
          onChange={(event) => setPan(Number(event.target.value))}
          onPointerUp={() => onChange(track, { pan })}
          onKeyUp={() => onChange(track, { pan })}
          onDoubleClick={() => { setPan(0); onChange(track, { pan: 0 }); }}
          aria-label={`${track.name} pan`}
        />
        <span>{formatPan(pan)}</span>
      </div>
      <div className="strip-body">
        <Fader
          value={gain}
          min={-24}
          max={12}
          label={`${track.name} volume`}
          onInput={setGain}
          onCommit={(v) => { setGain(v); onChange(track, { gainDb: v }); }}
        />
        <div className="strip-meter" aria-hidden="true">
          <span style={{ height: `${Math.max(0, Math.min(100, peak * 100))}%` }} />
        </div>
      </div>
      <div className="strip-gain">{formatDb(gain)}</div>
      <div className="strip-buttons">
        <button
          className={`strip-mute ${track.muted ? "active" : ""}`}
          onClick={(event) => { event.stopPropagation(); onChange(track, { muted: !track.muted }); }}
          title={track.muted ? "Unmute" : "Mute"}
          aria-pressed={track.muted}
        >M</button>
        <button
          className={`strip-solo ${track.solo ? "active" : ""}`}
          onClick={(event) => { event.stopPropagation(); onChange(track, { solo: !track.solo }); }}
          title={track.solo ? "Unsolo" : "Solo"}
          aria-pressed={track.solo}
        >S</button>
      </div>
      <div className="strip-name" title={track.name}>
        {track.kind === "video" ? <Video size={10} /> : null}
        <span>{track.name}</span>
      </div>
    </div>
  );
}

function MasterStrip({
  gainDb,
  peak,
  onChange,
}: {
  gainDb: number;
  peak: number;
  onChange: (gainDb: number) => void | Promise<void>;
}) {
  const [gain, setGain] = useState(gainDb);
  useEffect(() => { setGain(gainDb); }, [gainDb]);
  return (
    <div className="mixer-strip master">
      <div className="strip-pan" aria-hidden="true" />
      <div className="strip-body">
        <Fader
          value={gain}
          min={-24}
          max={12}
          label="Master volume"
          onInput={setGain}
          onCommit={(v) => { setGain(v); void onChange(v); }}
        />
        <div className="strip-meter" aria-hidden="true">
          <span style={{ height: `${Math.max(0, Math.min(100, peak * 100))}%` }} />
        </div>
      </div>
      <div className="strip-gain">{formatDb(gain)}</div>
      <div className="strip-buttons" aria-hidden="true" />
      <div className="strip-name master-label"><span>MASTER</span></div>
    </div>
  );
}

// Vertical fader: pointer-driven, 0.5 dB steps, arrow keys for keyboard users,
// double-click returns to unity.
function Fader({
  value,
  min,
  max,
  label,
  onInput,
  onCommit,
}: {
  value: number;
  min: number;
  max: number;
  label: string;
  onInput: (value: number) => void;
  onCommit: (value: number) => void;
}) {
  const grooveRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);
  const valueFromPointer = (clientY: number) => {
    const rect = grooveRef.current?.getBoundingClientRect();
    if (!rect || rect.height <= 0) return value;
    const ratio = 1 - Math.min(1, Math.max(0, (clientY - rect.top) / rect.height));
    return Math.round((min + ratio * (max - min)) * 2) / 2;
  };
  const pct = ((Math.max(min, Math.min(max, value)) - min) / (max - min)) * 100;
  const zeroPct = ((0 - min) / (max - min)) * 100;
  return (
    <div
      className="fader"
      role="slider"
      tabIndex={0}
      aria-label={label}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={Math.round(value * 10) / 10}
      aria-valuetext={formatDb(value)}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        draggingRef.current = true;
        event.currentTarget.setPointerCapture(event.pointerId);
        onInput(valueFromPointer(event.clientY));
      }}
      onPointerMove={(event) => {
        if (draggingRef.current) onInput(valueFromPointer(event.clientY));
      }}
      onPointerUp={(event) => {
        if (!draggingRef.current) return;
        draggingRef.current = false;
        event.currentTarget.releasePointerCapture(event.pointerId);
        onCommit(valueFromPointer(event.clientY));
      }}
      onDoubleClick={() => onCommit(0)}
      onKeyDown={(event) => {
        const step = event.shiftKey ? 0.1 : 0.5;
        if (event.key === "ArrowUp") {
          event.preventDefault();
          onCommit(Math.min(max, Math.round((value + step) * 10) / 10));
        } else if (event.key === "ArrowDown") {
          event.preventDefault();
          onCommit(Math.max(min, Math.round((value - step) * 10) / 10));
        }
      }}
      title="Drag to set level. Double-click for 0 dB."
    >
      <div className="fader-groove" ref={grooveRef}>
        <div className="fader-scale" aria-hidden="true">
          {[100, 75, 50, 25, 0].map((tick) => <span key={tick} style={{ bottom: `${tick}%` }} />)}
        </div>
        <div className="fader-zero" style={{ bottom: `${zeroPct}%` }} />
        <div className="fader-cap" style={{ bottom: `calc(${pct}% - 9px)` }} />
      </div>
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
  onStart,
  onStop
}: {
  stages: { stageId: string; displayName: string; status: string; actionCount: number; warnings: string[]; error?: string; tokens: number; elapsedMs: number; explanation?: string }[];
  running: boolean;
  disabled: boolean;
  onStart: (stageIds: string[]) => void;
  onStop: () => void;
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
        {running ? (
          <button
            className="auto-mix-stop"
            onClick={onStop}
            title="Stop after the current model call — finished stages keep their changes"
          >
            <Square size={13} />
            <span>Stop</span>
          </button>
        ) : null}
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
              {s.status !== "running" && s.status !== "cancelled" ? (
                <div className="auto-mix-stage-actions">
                  {s.actionCount} action{s.actionCount === 1 ? "" : "s"} applied
                </div>
              ) : null}
              {s.error && s.status !== "cancelled" ? <div className="auto-mix-stage-error">{s.error}</div> : null}
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

// Musical position as bar.beat.sixteenth (4/4 assumed), 1-based like Cubase.
function formatBars(seconds: number, bpm: number) {
  const beats = Math.max(0, seconds) * bpm / 60;
  const bar = Math.floor(beats / 4) + 1;
  const beat = Math.floor(beats % 4) + 1;
  const sixteenth = Math.floor((beats % 1) * 4) + 1;
  return `${bar}.${beat}.${sixteenth}`;
}

// Transport LCD readout: m:ss.t with tenths, DAW-style.
function formatLcdTime(seconds: number) {
  const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  const min = Math.floor(safe / 60);
  const sec = Math.floor(safe % 60).toString().padStart(2, "0");
  const tenths = Math.floor((safe % 1) * 10);
  return `${min}:${sec}.${tenths}`;
}

// Master output meter in the transport LCD. Subscribes to engine meter events
// itself so the 30 Hz updates re-render only this tiny component.
function TransportMeter() {
  const [peak, setPeak] = useState(0);
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.onMeters((event) => setPeak(event.masterPeak))
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch(() => undefined);
    return () => { cancelled = true; unlisten?.(); };
  }, []);
  const pct = Math.max(0, Math.min(100, peak * 100));
  return (
    <div className="lcd-meter" title="Master output level">
      <span style={{ width: `${pct}%` }} />
    </div>
  );
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
  reasoning?: string;
  tools?: string[];
  tokens?: { prompt: number; response: number; elapsedMs: number };
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

type VideoResultMessage = { role: "video"; path: string; cuts: number; lookPreset?: string };

type ChatMessage =
  | { role: "user"; text: string }
  | { role: "assistant"; text: string }
  | { role: "system"; text: string }
  | CritiqueMessage
  | AbJudgeMessage
  | AssistantTurnMessage
  | VideoResultMessage;

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
      {message.reasoning ? (
        <details className="activity-reasoning turn-reasoning" open>
          <summary>Reasoning</summary>
          <pre>{message.reasoning}</pre>
        </details>
      ) : null}
      {message.tools && message.tools.length > 0 ? (
        <div className="activity-tools turn-tools">
          {message.tools.map((tool, i) => <span key={i} className="activity-tool">{tool}</span>)}
        </div>
      ) : null}
      {message.explanation ? (
        <div className="turn-prose markdown-body">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.explanation}</ReactMarkdown>
        </div>
      ) : null}
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
        {message.tokens && (message.tokens.prompt > 0 || message.tokens.response > 0) ? (
          <span className="turn-tokens" title="Tokens used this turn (prompt → response) and wall-clock time">
            {message.tokens.prompt.toLocaleString()} → {message.tokens.response.toLocaleString()} tok
            {message.tokens.elapsedMs > 0 ? ` · ${(message.tokens.elapsedMs / 1000).toFixed(1)}s` : ""}
          </span>
        ) : null}
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
        <button
          type="button"
          className={`turn-toggle ${toggleState}`}
          onClick={onToggle}
          disabled={toggleDisabled || toggleState === "locked"}
          title={
            toggleState === "locked"
              ? "This turn made no project changes, so there is nothing to bypass"
              : toggleState === "on" ? "Bypass this turn" : "Re-enable this turn"
          }
          aria-pressed={toggleState === "on"}
        >
          <Power size={14} />
          <span>{toggleState === "locked" ? "No changes" : toggleState === "on" ? "On" : "Off"}</span>
        </button>
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
  const winnerHint = result.winner === "after"
    ? "The processed mix beats the original"
    : result.winner === "before"
      ? "The unprocessed original beats the current mix"
      : "Too close to call between the mix and the original";
  const issues = result.mixIssuesAfter;

  return (
    <div className="message critique ab-judge">
      <div className="crit-head">
        <div className={`crit-score ${winnerClass}`} title={`${winnerHint} (${(result.confidence * 100).toFixed(0)}% confidence)`}>
          <span className="crit-score-value">{winner}</span>
          <span className="crit-score-label">{(result.confidence * 100).toFixed(0)}%</span>
        </div>
        <div className="crit-summary">
          <strong>A/B judge — {winnerHint.toLowerCase()}</strong>
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

function pickVideoMimeType() {
  const options = [
    "video/mp4;codecs=avc1.42E01E,mp4a.40.2",
    "video/mp4;codecs=h264,aac",
    "video/mp4",
    "video/webm;codecs=vp9,opus",
    "video/webm;codecs=vp8,opus",
    "video/webm",
  ];
  return options.find((type) => typeof MediaRecorder !== "undefined" && MediaRecorder.isTypeSupported(type)) ?? "";
}

function sleep(ms: number) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

async function prepareVideoRecordingStream(stream: MediaStream) {
  const video = document.createElement("video");
  video.dataset.recorderStream = stream.id;
  video.muted = true;
  video.playsInline = true;
  video.autoplay = true;
  video.srcObject = stream;
  Object.assign(video.style, {
    position: "fixed",
    width: "320px",
    height: "180px",
    opacity: "0.01",
    pointerEvents: "none",
    left: "-10000px",
    top: "-10000px",
  });
  document.body.appendChild(video);
  await video.play().catch(() => undefined);
  await new Promise<void>((resolve) => {
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      resolve();
    };
    if (video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
      finish();
      return;
    }
    video.addEventListener("loadeddata", finish, { once: true });
    const [track] = stream.getVideoTracks();
    if (track && track.muted) {
      track.addEventListener("unmute", finish, { once: true });
    }
    window.setTimeout(finish, 700);
  });
  await waitForNonBlackFrame(video);
  return video;
}

async function waitForNonBlackFrame(video: HTMLVideoElement) {
  const canvas = document.createElement("canvas");
  canvas.width = 64;
  canvas.height = 36;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) return;
  for (let attempt = 0; attempt < 24; attempt++) {
    await sleep(80);
    if (video.readyState < HTMLMediaElement.HAVE_CURRENT_DATA || video.videoWidth === 0 || video.videoHeight === 0) {
      continue;
    }
    context.drawImage(video, 0, 0, canvas.width, canvas.height);
    const data = context.getImageData(0, 0, canvas.width, canvas.height).data;
    let lumaSum = 0;
    let lumaSqSum = 0;
    const pixels = data.length / 4;
    for (let i = 0; i < data.length; i += 4) {
      const luma = data[i] * 0.2126 + data[i + 1] * 0.7152 + data[i + 2] * 0.0722;
      lumaSum += luma;
      lumaSqSum += luma * luma;
    }
    const mean = lumaSum / pixels;
    const variance = lumaSqSum / pixels - mean * mean;
    if (mean > 3 || variance > 2) return;
  }
  throw new Error("Camera is delivering black frames. Close other camera apps/windows or select a different camera, then record again.");
}

function stopMediaRecorder(recorder: MediaRecorder, stream: MediaStream, chunks: Blob[], fallbackMimeType: string) {
  return new Promise<Blob>((resolve) => {
    recorder.onstop = () => {
      stream.getTracks().forEach((track) => track.stop());
      const previewElement = document.querySelector<HTMLVideoElement>(`video[data-recorder-stream="${stream.id}"]`);
      previewElement?.remove();
      resolve(new Blob(chunks, { type: recorder.mimeType || fallbackMimeType || "video/webm" }));
    };
    if (recorder.state === "inactive") {
      recorder.onstop(new Event("stop"));
    } else {
      recorder.requestData();
      recorder.stop();
    }
  });
}

function blobToDataUrl(blob: Blob) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () => reject(reader.error ?? new Error("Could not read video recording."));
    reader.readAsDataURL(blob);
  });
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
