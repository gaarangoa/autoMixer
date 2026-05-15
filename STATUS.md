# AutoMixer — current status

**Last updated:** 2026-05-11

A chat-driven mix-engineer app. Tauri 2 desktop shell, React frontend, Rust audio engine with cpal, local LLM via Ollama, and a Python audio-analysis sidecar.

## Stack

| Layer | Tech |
|---|---|
| Desktop shell | Tauri 2 |
| Frontend | React 19 + TypeScript + Vite |
| Audio engine | Rust + cpal + rubato + symphonia + hound + RBJ biquads + FDN reverb |
| State | JSON-Patch undo/redo per turn; `MixProject` persisted as JSON per session |
| LLM | Local Ollama (model selectable, current usage on gpt-oss / gemma / similar) |
| Music structure | `all-in-one` (Python) via FastAPI sidecar managed by `uv` |
| Build oddities | `[profile.dev] codegen-units = 1` to dodge a macOS rustc anon-symbol linker bug |

## Repo layout

```
/autoMixer/
  src-tauri/         Rust app (engine, assistant, commands, audio service client, auto-mix)
  client/            React + TS frontend (Vite)
  shared/types.ts    TS types mirroring Rust model
  audio-service/     Python sidecar (allin1 wrapper) managed via uv
  STATUS.md          this file
```

## Run

```sh
# from repo root
npm run dev            # Tauri-dev → vite + cargo run + spawns audio sidecar
# detached on macOS:
nohup npm run dev > /tmp/automixer-dev.log 2>&1 &
```

Sidecar deps install once: `cd audio-service && uv sync --python 3.11`.

## What works today

### Audio engine
- Cache import pipeline (decode → resample → analyze → write f32 cache → peaks → register source file → make track)
- Lock-free UI→audio command queue (rtrb), ArcSwap snapshots
- Fractional-sample read path so 44.1↔48 kHz cache vs output mismatch plays at correct pitch
- Per-track gain / pan / mute / solo / HP / LP / 4-band EQ / compressor / reverb send / delay send
- Master gain + true-peak limiter; **master bypass** toggle (A/B compare)
- Render-to-buffer + render-to-WAV for offline operations
- Telemetry: playhead + meters events
- Cargo profiles: `opt-level=1` for our code, `opt-level=3` for deps, `codegen-units=1` to dodge the linker bug

### Session model (`MixProject`)
Persists everything end-to-end and travels with bundles:
- Tracks (id, role, gain, pan, mute, solo, **aiGenerated**, chain, sends, automation, clips)
- Source files with `TrackAnalysis` (LUFS, peak, RMS, centroid, band energy, silence%, dynamic range)
- Regions, markers, master channel
- **`sections: MixSection[]`** with per-section `analysis: SectionAnalysis` (LUFS/peak/centroid/bands/dynamic-range)
- **`mixerProfile`** (preset id + aggressiveness/EQ/comp/space/stereo/loudness/genre/reference engineer)
- **`chatMessages`** — persisted chat history

### Sessions & project bundles
- In-app session picker dropdown (switch / rename / delete / new)
- "Save project as bundle…" writes `<name>.amix/` with `project.json` + `sources/<id>.f32cache` + `peaks/<id>.peaks.json` + `manifest.json`
- "Open project bundle…" copies into the app's data dir with a fresh session id
- Chat history travels with the bundle

### Assistant pipeline (Ollama)
- Skill router → action generator (two-stage) — both via `/api/generate` with **streaming NDJSON**
- Lenient JSON extraction handles ```fences/prose preamble
- UUID aliasing (tk0/tk1, rg0/rg1) to prevent model hallucination on long IDs
- Track aliases swap back to real ids after parse
- Track audio analysis included in every prompt (LUFS, centroid, band energy, etc.)
- **Mixer profile preamble** prepended to action + critique prompts (changes move size, EQ philosophy, comp aggressiveness, send levels, target LUFS by preset)
- **Per-section analysis** flows into the prompt as JSON when sections have been detected
- Restraint rules: smallest move wins, cuts before boosts, compress only when DR > 10 & silence < 30, master > 4 dB needs justification, 3–8 actions/turn cap
- Anti-template rule: 3+ same-tool actions must differ
- AI-stem rule: gentler treatment (broader Q, ratio ≤ 2, lower reverb, low-pass 12–14 kHz)
- Skill auto-expansion from emitted actions (no "skill not available" rejections)
- Two-attempt parse with repair pass
- Clamp-then-validate: out-of-range numbers become warnings, not failures

### LLM actions (vocabulary)
- Per-track: `set_track_gain`, `adjust_track_gain`, `set_track_pan`, `mute_track`, `solo_track`, `set_track_ai_generated`
- EQ / filters: `set_high_pass`, `set_low_pass`, `set_eq_band`
- Dynamics: `set_compressor`
- Sends: `set_reverb_send`, `set_delay_send`
- Sections / regions: `create_region`, `set_region_gain`, `apply_section_automation`
- Master: `set_master_gain`, `adjust_master_gain`
- Meta: `set_processor_param`, `delete_track`, `undo`, `redo`, `render_mix`

### Skills
balance · tonal_eq · dynamics · space_depth · mastering · region_automation · safety_undo · render_export · critique

### Critique mode
- Triggered when user asks for rating/critique/feedback/evaluation
- Renders the session offline to a buffer, computes master analysis (peak, RMS, LUFS, centroid, band energy, dynamic range, true-peak via 4× linear oversampling)
- Returns `MixCritique` with per-track ratings + per-section observations
- Categories: balance / tonality / dynamics / space / headroom / mono_compatibility / **arrangement**
- Per-section critic: flags energy inversions (chorus quieter than verse), near-duplicate sections (`arrangement`)
- AI-stem rule: don't blame stem-separation artifacts on the mix
- Robust to model emitting issues as plain strings (custom deserializer wraps them)

### Mixer profile system
Profiles change every prompt. Presets:
- **Balanced** (default) — moderate, streaming target
- **Scheps minimalist** — tiny moves, almost no compression, dry
- **CLA punch** — bold, surgical EQ, character comp, loud
- **Modern pop** — wide, sidechained, loud
- **Acoustic natural** — preserve dynamics, broad EQ, broadcast
- **Electronic / club** — aggressive sculpting, wide, lush

Per-session. Switch in the LLM settings panel (gear icon).

### Structure detection (`all-in-one`)
- `audio-service/` is a uv-managed FastAPI service exposing `/analyze/structure` and `/status`
- Spawned by Tauri at app startup; lazy-loads `allin1` on first request
- Cached by sha1(file content + mtime) → free re-analysis
- Returns BPM + beats + downbeats + sections (intro/verse/chorus/bridge/outro)
- Per-section measurements computed Rust-side after sidecar returns (slice rendered master, run existing analyzer)
- Music-note button in the transport triggers analysis
- Progress banner in the topbar shows stage + elapsed seconds while running
- **NATTEN does not support MPS** → first analysis is CPU-bound (~5 min on a 4-min song on M1); cached afterwards. CUDA would be 10–30 s.

### Section UI
- Colored ribbon above the track stack (intro=gray, verse=blue, chorus=orange, bridge=purple, outro=dim, solo=green, break=yellow)
- LUFS shown in each section band's corner
- Click to seek · **Shift-click to scope to chat** (yellow chip shows next chat is scoped) · **Alt-click to loop**
- Playhead overlay in the ribbon mirrors the main playhead; loop wraps automatically

### Live streaming + telemetry
- Ollama streams via NDJSON (`stream: true`); each chunk emits `llm:chunk` Tauri event with the phase
- Token counts (`prompt_eval_count`, `eval_count`) captured from the final chunk and emitted as `llm:stats`
- **Reasoning panel** (eye icon in assistant header): collapsible side panel that shows raw streamed model output per phase + per-phase token counts + elapsed time
- **Live chat bubble** that fills with the streamed text while the agent is thinking; replaced by the proper turn card on completion
- `llm:turn-start` / `llm:turn-end` events bookend the turn

### Autonomous mode (auto-mix)
A second mode alongside chat, toggled at the top of the assistant column. Seven stages:
1. **Gain staging** — set per-track gainDb + LCR pan defaults
2. **Cleanup HP/LP** — high-pass non-bass tracks; low-pass harsh top-end
3. **Corrective EQ** — narrow cuts only, one band per problem
4. **Dynamics** — compression only where DR > 10 AND silence < 30
5. **Tonal shaping** — gentle boosts only (presence/air/weight)
6. **Space & glue** — sends, varied per role
7. **Master & balance** — adjust_master_gain to hit profile LUFS target; section-scoped balance via create_region + apply_section_automation if sections exist

Each stage is its own LLM call with its own narrow prompt and its own action cap (200/200/60/50/40/80/20). Per-stage events: `auto-mix:start`, `auto-mix:stage-start`, `auto-mix:stage-done`, `auto-mix:complete`. Every action goes through the normal `apply_actions` so it appears in history and is individually undoable.

**Not yet wired:** pause/resume, back/forward navigation, critic check between stages, interjection chat during pause.

### UI affordances
- Top bar: Play / Stop / **MIX↔ORIG bypass toggle** / Undo / Redo / Export WAV / Import audio / **All AI bulk toggle** / **Music-note (analyze structure)** / **Scale (balance the song)**
- Session menu (click session name): switch / new / rename / save bundle / open bundle / delete
- Per-track head: name + role + **SEL** (manual selection, multi-select; empty = scope to all tracks) + M + S + **AI** + delete + gain slider + pan slider
- Master bar at the bottom of the timeline (gain slider + "0 dB" reset)
- Section ribbon above tracks (when sections detected)
- Assistant column: mode toggle (Chat | Auto-mix) · settings (Ollama URL, model, mixer profile) · reasoning panel (eye icon) · scope chip · chat input

## Known limitations / friction

1. **Local LLM is slow on big sessions.** Skill router alone takes ~80 s on 40 tracks because we pass every track. Action generation is even slower. Cloud model would be a step change.
2. **NATTEN ↔ MPS** — `allin1` can't run on Apple GPU. First analysis is CPU-only (~5 min on a 4-min song); cached afterwards.
3. **Autonomous mode controls are minimal** — start/stop only. Pause/resume + back/forward + critic check planned next.
4. **No live recording** — design proposed (cpal input stream + WAV writer + convert-to-track), not implemented.
5. **macOS rustc linker bug** — recurs after some incremental builds (`_anon.<hash>` symbol mismatches). Fix: `cargo clean -p automixer`. The `codegen-units=1` profile setting reduces frequency.
6. **Webview prompt/confirm** — `window.prompt` is silently disabled in Tauri 2; replaced with inline forms (rename) but `window.confirm` is still used for delete (works in current builds; may need replacement).

## Roadmap (immediate priorities)

| # | Item | Effort | Rationale |
|---|---|---|---|
| 1 | Critic between auto-mix stages (continue/repeat/skip/abort) | 1 day | the whole point of staged mixing is to check between |
| 2 | Pause / back-forward / replay-stage controls for auto-mix | 1 day | hand-off control during long runs |
| 3 | Interjection chat during auto-mix pause | half day | "stop, snare is too quiet" → next stage takes it as a hint |
| 4 | Move demucs scratch dir out of repo (`audio-service/demix/`) | 30 min | currently pollutes the working tree |
| 5 | Live recording from input device (Phase 1) | 1 day | propose new tracks from a mic / interface |
| 6 | Reference-track matching via MERT/CLAP embeddings | 3–4 days | "make it sound like this Daft Punk track" |
| 7 | Audio-LLM critique (Qwen2-Audio) as toggleable backend | 1 week | actual perceptual listening for critique, requires GPU |

## Memory pointers

The agent's prompt for action generation already includes:
- Mixer profile preamble (style guard rails)
- All tracks with their `audio` analysis
- Selected skills' capability snapshot
- Detected sections + per-section metrics + bpm
- Recent critique (if present) as a follow-through hint
- Restraint + anti-template + AI-stem rules
- Per-role compressor presets
- Explicit action schema with examples

If something feels stale or the agent stops being section-aware, re-run **"Detect structure"** — the session may have been analyzed before per-section metrics were added.
