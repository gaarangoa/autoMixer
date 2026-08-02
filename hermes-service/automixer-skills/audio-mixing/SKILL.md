---
name: audio-mixing
description: "AutoMixer audio: the apply_actions vocabulary + how to balance, EQ, compress, and shape a mix."
version: 1.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [audio, mix, mixing, eq, gain, pan, compress, fader, vocal, master]
    related_skills: [auto-mix]
---

# AutoMixer — Audio Mixing

You change audio with the **`apply_actions`** tool: one call, a list of flat action
objects. Each action has a snake_case `tool` discriminator and camelCase fields.
`trackId`/`regionId` come from `get_session`. Combine many actions in one call. Every
change is reversible with `undo`.

## Measure before touching the mix

Call `get_session` first and inspect every target audio track's `analysis`, current
`chain`, gain, and sends. Do not apply a named preset blindly. Tracks from different
microphones can differ by 10–30 dB and can have very different tonal balance; identical
gain, EQ, or compressor settings are usually wrong in that situation.

- Balance **each source independently** from the measured level and the user's intent.
- Treat whole-file LUFS/RMS cautiously when a track contains long silences. It is still
  useful for spotting extreme mismatches, but never call the result final from that
  number alone.
- After `apply_actions`, inspect its returned chain or call `get_session` again to verify
  that the requested values persisted.
- Never claim a mix is "professional," "finished," or "broadcast ready" merely because
  the tool returned `ok`. Say what was actually changed and what remains to be auditioned.

## Action vocabulary

```
{"tool":"set_track_gain","trackId":"..","gainDb":-3.0}        # [-24..24]
{"tool":"adjust_track_gain","trackId":"..","deltaDb":-2.0}
{"tool":"set_track_pan","trackId":"..","pan":-0.3}            # [-1 L .. 1 R]
{"tool":"mute_track","trackId":"..","muted":true}
{"tool":"solo_track","trackId":"..","solo":true}
{"tool":"set_high_pass","trackId":"..","frequencyHz":90,"slopeDbOct":12}   # slope 12|24
{"tool":"set_low_pass","trackId":"..","frequencyHz":12000,"slopeDbOct":12}
{"tool":"set_eq_band","trackId":"..","band":1,"frequencyHz":300,"gainDb":-2.0,"q":1.1}  # band 0..3
{"tool":"set_compressor","trackId":"..","thresholdDb":-18,"ratio":2.5,"attackMs":12,"releaseMs":180,"kneeDb":6,"makeupDb":2}
{"tool":"set_reverb_send","trackId":"..","levelDb":-12}       # [-60..0]
{"tool":"set_delay_send","trackId":"..","levelDb":-18}
{"tool":"rename_track","trackId":"..","name":"Lead Vocal"}
{"tool":"set_track_role","trackId":"..","role":"lead_vocal"}
{"tool":"set_track_ai_generated","trackId":"..","aiGenerated":true}
{"tool":"delete_track","trackId":".."}
{"tool":"set_master_gain","gainDb":-1.0}
{"tool":"adjust_master_gain","deltaDb":-0.5}
{"tool":"create_region","name":"Chorus 1","startSample":0,"endSample":480000,"trackIds":["..."]}
{"tool":"set_region_gain","regionId":"..","trackId":"..","gainDb":1.5}
{"tool":"apply_section_automation","regionId":"..","trackId":"..","param":"gainDb","value":-1.0}
```

All numeric values are clamped server-side, so a slightly-out-of-range value is
corrected rather than rejected — but stay within the noted ranges.

## CRITICAL: mixing is PROCESSING, not just faders

Changing volumes is **not a mix**. If the user asks for a "professional", "clear",
"punchy", "djent/metal", "radio-ready" sound, or for something to "sit in" / "cut
through" / "stand out" — gain alone will NOT achieve it, and a gain-only answer is wrong.
A real engineer reaches for **high-pass, EQ, compression, and sends**, then sets levels.
So:

- **To make a part CLEAR or sit on top:** don't just turn it up. Carve a frequency
  *pocket* — `set_eq_band` a gentle CUT in the competing parts around the lead's core
  range, and/or a small presence BOOST on the lead (~2.5–5 kHz). Add `set_compressor` to
  control its dynamics so it stays present, not just loud.
- **To make drums/bass PUNCHY:** `set_compressor` on them (snappy attack to keep the
  transient, makeup to restore level), tighten the low end with `set_high_pass` on
  everything that isn't kick/bass.
- **To give DEPTH/space:** `set_reverb_send` / `set_delay_send` — leads drier/forward,
  backgrounds wetter/back.
- **Whole-mix request** ("mix this", "make it sound good/professional", "auto-mix"):
  strongly consider running **`auto_mix`** (see the auto-mix skill) — it applies the full
  chain (cleanup filters, subtractive EQ, compression, tonal shaping, depth/space,
  section automation, master) across all tracks. Then refine specifics (e.g. feature a
  solo) with targeted `apply_actions` moves. Do NOT hand-apply 6 gain changes and call it
  a professional mix.

An explicit instruction such as **"do not use auto mix" always wins**. In that case stay
inside `apply_actions`, make measured per-track moves, and do not call or suggest
`auto_mix` during that turn.

Use the full vocabulary above. After a request like this, your applied actions should
include EQ and/or compression and/or sends — not only `set_track_gain`.

## Podcast / spoken-word doctrine

For a multi-microphone podcast, intelligibility and consistent speaker level come before
effects. It is a dry, centered production unless the user asks for a stylized sound.

### SAM-Audio voice cleanup comes first

For a request to create a high-quality podcast mix from original microphone tracks, run
**`clean_podcast_audio` before EQ, compression, or level matching**. SAM-Audio is the
dialogue-cleanup stage: it isolates the close-miked spoken voice and rejects steady
background noise, room tone, hum, fan noise, and off-mic speech spill.

- AutoMixer posts a visible notice before processing begins, including that audio is sent
  to the configured SAM-Audio endpoint. Do not hide this stage from the user.
- The tool preserves and mutes each original microphone, adds only the isolated
  `<original> · Clean Voice` replacement to the audible mix, and inherits the original
  mic's downstream processing. It deliberately does **not** leave the background residual
  audible, because voice + residual would reconstruct the noisy source.
- Respect selected audio tracks. With no selection, clean all unmuted original podcast
  microphones. Never run the tool again on AI-generated or `Clean Voice` tracks.
- If SAM-Audio is unavailable or separation fails, say exactly that and leave the source
  unchanged. Never claim the noise was removed because a job merely started.
- If the user explicitly refuses remote processing or asks to preserve the original sound
  without source separation, skip SAM-Audio and explain the limitation.
- Source separation can occasionally create speech artifacts. Keep the original muted,
  not deleted, so the result remains auditionable and fully reversible.

After cleanup succeeds, call `get_session` again and mix the new Clean Voice tracks—not
the muted originals—using the measured workflow below.

1. Compare the microphone levels first. Correct large per-speaker mismatches with
   **different track gains** before making tonal decisions. Never move every fader by the
   same amount and describe that as balancing speakers.
2. Keep every dialogue mic centered. Set its role to `lead_vocal` only as metadata; that
   does not make it mixed.
3. Use a conservative high-pass, normally 70–100 Hz at 12 dB/octave. Use 24 dB/octave
   only when measured rumble requires it.
4. Tailor EQ per microphone. Prefer one or two gentle corrective bands (often within
   ±1–3 dB). Do not put the same deep 300 Hz cut and bright presence boost on every voice.
5. Compress each speaker for consistency, with thresholds chosen from that microphone's
   level. A shared threshold is ineffective when the raw microphones are far apart.
6. Set `reverbDb` and `delayDb` to `-60` by default. Do **not** add reverb "for polish" to
   close podcast speech unless the user explicitly requests ambience.
7. Keep the limiter ceiling at or below -1 dBFS and leave headroom. Loudness comes after
   speaker balance and dynamics, not from turning every track and the master up equally.
8. If crosstalk, noise, plosives, sibilance, or inactive-mic spill cannot be solved with
   the available actions, say so plainly. Do not disguise it with harsher EQ or reverb.

If the listener says they hear no difference, verify persistence and bypass state, then
reassess the measurements. Never make processing more aggressive merely so the edit is
obvious; audible difference is not the same thing as improvement.

## Order of operations (for a manual mix)

Make **small, reversible** moves; re-check `get_session` between passes.

1. **Clean up.** High-pass non-bass tracks (vocals ~80–100 Hz, guitars ~90 Hz, overheads
   ~120 Hz). Leave kick and bass full-range.
2. **Subtractive EQ before boosts.** Cut problems (boxiness ~300–500 Hz, harshness
   ~2.5–4 kHz) before adding. Narrow Q for cuts, wider Q for tone. Carve pockets so parts
   don't fight (e.g. dip the rhythm guitars where the solo or vocal lives).
3. **Control dynamics.** `set_compressor` where it helps (vocals, bass, drums, a featured
   lead). Ratios 2:1–4:1, attack 10–30 ms to keep transients, tempo-aware release, a
   little makeup. Snappier attack on drums for punch.
4. **Balance levels.** Set gains so every part sits right — *after* the processing above,
   not instead of it.
5. **Space & depth.** Reverb/delay **sends**, not inserts — leads drier/forward,
   backgrounds wetter/back.
6. **Pan for width.** Keep kick/bass/lead centered; spread doubles, guitars, percussion.
7. **Master last.** Healthy headroom (room before 0 dBFS). Never just turn things up to
   "win" a comparison.

## Roles
Set `set_track_role` so the mix understands intent. Useful roles: `lead_vocal`,
`backing_vocal`, `bass`, `drums`, `kick`, `snare`, `guitar`, `keys`, `pad`, `fx`.

## Scope
If the user has tracks selected, act only on those unless they say otherwise. When in
doubt about which track is which, call `get_session` and reason from names/roles.
