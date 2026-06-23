"""automixer-mcp — a thin stdio MCP server that bridges the Hermes agent to
AutoMixer's in-process HTTP control surface.

Hermes discovers MCP servers as stdio child processes; this shim translates
each MCP tool call into an authenticated HTTP call against the control endpoint
that the running Tauri app exposes (port + bearer token published by the app to
``~/.automixer/control.json``). All validation/clamping and engine-sync stays in
Rust — this file is deliberately dumb plumbing.

Run via uv:  uv run --directory <this dir> server.py
"""

from __future__ import annotations

import json
import os
import urllib.request
from pathlib import Path
from typing import Any

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("automixer")

CONTROL_FILE = Path(os.environ.get("AUTOMIXER_CONTROL_FILE", str(Path.home() / ".automixer" / "control.json")))


def _control() -> tuple[str, str]:
    """Read the control surface's base URL + bearer token published by the app."""
    cfg = json.loads(CONTROL_FILE.read_text())
    return cfg["baseUrl"], cfg["token"]


def _request(method: str, path: str, body: dict | None = None, timeout: float = 30) -> Any:
    base, token = _control()
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        f"{base}{path}",
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def _track_summary(project: dict) -> list[dict]:
    """Trim the full project to the fields the agent needs to reason about tracks."""
    tracks = project.get("session", {}).get("tracks", [])
    out = []
    for t in tracks:
        entry = {
            "id": t.get("id"),
            "name": t.get("name"),
            "kind": t.get("kind", "audio"),
            "role": t.get("role"),
            "gainDb": t.get("gainDb"),
            "pan": t.get("pan"),
            "muted": t.get("muted"),
            "solo": t.get("solo"),
        }
        if t.get("kind") == "video":
            clips = []
            for c in t.get("videoClips", []):
                lay = c.get("layout") or {}
                clips.append({
                    "id": c.get("id"),
                    "name": c.get("name"),
                    "crop": {
                        "top": lay.get("cropTop", 0),
                        "right": lay.get("cropRight", 0),
                        "bottom": lay.get("cropBottom", 0),
                        "left": lay.get("cropLeft", 0),
                    },
                })
            entry["clips"] = clips
        out.append(entry)
    return out


@mcp.tool()
def get_session(session_id: str) -> dict:
    """Return the AutoMixer session's tracks (id, name, role, gain, pan, mute, solo,
    and for video tracks their clips + crop).

    IMPORTANT — respect the user's selection: the response includes
    `selectedTrackIds`, and every track has `selected: true/false`.

    FOCUS MODE — if exactly ONE video track is selected, the user is editing THAT track
    in isolation. You MUST:
      • act ONLY on the selected track — never read, change, or even mention the other
        cameras;
      • for a LOOK/color/grade change ("make it vivid", "warmer", "cinematic"), call
        edit_video with the look in `instructions`. It RENDERS the selected track with
        the grade as a new output and does NOT touch the source recording. Rendering is
        expected — that is how the look is produced.
      • NEVER bake a look into the source by calling set_clip_layout with
        saturation/brightness/contrast — that permanently alters the original footage,
        which the user does not want. set_clip_layout is only for geometry the user asks
        to persist (crop/reframe/rotate), not for color grading.
    So "make it vivid" on a selected track = edit_video(instructions="vivid, saturated").
    The sources stay exactly as recorded.

    If several tracks are selected, act on exactly those (edit_video cuts a multicam edit
    of them). If nothing is selected and the user named specific cameras, call
    select_tracks(...) first to scope the edit to exactly those source tracks — do not
    fall back to "all tracks". Never include the "Agent video edit" output as a source."""
    project = _request("GET", f"/control/session/{session_id}")
    selected = set()
    try:
        sel = _request("GET", f"/control/session/{session_id}/selection")
        selected = set(sel.get("trackIds") or [])
    except Exception:
        selected = set()
    tracks = _track_summary(project)
    for t in tracks:
        t["selected"] = t.get("id") in selected
    return {
        "sessionId": session_id,
        "selectedTrackIds": sorted(selected),
        "tracks": tracks,
    }


@mcp.tool()
def select_tracks(session_id: str, track_ids: list[str]) -> dict:
    """Set the user's track selection to exactly `track_ids` (use the ids from
    get_session). This is how you scope an edit to specific cameras: select the source
    tracks you want, then call edit_video — it edits only the selected tracks. Pass the
    SOURCE camera tracks (e.g. Video 9, Video 10), never the "Agent video edit" output.
    Selecting a single track puts you in focus mode (edit just that one). Pass an empty
    list to clear the selection."""
    return _request("POST", f"/control/session/{session_id}/selection", {"trackIds": track_ids})


@mcp.tool()
def set_track_gain(session_id: str, track_id: str, gain_db: float) -> dict:
    """Set a track's volume in decibels (clamped by the app to [-24, +24] dB)."""
    project = _request(
        "POST",
        f"/control/session/{session_id}/actions",
        {
            "actions": [{"tool": "set_track_gain", "trackId": track_id, "gainDb": gain_db}],
            "explanation": "hermes: set_track_gain",
        },
    )
    applied = next((t for t in _track_summary(project) if t["id"] == track_id), None)
    return {"ok": True, "track": applied}


@mcp.tool()
def apply_actions(session_id: str, actions: list[dict], explanation: str = "") -> dict:
    """Apply one or more mixing actions to the session (the full audio surface). Each
    action is a flat object with a `tool` discriminator (snake_case) and camelCase
    fields; trackIds/regionIds come from get_session. Combine many in one call.

    Available actions:
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

    All changes are reversible via undo. Returns the updated track summary.
    """
    project = _request(
        "POST",
        f"/control/session/{session_id}/actions",
        {"actions": actions, "explanation": explanation or "hermes: apply_actions"},
    )
    return {"ok": True, "tracks": _track_summary(project)}


@mcp.tool()
def undo(session_id: str) -> dict:
    """Undo the most recent change to the session."""
    project = _request("POST", f"/control/session/{session_id}/undo")
    return {"ok": True, "tracks": _track_summary(project)}


@mcp.tool()
def redo(session_id: str) -> dict:
    """Redo the change that was just undone."""
    project = _request("POST", f"/control/session/{session_id}/redo")
    return {"ok": True, "tracks": _track_summary(project)}


@mcp.tool()
def edit_video(session_id: str, instructions: str = "", interval_seconds: float = 1.0) -> dict:
    """Analyze and edit this session's video. The configured video model "sees" the
    frames; the tool renders a multicam cut + color grade. `instructions` guides the
    look/pacing (e.g. "cinematic, cut on the beat"). `interval_seconds` sets how often
    it samples a frame (smaller = more cuts). Returns the output path, number of cuts,
    and the inferred look preset. Slow (frame analysis + ffmpeg render can take minutes).

    Scope: automatically targets the user's SELECTED video tracks (see
    get_session.selectedTrackIds); it does not edit unselected cameras. Re-running it
    REPLACES the single "Agent video edit" output in place — it never stacks a new copy
    — so it's safe to iterate (e.g. bump a clip's saturation, then re-run)."""
    body = {"instructions": instructions, "intervalSeconds": interval_seconds}
    return _request("POST", f"/control/session/{session_id}/video-edit", body, timeout=1800)


@mcp.tool()
def set_clip_layout(
    session_id: str,
    track_id: str,
    clip_id: str,
    crop_top: float | None = None,
    crop_right: float | None = None,
    crop_bottom: float | None = None,
    crop_left: float | None = None,
    x: float | None = None,
    y: float | None = None,
    width: float | None = None,
    height: float | None = None,
    rotation: float | None = None,
    opacity: float | None = None,
    brightness: float | None = None,
    contrast: float | None = None,
    saturation: float | None = None,
    blur: float | None = None,
) -> dict:
    """Persistently change a SOURCE clip's geometry. `crop_*` = percent to REMOVE from
    each edge (0-45). x/y/width/height = percent of the canvas (reposition/resize).
    rotation = degrees. Get track_id + clip_id from get_session (video tracks list
    their clips). Only pass the fields you want to change. Takes effect on the next
    video render; the monitor previews it live.

    This MODIFIES the original recording, so use it only when the user explicitly wants
    a permanent geometry change. For a LOOK/color grade (vivid, warm, cinematic) do NOT
    use the saturation/brightness/contrast fields here — call edit_video instead, which
    grades the rendered OUTPUT and leaves the source footage untouched."""
    fields = {
        "trackId": track_id, "clipId": clip_id,
        "cropTop": crop_top, "cropRight": crop_right, "cropBottom": crop_bottom, "cropLeft": crop_left,
        "x": x, "y": y, "width": width, "height": height,
        "rotation": rotation, "opacity": opacity,
        "brightness": brightness, "contrast": contrast, "saturation": saturation, "blur": blur,
    }
    body = {k: v for k, v in fields.items() if v is not None}
    return _request("POST", f"/control/session/{session_id}/clip-layout", body, timeout=60)


@mcp.tool()
def auto_crop(session_id: str, track_id: str, clip_id: str, instructions: str = "") -> dict:
    """Let the video model LOOK at a frame of the clip and auto-crop it to satisfy
    `instructions` (e.g. 'keep the singer centered', 'reframe to portrait', 'tighten
    on the face'). Use when you don't have exact crop numbers. Get ids from get_session."""
    body = {"trackId": track_id, "clipId": clip_id, "instructions": instructions}
    return _request("POST", f"/control/session/{session_id}/auto-crop", body, timeout=300)


@mcp.tool()
def auto_mix(session_id: str, stages: list[str] | None = None) -> dict:
    """Run AutoMixer's auto-mix pipeline on this session and return a summary (stages
    run + total actions applied). The full pipeline is 10 stages: raw_session_prep,
    prep_intent, static_balance, cleanup_filters, subtractive_eq, dynamics,
    tonal_enhancement, depth_space, section_automation, mix_bus_loudness. Pass a subset
    via `stages` to run only those. Slow — each stage calls the mix model."""
    body = {"stages": stages} if stages else {}
    return _request("POST", f"/control/session/{session_id}/auto-mix", body, timeout=1800)


if __name__ == "__main__":
    mcp.run()
