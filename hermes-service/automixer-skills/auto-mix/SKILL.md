---
name: auto-mix
description: "AutoMixer's auto-mix pipeline — run it stage-by-stage so the user sees progress."
version: 2.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [automix, auto-mix, mix, pipeline, master, balance]
    related_skills: [audio-mixing]
---

# AutoMixer — Auto-Mix Pipeline (run it stage-by-stage)

`auto_mix` runs AutoMixer's mixing stages. Each stage calls the mix model and is
**slow on local llama.cpp (~3–8 min)**. Use it when the user wants a whole mix ("mix this", "make it
sound good", "auto-mix"), not for a single tweak (use `apply_actions` for that).

## IMPORTANT: run ONE stage per call, in order — never all at once
Do **not** call `auto_mix` with no arguments or with multiple stages (that runs the
pipeline as one long, opaque, 30+ minute call that looks frozen and can time out).
Instead, call it
**once per stage**, passing a single stage each time, and **tell the user what
that stage changed before moving to the next**. This shows live progress and keeps
each step short and cancellable.

Run the stages in this order, one `auto_mix` call each:

1. `prep_intent` — infer genre/intent and targets.
2. `static_balance` — core fader balance.
3. `cleanup_filters` — high-pass / low-pass to clear mud and harsh top.
4. `subtractive_eq` — cut problem frequencies.
5. `dynamics` — compression where needed.
6. `tonal_enhancement` — tasteful tone shaping.
7. `depth_space` — reverb/delay sends for depth and width.
8. `mix_bus_loudness` — master bus level + headroom.

`raw_session_prep` (a no-op observation) and `section_automation` (only useful when
the song has detected sections) are **optional** — skip them unless needed.

### How each call looks
```
auto_mix(stages=["static_balance"])
```
Then summarize the returned report in one line (e.g. "Balanced levels: vocals +2 dB,
guitars −1 dB, 6 changes"), then proceed to the next stage's call.

## After the run
Briefly summarize the whole mix, then offer to refine specific tracks with
`apply_actions`. Everything is reversible with `undo`.
