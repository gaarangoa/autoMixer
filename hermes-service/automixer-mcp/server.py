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
import asyncio
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("automixer")

CONTROL_FILE = Path(os.environ.get("AUTOMIXER_CONTROL_FILE", str(Path.home() / ".automixer" / "control.json")))
AUTO_MIX_HTTP_TIMEOUT_SECONDS = 2 * 60 * 60


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
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        # Surface the server's error BODY (e.g. "action 9 (set_compressor): missing field
        # `releaseMs`") so the agent can fix it, instead of a bare "HTTP Error 400".
        detail = ""
        try:
            detail = exc.read().decode().strip()
        except Exception:
            detail = ""
        raise RuntimeError(f"{exc.code} {exc.reason}: {detail}" if detail else f"{exc.code} {exc.reason}") from None


async def _request_async(method: str, path: str, body: dict | None = None, timeout: float = 30) -> Any:
    return await asyncio.to_thread(_request, method, path, body, timeout)


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


def _track_summary_compact(project: dict) -> list[dict]:
    """A lean view (no video clips, no filters) for mutation tools to echo back — full
    state is one get_session away, so we don't bloat the agent's context on every edit."""
    out = []
    for t in project.get("session", {}).get("tracks", []):
        out.append({
            "id": t.get("id"),
            "name": t.get("name"),
            "gainDb": t.get("gainDb"),
            "muted": t.get("muted"),
        })
    return out


@mcp.tool()
def get_session(session_id: str) -> dict:
    """Return the AutoMixer session's tracks (id, name, role, gain, pan, mute, solo, and
    for video tracks their clips + crop), plus `selectedTrackIds` and a `selected` flag on
    each track. Respect the user's selection — if any tracks are selected, act only on
    those. For audio work see the audio-mixing skill; for video work see the
    video-editing skill (selection/focus-mode rules and which tool to use live there)."""
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
def apply_actions(session_id: str, actions: list[dict], explanation: str = "") -> dict:
    """Apply one or more audio mixing actions (gain, pan, mute/solo, EQ, filters,
    compression, sends, regions, automation, master). Each action is a flat object with a
    snake_case `tool` discriminator and camelCase fields; trackIds/regionIds come from
    get_session. Combine many in one call; all changes are reversible via undo.

    A real mix is PROCESSING, not just faders: for a "professional / clear / punchy / make
    it sit / cut through" request, use set_eq_band + set_compressor + high-pass + sends —
    NOT gain alone. For a whole-mix request, consider auto_mix. ALWAYS read the
    **audio-mixing** skill first for the vocabulary and the doctrine."""
    project = _request(
        "POST",
        f"/control/session/{session_id}/actions",
        {"actions": actions, "explanation": explanation or "hermes: apply_actions"},
    )
    return {"ok": True, "tracks": _track_summary_compact(project)}


@mcp.tool()
def undo(session_id: str) -> dict:
    """Undo the most recent change to the session."""
    project = _request("POST", f"/control/session/{session_id}/undo")
    return {"ok": True, "tracks": _track_summary_compact(project)}


@mcp.tool()
def redo(session_id: str) -> dict:
    """Redo the change that was just undone."""
    project = _request("POST", f"/control/session/{session_id}/redo")
    return {"ok": True, "tracks": _track_summary_compact(project)}


@mcp.tool()
def edit_video(session_id: str, instructions: str = "", interval_seconds: float = 1.0) -> dict:
    """Start a review-first multicam video plan. The configured video model "sees" the
    frames and proposes a directed cut plan. `instructions` guides source roles,
    constraints, look, and pacing
    (e.g. "cinematic, cut on the beat"). `interval_seconds` sets how often it samples a
    frame (smaller = more cuts; 2-3 is a good default — 1 is fine-grained but slower).

    IMPORTANT: this returns IMMEDIATELY with {"status":"started"}. Planning runs in the
    BACKGROUND and the user sees live progress. When ready, AutoMixer opens the editable
    plan + directing contract; the user reviews it and clicks Process to render. So after
    calling this, say the PLAN is being generated — do NOT claim rendering is finished,
    do NOT call it again to "check", and do
    NOT call get_session expecting the new track yet. One edit runs at a time; calling
    again while one is planning returns a 409 (already running).

    Scope: automatically targets the user's SELECTED video tracks (see
    get_session.selectedTrackIds); it does not analyze unselected cameras. No output
    track is created until the user approves the plan and clicks Process."""
    body = {"instructions": instructions, "intervalSeconds": interval_seconds}
    return _request("POST", f"/control/session/{session_id}/video-edit", body, timeout=60)


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
    """Persistently change a SOURCE clip's GEOMETRY (crop/reframe/rotate/reposition). Get
    track_id + clip_id from get_session. Only pass fields you want to change. This MODIFIES
    the original recording — use it only for a permanent geometry change. For a LOOK/color
    grade, use edit_video instead (do NOT use the brightness/contrast/saturation fields
    here). See the **video-editing** skill for the field meanings/ranges and which video
    tool to use for which request."""
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
def apply_video_effects(
    session_id: str,
    track_id: str,
    clip_id: str,
    fade_in_seconds: float | None = None,
    fade_out_seconds: float | None = None,
    speed_factor: float | None = None,
) -> dict:
    """Apply a fade-in, fade-out, and/or speed change to a video clip — the simple way.
    This is what you use for requests like "fade in 2 seconds and fade out 10 seconds"
    or "make it half speed". It re-encodes the clip's video in place (fast — no multicam
    re-edit, no model calls) and is reversible. Get track_id + clip_id from get_session
    (video tracks list their clips); for the agent's rendered output use the
    "Agent video edit" track's clip. fade_in_seconds / fade_out_seconds are 0-10;
    speed_factor is 0.25-4 (1 = normal, 0.5 = half speed, 2 = 2x). Pass only what you
    want to change. Re-applies from the original each time, so changing the fade values
    doesn't stack. (Do NOT try to fake a video fade with audio gain/automation — use
    this.)"""
    fields = {
        "trackId": track_id,
        "clipId": clip_id,
        "fadeInSeconds": fade_in_seconds,
        "fadeOutSeconds": fade_out_seconds,
        "speedFactor": speed_factor,
    }
    body = {k: v for k, v in fields.items() if v is not None}
    return _request("POST", f"/control/session/{session_id}/clip-effects", body, timeout=600)


@mcp.tool()
def auto_crop(session_id: str, track_id: str, clip_id: str, instructions: str = "") -> dict:
    """Let the video model LOOK at a frame of the clip and auto-crop it to satisfy
    `instructions` (e.g. 'keep the singer centered', 'reframe to portrait', 'tighten
    on the face'). Use when you don't have exact crop numbers. Get ids from get_session."""
    body = {"trackId": track_id, "clipId": clip_id, "instructions": instructions}
    return _request("POST", f"/control/session/{session_id}/auto-crop", body, timeout=300)


@mcp.tool()
async def auto_mix(session_id: str, stages: list[str] | None = None) -> dict:
    """Run AutoMixer auto-mix STAGES and return a report (actions applied + rationale).
    Each stage is slow on local llama.cpp (~3-8 min, calls the mix model). ALWAYS call
    this ONE STAGE AT A TIME (pass exactly one stage in `stages`) and report progress
    to the user between calls. Do NOT call with no stages or many stages unless the
    user explicitly asks for a single opaque background run; a full run can exceed
    30 minutes. Stage order: prep_intent, static_balance, cleanup_filters,
    subtractive_eq, dynamics, tonal_enhancement, depth_space, mix_bus_loudness
    (raw_session_prep and section_automation are optional). See the auto-mix skill."""
    body = {"stages": stages} if stages else {}
    return await _request_async(
        "POST",
        f"/control/session/{session_id}/auto-mix",
        body,
        timeout=AUTO_MIX_HTTP_TIMEOUT_SECONDS,
    )


if __name__ == "__main__":
    mcp.run()
