---
name: video-editing
description: "AutoMixer video: multicam edits, fades/speed, crop/reframe, color looks, and the clip-layout fields."
version: 1.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [video, edit, multicam, fade, crop, reframe, cinematic, look, grade, speed]
    related_skills: []
---

# AutoMixer — Video Editing

The session's video tracks are **source cameras**; the agent's output lives on the
single **"Agent video edit"** track. Get track + clip ids (and their current crop) from
`get_session` — video tracks list their clips. Respect the user's **selection**
(`get_session.selectedTrackIds`): act only on selected tracks; use `select_tracks` to
scope to specific source cameras before an edit.

## Which tool for which request

| The user wants… | Use | Notes |
|---|---|---|
| A multicam cut / "edit the video" / a color **look** ("cinematic", "vivid", "warm") | `edit_video` | Returns IMMEDIATELY ("started"), analyzes and renders in the background, then adds/updates the `Agent video edit` track. Tell the user rendering started; do NOT claim it is already done or re-run to "check". Use `review_only=true` only when the user explicitly asks to approve the plan first. Looks are applied to the rendered edit — never bake them into source clips. |
| **Fade in/out** or **speed** ("fade in 2s, fade out 10s", "half speed") | `apply_video_effects` | Fast, in place, reversible. fadeIn/fadeOut 0–10s, speed 0.25–4. This is the ONLY way to fade — never fake it with audio gain/automation. |
| A persistent **crop / reframe / rotate** of a SOURCE clip | `set_clip_layout` | Geometry only (see fields below). Modifies the source recording, so use only when the user wants a permanent geometry change. |
| "Auto-crop to the subject" / "reframe to portrait" (no exact numbers) | `auto_crop` | The video model looks at a frame and chooses the crop. |

## set_clip_layout fields (all optional, pass only what changes)
- `crop_top` / `crop_right` / `crop_bottom` / `crop_left`: percent to REMOVE from each edge (0–45).
- `x` / `y` / `width` / `height`: percent of the canvas (reposition / resize).
- `rotation`: degrees. `opacity`: 0–1.
- `brightness` / `contrast` / `saturation` / `blur`: **do NOT use these for a look** — they bake color into the source. For looks, use `edit_video` (renders the grade, leaves sources untouched).

## edit_video instructions — looks & pacing
Put the look + pacing in `instructions`. Recognized look words include: cinematic/epic,
vivid/saturated/punchy, warm/golden/sunset, cool/teal, vintage/retro, moody, noir, mono,
dream. `interval_seconds` sets decision density. Use the 0.5-second default for
speaker-aware conversation edits; longer values are suitable for slower music edits.
Fades/speed can also be requested here, but for fade-only changes prefer
`apply_video_effects` (no re-edit).

For a conversation with separately recorded participant microphones and corresponding
cameras, preserve the A/B/C/D (or participant-name) relationship in the source names.
AutoMixer measures each microphone independently, pairs it with the matching camera, and
uses clear foreground speech as the authoritative cut cue. An unpaired room/overview
camera is reserved for brief pauses, overlap, reactions, or establishing context; it must
not replace a readable active-speaker camera merely because the wide frame looks cleaner.

When the user assigns source roles or limits, preserve them precisely in `instructions`:
name the main camera, the requested minimum coverage, which tracks are inserts/excluded,
maximum insert length/count, and whether visuals must remain original. These are binding
directing constraints, not suggestions. The backend compiles them into a visible directing
contract, validates the plan, and enforces main-camera coverage and insert duration.

By default, a direct request such as "create/edit the video" proceeds through analysis and
final rendering without another approval step. Set `review_only=true` only for an explicit
request to inspect or modify the cut plan before rendering.

## Editing taste
- Cut on the energy: hold a shot while it's working; cut when the moment shifts or the
  music lifts. Do not chase balanced coverage or novelty when the user established a main
  camera. Inserts must earn their place with a specific performance/story reason and return
  promptly to the main angle.
- Match the look to the material: warm for intimate/acoustic, punchy for energetic,
  cinematic (mild teal-orange + contrast + vignette) for "make it look like a film".
- Keep it reversible; everything here can be undone.
