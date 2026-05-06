# AutoMixer PoC Plan (Rust + Tauri)

## Goal

Build `automixer`: a chat-first assisted mixing room where the user acts as producer and the LLM acts as mix engineer. The user loads multiple tracks/stems, selects tracks or time regions, and describes desired changes in natural language. The system translates those requests into structured, validated, undoable mix actions executed by a real Rust audio engine.

The primary value of the product is *the LLM-as-mix-engineer workflow*. The audio engine exists to make that workflow trustworthy: clean parameter changes, transparent processing, no surprise clipping, and a foundation that scales from PoC to a real product without rewrite.

The assistant must be integrated early, not bolted on after the engine is complete. As soon as four stems can play in sync with gain/pan/mute/solo, the PoC should prove that natural-language requests become validated, audible, undoable mix actions.

It is pivotal that the LLM understands what it has in hand, but that does not mean dumping every option into every prompt. The app should expose a compact skill catalog first, then let the assistant select the relevant skill(s) for the user request. Only the selected skills expand into detailed processor/plugin parameters, allowed ranges, units, defaults, automation support, and safe change limits.

## Product Shape

The first screen is the working mix room:

- Multitrack timeline with waveform lanes
- Track headers with minimal controls: name, mute, solo, volume, pan
- Time/region selection
- Chat interface always visible
- History/undo for assistant changes
- Export/render button

Visible UI stays minimal because the primary workflow is natural-language direction:

- "Make the vocal more upfront in the chorus"
- "Tighten the kick and bass relationship"
- "Move the backing vocals wider but only in the hook"
- "Make the drums punchier without making the cymbals harsh"

## Architecture Decision

**Rust audio engine + Tauri shell + React/Vite UI.**

- Rust core owns all audio: decoding, mixing, DSP, scheduling, render.
- Tauri exposes engine commands to the UI via `#[tauri::command]` and pushes events back via `app.emit()`.
- React UI is unchanged in spirit from any web plan — a single-page app talking to a local backend, just over Tauri IPC instead of HTTP.
- LLM (Ollama) is called from Rust on the async runtime, never from the UI.

### Why Rust + Tauri (not browser audio)

- Browser `DynamicsCompressorNode` and biquad chains are too coarse for a product where DSP transparency is the foundation of trust.
- No browser memory ceiling: full multitrack of long songs is trivial when stems live on disk.
- Direct file access — no upload step, no `MAX_UPLOAD_MB`.
- Real sidechain, real lookahead, real convolution available when needed.
- Single signed binary, works offline.
- The audio engine built for the PoC is the engine kept for v2.

### What "best in class" means for the PoC

Not "competes with FabFilter." It means *transparent enough that the LLM's decisions are what you hear, not the engine's artifacts*:

- Click-free parameter smoothing on every parameter
- Honest gain staging and reliable sample-peak metering for PoC
- Textbook DSP (RBJ cookbook biquads, classic feedforward compressor, FDN reverb)
- Sample-accurate scheduling and lock-free parameter updates

Analog modeling, oversampled nonlinearities, character compressors, mastering-grade limiters — all v2.

## Audit Decisions Locked In

Resolutions to the open questions identified in the prior audit, baked into this plan:

| Decision | Resolution |
|---|---|
| Region-scoped processing mechanism | **Automation envelopes only** for PoC. No per-region effect chains. `set_region_gain` / `apply_section_automation` create or modify envelopes on the existing `AutomationLane`s. |
| `MixAction` schema | **Discriminated union** via `serde`'s tagged enum, one variant per tool. No generic `param`/`value` pairs. TypeScript types generated from Rust via `ts-rs`. |
| Undo strategy | **RFC 6902 JSON patches** against the canonical `MixSession`. Every apply produces a forward + inverse patch pair pushed to the history stack. |
| Analysis flow | **Pre-computed and injected** into the LLM context. Removed from the tool list. Re-runs only when processing changes affect output. |
| Source-file record schema | Defined explicitly below (`SourceFile`) with `id`, `original_name`, `cache_path`, `peak_path`, `duration_samples`, `sample_rate`, `channels`. |
| Sample-rate normalization | Resample on import to the session sample rate using `rubato`. Cache the normalized result. Playback never resamples. |
| EQ band layout | **Fixed 4-band** layout: low shelf, two peaks, high shelf, plus separate HP/LP filters. Default frequencies specified. |
| LLM capability awareness | **Skill-based capability registry required.** The assistant first sees compact skill cards, then selected skills expand into relevant processor/plugin parameters, current values, units, valid ranges, safe deltas, automation support, and musical purpose. |
| LLM implementation order | **Assistant loop moves before advanced DSP.** After multi-stem playback works, implement gain/pan/mute/solo tool calling and undo before EQ/compression/reverb automation. |
| `sidechain_duck` | **Deferred** out of PoC. Requires sidechain routing infrastructure beyond the fixed chain. |
| `analyze_track` / `analyze_mix` as LLM tools | **Removed.** Analysis is context, not a tool call. |
| Track-name resolution | Each `Track` has an optional `role` field (e.g. `"lead_vocal"`, `"kick"`) used for LLM aliasing. The LLM context includes both `name` and `role`. |
| LLM error contract | `AssistantResponse` is a discriminated union: `Ok { explanation, actions, warnings }`, `Clarification { question, reason }`, or `Err { kind, message, raw_model_output? }`. JSON-repair fallback before giving up. |
| Streaming | Explanation text may stream as **draft/status** only. Final explanation and actions are shown as committed only after the full response parses and validates. |
| Master chain in PoC | Limiter only. Bus compressor is later. |
| Disk streaming | Audio thread reads RAM only. A dedicated prefetch layer fills bounded per-track ring buffers from cache files. |
| Render output paths | The LLM never supplies arbitrary filesystem paths. Render/export paths come from a user-approved save dialog or app-managed render target. |
| History storage | History is stored outside the patched mix document. Patches apply to session state only, not the history log itself. |
| True-peak claim | PoC limiter guarantees sample-peak protection. Oversampled true-peak metering/limiting is optional or later unless explicitly implemented. |
| IPC boundary types | `MixSession` is the live-state type that crosses IPC for snapshots and incremental updates. `HistoryEntry` is what crosses for history events. `MixProject` is server-side only and never serialized whole to the UI. Documented in `commands.rs` and `ipc.ts`. |

## Configuration

Configuration lives in `~/.config/automixer/settings.json` (or `%APPDATA%` on Windows), seeded from `settings.example.json` shipped with the app:

```json
{
  "data_dir": "~/Library/Application Support/automixer",
  "ollama_base_url": "http://ollama-server:11434",
  "ollama_model": "gpt-oss:20b",
  "audio": {
    "block_size": 512,
    "output_device": null
  }
}
```

No `PORT`, no `MAX_UPLOAD_MB` — there is no HTTP server and no upload step. Files are referenced directly from disk and copied into `data_dir/sources/` on import.

## Core Domain Model

Defined in Rust with `serde` for JSON persistence and `ts-rs` to export TypeScript bindings to the UI. The mix document is the single source of truth for audible state; the audio engine derives runtime state from it. History is persisted beside the mix document, not inside the document being patched.

```rust
#[derive(Serialize, Deserialize, TS)]
pub struct MixSession {
    pub id: String,
    pub name: String,
    pub sample_rate: u32,
    pub bpm: Option<f32>,
    pub source_files: Vec<SourceFile>,
    pub tracks: Vec<Track>,
    pub buses: Vec<Bus>,
    pub master: MasterChannel,
    pub regions: Vec<Region>,
    pub markers: Vec<Marker>,
}

#[derive(Serialize, Deserialize, TS)]
pub struct MixProject {
    pub session: MixSession,
    pub history: Vec<HistoryEntry>,
    pub redo_stack: Vec<HistoryEntry>,
}

#[derive(Serialize, Deserialize, TS)]
pub struct SourceFile {
    pub id: String,
    pub original_name: String,
    pub cache_path: PathBuf,    // interleaved f32, normalized to session rate
    pub peak_path: PathBuf,     // multi-resolution peak file
    pub duration_samples: u64,
    pub sample_rate: u32,       // == session.sample_rate after import
    pub channels: u16,
    pub analysis: TrackAnalysis,
}

#[derive(Serialize, Deserialize, TS)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub role: Option<String>,   // e.g. "lead_vocal", "kick" — for LLM aliasing
    pub source_file_id: String,
    pub start_sample: u64,
    pub gain_db: f32,
    pub pan: f32,               // -1.0..1.0
    pub muted: bool,
    pub solo: bool,
    pub color: String,
    pub chain: TrackChain,
    pub sends: Sends,
    pub automation: Vec<AutomationLane>,
    pub clips: Vec<ClipRegion>,
}

#[derive(Serialize, Deserialize, TS)]
pub struct TrackChain {
    pub high_pass: HighPassState,
    pub low_pass: LowPassState,
    pub eq: EqState,             // 4 fixed bands
    pub compressor: CompressorState,
}

#[derive(Serialize, Deserialize, TS)]
pub struct AutomationLane {
    pub param: AutomatableParam,
    pub region_id: Option<String>,
    pub points: Vec<AutomationPoint>,
    pub curve: CurveType,
}

#[derive(Serialize, Deserialize, TS)]
pub struct Region {
    pub id: String,
    pub name: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub track_ids: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, TS)]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: i64,
    pub source: HistorySource,           // User | Assistant { request_id }
    pub forward_patch: serde_json::Value, // RFC 6902
    pub inverse_patch: serde_json::Value,
    pub explanation: Option<String>,
}
```

The LLM never edits arbitrary state. It proposes typed actions; the engine validates, converts to JSON patches against `MixSession`, applies them, and stores both patches in `MixProject.history`.

Use stable IDs in patch paths wherever possible. If arrays remain in the persisted JSON, patch generation must resolve by ID first and reject actions when a target ID is missing. Reordering tracks/regions should be represented as explicit actions, not accidental array-index patch churn.

### IPC boundary

`MixProject` is server-side only — Rust owns it, persists it, and patches it. It is never serialized whole to the UI. What crosses the Tauri IPC boundary:

| Direction | Payload | When |
|---|---|---|
| Rust → UI | `MixSession` (full snapshot) | On project load, on explicit refresh |
| Rust → UI | `SessionPatch { forward_patch, source }` | After every applied action (user or assistant) |
| Rust → UI | `HistoryEntry` | When history grows (append) or when undo/redo shifts the cursor |
| Rust → UI | `EngineEvent` (meters, playhead, underrun) | Continuous, throttled to ≥30Hz for meters |
| UI → Rust | `AssistantRequest`, `MixAction`, transport commands, import/export intent | User actions |

The React store mirrors `MixSession` and applies incoming `SessionPatch`es locally; it never reconstructs `MixProject`. History is a separate slice fed by `HistoryEntry` events. This split is the contract — `commands.rs` documents which types cross which way, and `ipc.ts` re-exports only the wire-facing types from the `ts-rs`-generated bindings.

## Audio Engine Architecture

Three threads, strictly separated:

1. **UI/Tauri command surface** — receives `invoke()` calls from React and forwards work to async/session tasks. It must not block on Ollama, disk persistence, decoding, analysis, or render.
2. **Audio thread** — cpal output callback. Real-time: no allocations, no locks, no syscalls, no logging. Reads commands from a lock-free queue at the top of each block.
3. **Session store task** — single owner of the canonical `MixProject`. Applies validated actions, produces patches, persists snapshots, and emits session-change events.
4. **Async runtime (tokio)** — Ollama HTTP, file decoding, peak generation, prefetch coordination, analysis, offline render workers.

### Communication

- **UI → audio:** `rtrb::Producer<EngineCommand>`. Commands are small `Copy` structs (param IDs + target values). Allocate command pools up front.
- **Audio → UI:** `rtrb::Producer<EngineEvent>` for meters, playhead, errors. Atomics for hot-path values like the playhead position.
- **UI → session store / tokio:** standard `mpsc` channels.
- **Session store / tokio → UI:** Tauri `app.emit()` events.

### Parameter smoothing

Every audio parameter wraps a `SmoothedParam<f32>` — one-pole low-pass with ~10ms time constant. Set the target from the audio thread when a command arrives; the smoother interpolates per-sample (or per-block, with per-sample interpolation only inside DSP units that need it). This is the single most important property of the engine: every gain, pan, EQ frequency, threshold change is click-free by construction.

### Block processing

The engine processes internally in fixed blocks (configurable, default 512 samples), but the cpal callback adapter must accept host-chosen callback sizes and feed/consume the internal block processor correctly. Each internal block:

1. Drain the command queue, update target values on smoothed params.
2. Evaluate automation envelopes for the block window.
3. Mix tracks: read already-prefetched audio from per-track RAM ring buffers → apply chain (HP, LP, EQ, comp, pan, gain) → sum to buses → master limiter → output.
4. Update meter atomics; push events for waveform/playhead.

### Source file pipeline

On import:

1. Decode with `symphonia` (handles WAV, FLAC, MP3, AAC, OGG).
2. Resample to `session.sample_rate` with `rubato` (sinc-based, high quality).
3. Write interleaved f32 to `data_dir/sources/{id}.f32cache`.
4. Generate multi-resolution peak file at zoom levels {256, 1024, 4096, 16384} samples per peak, written to `{id}.peaks`.
5. Run analysis pass (peak, RMS, LUFS estimate, spectral centroid, band energies, silence ratio, dynamic range).
6. Persist `SourceFile` record into the session.

Playback uses a dedicated prefetch layer:

- A non-real-time prefetch thread reads cache files and fills bounded per-track ring buffers.
- The audio thread reads only from RAM ring buffers. It never performs file I/O, page-fault-prone memory-map access, allocation, logging, or locking.
- Seek/loop commands invalidate and refill buffers before playback resumes, or crossfade into the refilled data when possible.
- If a buffer underruns, the audio thread outputs silence for that track, raises an `EngineEvent::Underrun`, and never blocks.
- Offline render may read cache files directly because it is not running on the cpal callback.

## Signal Chain

Per-track:

```text
Source → HP Filter → LP Filter → 4-band EQ → Compressor → Pan → Track Gain → Sends → Mix Bus
```

Master:

```text
Mix Bus → Limiter → Output
```

Aux sends (global, one of each):

```text
Reverb Send → FDN Reverb
Delay Send  → Feedback Delay
```

### EQ band layout (fixed)

| Band | Type | Default Freq | Default Q | Default Gain |
|------|------|--------------|-----------|--------------|
| 0 | Low shelf | 100 Hz | 0.7 | 0 dB |
| 1 | Peak | 400 Hz | 1.0 | 0 dB |
| 2 | Peak | 2500 Hz | 1.0 | 0 dB |
| 3 | High shelf | 8000 Hz | 0.7 | 0 dB |

HP and LP are separate biquads, configurable frequency and order (12 or 24 dB/oct via cascaded biquads).

### DSP implementations (PoC)

- **Biquad** (HP/LP/shelf/peak): RBJ cookbook coefficients.
- **Compressor**: feedforward, log-domain detector, separate attack/release, configurable knee width, makeup gain.
- **Reverb**: 8-line FDN with diffusion network and damping. One global instance, fed by sends.
- **Delay**: stereo feedback delay with fractional sample interpolation, low-pass in the feedback path. One global instance.
- **Limiter**: lookahead sample-peak limiter (5ms lookahead, soft-knee), prevents sample-peak overs. Oversampled true-peak detection/limiting is later unless implemented explicitly.

All DSP units use block processing and accept smoothed parameters.

## Base Mixing Tools

Tools are typed `MixAction` variants. Validated by `serde` deserialization + a `validate()` method that range-checks numeric params before patch generation.

### Session

`list_tracks`, `rename_track`, `set_track_color`, `create_marker`, `create_region`, `select_region`, `delete_region`, `undo`, `redo`

### Track Balance

`set_track_gain`, `adjust_track_gain`, `set_track_pan`, `mute_track`, `solo_track`, `set_clip_gain`, `set_region_gain`, `fade_region_in`, `fade_region_out`

### EQ and Filter

`set_high_pass`, `set_low_pass`, `set_eq_band`, `adjust_eq_band`, `reset_track_eq`, `apply_tonal_preset`

### Processor Parameters

`set_processor_param`

This generic action is available only for parameters in the capability registry. It lets the assistant use any exposed built-in processor or future plugin option without guessing hidden state.

### Dynamics

`set_compressor`, `adjust_compressor`, `set_gate`, `set_limiter`, `set_transient_shape`
*(`sidechain_duck` deferred — requires sidechain routing not in PoC chain.)*

### Space and Width

`set_reverb_send`, `set_delay_send`, `set_track_width`, `set_region_reverb_send`, `set_region_delay_send`

### Bus

`create_bus`, `route_track_to_bus`, `set_bus_gain`, `set_bus_compressor`, `set_bus_saturation`

### Automation

`add_automation_point`, `set_automation_curve`, `clear_automation`, `copy_automation`, `apply_section_automation`

### Render

`render_preview`, `render_mix`, `bounce_track`, `export_session`, `import_session`

*(`analyze_track` / `analyze_mix` removed: analysis is context, not a tool call.)*

## PoC Tool Subset

```text
create_region
set_track_gain
adjust_track_gain
set_track_pan
mute_track
solo_track
set_high_pass
set_low_pass
set_eq_band
set_compressor
set_reverb_send
set_delay_send
set_processor_param
set_region_gain
apply_section_automation
undo
render_mix
```

This is enough to make the assistant feel useful without overbuilding the plugin layer. `list_tracks` is not a mutating action; the current track list is provided in the capability snapshot and prompt context.

## Action Schema

Discriminated union, one variant per tool. Example excerpt:

```rust
#[derive(Serialize, Deserialize, TS)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum MixAction {
    SetTrackGain { track_id: String, gain_db: f32 },
    AdjustTrackGain { track_id: String, delta_db: f32 },
    SetTrackPan { track_id: String, pan: f32 },
    MuteTrack { track_id: String, muted: bool },
    SoloTrack { track_id: String, solo: bool },
    SetHighPass { track_id: String, frequency_hz: f32, slope_db_oct: u8 },
    SetLowPass { track_id: String, frequency_hz: f32, slope_db_oct: u8 },
    SetEqBand {
        track_id: String,
        band: u8,             // 0..=3
        frequency_hz: f32,
        gain_db: f32,
        q: f32,
    },
    SetCompressor {
        track_id: String,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        knee_db: f32,
        makeup_db: f32,
    },
    SetReverbSend { track_id: String, level_db: f32 },
    SetDelaySend { track_id: String, level_db: f32 },
    SetProcessorParam {
        target_id: String,
        processor_id: String,
        param_id: String,
        value: f32,
    },
    SetRegionGain { region_id: String, track_id: String, gain_db: f32 },
    ApplySectionAutomation {
        region_id: String,
        track_id: String,
        param: AutomatableParam,
        value: f32,
    },
    CreateRegion { name: String, start_sample: u64, end_sample: u64, track_ids: Option<Vec<String>> },
    Undo,
    RenderMix,
}
```

Each variant has a `validate()` returning `Result<(), ValidationError>` enforcing parameter ranges (e.g. `pan ∈ [-1, 1]`, `frequency_hz ∈ [20, 20000]`, `gain_db ∈ [-24, 24]`). Out-of-range LLM outputs are rejected, not clamped silently. `SetProcessorParam` is validated against the capability registry and is only allowed for parameters explicitly exposed to the assistant. `RenderMix` uses the current user-approved render destination or an app-managed default; the LLM never provides filesystem paths.

## Assistant Skill And Capability Registry

The assistant must not infer hidden program capabilities from prose, but the prompt should stay compact. Before each LLM call, the app builds a `SkillCatalog` plus a small session summary. The model first chooses which skill(s) are relevant. The app then expands only those skills into a detailed `CapabilitySnapshot` used by the final action-generation prompt and validators.

The registry covers built-in processors during the PoC and future plugin-hosted processors later. "Plugin" here means any processor module exposed to the mix graph, whether built in, bundled, or eventually loaded from an external plugin system.

### Skill catalog

Each skill is a small decision unit with trigger guidance, required context, actions it can use, and the processors/parameters it may expand. The skill catalog is always small enough to include in the first prompt.

PoC skills:

- `balance`: gain, pan, mute, solo, rough foreground/background moves
- `tonal_eq`: HP/LP, 4-band EQ, brightness, mud, harshness, body
- `dynamics`: compressor, punch, control, sustain, transient consistency
- `space_depth`: reverb send, delay send, front/back placement, ambience
- `region_automation`: region gain and section-scoped parameter automation
- `analysis_reader`: interpret raw/current analysis numbers and masking hints
- `render_export`: render/export intent, clipping warnings, render readiness
- `safety_undo`: small reversible moves, undo/redo, rejection of unsafe requests

Later skills:

- `sidechain`: kick/bass ducking and sidechain routing
- `width_imaging`: stereo width, mono compatibility, spatial widening
- `de_essing`: vocal sibilance control
- `transient_shaping`: drum attack/sustain shaping
- `reference_match`: compare against a reference track

```rust
#[derive(Serialize, Deserialize, TS)]
pub struct SkillCatalog {
    pub skills: Vec<SkillCard>,
}

#[derive(Serialize, Deserialize, TS)]
pub struct SkillCard {
    pub skill_id: String,
    pub display_name: String,
    pub when_to_use: String,
    pub musical_intents: Vec<String>,  // e.g. "more upfront", "less muddy"
    pub summary_actions: Vec<String>,  // compact names only
    pub required_context: Vec<String>, // e.g. "selected tracks", "region"
}

#[derive(Serialize, Deserialize, TS)]
pub struct SkillSelection {
    pub selected_skill_ids: Vec<String>,
    pub reason: String,
    pub needs_clarification: bool,
    pub clarification_question: Option<String>,
}
```

Skill selection rules:

- First pass receives only the user request, selected tracks/regions, compact track summary, and `SkillCatalog`.
- The model selects one to three skills. Most requests should use one skill; complex requests like "make drums punchier without harsh cymbals" may select `dynamics` + `tonal_eq`.
- If the request cannot be mapped safely to tracks, regions, or skills, the model asks one clarifying question instead of choosing actions.
- The second pass receives detailed capability data only for selected skills.
- Validators remain authoritative. A selected skill grants no permission beyond registered actions and parameters.

```rust
#[derive(Serialize, Deserialize, TS)]
pub struct CapabilitySnapshot {
    pub selected_skills: Vec<String>,
    pub tracks: Vec<TrackCapability>,
    pub processors: Vec<ProcessorDescriptor>,
    pub actions: Vec<ActionDescriptor>,
    pub current_selection: SelectionContext,
}

#[derive(Serialize, Deserialize, TS)]
pub struct TrackCapability {
    pub track_id: String,
    pub name: String,
    pub role: Option<String>,
    pub processors: Vec<ProcessorInstanceState>,
    pub sends: Sends,
    pub automatable_params: Vec<ParamRef>,
}

#[derive(Serialize, Deserialize, TS)]
pub struct ProcessorDescriptor {
    pub processor_id: String,       // e.g. "eq_4band", "compressor", "fdn_reverb"
    pub display_name: String,
    pub purpose: String,            // short musical explanation for the LLM
    pub params: Vec<ParamDescriptor>,
}

#[derive(Serialize, Deserialize, TS)]
pub struct ParamDescriptor {
    pub param_id: String,           // stable machine name
    pub label: String,
    pub unit: ParamUnit,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub current: Option<f32>,
    pub safe_step: f32,             // recommended subtle LLM move
    pub automatable: bool,
    pub semantic_tags: Vec<String>, // e.g. "brightness", "mud", "punch", "width"
}

#[derive(Serialize, Deserialize, TS)]
pub struct ParamRef {
    pub target_id: String,          // track, bus, send, master, or processor instance
    pub processor_id: String,
    pub param_id: String,
}
```

Rules:

- The LLM receives a compact skill catalog in the first pass and only selected detailed capabilities in the second pass.
- Every processor/plugin option the user could reasonably ask the assistant to use must be reachable through a skill and, when expanded, appear with a range, unit, current value, and musical purpose.
- The LLM must reference stable IDs from the snapshot. Human names and roles help selection, but actions use IDs.
- Validators reject unknown processors, unknown params, non-automatable automation targets, out-of-range values, and unsafe render/export targets.
- Expanded skill context should include a compact "what this control does" description for each relevant processor so the assistant knows when to use EQ, compression, sends, pan, gain, limiter, or later plugin options.
- For PoC built-ins, named discriminated `MixAction` variants remain the preferred action API because they are easier to validate and explain. `SetProcessorParam { target_id, processor_id, param_id, value }` is the general escape hatch for any exposed processor/plugin parameter and must validate against this registry before patch generation.

## Assistant Contract

```rust
pub struct AssistantRequest {
    pub session_id: String,
    pub user_text: String,
    pub selected_track_ids: Vec<String>,
    pub selected_region_ids: Vec<String>,
    pub selected_time_range: Option<TimeRange>,
}

#[derive(Serialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AssistantResponse {
    Ok {
        explanation: String,
        actions: Vec<MixAction>,
        warnings: Vec<String>,
    },
    Clarification {
        question: String,
        reason: String,
    },
    Err {
        kind: AssistantErrorKind,   // ModelUnreachable | InvalidJson | InvalidActions | NoActions | Cancelled
        message: String,
        raw_model_output: Option<String>,
    },
}
```

Draft/status text may stream via Tauri events as the model produces tokens. The UI should label streamed text as pending. Final explanation and actions are committed only after the full response parses and every action validates. A failed validation rejects the entire response — no partial application.

## LLM Pipeline

1. Build compact session summary: selected tracks/regions, track names/roles, important current values, and high-level analysis.
2. Build `SkillCatalog` from the live engine/plugin registry.
3. Skill-selection pass: ask the model to choose one to three skills or ask one clarifying question.
4. If clarification is needed, return `AssistantResponse::Err` or a dedicated clarification response before taking any action.
5. Build `CapabilitySnapshot` only for selected skills, including relevant actions, processor/plugin params, ranges, units, current values, safe steps, automation support, and musical purpose.
6. Action-generation pass: call Ollama with structured-output / JSON mode using the selected skill context and tool schema.
7. Stream pending status/draft explanation tokens to UI as they arrive.
8. On completion, attempt to parse the full response.
9. On parse failure, run a single repair pass (re-prompt with the model's bad output and the selected schemas, ask for valid JSON only).
10. On second failure, return `AssistantResponse::Err`.
11. On parse success, validate every action against the schema, current session, selected skills, and capability snapshot.
12. If all valid: generate forward + inverse patches, apply, push to history, push commands to audio thread, persist session.
13. If any invalid: return `Err { kind: InvalidActions, ... }` with the offending action(s), and discard any pending draft explanation.

### Prompt strategy

System prompt includes:

- Track list with `id`, `name`, `role` (so the model can refer to "the lead vocal" by role)
- Currently selected tracks/regions/time range
- Per-track analysis summary (peak, LUFS, spectral centroid, band energies)
- Mix-level analysis (peak, LUFS, balance, masking hints)
- Compact skill catalog for the first pass
- Selected skill capability snapshot for the second pass: relevant processor/plugin options, current parameter values, ranges, units, safe steps, automation support, and musical purpose
- Available tools for selected skills as JSON schema (generated from the Rust enum/registry)
- Mixing rules and safe parameter ranges
- Instruction to make small, reversible moves
- Instruction to ask a clarifying question only when the request is impossible or ambiguous enough to risk damage

The model is told to prefer subtle moves first. It can explain intent, but actual changes must be tool calls.

## Audio Analysis (Pre-Computed Context)

Computed once at import per track, cached in `SourceFile.analysis`. Re-run only when destructive processing or trim changes the source.

Per track:

- Peak level (dBFS)
- RMS level (dBFS)
- LUFS estimate (BS.1770 short-term)
- Spectral centroid (Hz)
- Band energy (low <250Hz / mid 250–4000Hz / high >4000Hz)
- Silence percentage
- Dynamic range estimate (PSR / crest factor)

Current session/mix context (computed on-demand before each LLM call from the active session state, including gain, pan, mute/solo, regions, automation, filters, and enabled processors where practical):

- Peak level
- LUFS estimate
- Stereo balance
- Frequency balance summary
- Per-track masking hints (band-overlap heuristic between pairs of tracks)

These do not need to be perfect. The goal is to give the assistant enough context to make better first-pass decisions. Source analysis describes the raw imported audio; current session analysis describes what the user is actually hearing now.

## Crate List

```toml
# audio
cpal       = "0.15"
symphonia  = { version = "0.5", features = ["all"] }
rubato     = "0.15"
hound      = "3"
rtrb       = "0.3"
crossbeam  = "0.8"

# domain & ipc
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
ts-rs      = "10"
json-patch = "3"
uuid       = { version = "1", features = ["v4", "serde"] }

# llm
reqwest    = { version = "0.12", features = ["json", "stream"] }
tokio      = { version = "1", features = ["full"] }

# infra
tauri      = "2"
tracing    = "0.1"
thiserror  = "1"
anyhow     = "1"
```

## File Structure

```text
automixer/
  Cargo.toml                  # workspace
  settings.example.json
  src-tauri/
    Cargo.toml
    tauri.conf.json
    src/
      main.rs
      commands.rs             # #[tauri::command] surface
      session_store.rs        # canonical session, persistence
      assistant/
        mod.rs
        ollama.rs
        prompt.rs
        repair.rs
        capabilities.rs       # builds prompt/runtime capability snapshot
      patch.rs                # JSON patch generation
      history.rs
  crates/
    engine/                   # the audio engine (no tauri deps)
      Cargo.toml
      src/
        lib.rs
        types.rs              # domain types, serde, ts-rs
        actions.rs            # MixAction enum + validate()
        capabilities.rs       # processor/parameter registry descriptors
        smoothing.rs
        graph.rs              # mixer graph, scheduling
        thread.rs             # audio thread, cpal callback
        commands.rs           # EngineCommand / EngineEvent
        dsp/
          biquad.rs
          eq.rs
          filter.rs
          compressor.rs
          limiter.rs
          reverb.rs
          delay.rs
          pan.rs
        source/
          decode.rs
          resample.rs
          cache.rs
          peaks.rs
        analysis/
          track.rs
          mix.rs
        render.rs             # offline render
  client/
    index.html
    package.json
    src/
      App.tsx
      ipc.ts                  # invoke wrappers, generated TS types
      audio/                  # waveform render, peak loading
      components/
        MixRoom.tsx
        Timeline.tsx
        TrackLane.tsx
        TrackHeader.tsx
        AssistantPanel.tsx
        Transport.tsx
      state/
        sessionStore.ts
        selectionStore.ts
  data/                       # local dev only; real data lives in app data dir
```

## Pre-PoC Spike (Week 0)

Before committing to the full plan, prove the foundation in one week:

1. cpal output of a sine wave with smoothed gain (day 1)
2. Decode 4 WAV stems with symphonia, resample with rubato to a common sample rate (day 1–2)
3. Mix all 4 in sync from disk-backed buffers, with per-track gain/pan, controlled from a CLI (day 2–3)
4. Wrap in Tauri, expose `set_gain` / `set_pan` from React buttons (day 3–4)
5. Add a biquad and a simple compressor, click-free param changes from React (day 4–5)

**Exit criteria:** four stems play in sync, parameter changes from the UI take effect with no audible clicks, no glitches under a 256-sample block size.

If the spike completes in a week → proceed with the milestones below. If it stretches past two weeks → reassess (likely cause: real-time audio threading is unfamiliar; either invest in learning or fall back to the web stack as v0).

## Build Milestones

### M1 — Workspace skeleton

- Cargo workspace with `src-tauri/` and `crates/engine/`
- React/Vite client wired into Tauri
- `settings.example.json` and load/merge logic
- IPC scaffolding: `ping` command, `app.emit` event round-trip
- `ts-rs` export pipeline producing `client/src/ipc/types.ts`

Done when the app opens, React calls `ping`, gets `pong`, and a generated TS type round-trips.

### M2 — Audio output and parameter smoothing

- cpal device selection and output callback
- Audio thread with `rtrb` command queue
- `SmoothedParam<f32>` with one-pole interpolation
- A test sine generator with smoothed frequency + gain controlled from React

Done when changing gain via a React slider produces zero clicks under repeated rapid changes.

### M3 — Source file pipeline

- Symphonia decode of WAV/FLAC/MP3
- Rubato resample to session rate
- Cache file write (`{id}.f32cache`) and peak file write (`{id}.peaks`)
- `SourceFile` record persisted in session
- Drag-drop import in React

Done when importing 4 stems produces normalized cache files and peak files visible in `data_dir/sources/`.

### M4 — Multi-stem playback

- Track lanes play from prefetch-filled per-track ring buffers
- Audio thread mixes N tracks: gain, pan, mute, solo, sum to mix bus, master gain to output
- Transport: play, pause, seek, loop
- Playhead atomic + UI animation

Done when 4 stems play in sync with click-free transport and channel controls.

### M5 — Early assistant loop for balance tools

- Ollama HTTP client on tokio runtime (no blocking the UI thread, never the audio thread)
- Generate compact `SkillCatalog` from available assistant skills
- Implement skill-selection pass before action generation
- Expand `CapabilitySnapshot` only for selected skills, starting with balance controls
- Tool JSON schema generated from `MixAction` enum
- Implement assistant actions for gain, pan, mute, solo, and `set_processor_param` where exposed
- Parse, validate, repair-on-fail, apply atomically
- Push resulting commands to the audio thread
- Persist session changes

Done when "make the vocal louder", "pan the guitars wider", or "mute the bass" selects the `balance` skill, expands only balance capabilities, and updates the audible mix through validated actions. Invalid model output produces a structured UI error rather than a silent failure.

### M6 — History and undo for assistant actions

- Every apply produces forward + inverse RFC 6902 patches against `MixSession`
- History stack stored in `MixProject`, outside the patched session document
- `undo` / `redo` apply inverse / forward patches and push reciprocal commands to the audio thread
- History UI shows source (User vs Assistant) and explanation

Done when every assistant balance action is undoable, history survives session reload, and undo is click-free.

### M7 — Waveform UI and regions

- Read peak files at appropriate zoom level, render canvas waveforms
- Click/drag time-range selection
- Region creation, naming, persistence
- Region selection feeds into assistant context and capability snapshot

Done when the user can select "verse", "chorus", or an arbitrary range and see it reflected in the chat panel.

### M8 — Per-track DSP chain

- HP / LP filters (cascaded biquads)
- 4-band EQ (RBJ cookbook biquads)
- Feedforward compressor (log detector, attack/release/knee/makeup)
- Pan + track gain stages
- Reverb send → global FDN reverb
- Delay send → global feedback delay
- Master limiter (5ms lookahead)
- Every parameter in session JSON, applied via `EngineCommand`, smoothed in audio thread

Done when structured state changes audibly affect playback, all click-free, no glitches under rapid changes, and each DSP feature is reachable through an assistant skill that can expand relevant processor parameters with current value, unit, range, safe step, and musical purpose.

### M9 — Region automation

- `AutomationLane` evaluation per block in the audio thread
- `set_region_gain` and `apply_section_automation` create/modify lanes
- Visual envelope display in the timeline (read-only for PoC; the LLM writes them)
- Curve types: linear, exponential, hold

Done when the assistant can change "the chorus" without affecting verses, and the change is visible as an envelope.

### M10 — Analysis pre-pass

- On import: peak, RMS, LUFS, spectral centroid, band energies, silence %, dynamic range
- Cached in `SourceFile.analysis`
- Mix-level analysis computed on-demand before each LLM call
- Both injected into the assistant system prompt

Done when the assistant prompt includes per-track and mix analysis as structured data.

### M11 — Offline render

- Engine runs offline (no cpal, faster than realtime, larger blocks)
- Bounce mix to WAV via `hound`
- Sample-peak check at output, warning if over 0 dBFS. If oversampled true-peak metering has been implemented, also report dBTP.
- Progress reporting to UI

Done when the user exports a deterministic mixed WAV from the same engine graph and scheduled parameter events. Do not require bit-matching a live device callback; real-time callback sizes, device conversion, and command timing can differ.

## PoC Acceptance Criteria

The PoC is successful when:

- User can import at least 4 stems
- Stems play in sync, click-free
- User can select a time range or region
- User can ask for a natural-language mix change
- Assistant returns structured tool actions, validated against typed schemas
- Actions update the audible mix, click-free
- Invalid LLM output produces a structured error in the UI
- Region-scoped changes (via automation) work
- User can undo any assistant change
- User can export a WAV

## Testing Strategy

- **Unit tests** in the `engine` crate: biquad coefficients vs known-good fixtures, compressor gain reduction curves vs reference, JSON-patch round-trips, `MixAction::validate()` boundaries.
- **Smoke tests** per milestone, documented as a checklist (e.g. M2: "drag slider rapidly for 30s, no clicks audible; meters update at ≥30Hz").
- **Render-determinism test**: same session + same source files → bit-identical offline render. Catches non-determinism in DSP early.
- **Schema round-trip test**: every `MixAction` variant serializes → deserializes → equals the original; the generated TS types compile against hand-written usage in the client.
- **Skill/capability registry tests**: every exposed processor/plugin parameter is reachable through at least one skill and has a stable ID, current value, unit, min/max, default, safe step, automatable flag, and musical-purpose text; `SetProcessorParam` rejects anything missing from the expanded selected-skill registry.

## Risks and Constraints

- **Real-time audio threading discipline.** The audio thread cannot allocate, lock, or syscall. A single `Vec::push` in the hot path can cause underruns. Mitigation: thread-local pools, `#[deny(clippy::disallowed_macros)]` for `println!`/`format!` in audio code, audit pass before merging.
- **DSP perfectionism delaying the LLM loop.** The product's value is the chat workflow. Ship the M5 assistant balance loop before advanced DSP, then expand the same capability registry as processors arrive.
- **Local LLM tool-calling reliability.** `gpt-oss:20b` and other Ollama models vary in JSON-schema adherence. The repair pass is the floor; if it's still unreliable, switch model or add few-shot examples.
- **LLM hallucinated track IDs / param values.** Mitigated by schema validation + range validation + the `role` field for aliasing. Invalid actions reject the whole response.
- **LLM underuses available processors or plugin options.** Mitigated by the skill/capability registry: the first pass chooses relevant skills from musical intent, and the second pass receives only those expanded parameters with purpose, semantic tags, safe step size, and current value.
- **Region automation visual feedback.** Read-only envelope display in PoC. If users want to edit envelopes by hand that's post-PoC.
- **Cross-platform audio quirks.** cpal abstracts CoreAudio/WASAPI/ALSA but device enumeration and exclusive mode differ. Default to shared mode, sane block sizes.
- **No collaborative or multi-instance editing.** Single-user, single-process.

## Non-Goals For PoC

- VST/AU plugin hosting
- Sample-accurate waveform editing (cut/paste/slip)
- Real-time collaborative editing
- Full DAW replacement
- MIDI sequencing
- Beat grid warping
- Commercial-grade loudness metering (BS.1770 integrated, true-peak ITU-R)
- Sidechain routing
- User-editable automation envelopes (LLM writes, UI displays)
- Per-region effect chains (only region-scoped automation)

## Later Expansion

- Sidechain routing infrastructure → `sidechain_duck` and parallel compression
- AudioWorklet-equivalent: scriptable user DSP (probably via WASM plugins in the engine)
- Stem classification (which track is drums, vox, bass)
- Automatic section detection
- Vocal-specific tools: de-esser, presence, breath control
- Drum tools: transient shaping, parallel compression
- Bass/kick conflict resolver
- Reference-track matching
- A/B assistant suggestions before applying
- Preset mix engineer personalities
- Versioned mix snapshots (branchable history)
- Character processors: tape saturation, console emulation, mastering limiter
- Multi-band compression
- Convolution reverb with real IRs
- Distributed render workers
