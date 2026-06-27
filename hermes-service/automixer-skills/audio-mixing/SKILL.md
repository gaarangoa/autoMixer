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

## How to approach a mix (doctrine)

Work in this order; make **small, reversible** moves and re-check `get_session` between
passes.

1. **Balance first.** Set rough static gains so every part is audible. Lead vocal and
   the main rhythmic element sit forward; pads/ambience sit back. Don't reach for EQ to
   fix a level problem — fix the fader.
2. **Clean up.** High-pass non-bass tracks (vocals ~80–100 Hz, guitars ~90 Hz, overheads
   ~120 Hz) to clear low-end mud. Leave kick and bass full-range.
3. **Subtractive EQ before boosts.** Cut problem frequencies (boxiness ~300–500 Hz,
   harshness ~2.5–4 kHz) before adding anything. Narrow Q for cuts, wider Q for tone.
4. **Control dynamics.** Compress only what needs it (vocals, bass). Gentle ratios
   (2:1–4:1), moderate attack (10–30 ms) to keep transients, release that breathes with
   the tempo. Add a little makeup to match the pre-compression level.
5. **Space & depth.** Use reverb/delay sends, not inserts, so tracks share a space.
   Lead vocal a touch drier and forward; backgrounds wetter and back.
6. **Pan for width.** Keep kick/bass/lead-vocal centered; spread doubles, guitars, and
   percussion left/right for a wide, balanced image.
7. **Master last.** Set master gain for healthy headroom (leave room before 0 dBFS).
   Never make something louder just to "win" a comparison.

## Roles
Set `set_track_role` so the mix understands intent. Useful roles: `lead_vocal`,
`backing_vocal`, `bass`, `drums`, `kick`, `snare`, `guitar`, `keys`, `pad`, `fx`.

## Scope
If the user has tracks selected, act only on those unless they say otherwise. When in
doubt about which track is which, call `get_session` and reason from names/roles.
