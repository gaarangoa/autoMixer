import { useEffect, useMemo, useRef, useState } from "react";
import { Download, MessageSquare, Pause, Play, RotateCcw, RotateCw, Square, Upload } from "lucide-react";
import type { AssistantResponse, MixProject, MixSession, Track } from "../../shared/types";
import { api } from "./api";
import { exportMix } from "./audio/export";
import { MixEngine } from "./audio/MixEngine";

export function App() {
  const engineRef = useRef(new MixEngine());
  const [project, setProject] = useState<MixProject>();
  const [selectedTrackIds, setSelectedTrackIds] = useState<string[]>([]);
  const [selectedRegionIds, setSelectedRegionIds] = useState<string[]>([]);
  const [chatText, setChatText] = useState("");
  const [messages, setMessages] = useState<{ role: "user" | "assistant" | "system"; text: string }[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [playhead, setPlayhead] = useState(0);

  const session = project?.session;

  useEffect(() => {
    void bootstrap();
  }, []);

  useEffect(() => {
    if (!session) return;
    void engineRef.current.loadSession(session).then(() => engineRef.current.updateSession(session));
  }, [session?.id, session?.tracks.length, session]);

  useEffect(() => {
    let frame = 0;
    const tick = () => {
      setPlayhead(engineRef.current.getPlayhead());
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, []);

  const duration = useMemo(() => {
    if (!session) return 0;
    return Math.max(0, ...session.tracks.map((track) => engineRef.current.getBuffer(track.id)?.duration ?? 0));
  }, [session, project?.session.tracks.length]);

  async function bootstrap() {
    setLoading(true);
    try {
      const sessions = await api.sessions();
      const loaded = sessions[0] ? await api.getSession(sessions[0].id) : await api.createSession("AutoMixer session");
      setProject(loaded);
    } catch (error) {
      setMessages([{ role: "system", text: error instanceof Error ? error.message : "Could not start app." }]);
    } finally {
      setLoading(false);
    }
  }

  async function importFiles(files: FileList | null) {
    if (!session || !files?.length) return;
    setBusy(true);
    try {
      const updated = await api.importFiles(session.id, Array.from(files));
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

  async function sendChat() {
    if (!session || !chatText.trim()) return;
    const userText = chatText.trim();
    setChatText("");
    setMessages((items) => [...items, { role: "user", text: userText }]);
    setBusy(true);
    try {
      const response = await api.assistant({
        sessionId: session.id,
        userText,
        selectedTrackIds,
        selectedRegionIds
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
      setMessages((items) => [
        ...items,
        { role: "assistant", text: `${response.explanation}\nSkills: ${response.selectedSkills.join(", ")}` }
      ]);
      engineRef.current.updateSession(response.session);
    } else if (response.status === "clarification") {
      setMessages((items) => [...items, { role: "assistant", text: response.question }]);
    } else {
      setMessages((items) => [...items, { role: "system", text: response.message }]);
    }
  }

  async function doUndo() {
    if (!session) return;
    const updated = await api.undo(session.id);
    setProject(updated);
    engineRef.current.updateSession(updated.session);
  }

  async function doRedo() {
    if (!session) return;
    const updated = await api.redo(session.id);
    setProject(updated);
    engineRef.current.updateSession(updated.session);
  }

  function togglePlay() {
    if (!session) return;
    if (playing) {
      engineRef.current.pause();
      setPlaying(false);
    } else {
      void engineRef.current.ensureContext().then(() => {
        engineRef.current.play(session);
        setPlaying(true);
      });
    }
  }

  function stop() {
    engineRef.current.stop();
    setPlaying(false);
  }

  function pushSystem(error: unknown) {
    setMessages((items) => [...items, { role: "system", text: error instanceof Error ? error.message : "Unexpected error" }]);
  }

  if (loading || !project || !session) return <main className="loading">Loading AutoMixer...</main>;

  return (
    <main className="app">
      <section className="mix">
        <header className="topbar">
          <div>
            <h1>AutoMixer</h1>
            <span>{session.tracks.length} tracks · {formatTime(playhead)} / {formatTime(duration)}</span>
          </div>
          <div className="transport">
            <button onClick={togglePlay} title={playing ? "Pause" : "Play"}>{playing ? <Pause size={18} /> : <Play size={18} />}</button>
            <button onClick={stop} title="Stop"><Square size={18} /></button>
            <button onClick={doUndo} title="Undo"><RotateCcw size={18} /></button>
            <button onClick={doRedo} title="Redo"><RotateCw size={18} /></button>
            <button onClick={() => void exportMix(session)} title="Export WAV"><Download size={18} /></button>
            <label className="upload">
              <Upload size={18} />
              <input type="file" multiple accept="audio/*" onChange={(event) => void importFiles(event.target.files)} />
            </label>
          </div>
        </header>

        <div className="timeline">
          {session.tracks.length === 0 ? (
            <div className="empty">
              <Upload size={28} />
              <span>Import stems to start mixing.</span>
            </div>
          ) : (
            session.tracks.map((track) => (
              <TrackRow
                key={track.id}
                track={track}
                selected={selectedTrackIds.includes(track.id)}
                playhead={playhead}
                duration={duration}
                buffer={engineRef.current.getBuffer(track.id)}
                onSelect={() => setSelectedTrackIds([track.id])}
                onChange={(patch) => void updateTrack(track, patch)}
              />
            ))
          )}
        </div>

        <History project={project} />
      </section>

      <aside className="assistant">
        <div className="assistant-head">
          <MessageSquare size={18} />
          <strong>Mix engineer</strong>
        </div>
        <div className="chat-log">
          {messages.length === 0 ? (
            <div className="hint">Select a track and ask for a mix change.</div>
          ) : (
            messages.map((message, index) => <div key={index} className={`message ${message.role}`}>{message.text}</div>)
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
  buffer,
  playhead,
  duration,
  onSelect,
  onChange
}: {
  track: Track;
  selected: boolean;
  buffer?: AudioBuffer;
  playhead: number;
  duration: number;
  onSelect: () => void;
  onChange: (patch: Partial<Track>) => void;
}) {
  return (
    <div className={`track ${selected ? "selected" : ""}`} onClick={onSelect}>
      <div className="track-head" style={{ borderLeftColor: track.color }}>
        <strong>{track.name}</strong>
        <span>{track.role ?? "track"}</span>
        <div className="toggles">
          <button className={track.muted ? "active" : ""} onClick={(event) => { event.stopPropagation(); onChange({ muted: !track.muted }); }}>M</button>
          <button className={track.solo ? "active" : ""} onClick={(event) => { event.stopPropagation(); onChange({ solo: !track.solo }); }}>S</button>
        </div>
        <label>Vol <input type="range" min="-24" max="12" step="0.5" value={track.gainDb} onChange={(event) => onChange({ gainDb: Number(event.target.value) })} /></label>
        <label>Pan <input type="range" min="-1" max="1" step="0.05" value={track.pan} onChange={(event) => onChange({ pan: Number(event.target.value) })} /></label>
      </div>
      <div className="wave-wrap">
        <Waveform buffer={buffer} color={track.color} />
        <div className="playhead" style={{ left: `${duration ? (playhead / duration) * 100 : 0}%` }} />
      </div>
    </div>
  );
}

function Waveform({ buffer, color }: { buffer?: AudioBuffer; color: string }) {
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
    if (!buffer) return;
    const data = buffer.getChannelData(0);
    const step = Math.max(1, Math.floor(data.length / rect.width));
    ctx.strokeStyle = color;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let x = 0; x < rect.width; x++) {
      let min = 1;
      let max = -1;
      const start = x * step;
      for (let i = 0; i < step && start + i < data.length; i++) {
        const sample = data[start + i];
        if (sample < min) min = sample;
        if (sample > max) max = sample;
      }
      const y1 = ((1 - max) * rect.height) / 2;
      const y2 = ((1 - min) * rect.height) / 2;
      ctx.moveTo(x, y1);
      ctx.lineTo(x, y2);
    }
    ctx.stroke();
  }, [buffer, color]);
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
