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

Use the full vocabulary above. After a request like this, your applied actions should
include EQ and/or compression and/or sends — not only `set_track_gain`.

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
