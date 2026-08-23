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

import asyncio
import json
import os
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("automixer")

CONTROL_FILE = Path(os.environ.get("AUTOMIXER_CONTROL_FILE", str(Path.home() / ".automixer" / "control.json")))
LONG_RUNNING_HTTP_TIMEOUT_SECONDS = 2 * 60 * 60


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
    """Run a blocking control-surface request without blocking MCP keepalives."""
    return await asyncio.to_thread(_request, method, path, body, timeout)


def _track_summary(project: dict) -> list[dict]:
    """Trim the full project to the fields the agent needs to reason about tracks."""
    session = project.get("session", {})
    tracks = session.get("tracks", [])
    source_analysis = {
        source.get("id"): source.get("analysis")
        for source in session.get("sourceFiles", [])
        if source.get("id")
    }
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
        else:
            # Audio decisions must be based on the actual source, not a generic preset.
            # Include the persisted processor state as well so a follow-up get_session
            # can verify more than the fader value.
            entry["analysis"] = source_analysis.get(t.get("sourceFileId"))
            entry["chain"] = t.get("chain")
            entry["sends"] = t.get("sends")
        out.append(entry)
    return out


def _track_summary_compact(project: dict) -> list[dict]:
    """A lean mutation result that still proves persisted audio processing."""
    out = []
    for t in project.get("session", {}).get("tracks", []):
        entry = {
            "id": t.get("id"),
            "name": t.get("name"),
            "kind": t.get("kind", "audio"),
            "gainDb": t.get("gainDb"),
            "muted": t.get("muted"),
        }
        if t.get("kind") != "video":
            entry["chain"] = t.get("chain")
            entry["sends"] = t.get("sends")
        out.append(entry)
    return out


@mcp.tool()
def get_session(session_id: str) -> dict:
    """Return the AutoMixer session's tracks. Audio tracks include source analysis,
    processor chain, and sends; video tracks include clips and crop. Also returns
    `selectedTrackIds` and a `selected` flag on each track. Respect the user's selection
    — if any tracks are selected, act only on those. For audio work see the audio-mixing
    skill; for video work see the video-editing skill."""
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
    NOT gain alone. For a whole-mix request, build the measured full processing chain
    in this one batched action call. ALWAYS read the **audio-mixing** skill first for
    the vocabulary and the doctrine."""
    project = _request(
        "POST",
        f"/control/session/{session_id}/actions",
        {"actions": actions, "explanation": explanation or "hermes: apply_actions"},
    )
    return {"ok": True, "tracks": _track_summary_compact(project)}


@mcp.tool()
async def create_mix_track(
    session_id: str,
    track_ids: list[str] | None = None,
    name: str = "",
    mono: bool = False,
    include_master: bool = False,
    mute_sources: bool = False,
) -> dict:
    """Bounce selected audio tracks through their current processing into a new track.

    Normally omit ``track_ids`` so the tool uses the user's current track selection.
    The bounce includes clip edits, gain, pan, filters, EQ, compression, automation, and
    shared sends. Master processing is excluded by default so it is applied exactly once
    when the new track plays in the project. Set ``mono`` for a mono dialogue bounce.

    Sources are preserved. To prevent doubled playback, the new mix track starts muted
    unless ``mute_sources`` is true; with ``mute_sources=true`` the selected sources are
    muted and the new mix track is audible. The whole operation is one undoable change.
    ``include_master=true`` bakes the current master into the file and is intended only
    when the user explicitly requests a master-processed print.
    """
    body = {
        "trackIds": track_ids or [],
        "name": name.strip() or None,
        "mono": mono,
        "includeMaster": include_master,
        "muteSources": mute_sources,
    }
    result = await _request_async(
        "POST",
        f"/control/session/{session_id}/mix-track",
        body,
        timeout=LONG_RUNNING_HTTP_TIMEOUT_SECONDS,
    )
    return {
        "ok": True,
        "mixTrackId": result.get("mixTrackId"),
        "mixTrackName": result.get("mixTrackName"),
        "sourceTrackIds": result.get("sourceTrackIds", []),
        "channels": result.get("channels"),
        "includedMaster": result.get("includedMaster", False),
        "sourcesMuted": result.get("sourcesMuted", False),
        "mixTrackMuted": result.get("mixTrackMuted", True),
    }


@mcp.tool()
async def clean_podcast_audio(
    session_id: str,
    track_ids: list[str] | None = None,
    prompt: str = "",
) -> dict:
    """Run the high-quality SAM-Audio spoken-voice cleanup stage for podcast audio.

    This is a long-running, non-destructive operation. Before processing, AutoMixer posts
    a visible notice that audio is being sent to the configured SAM-Audio endpoint. For
    each successful microphone it creates a new ``<original> · Clean Voice`` track,
    preserves and mutes the original mic, and inherits the mic's downstream gain/EQ/
    compression on the cleaned replacement. It does NOT leave the separated background
    residual audible. The project history can undo each replacement.

    Pass explicit audio ``track_ids`` to scope the cleanup. If omitted, the current audio
    selection is used; if no tracks are selected, all unmuted original audio tracks are
    cleaned. An explicitly selected generated mix/bounce track (role ``mix``), including
    ``Selected Tracks · Mix``, is valid input and should be cleaned directly when the user
    asks. Do not call this on an existing Clean Voice track or another generated stem. Use the
    optional ``prompt`` only to describe the sound to retain with a short lowercase noun
    or verb phrase such as ``woman speaking`` or ``people speaking``. Normally omit it:
    AutoMixer uses ``person speaking`` for an individual microphone and ``people speaking``
    for a mix/bounce that may contain a conversation. Never put denoising instructions or
    a production brief in this field: SAM-Audio may classify the requested voice as residual.
    """
    body: dict[str, Any] = {"trackIds": track_ids or []}
    if prompt.strip():
        body["prompt"] = prompt.strip()
    return await _request_async(
        "POST",
        f"/control/session/{session_id}/podcast-cleanup",
        body,
        timeout=LONG_RUNNING_HTTP_TIMEOUT_SECONDS,
    )


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
def reset_all_changes(session_id: str) -> dict:
    """Hard-reset all edits and restore the project's original imported state.

    Use this when the user asks for a complete reset, hard reset, untouched project,
    original starting state, or to remove every change. This reverses the entire edit
    history (both agent and manual edits) while preserving the imported audio/video,
    source files, and tracks. It is deliberately different from clearing/deleting a
    session. The reverted history remains redoable until a new edit is made. Do not call
    ``undo`` repeatedly for this request; call this tool exactly once.
    """
    result = _request("POST", f"/control/session/{session_id}/reset-all-changes")
    project = result.get("project", {})
    return {
        "ok": True,
        "status": result.get("status"),
        "revertedEntries": result.get("revertedEntries", 0),
        "message": result.get("message", "Project reset completed."),
        "tracks": _track_summary_compact(project),
    }


@mcp.tool()
def edit_video(
    session_id: str,
    instructions: str = "",
    interval_seconds: float = 0.5,
    review_only: bool = False,
) -> dict:
    """Create a directed multicam video. The configured video model "sees" the frames,
    proposes the cuts, renders the MP4, and adds/updates the ``Agent video edit`` track.
    `instructions` guides source roles, constraints, look, and pacing
    (e.g. "cinematic, cut on the beat"). `interval_seconds` sets the audio/frame decision
    resolution. The 0.5-second default is appropriate for speaker-aware conversation edits.

    IMPORTANT: this returns IMMEDIATELY with {"status":"started"}. Analysis and rendering
    continue in the BACKGROUND and the user sees live progress. After calling it, say that
    rendering started — do NOT claim it is already finished, call it again to "check", or
    call get_session expecting the output immediately. One edit runs at a time; another
    call while it is running returns a 409.

    Scope: automatically targets the user's SELECTED video tracks (see
    get_session.selectedTrackIds); it does not analyze unselected cameras. In conversation
    sessions it automatically pairs participant microphones and cameras by labels such as
    speaker-a/camera-a or a shared participant name. Clear speech selects that participant's
    camera; an unpaired room overview is reserved for brief pauses or overlap. Set
    ``review_only=true`` only when the user explicitly asks to inspect/approve the cut
    plan before rendering; that mode stops at the editable plan and creates no output
    track until the user chooses to render it."""
    body = {
        "instructions": instructions,
        "intervalSeconds": interval_seconds,
        "reviewOnly": review_only,
    }
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


if __name__ == "__main__":
    mcp.run()
