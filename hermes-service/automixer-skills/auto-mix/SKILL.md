---
name: auto-mix
description: "AutoMixer's one-shot auto-mix pipeline — when to run it and which of its 10 stages to use."
version: 1.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [automix, auto-mix, mix, pipeline, master, balance]
    related_skills: [audio-mixing]
---

# AutoMixer — Auto-Mix Pipeline

`auto_mix` runs AutoMixer's built-in mixing pipeline end-to-end and reports a summary
(stages run + total actions applied). It's the fast path to a full, balanced mix — use
it when the user wants a whole mix done ("mix this", "make it sound good", "auto-mix"),
not for a single tweak (use `apply_actions` for that — see the audio-mixing skill).

## The 10 stages (run in this order)
1. `raw_session_prep` — normalize/prepare the raw session.
2. `prep_intent` — infer genre/intent and targets.
3. `static_balance` — set the core fader balance.
4. `cleanup_filters` — high-pass / low-pass to clear mud and harsh top.
5. `subtractive_eq` — cut problem frequencies.
6. `dynamics` — compression where needed.
7. `tonal_enhancement` — tasteful tone shaping.
8. `depth_space` — reverb/delay sends for depth and width.
9. `section_automation` — level rides across sections (verse/chorus).
10. `mix_bus_loudness` — master bus level + headroom.

## How to run it
- Whole mix: call `auto_mix` with no stages (runs all 10).
- Targeted: pass a subset, e.g. `["static_balance","cleanup_filters","subtractive_eq"]`
  for a quick rough balance, or `["mix_bus_loudness"]` to just touch the master.
- It's **slow** (each stage calls the mix model) and shows live progress in the UI. After
  it finishes, you can refine with individual `apply_actions` moves.

Everything it does is reversible with `undo`.
