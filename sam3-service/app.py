"""SAM 3 video-segmentation sidecar (HTTP).

Runs on the CUDA box (the Spark). POST a video + a text prompt ("the guitarist",
"the red car") and it segments/tracks every matching object through the whole
video, serving back a matte (mask.mp4), a cutout over black, and a QC overlay.

Async job model — the request returns instantly and the client polls.
"""
import os
import shutil
import threading
import traceback
import uuid

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse

from sam3_infer import Sam3Engine

JOB_DIR = os.environ.get("SAM3_JOB_DIR", "/tmp/sam3-jobs")
os.makedirs(JOB_DIR, exist_ok=True)

ENGINE = Sam3Engine(idle_unload_s=int(os.environ.get("SAM3_IDLE_UNLOAD_S", "900")))
JOBS: dict[str, dict] = {}

app = FastAPI(title="sam3-service")


@app.get("/health")
def health():
    return {"status": "ok", **ENGINE.status()}


def _run_job(job_id: str, in_path: str, prompt: str, cutout: bool, overlay: bool, background_path: str | None):
    job = JOBS[job_id]
    out_dir = os.path.join(JOB_DIR, job_id)
    os.makedirs(out_dir, exist_ok=True)
    try:
        job["status"] = "running"
        meta = ENGINE.segment(in_path, prompt, out_dir, want_cutout=cutout, want_overlay=overlay,
                              background_path=background_path)
        job.update(status="done", **meta)
    except Exception as e:  # noqa: BLE001 — surface any failure to the client
        job.update(status="error", error=f"{e}\n{traceback.format_exc()}")
    finally:
        for path in (in_path, background_path):
            if path:
                try:
                    os.remove(path)
                except OSError:
                    pass


@app.post("/segment")
async def segment(
    file: UploadFile = File(...),
    prompt: str = Form(...),
    cutout: bool = Form(True),
    overlay: bool = Form(True),
    background: UploadFile | None = File(None),
):
    job_id = uuid.uuid4().hex
    in_path = os.path.join(JOB_DIR, f"{job_id}-in.mp4")
    with open(in_path, "wb") as f:
        while chunk := await file.read(1 << 20):
            f.write(chunk)
    background_path = None
    if background is not None:
        suffix = os.path.splitext(background.filename or "bg.png")[1] or ".png"
        background_path = os.path.join(JOB_DIR, f"{job_id}-bg{suffix}")
        with open(background_path, "wb") as f:
            while chunk := await background.read(1 << 20):
                f.write(chunk)
    JOBS[job_id] = {"status": "queued", "prompt": prompt}
    threading.Thread(target=_run_job, args=(job_id, in_path, prompt, cutout, overlay, background_path), daemon=True).start()
    return {"job_id": job_id}


@app.get("/jobs/{job_id}")
def job_status(job_id: str):
    job = JOBS.get(job_id)
    if not job:
        raise HTTPException(404, "unknown job")
    return job


def _serve(job_id: str, name: str):
    path = os.path.join(JOB_DIR, job_id, name)
    if not os.path.exists(path):
        raise HTTPException(404, f"{name} not ready")
    return FileResponse(path, filename=name)


@app.get("/jobs/{job_id}/mask.mp4")
def job_mask(job_id: str):
    return _serve(job_id, "mask.mp4")


@app.get("/jobs/{job_id}/cutout.mp4")
def job_cutout(job_id: str):
    return _serve(job_id, "cutout.mp4")


@app.get("/jobs/{job_id}/overlay.mp4")
def job_overlay(job_id: str):
    return _serve(job_id, "overlay.mp4")


@app.get("/jobs/{job_id}/composite.mp4")
def job_composite(job_id: str):
    return _serve(job_id, "composite.mp4")


@app.delete("/jobs/{job_id}")
def job_delete(job_id: str):
    JOBS.pop(job_id, None)
    shutil.rmtree(os.path.join(JOB_DIR, job_id), ignore_errors=True)
    return {"ok": True}
