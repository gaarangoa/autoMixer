"""AutoMixer audio-analysis sidecar.

Runs `all-in-one` for music structure detection (beats, downbeats, sections)
behind a small FastAPI server. Results are cached on disk by file content +
mtime so re-analysis of the same mix is free.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import threading
from pathlib import Path

import traceback

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel

logger = logging.getLogger("automixer.audio")
logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")

CACHE_DIR = Path(
    os.environ.get(
        "AUTOMIXER_AUDIO_CACHE",
        Path.home() / ".automixer" / "audio-cache",
    )
)
CACHE_DIR.mkdir(parents=True, exist_ok=True)

app = FastAPI(title="AutoMixer Audio Service")
_analyze_lock = threading.Lock()


@app.exception_handler(Exception)
async def _unhandled(_: Request, exc: Exception):
    logger.error("unhandled exception: %s\n%s", exc, traceback.format_exc())
    return JSONResponse(
        status_code=500,
        content={
            "detail": f"{type(exc).__name__}: {exc}",
            "traceback": traceback.format_exc(),
        },
    )
_allin1 = None  # lazy import: torch+demucs are heavy

_progress: dict = {"stage": "idle", "message": "", "startedAt": 0.0}
_progress_lock = threading.Lock()


def _set_stage(stage: str, message: str = "") -> None:
    import time as _t
    with _progress_lock:
        _progress["stage"] = stage
        _progress["message"] = message
        if stage == "idle" or stage == "done":
            _progress["startedAt"] = 0.0
        elif not _progress.get("startedAt"):
            _progress["startedAt"] = _t.time()
    logger.info("[stage] %s — %s", stage, message)


@app.get("/status")
def status():
    import time as _t
    with _progress_lock:
        snap = dict(_progress)
    started = snap.get("startedAt") or 0.0
    snap["elapsedSeconds"] = round(_t.time() - started, 1) if started else 0.0
    return snap


class StructureRequest(BaseModel):
    wav_path: str


class Section(BaseModel):
    start: float
    end: float
    label: str


class StructureResponse(BaseModel):
    bpm: float
    beats: list[float]
    downbeats: list[float]
    sections: list[Section]


def _cache_key(wav_path: str) -> str:
    p = Path(wav_path).resolve()
    h = hashlib.sha1()
    h.update(str(p).encode())
    if p.exists():
        st = p.stat()
        h.update(f"{st.st_size}:{st.st_mtime_ns}".encode())
    return h.hexdigest()


def _load_allin1():
    global _allin1
    if _allin1 is None:
        logger.info("loading allin1 (first call — pulls in torch + demucs)")
        import allin1  # noqa: WPS433
        _allin1 = allin1
    return _allin1


def _device() -> str:
    """Pick the best torch device.

    Tried MPS on M1 + natten 0.15.1 and the C++ attention kernel aborts the
    worker process without a recoverable Python exception (the model prints
    "NATTEN does not support mps:0 devices yet." and exits). So Apple Silicon
    is forced to CPU here. CUDA is fine when available.
    """
    try:
        import torch
    except Exception:
        return "cpu"
    if torch.cuda.is_available():
        return "cuda"
    return "cpu"


@app.get("/health")
def health():
    return {"ok": True, "service": "automixer-audio", "device": _device()}


@app.post("/analyze/structure", response_model=StructureResponse)
def analyze_structure(req: StructureRequest):
    wav_path = req.wav_path
    if not Path(wav_path).exists():
        raise HTTPException(status_code=404, detail=f"file not found: {wav_path}")

    cache_path = CACHE_DIR / f"{_cache_key(wav_path)}.json"
    if cache_path.exists():
        try:
            return json.loads(cache_path.read_text())
        except Exception:
            cache_path.unlink(missing_ok=True)

    with _analyze_lock:
        if cache_path.exists():
            try:
                return json.loads(cache_path.read_text())
            except Exception:
                cache_path.unlink(missing_ok=True)

        try:
            _set_stage("loading_model", "Loading allin1 (first call pulls torch + demucs + checkpoints)")
            allin1 = _load_allin1()
            device = _device()
            logger.info("analyze_structure path=%s device=%s", wav_path, device)
            _set_stage(
                "analyzing",
                f"Running structure detection on {device.upper()} (demucs separation + transformer inference)",
            )
            results = allin1.analyze(paths=[wav_path], device=device, keep_byproducts=False)
            result = results[0]
            _set_stage("finalizing", "Caching result")
        except Exception:
            _set_stage("idle", "")
            raise

        sections = [
            Section(start=float(s.start), end=float(s.end), label=str(s.label))
            for s in getattr(result, "segments", [])
        ]
        response = StructureResponse(
            bpm=float(getattr(result, "bpm", 0.0) or 0.0),
            beats=[float(b) for b in getattr(result, "beats", [])],
            downbeats=[float(d) for d in getattr(result, "downbeats", [])],
            sections=sections,
        )
        cache_path.write_text(response.model_dump_json())
        _set_stage("done", f"{len(sections)} sections, {response.bpm:.0f} bpm")
        return response
