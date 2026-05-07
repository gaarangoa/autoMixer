import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Download, FilePlus2, MessageSquare, Pause, Play, Power, RefreshCw, RotateCcw, RotateCw, Settings, Square, Trash2, Upload } from "lucide-react";
import type { AssistantResponse, HistoryEntry, JsonPatch, MixAction, MixCritique, MixProject, MixSession, Track } from "../../shared/types";
import { open, save } from "@tauri-apps/plugin-dialog";
import { api } from "./api";

const DEFAULT_OLLAMA_URL = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL = "gpt-oss:20b";

export function App() {
  const initialOllamaUrlRef = useRef(localStorage.getItem("autoMixer.ollamaUrl"));
  const initialOllamaModelRef = useRef(localStorage.getItem("autoMixer.ollamaModel"));
  const playStartedAtRef = useRef(0);
  const pausedAtRef = useRef(0);
  const [project, setProject] = useState<MixProject>();
  const [selectedTrackIds, setSelectedTrackIds] = useState<string[]>([]);
  const [selectedRegionIds, setSelectedRegionIds] = useState<string[]>([]);
  const [chatText, setChatText] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [startupError, setStartupError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [playhead, setPlayhead] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [ollamaUrl, setOllamaUrl] = useState(() => initialOllamaUrlRef.current ?? DEFAULT_OLLAMA_URL);
  const [ollamaModel, setOllamaModel] = useState(() => initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL);
  const [modelOptions, setModelOptions] = useState<string[]>(() => [initialOllamaModelRef.current ?? DEFAULT_OLLAMA_MODEL]);
  const [modelStatus, setModelStatus] = useState("Not checked");
  const [modelsLoading, setModelsLoading] = useState(false);

  const session = project?.session;
  const chatLogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const node = chatLogRef.current;
    if (!node) return;
    node.scrollTop = node.scrollHeight;
  }, [messages.length, busy]);

  useEffect(() => {
    void bootstrap();
  }, []);

  useEffect(() => {
    localStorage.setItem("autoMixer.ollamaUrl", ollamaUrl);
  }, [ollamaUrl]);

  useEffect(() => {
    localStorage.setItem("autoMixer.ollamaModel", ollamaModel);
    setModelOptions((items) => items.includes(ollamaModel) ? items : [...items, ollamaModel]);
  }, [ollamaModel]);

  useEffect(() => {
    let frame = 0;
    const tick = () => {
      setPlayhead(playing ? Math.max(0, (performance.now() - playStartedAtRef.current) / 1000) : pausedAtRef.current);
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing]);

  const duration = useMemo(() => {
    if (!session) return 0;
    const sources = new Map(session.sourceFiles.map((source) => [source.id, source]));
    return Math.max(0, ...session.tracks.map((track) => {
      const source = sources.get(track.sourceFileId);
      return ((track.startSample + (source?.durationSamples ?? 0)) / session.sampleRate);
    }));
  }, [session]);

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
      setSelectedTrackIds(updated.session.tracks.slice(-1).map((track) => track.id));
    } catch (error) {
      pushSystem(error);
    } finally {
      setBusy(false);
    }
  }

  async function updateTrack(track: Track, patch: Partial<Track>) {
    if (!session) return;
    const actions = [];
    if (patch.gainDb !== undefined) actions.push({ tool: "set_track_gain" as const, trackId: track.id, gainDb: patch.gainDb });
    if (patch.pan !== undefined) actions.push({ tool: "set_track_pan" as const, trackId: track.id, pan: patch.pan });
    if (patch.muted !== undefined) actions.push({ tool: "mute_track" as const, trackId: track.id, muted: patch.muted });
    if (patch.solo !== undefined) actions.push({ tool: "solo_track" as const, trackId: track.id, solo: patch.solo });
    if (!actions.length) return;
    const updated = await api.applyActions(session.id, actions, "Manual control change");
    setProject(updated);
  }

  async function deleteTrack(track: Track) {
    if (!session) return;
    const confirmed = window.confirm(`Delete track "${track.name}"?`);
    if (!confirmed) return;
    setBusy(true);
    try {
      const updated = await api.applyActions(session.id, [{ tool: "delete_track", trackId: track.id }], `Deleted ${track.name}`);
      setProject(updated);
      setSelectedTrackIds((ids) => ids.filter((id) => id !== track.id));
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
      const response = await api.assistant({
        sessionId: session.id,
        userText,
        selectedTrackIds,
        selectedRegionIds,
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
    const updated = await api.undo(session.id);
    setProject(updated);
  }

  async function doRedo() {
    if (!session) return;
    const updated = await api.redo(session.id);
    setProject(updated);
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

  async function stop() {
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
      setMessages([]);
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
    setMessages((items) => [...items, { role: "system", text: error instanceof Error ? error.message : "Unexpected error" }]);
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
          <div>
            <h1>AutoMixer</h1>
            <span>{session.tracks.length} tracks · {formatTime(playhead)} / {formatTime(duration)}</span>
          </div>
          <div className="transport">
            <button onClick={() => void togglePlay()} title={playing ? "Pause" : "Play"}>{playing ? <Pause size={18} /> : <Play size={18} />}</button>
            <button onClick={() => void stop()} title="Stop"><Square size={18} /></button>
            <button onClick={doUndo} title="Undo"><RotateCcw size={18} /></button>
            <button onClick={doRedo} title="Redo"><RotateCw size={18} /></button>
            <button onClick={() => void renderCurrentMix()} title="Export WAV"><Download size={18} /></button>
            <button className="upload" onClick={() => void importFiles()} title="Import audio">
              <Upload size={18} />
            </button>
            <button onClick={() => void resetSession()} title="New session" disabled={busy}>
              <FilePlus2 size={18} />
            </button>
          </div>
        </header>

        <div className="timeline">
          {session.tracks.length === 0 ? (
            <div className="empty">
              <Upload size={28} />
              <span>Import stems to start mixing.</span>
            </div>
          ) : (
            session.tracks.map((track) => {
              const source = session.sourceFiles.find((item) => item.id === track.sourceFileId);
              return (
                <TrackRow
                  key={track.id}
                  track={track}
                  selected={selectedTrackIds.includes(track.id)}
                  playhead={playhead}
                  duration={duration}
                  peaks={source?.peakPreview}
                  onSelect={() => setSelectedTrackIds([track.id])}
                  onChange={(patch) => void updateTrack(track, patch)}
                  onDelete={() => void deleteTrack(track)}
                />
              );
            })
          )}
        </div>

        <History project={project} />
      </section>

      <aside className={`assistant ${settingsOpen ? "settings-open" : ""}`}>
        <div className="assistant-head">
          <div className="assistant-title">
            <MessageSquare size={18} />
            <strong>Mix engineer</strong>
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
            <div className="llm-actions">
              <button onClick={() => void loadOllamaModels()} disabled={modelsLoading} title="Refresh models">
                <RefreshCw size={16} />
              </button>
              <span>{modelStatus}</span>
            </div>
          </div>
        ) : null}
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
        </div>
        <div className="selected">
          Selected: {selectedTrackIds.map((id) => session.tracks.find((track) => track.id === id)?.name).filter(Boolean).join(", ") || "none"}
        </div>
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
      </aside>
    </main>
  );
}

function TrackRow({
  track,
  selected,
  peaks,
  playhead,
  duration,
  onSelect,
  onChange,
  onDelete
}: {
  track: Track;
  selected: boolean;
  peaks?: number[];
  playhead: number;
  duration: number;
  onSelect: () => void;
  onChange: (patch: Partial<Track>) => void;
  onDelete: () => void;
}) {
  return (
    <div className={`track ${selected ? "selected" : ""}`} onClick={onSelect}>
      <div className="track-head" style={{ borderLeftColor: track.color }}>
        <strong>{track.name}</strong>
        <span>{track.role ?? "track"}</span>
        <div className="toggles">
          <button className={track.muted ? "active" : ""} onClick={(event) => { event.stopPropagation(); onChange({ muted: !track.muted }); }}>M</button>
          <button className={track.solo ? "active" : ""} onClick={(event) => { event.stopPropagation(); onChange({ solo: !track.solo }); }}>S</button>
          <button className="danger" title="Delete track" onClick={(event) => { event.stopPropagation(); onDelete(); }}><Trash2 size={14} /></button>
        </div>
        <label>Vol <input type="range" min="-24" max="12" step="0.5" value={track.gainDb} onChange={(event) => onChange({ gainDb: Number(event.target.value) })} /></label>
        <label>Pan <input type="range" min="-1" max="1" step="0.05" value={track.pan} onChange={(event) => onChange({ pan: Number(event.target.value) })} /></label>
      </div>
      <div className="wave-wrap">
        <Waveform peaks={peaks} color={track.color} />
        <div className="playhead" style={{ left: `${duration ? (playhead / duration) * 100 : 0}%` }} />
      </div>
    </div>
  );
}

function Waveform({ peaks, color }: { peaks?: number[]; color: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const scale = window.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.floor(rect.width * scale));
    canvas.height = Math.max(1, Math.floor(rect.height * scale));
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(scale, scale);
    ctx.clearRect(0, 0, rect.width, rect.height);
    ctx.fillStyle = "#161b20";
    ctx.fillRect(0, 0, rect.width, rect.height);
    if (!peaks?.length) return;
    const step = Math.max(1, peaks.length / rect.width);
    ctx.strokeStyle = color;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let x = 0; x < rect.width; x++) {
      const sample = Math.min(1, Math.max(0.02, peaks[Math.min(peaks.length - 1, Math.floor(x * step))] ?? 0));
      const min = -sample;
      const max = sample;
      const y1 = ((1 - max) * rect.height) / 2;
      const y2 = ((1 - min) * rect.height) / 2;
      ctx.moveTo(x, y1);
      ctx.lineTo(x, y2);
    }
    ctx.stroke();
  }, [peaks, color]);
  return <canvas ref={canvasRef} />;
}

function History({ project }: { project: MixProject }) {
  return (
    <div className="history">
      <strong>History</strong>
      {project.history.slice(-5).reverse().map((entry) => (
        <span key={entry.id}>{entry.explanation ?? entry.source}</span>
      ))}
    </div>
  );
}

function formatTime(seconds: number) {
  const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  const min = Math.floor(safe / 60);
  const sec = Math.floor(safe % 60).toString().padStart(2, "0");
  return `${min}:${sec}`;
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

type ChatMessage =
  | { role: "user"; text: string }
  | { role: "assistant"; text: string }
  | { role: "system"; text: string }
  | CritiqueMessage
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
